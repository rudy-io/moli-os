//! A simulated NCP for tests: the far end of an in-memory link that speaks
//! real ASH (through the frame codec) and answers EZSP commands from a table
//! each test fills.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::ash::{Decoder, Frame};
use crate::ezsp::{Version, decode, ids};

/// Reset code of the RSTACK answering an RST: software reset.
const RESET_SOFTWARE: u8 = 0x0B;
/// Reset code of a spontaneous restart ([`FakeNcp::reset`]): power-on.
const RESET_POWER_ON: u8 = 0x02;
/// ASH protocol version in RSTACK.
const ASH_VERSION: u8 = 2;

/// What a test scripted, and what the host sent so far.
#[derive(Debug)]
struct Script {
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
