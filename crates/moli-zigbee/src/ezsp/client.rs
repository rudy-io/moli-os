//! The EZSP client: numbers commands, matches their responses and forwards
//! callbacks, over an ASH session driven by one background task.
//!
//! The task (the "pump") owns the port and the [`Session`]: it writes the
//! commands one at a time (the ASH window is one frame), runs the
//! retransmission timer, routes responses to their callers by sequence
//! number and callbacks to a channel. [`Ezsp`] is a cheap handle on it.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};

use super::{Header, Version, decode, encode, ids, legacy_version_request};
use crate::ash::{Event, Session};

/// Oldest EZSP protocol this driver speaks.
pub const MIN_PROTOCOL: u8 = 13;
/// Newest EZSP protocol this driver knows: `EZSP_PROTOCOL_VERSION` of
/// zigbee-herdsman (MIT), `src/adapter/ember/ezsp/consts.ts`.
pub const MAX_PROTOCOL: u8 = 0x13;

/// How long the NCP gets to answer an RST, and to answer a command.
const RESET_TIMEOUT: Duration = Duration::from_secs(5);
const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// RSTs sent before giving up on the NCP.
const RESET_ATTEMPTS: u8 = 2;
/// Commands waiting for the pump.
const COMMAND_QUEUE: usize = 32;
/// Callbacks not yet read; beyond, they are dropped rather than stalling
/// the link (and the responses behind them).
const CALLBACK_QUEUE: usize = 256;
/// Largest read from the port; an ASH frame is far smaller.
const READ_BUFFER: usize = 256;

/// Why the link to the NCP is unusable; found in an `anyhow::Error` with
/// `downcast_ref`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EzspFailure {
    /// The NCP firmware speaks an EZSP protocol older than [`MIN_PROTOCOL`].
    FirmwareTooOld { protocol: u8 },
    /// The NCP restarted or reported a fatal error: its state is gone.
    NcpReset,
    /// No answer to the reset, a frame never acknowledged, or the port closed.
    LinkLost,
}

impl fmt::Display for EzspFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FirmwareTooOld { protocol } => write!(
                f,
                "NCP firmware too old: EZSP protocol {protocol}, {MIN_PROTOCOL} or newer needed"
            ),
            Self::NcpReset => f.write_str("the NCP reset"),
            Self::LinkLost => f.write_str("link to the NCP lost"),
        }
    }
}

impl std::error::Error for EzspFailure {}

/// An unsolicited frame from the NCP.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Callback {
    pub id: u16,
    pub params: Vec<u8>,
}

/// Delays, shortened by tests.
#[derive(Clone, Copy, Debug)]
struct Timing {
    reset: Duration,
    call: Duration,
}

impl Timing {
    const DEFAULT: Self = Self {
        reset: RESET_TIMEOUT,
        call: CALL_TIMEOUT,
    };
}

/// What a command gets back: its response, or the failure that ended the link.
type Reply = Result<(Header, Vec<u8>), EzspFailure>;

/// Commands awaiting a response, shared by the handles and the pump.
#[derive(Debug, Default)]
struct Calls {
    next_seq: u8,
    pending: HashMap<u8, oneshot::Sender<Reply>>,
    /// Set once the pump stopped: every later command fails with it.
    failure: Option<EzspFailure>,
}

impl Calls {
    fn fail(&mut self, failure: EzspFailure) {
        let failure = *self.failure.get_or_insert(failure);
        for (_, waiter) in self.pending.drain() {
            let _ = waiter.send(Err(failure));
        }
    }
}

fn lock(calls: &Mutex<Calls>) -> MutexGuard<'_, Calls> {
    calls.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A handle on the EZSP link; clones share it. The link closes when the
/// last one is dropped.
#[derive(Clone, Debug)]
pub struct Ezsp {
    frames: mpsc::Sender<Vec<u8>>,
    calls: Arc<Mutex<Calls>>,
    timing: Timing,
}

impl Ezsp {
    /// Reset the NCP (RST, 5 s for the RSTACK, one more RST), then agree on
    /// the EZSP protocol: the legacy `version` command asking for
    /// [`MIN_PROTOCOL`], then the extended one at the protocol the NCP
    /// announced, capped at [`MAX_PROTOCOL`].
    ///
    /// Returns the handle, the NCP version and the callbacks; the channel
    /// ends with the link.
    pub async fn connect<T>(io: T) -> anyhow::Result<(Self, Version, mpsc::Receiver<Callback>)>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        Self::connect_with(io, Timing::DEFAULT).await
    }

    async fn connect_with<T>(
        io: T,
        timing: Timing,
    ) -> anyhow::Result<(Self, Version, mpsc::Receiver<Callback>)>
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let (frames, frames_rx) = mpsc::channel(COMMAND_QUEUE);
        let (callbacks, callbacks_rx) = mpsc::channel(CALLBACK_QUEUE);
        let (up, up_rx) = oneshot::channel();
        let calls = Arc::new(Mutex::new(Calls::default()));
        let pump = Pump {
            io,
            session: Session::new(),
            frames: frames_rx,
            queue: VecDeque::new(),
            sending: false,
            calls: Arc::clone(&calls),
            callbacks,
        };
        tokio::spawn(pump.run(timing.reset, up));
        let reset_code = up_rx.await.unwrap_or(Err(EzspFailure::LinkLost))?;
        tracing::debug!(reset_code, "ASH link up");
        let ezsp = Self {
            frames,
            calls,
            timing,
        };
        let version = ezsp.negotiate().await?;
        Ok((ezsp, version, callbacks_rx))
    }

    /// Send a command and wait (5 s) for its response; returns the response
    /// parameters.
    ///
    /// # Errors
    ///
    /// An [`EzspFailure`] once the link is down (at once for every later
    /// call); otherwise a timeout, or a response that is not this command's.
    pub async fn call(&self, id: u16, params: &[u8]) -> anyhow::Result<Vec<u8>> {
        let (header, params) = self.request(id, |seq| encode(seq, id, params)).await?;
        check(id, header)?;
        Ok(params)
    }

    async fn negotiate(&self) -> anyhow::Result<Version> {
        let (header, params) = self
            .request(ids::VERSION, |seq| {
                legacy_version_request(seq, MIN_PROTOCOL)
            })
            .await?;
        check(ids::VERSION, header)?;
        let announced = Version::parse(&params)?;
        if announced.protocol < MIN_PROTOCOL {
            return Err(EzspFailure::FirmwareTooOld {
                protocol: announced.protocol,
            }
            .into());
        }
        let wanted = announced.protocol.min(MAX_PROTOCOL);
        let version = Version::parse(&self.call(ids::VERSION, &[wanted]).await?)?;
        ensure!(
            version.protocol == wanted,
            "the NCP refused EZSP protocol {wanted} (it answered {})",
            version.protocol
        );
        tracing::debug!(
            protocol = version.protocol,
            firmware = %version.display(),
            "EZSP version agreed"
        );
        Ok(version)
    }

    /// Send the frame `build` makes with a fresh sequence number, and wait
    /// for the response carrying that number.
    async fn request(
        &self,
        id: u16,
        build: impl FnOnce(u8) -> Vec<u8>,
    ) -> anyhow::Result<(Header, Vec<u8>)> {
        let (seq, mut reply) = self.register()?;
        if self.frames.send(build(seq)).await.is_err() {
            // The pump is gone, having failed every pending command.
            let mut calls = lock(&self.calls);
            calls.pending.remove(&seq);
            return Err(calls.failure.unwrap_or(EzspFailure::LinkLost).into());
        }
        match tokio::time::timeout(self.timing.call, &mut reply).await {
            Ok(Ok(reply)) => reply.map_err(anyhow::Error::from),
            Ok(Err(_)) => Err(lock(&self.calls)
                .failure
                .unwrap_or(EzspFailure::LinkLost)
                .into()),
            Err(_) => {
                // Free the number, unless a response took it meanwhile and
                // another command reuses it (that one's receiver is alive).
                drop(reply);
                let mut calls = lock(&self.calls);
                if calls
                    .pending
                    .get(&seq)
                    .is_some_and(oneshot::Sender::is_closed)
                {
                    calls.pending.remove(&seq);
                }
                bail!(
                    "EZSP command {id:#06x}: no response within {:?}",
                    self.timing.call
                )
            }
        }
    }

    /// Take a sequence number no pending command uses.
    fn register(&self) -> anyhow::Result<(u8, oneshot::Receiver<Reply>)> {
        let mut calls = lock(&self.calls);
        if let Some(failure) = calls.failure {
            return Err(failure.into());
        }
        // Callers that gave up (a dropped `call` future) free their number.
        calls.pending.retain(|_, waiter| !waiter.is_closed());
        let seq = (0..=u8::MAX)
            .map(|i| calls.next_seq.wrapping_add(i))
            .find(|seq| !calls.pending.contains_key(seq))
            .ok_or_else(|| anyhow!("too many EZSP commands awaiting a response"))?;
        calls.next_seq = seq.wrapping_add(1);
        let (waiter, reply) = oneshot::channel();
        calls.pending.insert(seq, waiter);
        Ok((seq, reply))
    }
}

/// The response is this command's, and whole.
fn check(id: u16, header: Header) -> anyhow::Result<()> {
    if header.id == ids::INVALID_COMMAND && id != ids::INVALID_COMMAND {
        bail!("the NCP does not know EZSP command {id:#06x}");
    }
    ensure!(
        header.id == id,
        "EZSP command {id:#06x} answered by frame {:#06x}",
        header.id
    );
    ensure!(!header.truncated, "EZSP response to {id:#06x} truncated");
    if header.overflow {
        tracing::warn!(id, "the NCP ran out of memory");
    }
    Ok(())
}

/// The background task owning the port and the ASH session.
struct Pump<T> {
    io: T,
    session: Session,
    frames: mpsc::Receiver<Vec<u8>>,
    /// Commands not yet handed to the session.
    queue: VecDeque<Vec<u8>>,
    /// The session holds a command awaiting its ACK.
    sending: bool,
    calls: Arc<Mutex<Calls>>,
    callbacks: mpsc::Sender<Callback>,
}

impl<T: AsyncRead + AsyncWrite + Unpin> Pump<T> {
    async fn run(mut self, reset_timeout: Duration, up: oneshot::Sender<Result<u8, EzspFailure>>) {
        let failure = match self.reset(reset_timeout).await {
            Ok(reset_code) => {
                let _ = up.send(Ok(reset_code));
                self.serve().await
            }
            Err(failure) => {
                let _ = up.send(Err(failure));
                Some(failure)
            }
        };
        if let Some(failure) = failure {
            tracing::warn!(%failure, "EZSP link stopped");
            lock(&self.calls).fail(failure);
        }
    }

    /// RST until the NCP answers: [`RESET_ATTEMPTS`] tries of `timeout` each.
    async fn reset(&mut self, timeout: Duration) -> Result<u8, EzspFailure> {
        let mut buf = [0u8; READ_BUFFER];
        for attempt in 1..=RESET_ATTEMPTS {
            let rst = self.session.reset_request();
            self.write(&rst).await?;
            let deadline = tokio::time::Instant::now() + timeout;
            while let Ok(read) = tokio::time::timeout_at(deadline, self.io.read(&mut buf)).await {
                let n = match read {
                    Ok(0) | Err(_) => return Err(EzspFailure::LinkLost),
                    Ok(n) => n,
                };
                for event in self.session.on_bytes(&buf[..n], Instant::now()) {
                    match event {
                        Event::Connected { reset_code } => return Ok(reset_code),
                        other => tracing::debug!(?other, "ASH event while resetting"),
                    }
                }
            }
            tracing::warn!(attempt, "no RSTACK from the NCP");
        }
        Err(EzspFailure::LinkLost)
    }

    /// Run the link; returns why it stopped, or `None` once every handle is gone.
    async fn serve(&mut self) -> Option<EzspFailure> {
        let mut buf = [0u8; READ_BUFFER];
        loop {
            if let Err(failure) = self.transmit().await {
                return Some(failure);
            }
            let deadline = self.session.next_timeout();
            let wake = tokio::time::Instant::from_std(deadline.unwrap_or_else(Instant::now));
            let events = tokio::select! {
                read = self.io.read(&mut buf) => match read {
                    Ok(0) => {
                        tracing::debug!("NCP port closed");
                        return Some(EzspFailure::LinkLost);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "NCP port read failed");
                        return Some(EzspFailure::LinkLost);
                    }
                    Ok(n) => self.session.on_bytes(&buf[..n], Instant::now()),
                },
                frame = self.frames.recv() => match frame {
                    Some(frame) => {
                        self.queue.push_back(frame);
                        continue;
                    }
                    None => return None,
                },
                () = tokio::time::sleep_until(wake), if deadline.is_some() => {
                    self.session.on_bytes(&[], Instant::now())
                }
            };
            for event in events {
                if let Some(failure) = self.on_event(event) {
                    return Some(failure);
                }
            }
        }
    }

    /// Hand the next command to the session if it is free, then write every
    /// frame the session has for the NCP.
    async fn transmit(&mut self) -> Result<(), EzspFailure> {
        if !self.sending
            && let Some(frame) = self.queue.pop_front()
        {
            // Free and up (a down link stops the pump), so never busy here.
            self.session
                .send(frame)
                .map_err(|_| EzspFailure::LinkLost)?;
            self.sending = true;
        }
        let now = Instant::now();
        while let Some(bytes) = self.session.poll_transmit(now) {
            self.write(&bytes).await?;
        }
        Ok(())
    }

    async fn write(&mut self, bytes: &[u8]) -> Result<(), EzspFailure> {
        let written = async {
            self.io.write_all(bytes).await?;
            self.io.flush().await
        };
        written.await.map_err(|error| {
            tracing::warn!(%error, "NCP port write failed");
            EzspFailure::LinkLost
        })
    }

    fn on_event(&mut self, event: Event) -> Option<EzspFailure> {
        match event {
            Event::Received(frame) => self.on_frame(&frame),
            Event::Delivered => self.sending = false,
            Event::Connected { .. } => {}
            Event::NcpReset { reset_code } => {
                tracing::warn!(reset_code, "the NCP reset");
                return Some(EzspFailure::NcpReset);
            }
            Event::NcpError { code } => {
                tracing::warn!(code, "the NCP reported a fatal error");
                return Some(EzspFailure::NcpReset);
            }
            Event::Failed => return Some(EzspFailure::LinkLost),
        }
        None
    }

    /// Route an EZSP frame: a callback to the channel, a response to its caller.
    fn on_frame(&self, frame: &[u8]) {
        let (header, params) = match decode(frame) {
            Ok(decoded) => decoded,
            Err(error) => {
                tracing::debug!(%error, "EZSP frame dropped");
                return;
            }
        };
        if header.callback {
            let callback = Callback {
                id: header.id,
                params: params.to_vec(),
            };
            if let Err(TrySendError::Full(callback)) = self.callbacks.try_send(callback) {
                tracing::warn!(
                    id = callback.id,
                    "EZSP callback queue full: callback dropped"
                );
            }
            return;
        }
        let waiter = header
            .is_response
            .then(|| lock(&self.calls).pending.remove(&header.seq))
            .flatten();
        if let Some(waiter) = waiter {
            let _ = waiter.send(Ok((header, params.to_vec())));
        } else {
            tracing::debug!(
                seq = header.seq,
                id = header.id,
                "EZSP response for no pending command dropped"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use tokio::io::AsyncReadExt;

    use super::*;
    use crate::ash::{Decoder, Frame};
    use crate::ezsp::ids;
    use crate::sim::FakeNcp;

    const EMBER_13: Version = Version {
        protocol: 13,
        stack_type: 2,
        stack_version: 0x7440,
    };
    /// Fictitious EUI64 `0x00124b0000000001`, little-endian.
    const EUI64: [u8; 8] = [0x01, 0, 0, 0, 0, 0x4b, 0x12, 0x00];

    fn failure(err: &anyhow::Error) -> Option<EzspFailure> {
        err.downcast_ref::<EzspFailure>().copied()
    }

    async fn connected(timing: Timing) -> (FakeNcp, Ezsp, mpsc::Receiver<Callback>) {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        let (ezsp, _, callbacks) = Ezsp::connect_with(io, timing).await.unwrap();
        (ncp, ezsp, callbacks)
    }

    #[tokio::test]
    async fn connects_and_negotiates() {
        let (ncp, io) = FakeNcp::spawn(EMBER_13);
        let (_ezsp, version, _callbacks) = Ezsp::connect(io).await.unwrap();
        assert_eq!(version, EMBER_13);
        // The legacy request, then the extended one at the NCP's protocol.
        assert_eq!(
            ncp.received(),
            [(ids::VERSION, vec![13]), (ids::VERSION, vec![13])]
        );
    }

    #[tokio::test]
    async fn old_firmware_is_refused() {
        let (ncp, io) = FakeNcp::spawn(Version {
            protocol: 8,
            ..EMBER_13
        });
        let err = Ezsp::connect(io).await.unwrap_err();
        assert_eq!(
            failure(&err),
            Some(EzspFailure::FirmwareTooOld { protocol: 8 })
        );
        // Nothing follows the legacy version request.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(ncp.received(), [(ids::VERSION, vec![13])]);
    }

    #[tokio::test]
    async fn calls_are_matched_by_sequence() {
        let (ncp, ezsp, _callbacks) = connected(Timing::DEFAULT).await;
        ncp.respond(ids::GET_EUI64, &EUI64);
        ncp.respond(ids::GET_NODE_ID, &[0x00, 0x00]);
        // Both commands are in before the first answer, which comes last.
        ncp.reverse_next(2);
        let (eui64, node_id) = tokio::join!(
            ezsp.call(ids::GET_EUI64, &[]),
            ezsp.call(ids::GET_NODE_ID, &[])
        );
        assert_eq!(eui64.unwrap(), EUI64);
        assert_eq!(node_id.unwrap(), [0x00, 0x00]);
        assert_eq!(
            ncp.received()[2..],
            [(ids::GET_EUI64, vec![]), (ids::GET_NODE_ID, vec![])]
        );
    }

    #[tokio::test]
    async fn callbacks_are_forwarded() {
        let (ncp, _ezsp, mut callbacks) = connected(Timing::DEFAULT).await;
        ncp.callback(ids::STACK_STATUS_HANDLER, &[0x90]);
        let callback = tokio::time::timeout(Duration::from_secs(2), callbacks.recv())
            .await
            .unwrap();
        assert_eq!(
            callback,
            Some(Callback {
                id: ids::STACK_STATUS_HANDLER,
                params: vec![0x90],
            })
        );
    }

    #[tokio::test]
    async fn ncp_reset_fails_pending_calls() {
        let (ncp, ezsp, _callbacks) = connected(Timing::DEFAULT).await;
        // Never answered: pending until the reset.
        let pending = ezsp.call(ids::NETWORK_INIT, &[0x00, 0x00]);
        let reset = async {
            ncp.wait_received(3).await;
            ncp.reset();
        };
        let (result, ()) = tokio::join!(pending, reset);
        assert_eq!(failure(&result.unwrap_err()), Some(EzspFailure::NcpReset));
        // Later calls fail at once, with the same failure.
        let started = Instant::now();
        let err = ezsp.call(ids::NOP, &[]).await.unwrap_err();
        assert_eq!(failure(&err), Some(EzspFailure::NcpReset));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn closed_link_fails_pending_calls() {
        let (ncp, ezsp, mut callbacks) = connected(Timing::DEFAULT).await;
        let pending = ezsp.call(ids::NETWORK_INIT, &[0x00, 0x00]);
        let close = async {
            ncp.wait_received(3).await;
            drop(ncp);
        };
        let (result, ()) = tokio::join!(pending, close);
        assert_eq!(failure(&result.unwrap_err()), Some(EzspFailure::LinkLost));
        // The callback channel ends with the link.
        assert_eq!(callbacks.recv().await, None);
    }

    #[tokio::test]
    async fn unanswered_call_times_out_and_the_link_stays_up() {
        let timing = Timing {
            call: Duration::from_millis(100),
            ..Timing::DEFAULT
        };
        let (ncp, ezsp, _callbacks) = connected(timing).await;
        let err = ezsp.call(ids::NOP, &[]).await.unwrap_err();
        assert_eq!(failure(&err), None);
        assert!(err.to_string().contains("no response"), "{err}");
        ncp.respond(ids::NOP, &[]);
        assert_eq!(ezsp.call(ids::NOP, &[]).await.unwrap(), Vec::<u8>::new());
    }

    #[tokio::test]
    async fn silent_ncp_gets_two_resets_then_the_link_is_lost() {
        let (host, mut ncp) = tokio::io::duplex(4096);
        let timing = Timing {
            reset: Duration::from_millis(50),
            ..Timing::DEFAULT
        };
        let err = Ezsp::connect_with(host, timing).await.unwrap_err();
        assert_eq!(failure(&err), Some(EzspFailure::LinkLost));
        let mut wire = Vec::new();
        ncp.read_to_end(&mut wire).await.unwrap();
        let mut decoder = Decoder::default();
        let resets = wire
            .iter()
            .filter_map(|&b| decoder.push(b))
            .filter(|frame| frame == &Ok(Frame::Rst))
            .count();
        assert_eq!(resets, 2);
    }
}
