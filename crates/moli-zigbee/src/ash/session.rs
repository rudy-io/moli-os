//! ASH session: the link state machine on top of the frame codec (UG101).
//!
//! Pure: no I/O and no clock of its own. The caller feeds the bytes it reads
//! with the current time, writes whatever [`Session::poll_transmit`] hands
//! back, and wakes up at [`Session::next_timeout`] to call
//! `on_bytes(&[], now)` so timer expiries surface as events.

use std::time::{Duration, Instant};

use super::codec::CANCEL;
use super::{AshError, Decoder, Frame};

/// What the link reports to the EZSP layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// The NCP answered our reset: the link is up.
    Connected { reset_code: u8 },
    /// An EZSP frame from the NCP, in order and exactly once.
    Received(Vec<u8>),
    /// Our frame in flight was acknowledged: [`Session::send`] is free again.
    Delivered,
    /// The NCP reset on its own after the link was up.
    NcpReset { reset_code: u8 },
    /// The NCP reported a fatal error and stopped the link.
    NcpError { code: u8 },
    /// Our frame was never acknowledged despite the retransmissions.
    Failed,
}

/// The session cannot take a frame now: one is awaiting its ACK, or the link is down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Busy;

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ASH link busy or down")
    }
}

impl std::error::Error for Busy {}

/// First retransmission timeout, before any ACK time is measured.
const INITIAL_TIMEOUT: Duration = Duration::from_millis(1600);
/// Bounds of the adaptive retransmission timeout.
const MIN_TIMEOUT: Duration = Duration::from_millis(400);
const MAX_TIMEOUT: Duration = Duration::from_millis(3200);
/// Timer expiries (each followed by a retransmission) before the link fails.
const MAX_RETRANSMISSIONS: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Never reset, or stopped (failure, NCP reset or error): waits for a reset.
    Down,
    /// RST sent, waiting for the RSTACK.
    Resetting,
    Connected,
}

/// Our DATA frame awaiting its ACK (window of one).
#[derive(Debug)]
struct InFlight {
    frm: u8,
    payload: Vec<u8>,
    /// When the last copy was written; `None` while a copy is due.
    sent_at: Option<Instant>,
    /// Copies written so far; from the second one on, they are marked `re_tx`.
    copies: u8,
    /// Timer expiries so far.
    timeouts: u8,
}

/// The ASH link with the NCP.
#[derive(Debug)]
pub struct Session {
    decoder: Decoder,
    state: State,
    /// Number of our next new DATA frame.
    tx_seq: u8,
    /// Number of the next DATA frame expected from the NCP: our ack number.
    rx_seq: u8,
    in_flight: Option<InFlight>,
    /// ACK or NAK due; a newer one replaces it.
    control: Option<Frame>,
    /// A NAK went out for `rx_seq`: no other until a frame is accepted (the
    /// UG101 reject condition), so a burst of line noise costs one NAK.
    rejecting: bool,
    /// Current retransmission timeout.
    timeout: Duration,
    /// Events not handed out yet (a failure found by `poll_transmit`).
    events: Vec<Event>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    /// A session that stays down until [`Session::reset_request`] is written.
    #[must_use]
    pub fn new() -> Self {
        Self {
            decoder: Decoder::default(),
            state: State::Down,
            tx_seq: 0,
            rx_seq: 0,
            in_flight: None,
            control: None,
            rejecting: false,
            timeout: INITIAL_TIMEOUT,
            events: Vec::new(),
        }
    }

    /// Start over: forget every frame and return the bytes to write (a cancel
    /// byte to flush the NCP receiver, then RST). The RSTACK brings
    /// [`Event::Connected`]; how long to wait for it is up to the caller.
    pub fn reset_request(&mut self) -> Vec<u8> {
        *self = Self::new();
        self.state = State::Resetting;
        let mut bytes = vec![CANCEL];
        bytes.extend(Frame::Rst.encode());
        bytes
    }

    /// Feed the bytes read from the NCP (possibly none, to run the timer)
    /// and collect what happened. Drain [`Session::poll_transmit`] after.
    pub fn on_bytes(&mut self, bytes: &[u8], now: Instant) -> Vec<Event> {
        for &byte in bytes {
            match self.decoder.push(byte) {
                Some(Ok(frame)) => self.on_frame(frame, now),
                // A cancel byte only discards a partial frame.
                Some(Err(AshError::Cancelled)) | None => {}
                Some(Err(_)) => {
                    if self.state == State::Connected {
                        self.reject();
                    }
                }
            }
        }
        // After the frames: an ACK in this very read beats the timer.
        self.tick(now);
        std::mem::take(&mut self.events)
    }

    /// Queue an EZSP frame; [`Event::Delivered`] tells when it is acknowledged.
    ///
    /// # Errors
    ///
    /// [`Busy`] while a frame awaits its ACK or the link is down.
    pub fn send(&mut self, payload: Vec<u8>) -> Result<(), Busy> {
        if self.state != State::Connected || self.in_flight.is_some() {
            return Err(Busy);
        }
        self.in_flight = Some(InFlight {
            frm: self.tx_seq,
            payload,
            sent_at: None,
            copies: 0,
            timeouts: 0,
        });
        self.tx_seq = (self.tx_seq + 1) & 7;
        Ok(())
    }

    /// The next frame to write (ACK or NAK first, then our DATA frame or its
    /// retransmission), or `None`. Call it until `None` after every change.
    pub fn poll_transmit(&mut self, now: Instant) -> Option<Vec<u8>> {
        self.tick(now);
        if self.state != State::Connected {
            return None;
        }
        if let Some(control) = self.control.take() {
            return Some(control.encode());
        }
        let ack = self.rx_seq;
        let frame = self.in_flight.as_mut().filter(|f| f.sent_at.is_none())?;
        frame.sent_at = Some(now);
        frame.copies = frame.copies.saturating_add(1);
        Some(
            Frame::Data {
                frm: frame.frm,
                re_tx: frame.copies > 1,
                ack,
                payload: frame.payload.clone(),
            }
            .encode(),
        )
    }

    /// When the retransmission timer of our frame in flight expires, if one
    /// runs. At that instant, call `on_bytes(&[], now)` and drain
    /// [`Session::poll_transmit`].
    #[must_use]
    pub fn next_timeout(&self) -> Option<Instant> {
        if self.state != State::Connected {
            return None;
        }
        let sent_at = self.in_flight.as_ref()?.sent_at?;
        Some(sent_at + self.timeout)
    }

    fn on_frame(&mut self, frame: Frame, now: Instant) {
        match (self.state, frame) {
            (State::Resetting, Frame::RstAck { reset_code, .. }) => {
                self.state = State::Connected;
                self.events.push(Event::Connected { reset_code });
            }
            (State::Connected, Frame::RstAck { reset_code, .. }) => {
                self.stop();
                self.events.push(Event::NcpReset { reset_code });
            }
            (State::Resetting | State::Connected, Frame::Error { code, .. }) => {
                // While resetting, a stale ERROR does not cancel the RSTACK to come.
                if self.state == State::Connected {
                    self.stop();
                }
                self.events.push(Event::NcpError { code });
            }
            (
                State::Connected,
                Frame::Data {
                    frm,
                    re_tx,
                    ack,
                    payload,
                },
            ) => {
                self.on_ack(ack, now);
                if frm == self.rx_seq {
                    self.rx_seq = (self.rx_seq + 1) & 7;
                    self.rejecting = false;
                    self.control = Some(Frame::Ack {
                        ack: self.rx_seq,
                        not_ready: false,
                    });
                    self.events.push(Event::Received(payload));
                } else if re_tx {
                    // Already delivered and our ACK was lost: acknowledge again.
                    self.control = Some(Frame::Ack {
                        ack: self.rx_seq,
                        not_ready: false,
                    });
                } else {
                    self.reject();
                }
            }
            (State::Connected, Frame::Ack { ack, .. }) => self.on_ack(ack, now),
            (State::Connected, Frame::Nak { ack, .. }) => {
                self.on_ack(ack, now);
                // The NCP still expects our frame: send it again now.
                if let Some(frame) = self.in_flight.as_mut().filter(|f| f.frm == ack) {
                    frame.sent_at = None;
                }
            }
            _ => {}
        }
    }

    /// The NCP expects frame `ack` next, which acknowledges our frame in
    /// flight when it is the one before.
    fn on_ack(&mut self, ack: u8, now: Instant) {
        let Some(frame) = &self.in_flight else {
            return;
        };
        if frame.copies == 0 || ack != (frame.frm + 1) & 7 {
            return;
        }
        // Only a frame written once gives an unambiguous ACK time (Karn):
        // t = 7/8 t + 1/2 measured, within bounds (UG101).
        if let (Some(sent_at), 1) = (frame.sent_at, frame.copies) {
            let measured = now.saturating_duration_since(sent_at);
            self.timeout = (self.timeout * 7 / 8 + measured / 2).clamp(MIN_TIMEOUT, MAX_TIMEOUT);
        }
        self.in_flight = None;
        self.events.push(Event::Delivered);
    }

    /// Ask once for the frame we expect, unless a NAK is already out.
    fn reject(&mut self) {
        if !self.rejecting {
            self.rejecting = true;
            self.control = Some(Frame::Nak {
                ack: self.rx_seq,
                not_ready: false,
            });
        }
    }

    /// Run the retransmission timer.
    fn tick(&mut self, now: Instant) {
        if self.state != State::Connected {
            return;
        }
        let timeout = self.timeout;
        let Some(frame) = self.in_flight.as_mut() else {
            return;
        };
        let Some(sent_at) = frame.sent_at else {
            return;
        };
        if now < sent_at + timeout {
            return;
        }
        if frame.timeouts >= MAX_RETRANSMISSIONS {
            self.stop();
            self.events.push(Event::Failed);
        } else {
            frame.timeouts += 1;
            frame.sent_at = None;
            // Back off on loss (UG101), within the ceiling.
            self.timeout = (timeout * 2).min(MAX_TIMEOUT);
        }
    }

    /// The link is down: drop what was in progress until the next reset.
    fn stop(&mut self) {
        self.state = State::Down;
        self.in_flight = None;
        self.control = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSTACK: [u8; 6] = [0xC1, 0x02, 0x02, 0x9B, 0x7B, 0x7E];
    /// An ACK frame whose last CRC byte is wrong.
    const CORRUPT: [u8; 4] = [0x81, 0x60, 0x58, 0x7E];

    /// Decode the frames of a chunk of wire bytes.
    fn frames(bytes: &[u8]) -> Vec<Frame> {
        let mut dec = Decoder::default();
        bytes
            .iter()
            .filter_map(|&b| dec.push(b))
            .map(Result::unwrap)
            .collect()
    }

    /// The single frame the session wants to write now.
    fn next_frame(s: &mut Session, now: Instant) -> Frame {
        let bytes = s.poll_transmit(now).expect("a frame to transmit");
        let mut out = frames(&bytes);
        assert_eq!(out.len(), 1, "one frame per poll");
        out.remove(0)
    }

    fn data(frm: u8, re_tx: bool, ack: u8, payload: &[u8]) -> Frame {
        Frame::Data {
            frm,
            re_tx,
            ack,
            payload: payload.to_vec(),
        }
    }

    fn ack(ack: u8) -> Frame {
        Frame::Ack {
            ack,
            not_ready: false,
        }
    }

    fn nak(ack: u8) -> Frame {
        Frame::Nak {
            ack,
            not_ready: false,
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn connected(now: Instant) -> Session {
        let mut s = Session::new();
        s.reset_request();
        assert_eq!(
            s.on_bytes(&RSTACK, now),
            [Event::Connected { reset_code: 2 }]
        );
        assert_eq!(s.poll_transmit(now), None);
        s
    }

    #[test]
    fn handshake() {
        let now = Instant::now();
        let mut s = Session::new();
        assert_eq!(s.reset_request(), [0x1A, 0xC0, 0x38, 0xBC, 0x7E]);
        // Nothing else to send while waiting for the NCP.
        assert_eq!(s.poll_transmit(now), None);
        assert_eq!(s.next_timeout(), None);
        assert_eq!(
            s.on_bytes(&RSTACK, now),
            [Event::Connected { reset_code: 2 }]
        );
    }

    #[test]
    fn nothing_before_handshake() {
        let now = Instant::now();
        let mut s = Session::new();
        assert_eq!(s.send(vec![1]), Err(Busy));
        // Frames are ignored until the link is up: no ACK, no NAK, no RSTACK.
        assert!(
            s.on_bytes(&data(0, false, 0, &[1]).encode(), now)
                .is_empty()
        );
        assert!(s.on_bytes(&CORRUPT, now).is_empty());
        assert!(s.on_bytes(&RSTACK, now).is_empty());
        assert_eq!(s.poll_transmit(now), None);
    }

    #[test]
    fn data_is_acked_and_delivered() {
        let t0 = Instant::now();
        let mut s = connected(t0);
        s.send(vec![0x00, 0x00, 0x00, 0x0D]).unwrap();
        assert_eq!(
            next_frame(&mut s, t0),
            data(0, false, 0, &[0x00, 0x00, 0x00, 0x0D])
        );
        assert_eq!(s.poll_transmit(t0), None);
        let t1 = t0 + ms(30);
        assert_eq!(s.on_bytes(&ack(1).encode(), t1), [Event::Delivered]);
        assert_eq!(s.next_timeout(), None);
        // The next frame takes the next number.
        s.send(vec![7]).unwrap();
        assert_eq!(next_frame(&mut s, t1), data(1, false, 0, &[7]));
    }

    #[test]
    fn piggybacked_ack_delivers() {
        let t0 = Instant::now();
        let mut s = connected(t0);
        s.send(vec![1]).unwrap();
        next_frame(&mut s, t0);
        // The NCP answers with its own DATA frame acknowledging ours.
        let events = s.on_bytes(&data(0, false, 1, &[9, 9]).encode(), t0);
        assert_eq!(events, [Event::Delivered, Event::Received(vec![9, 9])]);
        assert_eq!(next_frame(&mut s, t0), ack(1));
        // Our next DATA frame carries the ack number too.
        s.send(vec![2]).unwrap();
        assert_eq!(next_frame(&mut s, t0), data(1, false, 1, &[2]));
    }

    #[test]
    fn incoming_data_is_acked() {
        let now = Instant::now();
        let mut s = connected(now);
        let events = s.on_bytes(&data(0, false, 0, &[0x00, 0x80, 0x00]).encode(), now);
        assert_eq!(events, [Event::Received(vec![0x00, 0x80, 0x00])]);
        assert_eq!(next_frame(&mut s, now), ack(1));
        assert_eq!(s.poll_transmit(now), None);
        // Numbers wrap around after 7.
        for frm in 1..=8u8 {
            let events = s.on_bytes(&data(frm % 8, false, 0, &[frm]).encode(), now);
            assert_eq!(events, [Event::Received(vec![frm])]);
            assert_eq!(next_frame(&mut s, now), ack((frm + 1) % 8));
        }
    }

    #[test]
    fn duplicate_is_not_redelivered() {
        let now = Instant::now();
        let mut s = connected(now);
        assert_eq!(
            s.on_bytes(&data(0, false, 0, &[5]).encode(), now),
            [Event::Received(vec![5])]
        );
        assert_eq!(next_frame(&mut s, now), ack(1));
        // Our ACK was lost: the NCP sends the frame again, marked re_tx.
        assert!(s.on_bytes(&data(0, true, 0, &[5]).encode(), now).is_empty());
        assert_eq!(next_frame(&mut s, now), ack(1));
        assert_eq!(s.poll_transmit(now), None);
    }

    #[test]
    fn out_of_sequence_is_nakked() {
        let now = Instant::now();
        let mut s = connected(now);
        // Frame 0 was lost; frame 1 arrives.
        assert!(
            s.on_bytes(&data(1, false, 0, &[1]).encode(), now)
                .is_empty()
        );
        assert_eq!(next_frame(&mut s, now), nak(0));
        // The retransmitted frame 0 is accepted.
        assert_eq!(
            s.on_bytes(&data(0, true, 0, &[0]).encode(), now),
            [Event::Received(vec![0])]
        );
        assert_eq!(next_frame(&mut s, now), ack(1));
    }

    #[test]
    fn retransmits_then_fails() {
        let t0 = Instant::now();
        let mut s = connected(t0);
        s.send(vec![3]).unwrap();
        assert_eq!(next_frame(&mut s, t0), data(0, false, 0, &[3]));
        let first = t0 + ms(1600);
        assert_eq!(s.next_timeout(), Some(first));
        // Not yet.
        assert_eq!(s.poll_transmit(t0 + ms(1599)), None);
        let mut deadline = first;
        for _ in 0..3 {
            assert!(s.on_bytes(&[], deadline).is_empty());
            assert_eq!(next_frame(&mut s, deadline), data(0, true, 0, &[3]));
            // The timer doubles on each expiry, up to the 3.2 s ceiling.
            let next = s.next_timeout().expect("a retransmission timer");
            assert_eq!(next, deadline + ms(3200));
            deadline = next;
        }
        assert_eq!(s.on_bytes(&[], deadline), [Event::Failed]);
        assert_eq!(s.poll_transmit(deadline), None);
        assert_eq!(s.next_timeout(), None);
        assert_eq!(s.send(vec![4]), Err(Busy));
        // A new reset brings the link back, numbering from zero.
        s.reset_request();
        assert_eq!(
            s.on_bytes(&RSTACK, deadline),
            [Event::Connected { reset_code: 2 }]
        );
        s.send(vec![4]).unwrap();
        assert_eq!(next_frame(&mut s, deadline), data(0, false, 0, &[4]));
        assert_eq!(s.next_timeout(), Some(deadline + ms(1600)));
    }

    #[test]
    fn failure_waits_for_on_bytes() {
        let t0 = Instant::now();
        let mut s = connected(t0);
        s.send(vec![3]).unwrap();
        next_frame(&mut s, t0);
        for _ in 0..3 {
            let deadline = s.next_timeout().unwrap();
            assert_eq!(next_frame(&mut s, deadline), data(0, true, 0, &[3]));
        }
        let deadline = s.next_timeout().unwrap();
        // The client polls first: nothing to write, the failure is kept for on_bytes.
        assert_eq!(s.poll_transmit(deadline), None);
        assert_eq!(s.on_bytes(&[], deadline), [Event::Failed]);
        assert!(s.on_bytes(&[], deadline).is_empty());
    }

    #[test]
    fn nak_triggers_retransmission() {
        let t0 = Instant::now();
        let mut s = connected(t0);
        s.send(vec![6]).unwrap();
        next_frame(&mut s, t0);
        let t1 = t0 + ms(50);
        assert!(s.on_bytes(&nak(0).encode(), t1).is_empty());
        assert_eq!(next_frame(&mut s, t1), data(0, true, 0, &[6]));
        assert_eq!(s.poll_transmit(t1), None);
        assert_eq!(s.on_bytes(&ack(1).encode(), t1), [Event::Delivered]);
    }

    #[test]
    fn ack_time_adapts_timeout() {
        let t0 = Instant::now();
        let mut s = connected(t0);
        s.send(vec![1]).unwrap();
        next_frame(&mut s, t0);
        // ACK after 200 ms: 7/8 * 1.6 s + 1/2 * 0.2 s = 1.5 s.
        let t1 = t0 + ms(200);
        assert_eq!(s.on_bytes(&ack(1).encode(), t1), [Event::Delivered]);
        s.send(vec![2]).unwrap();
        next_frame(&mut s, t1);
        assert_eq!(s.next_timeout(), Some(t1 + ms(1500)));
        // Prompt ACKs drive it down to the 0.4 s floor, never below.
        let mut sent = t1;
        for i in 2..40u8 {
            let now = sent + ms(1);
            assert_eq!(s.on_bytes(&ack(i % 8).encode(), now), [Event::Delivered]);
            s.send(vec![i]).unwrap();
            next_frame(&mut s, now);
            sent = now;
        }
        assert_eq!(s.next_timeout(), Some(sent + ms(400)));
    }

    #[test]
    fn retransmitted_frame_is_not_timed() {
        let t0 = Instant::now();
        let mut s = connected(t0);
        s.send(vec![1]).unwrap();
        next_frame(&mut s, t0);
        let t1 = s.next_timeout().unwrap();
        next_frame(&mut s, t1);
        // Acknowledged right after the retransmission: an ambiguous sample, the
        // doubled timer (3.2 s) is kept rather than measured.
        assert_eq!(s.on_bytes(&ack(1).encode(), t1 + ms(1)), [Event::Delivered]);
        s.send(vec![2]).unwrap();
        next_frame(&mut s, t1 + ms(1));
        assert_eq!(s.next_timeout(), Some(t1 + ms(1) + ms(3200)));
    }

    #[test]
    fn busy_when_window_full() {
        let now = Instant::now();
        let mut s = connected(now);
        s.send(vec![1]).unwrap();
        assert_eq!(s.send(vec![2]), Err(Busy));
        next_frame(&mut s, now);
        assert_eq!(s.send(vec![2]), Err(Busy));
        // An ACK for another frame number does not free the window.
        assert!(s.on_bytes(&ack(0).encode(), now).is_empty());
        assert_eq!(s.send(vec![2]), Err(Busy));
        assert_eq!(s.on_bytes(&ack(1).encode(), now), [Event::Delivered]);
        assert_eq!(s.send(vec![2]), Ok(()));
    }

    #[test]
    fn unsolicited_rstack_is_reported() {
        let now = Instant::now();
        let mut s = connected(now);
        s.send(vec![1]).unwrap();
        next_frame(&mut s, now);
        let reset = Frame::RstAck {
            version: 2,
            reset_code: 0x0B,
        };
        assert_eq!(
            s.on_bytes(&reset.encode(), now),
            [Event::NcpReset { reset_code: 0x0B }]
        );
        // No reconnection on its own: the frame in flight is dropped.
        assert_eq!(s.poll_transmit(now), None);
        assert_eq!(s.next_timeout(), None);
        assert_eq!(s.send(vec![2]), Err(Busy));
    }

    #[test]
    fn error_frame_is_reported() {
        let now = Instant::now();
        let mut s = connected(now);
        let wire = Frame::Error {
            version: 2,
            code: 0x51,
        }
        .encode();
        assert_eq!(wire[..3], [0xC2, 0x02, 0x51]);
        assert_eq!(s.on_bytes(&wire, now), [Event::NcpError { code: 0x51 }]);
        assert_eq!(s.send(vec![1]), Err(Busy));
    }

    #[test]
    fn corrupt_frame_triggers_nak() {
        let now = Instant::now();
        let mut s = connected(now);
        assert!(s.on_bytes(&CORRUPT, now).is_empty());
        assert_eq!(next_frame(&mut s, now), nak(0));
        // Until a good frame arrives, further garbage is not NAKed again.
        assert!(s.on_bytes(&CORRUPT, now).is_empty());
        assert_eq!(s.poll_transmit(now), None);
        assert_eq!(
            s.on_bytes(&data(0, true, 0, &[1]).encode(), now),
            [Event::Received(vec![1])]
        );
        assert_eq!(next_frame(&mut s, now), ack(1));
        assert!(s.on_bytes(&CORRUPT, now).is_empty());
        assert_eq!(next_frame(&mut s, now), nak(1));
    }

    #[test]
    fn frames_split_across_reads() {
        let now = Instant::now();
        let mut s = connected(now);
        let wire = data(0, false, 0, &[1, 2, 3]).encode();
        let (a, b) = wire.split_at(2);
        assert!(s.on_bytes(a, now).is_empty());
        assert_eq!(s.on_bytes(b, now), [Event::Received(vec![1, 2, 3])]);
    }
}
