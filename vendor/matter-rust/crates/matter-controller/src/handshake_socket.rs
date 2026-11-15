//! The [`AsyncDatagram`] a spawned CASE-connect task drives instead of a real
//! socket, for the event-driven connect path.
//!
//! Connecting to a device runs a multi-round-trip CASE (SIGMA-I) handshake plus
//! an mDNS resolution — both of which `.await` on the network. Running them
//! inline in the actor's `select!` loop would freeze every other session's MRP,
//! liveness, and inbound handling for the whole connect window. Instead the
//! connect runs on a spawned task, but the task never touches a socket: the
//! **actor keeps sole ownership of the UDP socket**.
//!
//! This type bridges the two. The task drives [`run_case_establish`] over a
//! `HandshakeSocket`, whose:
//! - `send_to` hands each outbound datagram to the actor (tagged with this
//!   task's `node_id`) over a channel; the actor records the peer route and
//!   performs the real send on its own socket.
//! - `recv_from` awaits the inbound handshake replies the actor's `select!`
//!   loop demuxes back to this task (unsecured, session-id-0 datagrams from the
//!   task's peer).
//!
//! Because every datagram still leaves and arrives on the actor's own socket,
//! the established session lives on that socket from the first message — there
//! is no second socket and no session migration. The whole exchange is
//! hermetically testable with an ordinary in-memory socket pair.
//!
//! [`run_case_establish`]: matter_commissioning::driver::run_case_establish

use std::io;
use std::net::SocketAddr;

use matter_commissioning::driver::AsyncDatagram;
use tokio::sync::{mpsc, Mutex};

/// One outbound handshake datagram produced by a CASE-connect task. The actor
/// drains these, installs the `peer` → task inbound route (so the device's
/// replies demux back), and sends `bytes` on its own socket.
pub(crate) struct HandshakeOutbound {
    /// The connecting device's operational node id — identifies which in-flight
    /// connect task produced this datagram (connects coalesce one-per-node).
    pub node_id: u64,
    /// The datagram bytes to put on the wire verbatim.
    pub bytes: Vec<u8>,
    /// The device address to send to (the task resolved it via mDNS). The actor
    /// learns the peer from here — installing the inbound route exactly when the
    /// first datagram goes out, so it is always in place before any reply.
    pub peer: SocketAddr,
}

/// The [`AsyncDatagram`] seam a spawned CASE-connect task drives in place of a
/// real socket. See the module docs.
pub(crate) struct HandshakeSocket {
    node_id: u64,
    outbound: mpsc::Sender<HandshakeOutbound>,
    /// Inbound handshake replies the actor forwards to this task. Behind a
    /// `Mutex` held across the await, exactly like [`InMemoryDatagram`]; the
    /// task drives its handshake from a single future so it is never polled
    /// concurrently.
    ///
    /// [`InMemoryDatagram`]: matter_commissioning::driver::InMemoryDatagram
    inbound: Mutex<mpsc::Receiver<(Vec<u8>, SocketAddr)>>,
}

impl HandshakeSocket {
    /// Build the seam for a connect task to `node_id`: `outbound` is the shared
    /// actor-owned send channel; `inbound` is this task's private reply queue.
    pub(crate) fn new(
        node_id: u64,
        outbound: mpsc::Sender<HandshakeOutbound>,
        inbound: mpsc::Receiver<(Vec<u8>, SocketAddr)>,
    ) -> Self {
        Self {
            node_id,
            outbound,
            inbound: Mutex::new(inbound),
        }
    }
}

impl AsyncDatagram for HandshakeSocket {
    /// Hand the datagram to the actor to send on its own socket. Returns
    /// [`io::ErrorKind::BrokenPipe`] if the actor has gone (its receiver
    /// dropped) — surfaced to the handshake as a transport error.
    async fn send_to(&self, buf: &[u8], peer: SocketAddr) -> io::Result<()> {
        self.outbound
            .send(HandshakeOutbound {
                node_id: self.node_id,
                bytes: buf.to_vec(),
                peer,
            })
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "actor loop gone"))
    }

    /// Await the next inbound handshake reply the actor demuxes to this task.
    /// Returns [`io::ErrorKind::BrokenPipe`] if the actor drops the sender (e.g.
    /// the connect was abandoned).
    async fn recv_from(&self) -> io::Result<(Vec<u8>, SocketAddr)> {
        let mut rx = self.inbound.lock().await;
        rx.recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "actor loop gone"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    /// `send_to` tags each datagram with the task's node id + destination and
    /// hands it to the actor's outbound channel; `recv_from` yields whatever the
    /// actor forwards to the task's inbound channel.
    #[tokio::test]
    async fn bridges_outbound_and_inbound_over_channels() {
        let (out_tx, mut out_rx) = mpsc::channel::<HandshakeOutbound>(4);
        let (in_tx, in_rx) = mpsc::channel::<(Vec<u8>, SocketAddr)>(4);
        let sock = HandshakeSocket::new(0x1234, out_tx, in_rx);
        let peer: SocketAddr = "127.0.0.1:5540".parse().unwrap();

        // Outbound: send_to → the actor sees the bytes tagged with node id+peer.
        sock.send_to(b"sigma1", peer).await.unwrap();
        let out = out_rx.recv().await.unwrap();
        assert_eq!(out.node_id, 0x1234);
        assert_eq!(out.bytes, b"sigma1");
        assert_eq!(out.peer, peer);

        // Inbound: what the actor forwards is what recv_from yields.
        in_tx.send((b"sigma2".to_vec(), peer)).await.unwrap();
        let (bytes, from) = sock.recv_from().await.unwrap();
        assert_eq!(bytes, b"sigma2");
        assert_eq!(from, peer);
    }

    /// When the actor is gone (channels dropped) both directions surface a
    /// `BrokenPipe` transport error rather than hanging.
    #[tokio::test]
    async fn reports_broken_pipe_when_actor_gone() {
        let (out_tx, out_rx) = mpsc::channel::<HandshakeOutbound>(1);
        let (in_tx, in_rx) = mpsc::channel::<(Vec<u8>, SocketAddr)>(1);
        let sock = HandshakeSocket::new(1, out_tx, in_rx);
        let peer: SocketAddr = "127.0.0.1:5540".parse().unwrap();

        drop(out_rx); // actor's outbound receiver gone
        let err = sock.send_to(b"x", peer).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);

        drop(in_tx); // actor's inbound sender gone → recv_from ends
        let err = sock.recv_from().await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }
}
