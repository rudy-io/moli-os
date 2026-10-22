//! Telling the home network that a HomeKit bridge is here: an mDNS
//! responder for `<name>._hap._tcp.local` (PTR, SRV, TXT, A), sharing port
//! 5353 with whatever else answers there (Avahi). Announced at start and
//! whenever its TXT changes (paired or not, configuration number).

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use anyhow::Context as _;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::watch;

const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const PORT: u16 = 5353;
const SERVICE: &str = "_hap._tcp.local";
const SERVICES: &str = "_services._dns-sd._udp.local";

const PTR: u16 = 12;
const TXT: u16 = 16;
const SRV: u16 = 33;
const A: u16 = 1;
const ANY: u16 = 255;
const IN: u16 = 1;
/// « Cache flush »: this answer replaces what was known.
const FLUSH: u16 = 0x8000;

/// What is announced.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub name: String,
    pub host: String,
    pub ip: Ipv4Addr,
    pub port: u16,
    pub txt: Vec<String>,
}

fn name(out: &mut Vec<u8>, name: &str) {
    for label in name.trim_end_matches('.').split('.') {
        let label = &label.as_bytes()[..label.len().min(63)];
        out.push(u8::try_from(label.len()).unwrap_or(63));
        out.extend_from_slice(label);
    }
    out.push(0);
}

fn record(out: &mut Vec<u8>, owner: &str, kind: u16, class: u16, ttl: u32, data: &[u8]) {
    name(out, owner);
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&u16::try_from(data.len()).unwrap_or(0).to_be_bytes());
    out.extend_from_slice(data);
}

impl Record {
    fn instance(&self) -> String {
        format!("{}.{SERVICE}", self.name.replace('.', " "))
    }

    fn host_name(&self) -> String {
        format!("{}.local", self.host)
    }

    /// The whole answer (PTR, SRV, TXT, A); `ttl` 0 says goodbye.
    pub fn response(&self, ttl: bool) -> Vec<u8> {
        let (long, short) = if ttl { (4500, 120) } else { (0, 0) };
        let mut out = vec![0, 0, 0x84, 0, 0, 0, 0, 4, 0, 0, 0, 0];
        let mut ptr = Vec::new();
        name(&mut ptr, &self.instance());
        record(&mut out, SERVICE, PTR, IN, long, &ptr);
        let mut srv = vec![0, 0, 0, 0];
        srv.extend_from_slice(&self.port.to_be_bytes());
        name(&mut srv, &self.host_name());
        record(&mut out, &self.instance(), SRV, IN | FLUSH, short, &srv);
        let mut txt = Vec::new();
        for entry in &self.txt {
            let entry = &entry.as_bytes()[..entry.len().min(255)];
            txt.push(u8::try_from(entry.len()).unwrap_or(255));
            txt.extend_from_slice(entry);
        }
        record(&mut out, &self.instance(), TXT, IN | FLUSH, long, &txt);
        record(
            &mut out,
            &self.host_name(),
            A,
            IN | FLUSH,
            short,
            &self.ip.octets(),
        );
        out
    }

    /// Whether a query asks for us.
    fn asked(&self, query: &[u8]) -> bool {
        questions(query).is_some_and(|qs| {
            qs.iter().any(|(owner, kind)| {
                let owner = owner.to_ascii_lowercase();
                let wants = |n: &str, k: u16| {
                    owner == n.to_ascii_lowercase() && (*kind == k || *kind == ANY)
                };
                wants(SERVICE, PTR)
                    || wants(SERVICES, PTR)
                    || wants(&self.instance(), SRV)
                    || wants(&self.instance(), TXT)
                    || wants(&self.host_name(), A)
            })
        })
    }
}

/// A name at `pos` (compression followed, a few hops at most).
fn read_name(packet: &[u8], mut pos: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut end = None;
    for _ in 0..32 {
        let len = *packet.get(pos)?;
        if len & 0xC0 == 0xC0 {
            let target = usize::from(u16::from_be_bytes([len & 0x3F, *packet.get(pos + 1)?]));
            end.get_or_insert(pos + 2);
            pos = target;
            continue;
        }
        if len == 0 {
            return Some((labels.join("."), end.unwrap_or(pos + 1)));
        }
        let label = packet.get(pos + 1..pos + 1 + usize::from(len))?;
        labels.push(String::from_utf8_lossy(label).into_owned());
        pos += 1 + usize::from(len);
    }
    None
}

/// A query's questions; `None` for an answer or a malformed packet.
fn questions(packet: &[u8]) -> Option<Vec<(String, u16)>> {
    if packet.len() < 12 || packet[2] & 0x80 != 0 {
        return None;
    }
    let count = u16::from_be_bytes([packet[4], packet[5]]);
    let mut pos = 12;
    let mut out = Vec::new();
    for _ in 0..count.min(32) {
        let (owner, next) = read_name(packet, pos)?;
        let kind = u16::from_be_bytes([*packet.get(next)?, *packet.get(next + 1)?]);
        pos = next + 4;
        out.push((owner, kind));
    }
    Some(out)
}

/// The address the house's network reaches us at (no packet is sent).
pub fn local_ip() -> anyhow::Result<Ipv4Addr> {
    let probe = std::net::UdpSocket::bind("0.0.0.0:0")?;
    probe.connect("192.0.2.1:9")?;
    match probe.local_addr()? {
        SocketAddr::V4(a) if !a.ip().is_loopback() => Ok(*a.ip()),
        _ => anyhow::bail!("no IPv4 address on the home network"),
    }
}

fn socket(ip: Ipv4Addr) -> anyhow::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.set_reuse_port(true)?;
    socket.set_nonblocking(true)?;
    socket
        .bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT).into())
        .context("mDNS port 5353")?;
    socket.join_multicast_v4(&GROUP, &ip)?;
    socket.set_multicast_if_v4(&ip)?;
    socket.set_multicast_loop_v4(true)?;
    Ok(UdpSocket::from_std(socket.into())?)
}

/// Answers for as long as it runs; `record` changes are announced.
pub async fn serve(mut record: watch::Receiver<Record>) -> anyhow::Result<()> {
    let ip = record.borrow().ip;
    let socket = socket(ip)?;
    let group = SocketAddr::from((GROUP, PORT));
    let mut buf = vec![0u8; 9000];
    let mut announce = 3u8;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            changed = record.changed() => {
                if changed.is_err() {
                    let goodbye = record.borrow().response(false);
                    let _ = socket.send_to(&goodbye, group).await;
                    return Ok(());
                }
                announce = 3;
            }
            _ = tick.tick(), if announce > 0 => {
                announce -= 1;
                let answer = record.borrow().response(true);
                socket.send_to(&answer, group).await?;
            }
            read = socket.recv_from(&mut buf) => {
                let (n, from) = read?;
                let answer = {
                    let current = record.borrow();
                    current.asked(&buf[..n]).then(|| current.response(true))
                };
                if let Some(mut answer) = answer {
                    if from.port() == PORT {
                        socket.send_to(&answer, group).await?;
                    } else {
                        // A one-shot query (« legacy unicast »): answered to
                        // its sender, under its id.
                        answer[..2].copy_from_slice(&buf[..2]);
                        socket.send_to(&answer, from).await?;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moli() -> Record {
        Record {
            name: "Moli".into(),
            host: "moli-4f2a".into(),
            ip: Ipv4Addr::new(192, 168, 1, 42),
            port: 51826,
            txt: vec!["c#=1".into(), "sf=1".into(), "ci=2".into()],
        }
    }

    fn query(owner: &str, kind: u16) -> Vec<u8> {
        let mut q = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        name(&mut q, owner);
        q.extend_from_slice(&kind.to_be_bytes());
        q.extend_from_slice(&IN.to_be_bytes());
        q
    }

    #[test]
    fn it_answers_for_hap_and_its_own_names_only() {
        let r = moli();
        assert!(r.asked(&query("_hap._tcp.local", PTR)));
        assert!(r.asked(&query("Moli._hap._tcp.local", SRV)));
        assert!(r.asked(&query("moli-4f2a.local", ANY)));
        assert!(!r.asked(&query("_airplay._tcp.local", PTR)));
        assert!(!r.asked(&query("other.local", A)));
        // An answer from someone else is not a question.
        assert!(!r.asked(&r.response(true)));
    }

    #[test]
    fn the_answer_holds_four_records_and_compressed_names_are_read() {
        let answer = moli().response(true);
        assert_eq!(&answer[2..4], &[0x84, 0]);
        assert_eq!(u16::from_be_bytes([answer[6], answer[7]]), 4);
        let (owner, _) = read_name(&answer, 12).unwrap();
        assert_eq!(owner, "_hap._tcp.local");
        // A pointer back to offset 12.
        let mut packet = answer.clone();
        let at = packet.len();
        packet.extend_from_slice(&[0xC0, 12]);
        assert_eq!(
            read_name(&packet, at).unwrap(),
            ("_hap._tcp.local".into(), at + 2)
        );
        // A loop of pointers ends.
        assert!(read_name(&[0xC0, 0], 0).is_none());
    }
}
