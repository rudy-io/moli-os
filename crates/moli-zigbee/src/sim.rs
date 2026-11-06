//! A simulated NCP for tests: the far end of an in-memory link that speaks
//! real ASH (through the frame codec) and answers EZSP commands from a table
//! each test fills.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::aps::ApsFrame;
use crate::ash::{Decoder, Frame};
use crate::ezsp::{Version, decode, ids};

/// Reset code of the RSTACK answering an RST: software reset.
const RESET_SOFTWARE: u8 = 0x0B;
/// Reset code of a spontaneous restart ([`FakeNcp::reset`]): power-on.
const RESET_POWER_ON: u8 = 0x02;
/// ASH protocol version in RSTACK.
const ASH_VERSION: u8 = 2;

/// A request a device received through [`FakeRadio`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RadioRequest {
    pub(crate) nwk: u16,
    pub(crate) profile: u16,
    pub(crate) cluster: u16,
    pub(crate) endpoint: u8,
    pub(crate) payload: Vec<u8>,
}

type Answers = Arc<dyn Fn(&RadioRequest) -> Option<Vec<u8>> + Send + Sync>;

#[derive(Default)]
struct RadioState {
    answers: Option<Answers>,
    /// How many answers still get lost; a request delivered all the same.
    lose: usize,
    /// Devices that do not even acknowledge.
    deaf: Vec<u16>,
    requests: Vec<RadioRequest>,
}

/// The devices on the far side of a [`FakeNcp`] (EZSP 13 frames): every
/// `sendUnicast` is acknowledged (`messageSentHandler`), then answered as a
/// test says, through an `incomingMessageHandler`.
pub(crate) struct FakeRadio {
    state: Arc<Mutex<RadioState>>,
    task: JoinHandle<()>,
}

impl FakeRadio {
    pub(crate) fn spawn(
        ncp: &FakeNcp,
        answers: impl Fn(&RadioRequest) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        let state = Arc::new(Mutex::new(RadioState {
            answers: Some(Arc::new(answers)),
            ..RadioState::default()
        }));
        let script = Arc::clone(&ncp.script);
        let control = ncp.control.clone();
        let arrived = ncp.lock().arrived.clone();
        let shared = Arc::clone(&state);
        let task = tokio::spawn(async move {
            let mut seen = 0;
            loop {
                arrived.notified().await;
                let received = {
                    let script = script.lock().unwrap_or_else(PoisonError::into_inner);
                    script.received[seen..].to_vec()
                };
                seen += received.len();
                for (id, params) in received {
                    if id == ids::SEND_UNICAST {
                        answer(&shared, &control, &params);
                    }
                }
            }
        });
        Self { state, task }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RadioState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The next `count` answers get lost.
    pub(crate) fn lose_answers(&self, count: usize) {
        self.lock().lose = count;
    }

    /// `nwk` stops acknowledging anything.
    pub(crate) fn deafen(&self, nwk: u16) {
        self.lock().deaf.push(nwk);
    }

    /// Answers `nwk` again.
    pub(crate) fn hear(&self, nwk: u16) {
        self.lock().deaf.retain(|&deaf| deaf != nwk);
    }

    /// Answers differently from now on.
    pub(crate) fn answer_with(
        &self,
        answers: impl Fn(&RadioRequest) -> Option<Vec<u8>> + Send + Sync + 'static,
    ) {
        self.lock().answers = Some(Arc::new(answers));
    }

    /// Everything sent so far, in order.
    pub(crate) fn requests(&self) -> Vec<RadioRequest> {
        self.lock().requests.clone()
    }
}

impl Drop for FakeRadio {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One `sendUnicast` (v13 layout), acknowledged and answered.
fn answer(state: &Arc<Mutex<RadioState>>, control: &mpsc::UnboundedSender<Control>, params: &[u8]) {
    let nwk = u16::from_le_bytes([params[1], params[2]]);
    let (frame, rest) = ApsFrame::decode(&params[3..]).unwrap();
    let tag = rest[0];
    let payload = rest[2..].to_vec();
    let request = RadioRequest {
        nwk,
        profile: frame.profile,
        cluster: frame.cluster,
        endpoint: frame.dst_ep,
        payload,
    };
    let reply = {
        let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
        state.requests.push(request.clone());
        if state.deaf.contains(&nwk) {
            return;
        }
        if state.lose > 0 {
            state.lose -= 1;
            None
        } else {
            state.answers.clone().and_then(|answers| answers(&request))
        }
    };
    let mut sent = vec![0, params[1], params[2]];
    frame.encode(&mut sent);
    sent.extend_from_slice(&[tag, 0x00, 0x00]);
    let _ = control.send(Control::Callback(ids::MESSAGE_SENT_HANDLER, sent));
    if let Some(reply) = reply {
        let zdo = frame.profile == crate::zdo::PROFILE;
        let message = incoming_from(
            nwk,
            frame.profile,
            if zdo {
                frame.cluster | 0x8000
            } else {
                frame.cluster
            },
            frame.dst_ep,
            &reply,
        );
        let _ = control.send(Control::Callback(ids::INCOMING_MESSAGE_HANDLER, message));
    }
}

/// The `incomingMessageHandler` (v13) of a message from `nwk`, LQI 200.
pub(crate) fn incoming_from(
    nwk: u16,
    profile: u16,
    cluster: u16,
    endpoint: u8,
    payload: &[u8],
) -> Vec<u8> {
    let mut params = vec![0x00];
    ApsFrame {
        profile,
        cluster,
        src_ep: endpoint,
        dst_ep: 1,
        ..ApsFrame::default()
    }
    .encode(&mut params);
    params.push(200);
    params.push(0xC4);
    params.extend_from_slice(&nwk.to_le_bytes());
    params.extend_from_slice(&[0xFF, 0xFF]);
    params.push(u8::try_from(payload.len()).unwrap());
    params.extend_from_slice(payload);
    params
}

/// A value in a read response: its data type and bytes.
type Stored = (u8, Vec<u8>);

fn text_value(text: &str) -> Stored {
    let mut bytes = vec![u8::try_from(text.len()).unwrap()];
    bytes.extend_from_slice(text.as_bytes());
    (0x42, bytes)
}

/// The usual answers of a device: `node` is its node descriptor (after the
/// address), `simple` its clusters, `attribute` what its attributes hold.
fn device_answers(
    logical_type: u8,
    capability: u8,
    clusters: &'static [u16],
    attribute: fn(u16, u16) -> Option<Stored>,
) -> impl Fn(&RadioRequest) -> Option<Vec<u8>> + Send + Sync + Clone + 'static {
    move |request: &RadioRequest| {
        let payload = &request.payload;
        if request.profile == crate::zdo::PROFILE {
            let [seq, nwk0, nwk1, ..] = *payload.as_slice() else {
                return Some(vec![payload[0], 0x00]);
            };
            return Some(match request.cluster {
                crate::zdo::NODE_DESCRIPTOR_REQUEST => vec![
                    seq,
                    0x00,
                    nwk0,
                    nwk1,
                    logical_type,
                    0x40,
                    capability,
                    0x66,
                    0x11,
                    0x52,
                    0x80,
                    0x00,
                    0x00,
                    0x2C,
                    0x80,
                    0x00,
                    0x00,
                ],
                crate::zdo::ACTIVE_ENDPOINTS_REQUEST => vec![seq, 0x00, nwk0, nwk1, 1, 1],
                crate::zdo::SIMPLE_DESCRIPTOR_REQUEST => {
                    let mut answer = vec![seq, 0x00, nwk0, nwk1, 0, 1, 0x04, 0x01, 0x0D, 0x01, 1];
                    answer.push(u8::try_from(clusters.len()).unwrap());
                    answer.extend(clusters.iter().flat_map(|id| id.to_le_bytes()));
                    answer.push(0);
                    answer
                }
                _ => vec![seq, 0x00],
            });
        }
        let (control, seq, command) = (payload[0], payload[1], payload[2]);
        if control & 0x01 == 1 {
            // A cluster command: done.
            return Some(vec![0x18, seq, 0x0B, command, 0x00]);
        }
        Some(match command {
            0x00 => {
                let mut answer = vec![0x18, seq, 0x01];
                for id in payload[3..].as_chunks::<2>().0 {
                    answer.extend_from_slice(id);
                    match attribute(request.cluster, u16::from_le_bytes(*id)) {
                        Some((data_type, bytes)) => {
                            answer.extend_from_slice(&[0x00, data_type]);
                            answer.extend_from_slice(&bytes);
                        }
                        None => answer.push(0x86),
                    }
                }
                answer
            }
            0x02 => vec![0x18, seq, 0x04, 0x00],
            0x06 => vec![0x18, seq, 0x07, 0x00],
            _ => return None,
        })
    }
}

/// A mains colour lamp, on endpoint 1.
pub(crate) fn lamp_answers() -> impl Fn(&RadioRequest) -> Option<Vec<u8>> + Send + Sync + Clone {
    device_answers(
        1,
        0x8E,
        &[0x0000, 0x0006, 0x0008, 0x0300],
        |cluster, id| match (cluster, id) {
            (0x0000, 0x0005) => Some(text_value("Some lamp")),
            (0x0000, 0x0004) => Some(text_value("Some maker")),
            (0x0000, 0x4000) => Some(text_value("1.2.3")),
            (0x0006, 0x0000) => Some((0x10, vec![0x01])),
            (0x0008, 0x0000) => Some((0x20, vec![0xFE])),
            (0x0300, 0x0003 | 0x0004) => Some((0x21, vec![0x00, 0x60])),
            (0x0300, 0x0007) => Some((0x21, vec![0x72, 0x01])),
            (0x0300, 0x0008) => Some((0x30, vec![0x02])),
            (0x0300, 0x400A) => Some((0x19, vec![0x1F, 0x00])),
            (0x0300, 0x400B) => Some((0x21, vec![0x99, 0x00])),
            (0x0300, 0x400C) => Some((0x21, vec![0x2C, 0x02])),
            _ => None,
        },
    )
}

/// A mains plug measuring what it powers, on endpoint 1.
pub(crate) fn plug_answers() -> impl Fn(&RadioRequest) -> Option<Vec<u8>> + Send + Sync + Clone {
    device_answers(
        1,
        0x8E,
        &[0x0000, 0x0006, 0x0702, 0x0B04],
        |cluster, id| match (cluster, id) {
            (0x0000, 0x0005) => Some(text_value("Some plug")),
            (0x0000, 0x0004) => Some(text_value("Some maker")),
            (0x0702, 0x0301) | (0x0B04, 0x0600 | 0x0602 | 0x0604) => Some((0x21, vec![1, 0])),
            (0x0702, 0x0302) | (0x0B04, 0x0603) => Some((0x21, vec![0xE8, 0x03])),
            (0x0B04, 0x0601 | 0x0605) => Some((0x21, vec![10, 0])),
            _ => None,
        },
    )
}

/// A battery door sensor, on endpoint 1.
pub(crate) fn sensor_answers() -> impl Fn(&RadioRequest) -> Option<Vec<u8>> + Send + Sync + Clone {
    device_answers(2, 0x80, &[0x0000, 0x0001, 0x0500], |cluster, id| {
        match (cluster, id) {
            (0x0000, 0x0005) => Some(text_value("Some door")),
            (0x0000, 0x0004) => Some(text_value("Some maker")),
            (0x0001, 0x0020) => Some((0x20, vec![30])),
            (0x0001, 0x0021) => Some((0x20, vec![200])),
            (0x0500, 0x0000) => Some((0x30, vec![0x01])),
            (0x0500, 0x0001) => Some((0x31, vec![0x15, 0x00])),
            (0x0500, 0x0002) => Some((0x19, vec![0x00, 0x00])),
            _ => None,
        }
    })
}

/// What a test scripted, and what the host sent so far.
#[derive(Debug)]
struct Script {
    /// Told of each command received.
    arrived: Arc<tokio::sync::Notify>,
    version: Version,
    /// Parameters of the response to each frame ID; an ID missing here is
    /// never answered (the command stays pending on the host).
    responses: HashMap<u16, Vec<u8>>,
    /// Every EZSP command received, in order: frame ID and parameters.
    received: Vec<(u16, Vec<u8>)>,
    /// Hold the next responses until this many are gathered, then send them
    /// newest first.
    reverse: usize,
    held: Vec<Vec<u8>>,
    /// DATA frames are lost on the line: neither acknowledged nor logged.
    muted: bool,
}

#[derive(Debug)]
enum Control {
    Callback(u16, Vec<u8>),
    Reset,
}

/// The simulated NCP; dropping it closes the link.
#[derive(Debug)]
pub(crate) struct FakeNcp {
    script: Arc<Mutex<Script>>,
    control: mpsc::UnboundedSender<Control>,
    task: JoinHandle<()>,
}

impl FakeNcp {
    /// Start an NCP answering `version`; returns it and the host end of the link.
    pub(crate) fn spawn(version: Version) -> (Self, DuplexStream) {
        let (host, ncp) = tokio::io::duplex(4096);
        let script = Arc::new(Mutex::new(Script {
            arrived: Arc::new(tokio::sync::Notify::new()),
            version,
            responses: HashMap::new(),
            received: Vec::new(),
            reverse: 0,
            held: Vec::new(),
            muted: false,
        }));
        let (control, control_rx) = mpsc::unbounded_channel();
        let link = Link {
            io: ncp,
            up: false,
            tx_seq: 0,
            rx_seq: 0,
            script: Arc::clone(&script),
        };
        let task = tokio::spawn(link.run(control_rx));
        (
            Self {
                script,
                control,
                task,
            },
            host,
        )
    }

    /// Answer every later `id` command with `params`.
    pub(crate) fn respond(&self, id: u16, params: &[u8]) {
        self.lock().responses.insert(id, params.to_vec());
    }

    /// Never answer `id` commands again (until the next [`Self::respond`]).
    pub(crate) fn silence(&self, id: u16) {
        self.lock().responses.remove(&id);
    }

    /// Hold the next `count` responses, then send them in reverse order.
    pub(crate) fn reverse_next(&self, count: usize) {
        let mut script = self.lock();
        script.reverse = count;
        script.held.clear();
    }

    /// Lose (`true`) or take again the host's DATA frames; the host
    /// retransmits a lost one.
    pub(crate) fn mute(&self, muted: bool) {
        self.lock().muted = muted;
    }

    /// Send a callback frame to the host.
    pub(crate) fn callback(&self, id: u16, params: &[u8]) {
        let _ = self.control.send(Control::Callback(id, params.to_vec()));
    }

    /// Restart on its own: RSTACK, then ignore DATA until the host's next RST.
    pub(crate) fn reset(&self) {
        let _ = self.control.send(Control::Reset);
    }

    /// The EZSP commands received so far (frame ID, parameters).
    pub(crate) fn received(&self) -> Vec<(u16, Vec<u8>)> {
        self.lock().received.clone()
    }

    /// Wait until at least `count` commands were received; panics after 5 s.
    pub(crate) async fn wait_received(&self, count: usize) {
        let wait = async {
            while self.lock().received.len() < count {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .unwrap_or_else(|_| panic!("the fake NCP never received {count} commands"));
    }

    /// Wait until an `id` command was received; panics after 5 s.
    pub(crate) async fn wait_command(&self, id: u16) {
        let wait = async {
            while !self.lock().received.iter().any(|(got, _)| *got == id) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .unwrap_or_else(|_| panic!("the fake NCP never received command {id:#06x}"));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Script> {
        self.script.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for FakeNcp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The NCP side of the ASH link (no retransmission: the link is lossless).
struct Link {
    io: DuplexStream,
    /// Reset by the host (RST seen) since the last restart.
    up: bool,
    /// Number of our next DATA frame.
    tx_seq: u8,
    /// Number of the next DATA frame expected from the host.
    rx_seq: u8,
    script: Arc<Mutex<Script>>,
}

impl Link {
    async fn run(mut self, mut control: mpsc::UnboundedReceiver<Control>) {
        let mut decoder = Decoder::default();
        let mut buf = [0u8; 256];
        loop {
            let done = tokio::select! {
                read = self.io.read(&mut buf) => match read {
                    Ok(0) | Err(_) => return,
                    Ok(n) => {
                        let frames: Vec<Frame> = buf[..n]
                            .iter()
                            .filter_map(|&b| decoder.push(b))
                            .filter_map(Result::ok)
                            .collect();
                        let mut result = Ok(());
                        for frame in frames {
                            result = self.on_frame(frame).await;
                            if result.is_err() {
                                break;
                            }
                        }
                        result
                    }
                },
                order = control.recv() => match order {
                    None => return,
                    Some(Control::Callback(id, params)) => {
                        // Asynchronous callback: response bit and callback type 0x10.
                        let mut frame = vec![0, 0x90, 0x01];
                        frame.extend_from_slice(&id.to_le_bytes());
                        frame.extend_from_slice(&params);
                        self.send_data(frame).await
                    }
                    Some(Control::Reset) => {
                        self.up = false;
                        self.write(&Frame::RstAck { version: ASH_VERSION, reset_code: RESET_POWER_ON }).await
                    }
                },
            };
            if done.is_err() {
                return;
            }
        }
    }

    async fn on_frame(&mut self, frame: Frame) -> std::io::Result<()> {
        match frame {
            Frame::Rst => {
                self.up = true;
                self.tx_seq = 0;
                self.rx_seq = 0;
                self.write(&Frame::RstAck {
                    version: ASH_VERSION,
                    reset_code: RESET_SOFTWARE,
                })
                .await
            }
            Frame::Data {
                frm,
                re_tx,
                payload,
                ..
            } if self.up => {
                if self.lock().muted {
                    return Ok(());
                }
                if frm != self.rx_seq {
                    // A copy of a frame already taken (its ACK was lost), or a gap.
                    let ack = self.rx_seq;
                    return if re_tx {
                        self.write(&Frame::Ack {
                            ack,
                            not_ready: false,
                        })
                        .await
                    } else {
                        self.write(&Frame::Nak {
                            ack,
                            not_ready: false,
                        })
                        .await
                    };
                }
                self.rx_seq = (self.rx_seq + 1) & 7;
                let ack = self.rx_seq;
                self.write(&Frame::Ack {
                    ack,
                    not_ready: false,
                })
                .await?;
                for reply in self.command(&payload) {
                    self.send_data(reply).await?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Log a command and build the responses due now.
    fn command(&self, frame: &[u8]) -> Vec<Vec<u8>> {
        let Ok((header, params)) = decode(frame) else {
            return Vec::new();
        };
        let mut script = self.lock();
        script.received.push((header.id, params.to_vec()));
        script.arrived.notify_one();
        let reply = if header.id == ids::VERSION {
            let v = script.version;
            let [low, high] = v.stack_version.to_le_bytes();
            let body = [v.protocol, v.stack_type, low, high];
            if frame[2] & 0x03 == 0x01 {
                response(header.seq, header.id, &body)
            } else {
                // Legacy format: one-byte frame ID.
                let mut legacy = vec![header.seq, 0x80, 0x00];
                legacy.extend_from_slice(&body);
                legacy
            }
        } else if let Some(params) = script.responses.get(&header.id) {
            response(header.seq, header.id, params)
        } else {
            return Vec::new();
        };
        if script.reverse == 0 {
            return vec![reply];
        }
        script.held.push(reply);
        if script.held.len() < script.reverse {
            return Vec::new();
        }
        script.reverse = 0;
        let mut held = std::mem::take(&mut script.held);
        held.reverse();
        held
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Script> {
        self.script.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn send_data(&mut self, payload: Vec<u8>) -> std::io::Result<()> {
        let frame = Frame::Data {
            frm: self.tx_seq,
            re_tx: false,
            ack: self.rx_seq,
            payload,
        };
        self.tx_seq = (self.tx_seq + 1) & 7;
        self.write(&frame).await
    }

    async fn write(&mut self, frame: &Frame) -> std::io::Result<()> {
        self.io.write_all(&frame.encode()).await
    }
}

/// A response in the extended format.
fn response(seq: u8, id: u16, params: &[u8]) -> Vec<u8> {
    let mut frame = vec![seq, 0x80, 0x01];
    frame.extend_from_slice(&id.to_le_bytes());
    frame.extend_from_slice(params);
    frame
}
