//! The one socket every bulb is reached through. A reply counts only if it
//! comes from the address and port asked, to our source, with the sequence
//! and the serial asked: the LAN protocol has no authentication, and an
//! 8-bit sequence alone is easy to guess.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use ring::rand::{SecureRandom as _, SystemRandom};
use tokio::net::UdpSocket;
use tokio::time::Instant;

use crate::proto::{self, Message, Reply, Serial};

/// How long a bulb has to answer one message sent once (a poll).
pub const ONCE: Duration = Duration::from_millis(500);
/// A question to a bulb met or completed: sent at 0, 0.4 and 1.2 s.
pub const READ: Duration = Duration::from_secs(2);
/// An order: sent at 0, 0.4, 1.2 and 2.7 s, within the hub's 5 s.
pub const ORDER: Duration = Duration::from_secs(4);
const FIRST_RETRY: Duration = Duration::from_millis(400);
const LAST_RETRY: Duration = Duration::from_millis(1500);
/// How long a search listens, in two rounds of messages.
const SEARCH_ROUND: Duration = Duration::from_millis(600);

/// What came back.
#[derive(Debug)]
pub enum Answer {
    Got(Message),
    /// The firmware does not know the message.
    Unhandled,
    Silent,
}

impl Answer {
    pub fn message(self) -> Option<Message> {
        match self {
            Self::Got(m) => Some(m),
            Self::Unhandled | Self::Silent => None,
        }
    }
}

pub struct Link {
    socket: UdpSocket,
    source: u32,
    sequence: u8,
}

impl Link {
    pub async fn open() -> anyhow::Result<Self> {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
        socket.set_broadcast(true)?;
        let mut random = [0u8; 5];
        SystemRandom::new()
            .fill(&mut random)
            .map_err(|_| anyhow::anyhow!("no randomness"))?;
        // 0 and 1 have a meaning of their own to a bulb (answer everyone).
        let source = u32::from_le_bytes([random[0], random[1], random[2], random[3]]).max(2);
        Ok(Self {
            socket,
            source,
            sequence: random[4],
        })
    }

    fn next(&mut self) -> u8 {
        self.sequence = self.sequence.wrapping_add(1);
        self.sequence
    }

    /// One message to one bulb and its answer of type `expect`, within
    /// `until`; sent again (same sequence, so a late copy still counts)
    /// at growing intervals if `retry`, else once.
    pub async fn ask(
        &mut self,
        to: SocketAddrV4,
        target: Serial,
        (kind, payload): (u16, &[u8]),
        (reply, expect): (Reply, u16),
        (until, retry): (Instant, bool),
    ) -> Answer {
        let sequence = self.next();
        let frame = proto::encode(self.source, Some(target), sequence, kind, payload, reply);
        let mut wait = FIRST_RETRY;
        let mut buf = [0u8; 1024];
        loop {
            let _ = self.socket.send_to(&frame, to).await;
            let resend = if retry {
                (Instant::now() + wait).min(until)
            } else {
                until
            };
            wait = (wait * 2).min(LAST_RETRY);
            while let Ok(Ok((n, from))) =
                tokio::time::timeout_at(resend, self.socket.recv_from(&mut buf)).await
            {
                if from != SocketAddr::V4(to) {
                    continue;
                }
                let Some(m) = proto::decode(&buf[..n]) else {
                    continue;
                };
                if m.source != self.source || m.sequence != sequence || m.target != target {
                    continue;
                }
                if m.kind == expect {
                    return Answer::Got(m);
                }
                if m.kind == proto::STATE_UNHANDLED && proto::unhandled(&m.payload) == Some(kind) {
                    return Answer::Unhandled;
                }
            }
            if Instant::now() >= until {
                return Answer::Silent;
            }
        }
    }

    /// Who answers `GetService` at `hosts`, and to a broadcast to `broadcast`
    /// if given: every (serial, address) heard. Without a broadcast, only
    /// the answers from `hosts` count.
    pub async fn search(
        &mut self,
        hosts: &[SocketAddrV4],
        broadcast: Option<SocketAddrV4>,
    ) -> HashMap<Serial, Vec<SocketAddrV4>> {
        let mut found: HashMap<Serial, Vec<SocketAddrV4>> = HashMap::new();
        let sequence = self.next();
        let frame = proto::encode(
            self.source,
            None,
            sequence,
            proto::GET_SERVICE,
            &[],
            Reply::Response,
        );
        let mut to: Vec<SocketAddrV4> = hosts.to_vec();
        to.extend(broadcast);
        for _ in 0..2 {
            for at in &to {
                let _ = self.socket.send_to(&frame, at).await;
            }
            let deadline = Instant::now() + SEARCH_ROUND;
            let mut buf = [0u8; 1024];
            while let Ok(Ok((n, from))) =
                tokio::time::timeout_at(deadline, self.socket.recv_from(&mut buf)).await
            {
                let (Some(m), SocketAddr::V4(from)) = (proto::decode(&buf[..n]), from) else {
                    continue;
                };
                if m.source != self.source
                    || m.sequence != sequence
                    || m.kind != proto::STATE_SERVICE
                    || (broadcast.is_none() && !hosts.contains(&from))
                {
                    continue;
                }
                if let Some(port) = proto::udp_port(&m.payload) {
                    let at = SocketAddrV4::new(*from.ip(), port);
                    let heard = found.entry(m.target).or_default();
                    if !heard.contains(&at) {
                        heard.push(at);
                    }
                }
            }
        }
        found
    }
}
