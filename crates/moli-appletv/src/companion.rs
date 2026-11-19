//! The Companion link: TCP (49153), frames of a type byte and a 24-bit
//! big-endian length, OPACK inside. Pairing frames go in clear; once pair
//! verify is done, every payload is sealed with ChaCha20-Poly1305 under the
//! session's keys, the frame's four header bytes as associated data and a
//! counter per direction as nonce (twelve bytes, little-endian).
//!
//! After that, messages are dictionaries: `_i` what, `_t` 1 event, 2
//! request, 3 response, `_x` the request's number (the response carries
//! it), `_c` the content, `_em` an error.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, bail};
use ring::aead::Nonce;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::{mpsc, oneshot};

use crate::hap::{self, Credentials};
use crate::opack::{self, Value};
use crate::tlv8;

pub const PORT: u16 = 49153;

/// How long a request or a pairing step waits for its answer.
pub const ANSWER: Duration = Duration::from_secs(5);

/// A frame bigger than this is refused (messages are a few kilobytes).
const LARGEST: usize = 1 << 20;

const TAG: usize = 16;

pub mod frame {
    pub const PS_START: u8 = 3;
    pub const PS_NEXT: u8 = 4;
    pub const PV_START: u8 = 5;
    pub const PV_NEXT: u8 = 6;
    pub const E_OPACK: u8 = 8;
}

const EVENT: i64 = 1;
const REQUEST: i64 = 2;
const RESPONSE: i64 = 3;

fn header(kind: u8, len: usize) -> [u8; 4] {
    let l = u32::try_from(len).unwrap_or(0).to_be_bytes();
    [kind, l[1], l[2], l[3]]
}

async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> anyhow::Result<([u8; 4], Vec<u8>)> {
    let mut head = [0u8; 4];
    r.read_exact(&mut head).await?;
    let len = usize::from(head[1]) << 16 | usize::from(head[2]) << 8 | usize::from(head[3]);
    if len > LARGEST {
        bail!("Companion: frame of {len} bytes refused");
    }
    let mut payload = vec![0u8; len];
    r.read_exact(&mut payload).await?;
    Ok((head, payload))
}

fn counter_nonce(n: u64) -> Nonce {
    let mut bytes = [0u8; 12];
    bytes[..8].copy_from_slice(&n.to_le_bytes());
    Nonce::assume_unique_for_key(bytes)
}

/// One direction's key and counter.
struct Cipher {
    key: [u8; 32],
    count: u64,
}

impl std::fmt::Debug for Cipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key, even in a log.
        f.debug_struct("Cipher")
            .field("count", &self.count)
            .finish_non_exhaustive()
    }
}

impl Cipher {
    fn seal(&mut self, head: [u8; 4], data: &[u8]) -> Vec<u8> {
        let out = hap::seal(&self.key, counter_nonce(self.count), &head, data);
        self.count += 1;
        out
    }

    fn open(&mut self, head: [u8; 4], data: &[u8]) -> anyhow::Result<Vec<u8>> {
        let out = hap::open(&self.key, counter_nonce(self.count), &head, data)
            .context("Companion: a frame did not decrypt")?;
        self.count += 1;
        Ok(out)
    }
}

/// One pairing step in clear: send, then wait for the step's answer.
async fn step<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    kind: u8,
    fields: Vec<(&str, Value)>,
    expect: u8,
) -> anyhow::Result<tlv8::Tlv> {
    let body = opack::pack(&Value::dict(fields));
    stream.write_all(&header(kind, body.len())).await?;
    stream.write_all(&body).await?;
    tokio::time::timeout(ANSWER, async {
        loop {
            let (head, payload) = read_frame(stream).await?;
            if head[0] != expect {
                // Anything else before pairing is done is not for us.
                continue;
            }
            let (message, _) = opack::unpack(&payload)?;
            let data = message
                .get("_pd")
                .and_then(Value::as_bytes)
                .context("Companion: no pairing data in the answer")?;
            return tlv8::read(data);
        }
    })
    .await
    .context("the Apple TV did not answer in time")?
}

/// Pair setup's first half: connected and asked to pair, the Apple TV
/// shows its code and waits for [`Pairing::finish`].
#[derive(Debug)]
pub struct Pairing {
    stream: TcpStream,
    setup: hap::Setup,
    m2: tlv8::Tlv,
    since: std::time::Instant,
}

impl Pairing {
    pub async fn start(host: &str, port: u16) -> anyhow::Result<Self> {
        let mut stream = connect(host, port).await?;
        let setup = hap::Setup::new();
        let m2 = step(
            &mut stream,
            frame::PS_START,
            vec![
                ("_pd", Value::Bytes(hap::Setup::start())),
                ("_pwTy", Value::Int(1)),
            ],
            frame::PS_NEXT,
        )
        .await?;
        // A refusal (busy, wait) comes here, not with the code.
        hap::answer(&m2, 2)?;
        if m2.get(tlv8::SALT).is_none() || m2.get(tlv8::PUBLIC_KEY).is_none() {
            bail!("the Apple TV started no pairing (no salt or key in its answer)");
        }
        Ok(Self {
            stream,
            setup,
            m2,
            since: std::time::Instant::now(),
        })
    }

    /// Whether the Apple TV closed the request while the code was awaited
    /// (it then never reads what follows).
    /// Looked at, never read: what is there stays for the next step.
    async fn closed(&self) -> Option<String> {
        let mut probe = [0u8; 1];
        let waited = self.since.elapsed().as_secs();
        let peeked =
            tokio::time::timeout(Duration::from_millis(20), self.stream.peek(&mut probe)).await;
        match peeked {
            // Nothing there: still open.
            Err(_) => None,
            Ok(Ok(0)) => Some(format!("the Apple TV closed the pairing after {waited} s")),
            Ok(Ok(_)) => {
                tracing::info!(
                    "Apple TV: sent something unasked while the code was awaited ({waited} s)"
                );
                None
            }
            Ok(Err(e)) => Some(format!("the pairing link failed after {waited} s: {e}")),
        }
    }

    /// The code the Apple TV shows, and the name it will list Moli under.
    pub async fn finish(mut self, pin: &str, name: &str) -> anyhow::Result<Credentials> {
        if let Some(why) = self.closed().await {
            bail!(why);
        }
        tracing::info!(
            "Apple TV: code received after {} s, sending its proof",
            self.since.elapsed().as_secs()
        );
        let m3 = self.setup.code(&self.m2, pin)?;
        let m4 = step(
            &mut self.stream,
            frame::PS_NEXT,
            vec![("_pd", Value::Bytes(m3)), ("_pwTy", Value::Int(1))],
            frame::PS_NEXT,
        )
        .await
        .context("no answer to the code's proof")?;
        let m5 = self.setup.exchange(&m4, name)?;
        let m6 = step(
            &mut self.stream,
            frame::PS_NEXT,
            vec![("_pd", Value::Bytes(m5)), ("_pwTy", Value::Int(1))],
            frame::PS_NEXT,
        )
        .await?;
        self.setup.finish(&m6)
    }
}

async fn connect(host: &str, port: u16) -> anyhow::Result<TcpStream> {
    let stream = tokio::time::timeout(ANSWER, TcpStream::connect((host, port)))
        .await
        .context("the Apple TV did not answer")??;
    stream.set_nodelay(true).ok();
    Ok(stream)
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>;

/// An encrypted session: requests answered, events handed out.
#[derive(Debug)]
pub struct Session {
    writer: OwnedWriteHalf,
    out: Cipher,
    next: i64,
    pending: Pending,
    reader: tokio::task::JoinHandle<()>,
}

/// An event the Apple TV sent: its name and content.
pub type Event = (String, Value);

impl Drop for Session {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Session {
    /// Connects, verifies the pairing, and starts reading. The events come
    /// on the returned channel, which closes when the link does.
    pub async fn open(
        host: &str,
        port: u16,
        credentials: &Credentials,
    ) -> anyhow::Result<(Self, mpsc::Receiver<Event>)> {
        let mut stream = connect(host, port).await?;
        let (mut verify, m1) = hap::Verify::start()?;
        let m2 = step(
            &mut stream,
            frame::PV_START,
            vec![("_pd", Value::Bytes(m1)), ("_auTy", Value::Int(4))],
            frame::PV_NEXT,
        )
        .await?;
        let m3 = verify.prove(credentials, &m2)?;
        let m4 = step(
            &mut stream,
            frame::PV_NEXT,
            vec![("_pd", Value::Bytes(m3))],
            frame::PV_NEXT,
        )
        .await?;
        let (out_key, in_key) = verify.keys(&m4)?;
        let (mut read, writer) = stream.into_split();
        let pending: Pending = Arc::default();
        let (events, rx) = mpsc::channel(64);
        let reader = {
            let pending = pending.clone();
            let mut input = Cipher {
                key: in_key,
                count: 0,
            };
            tokio::spawn(async move {
                if let Err(e) = read_loop(&mut read, &mut input, &pending, &events).await {
                    tracing::debug!("Apple TV link closed: {e:#}");
                }
            })
        };
        let start = {
            let rng = ring::rand::SystemRandom::new();
            let mut b = [0u8; 2];
            ring::rand::SecureRandom::fill(&rng, &mut b).ok();
            i64::from(u16::from_le_bytes(b))
        };
        Ok((
            Self {
                writer,
                out: Cipher {
                    key: out_key,
                    count: 0,
                },
                next: start,
                pending,
                reader,
            },
            rx,
        ))
    }

    async fn send(&mut self, message: &Value) -> anyhow::Result<()> {
        let body = opack::pack(message);
        let head = header(frame::E_OPACK, body.len() + TAG);
        let sealed = self.out.seal(head, &body);
        self.writer.write_all(&head).await?;
        self.writer.write_all(&sealed).await?;
        Ok(())
    }

    fn number(&mut self) -> i64 {
        let x = self.next;
        self.next += 1;
        x
    }

    /// A request and its answer's content (`_c`), or the error it says.
    pub async fn request(&mut self, what: &str, content: Value) -> anyhow::Result<Value> {
        let x = self.number();
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(x, tx);
        let message = Value::dict([
            ("_i", Value::from(what)),
            ("_t", Value::Int(REQUEST)),
            ("_c", content),
            ("_x", Value::Int(x)),
        ]);
        if let Err(e) = self.send(&message).await {
            self.forget(x);
            return Err(e);
        }
        let answer = tokio::time::timeout(ANSWER, rx).await;
        self.forget(x);
        let answer = answer
            .with_context(|| format!("the Apple TV did not answer {what}"))?
            .context("the link to the Apple TV closed")?;
        if let Some(error) = answer.get("_em") {
            bail!(
                "the Apple TV refused {what}: {}",
                error.as_str().unwrap_or("?")
            );
        }
        Ok(answer.get("_c").cloned().unwrap_or(Value::Null))
    }

    /// An event to the Apple TV (no answer).
    pub async fn event(&mut self, what: &str, content: Value) -> anyhow::Result<()> {
        let x = self.number();
        self.send(&Value::dict([
            ("_i", Value::from(what)),
            ("_t", Value::Int(EVENT)),
            ("_c", content),
            ("_x", Value::Int(x)),
        ]))
        .await
    }

    fn forget(&self, x: i64) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&x);
    }

    /// Whether the link still reads.
    pub fn alive(&self) -> bool {
        !self.reader.is_finished()
    }
}

async fn read_loop<R: AsyncRead + Unpin>(
    read: &mut R,
    input: &mut Cipher,
    pending: &Pending,
    events: &mpsc::Sender<Event>,
) -> anyhow::Result<()> {
    loop {
        let (head, payload) = read_frame(read).await?;
        if payload.is_empty() {
            continue;
        }
        let plain = input.open(head, &payload)?;
        if head[0] != frame::E_OPACK {
            continue;
        }
        let Ok((message, _)) = opack::unpack(&plain) else {
            tracing::debug!("Apple TV: a message did not read");
            continue;
        };
        match message.get("_t").and_then(Value::as_int) {
            Some(RESPONSE) => {
                let x = message.get("_x").and_then(Value::as_int);
                let waiting = x.and_then(|x| {
                    pending
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&x)
                });
                if let Some(tx) = waiting {
                    let _ = tx.send(message);
                }
            }
            Some(EVENT) => {
                let name = message
                    .get("_i")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let content = message.get("_c").cloned().unwrap_or(Value::Null);
                // A full queue drops an event rather than stalling the link.
                let _ = events.try_send((name, content));
            }
            // A request from the Apple TV: Moli offers nothing to it.
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hap::tests::Device;
    use tokio::net::TcpListener;

    /// A pretend Apple TV: pair verify, then answers a request and sends an
    /// event, all encrypted as the real one does.
    async fn fake(listener: TcpListener, device: Device) {
        let (mut s, _) = listener.accept().await.unwrap();
        let (_, payload) = read_frame(&mut s).await.unwrap();
        let m1 = opack::unpack(&payload).unwrap().0;
        assert_eq!(m1.get("_auTy").and_then(Value::as_int), Some(4));
        let (m2, shared, ..) = device.answer(m1.get("_pd").unwrap().as_bytes().unwrap());
        let body = opack::pack(&Value::dict([("_pd", Value::Bytes(m2))]));
        s.write_all(&header(frame::PV_NEXT, body.len()))
            .await
            .unwrap();
        s.write_all(&body).await.unwrap();
        let (head, _m3) = read_frame(&mut s).await.unwrap();
        assert_eq!(head[0], frame::PV_NEXT);
        let m4 = opack::pack(&Value::dict([(
            "_pd",
            Value::Bytes(tlv8::write(&[(tlv8::STATE, &[4])])),
        )]));
        s.write_all(&header(frame::PV_NEXT, m4.len()))
            .await
            .unwrap();
        s.write_all(&m4).await.unwrap();
        // The device's side of the keys: it reads with Moli's out key.
        let mut from_moli = Cipher {
            key: crate::hap::tests_derive("", "ClientEncrypt-main", &shared),
            count: 0,
        };
        let mut to_moli = Cipher {
            key: crate::hap::tests_derive("", "ServerEncrypt-main", &shared),
            count: 0,
        };
        let (head, sealed) = read_frame(&mut s).await.unwrap();
        let request = opack::unpack(&from_moli.open(head, &sealed).unwrap())
            .unwrap()
            .0;
        assert_eq!(request.get("_i").and_then(Value::as_str), Some("_hidC"));
        let x = request.get("_x").cloned().unwrap();
        let mut send = |message: Value| {
            let body = opack::pack(&message);
            let head = header(frame::E_OPACK, body.len() + TAG);
            [head.to_vec(), to_moli.seal(head, &body)].concat()
        };
        let event = send(Value::dict([
            ("_i", Value::from("SystemStatus")),
            ("_t", Value::Int(EVENT)),
            ("_c", Value::dict([("state", Value::Int(3))])),
        ]));
        let response = send(Value::dict([
            ("_t", Value::Int(RESPONSE)),
            ("_x", x),
            ("_c", Value::dict([("ok", Value::Bool(true))])),
        ]));
        s.write_all(&event).await.unwrap();
        s.write_all(&response).await.unwrap();
        // A refused request.
        let (head, sealed) = read_frame(&mut s).await.unwrap();
        let request = opack::unpack(&from_moli.open(head, &sealed).unwrap())
            .unwrap()
            .0;
        let refusal = send(Value::dict([
            ("_t", Value::Int(RESPONSE)),
            ("_x", request.get("_x").cloned().unwrap()),
            ("_em", Value::from("No request handler")),
        ]));
        s.write_all(&refusal).await.unwrap();
        // Then the link closes.
    }

    #[tokio::test]
    async fn a_verified_session_asks_and_hears() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let device = Device::new();
        let credentials = device.credentials();
        let server = tokio::spawn(fake(listener, device));
        let (mut session, mut events) = Session::open("127.0.0.1", port, &credentials)
            .await
            .unwrap();
        let answer = session
            .request(
                "_hidC",
                Value::dict([("_hBtS", Value::Int(1)), ("_hidC", Value::Int(6))]),
            )
            .await
            .unwrap();
        assert_eq!(answer.get("ok"), Some(&Value::Bool(true)));
        let (name, content) = events.recv().await.unwrap();
        assert_eq!(name, "SystemStatus");
        assert_eq!(content.get("state").and_then(Value::as_int), Some(3));
        let err = session
            .request("Nope", Value::dict::<&str>([]))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("No request handler"), "{err}");
        server.await.unwrap();
        // The device hung up: the events channel closes.
        assert!(events.recv().await.is_none());
        assert!(!session.alive());
    }

    #[test]
    fn a_frame_header_is_type_then_24_bit_length() {
        assert_eq!(header(frame::E_OPACK, 0x01_0203), [8, 1, 2, 3]);
        assert_eq!(counter_nonce(1).as_ref()[..2], [1, 0]);
    }
}
