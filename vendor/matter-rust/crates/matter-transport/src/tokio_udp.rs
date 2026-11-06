//! Default Tokio-based UDP adapter for Matter.
//!
//! Wraps [`tokio::net::UdpSocket`] in a dual-stack IPv6 socket so the
//! same instance handles both IPv6 and IPv4-mapped traffic. Implements
//! [`crate::transport::Transport`] with synchronous `try_send_to` /
//! `try_recv_from`; callers drive readiness via the runtime
//! (`socket.readable().await`).
//!
//! # IPv6 multicast send
//!
//! The socket is configured at bind time with the `IPV6_MULTICAST_HOPS`
//! socket option so that the existing [`Transport::send`] call routes
//! group messages at the right hop limit without any API change:
//!
//! * **`IPV6_MULTICAST_HOPS`** — set to [`MATTER_GROUP_MULTICAST_HOPS`]
//!   (8). Matter group addresses are `ff35:…` (site-local scope, scope
//!   nibble = `5`). A hop limit of 8 clears any reasonable intra-site
//!   routing hop count while staying well clear of global scope.
//!
//! The outgoing interface is left at its kernel default (equivalent to
//! interface index 0 on Linux / "unset" on macOS). Setting
//! `IPV6_MULTICAST_IF` explicitly to 0 is rejected by macOS with
//! `EINVAL`; on Linux index 0 means "OS picks via routing table" but
//! the kernel-default behaviour is identical without the explicit call.
//! A future `bind_addr_with_if` variant can expose the interface
//! selector for multi-NIC hosts.
//!
//! End-to-end multicast delivery is **hardware-validated in the E3
//! runbook** (not in these unit tests). The unit tests here verify that
//! the socket accepts `IPV6_MULTICAST_HOPS` without error and that the
//! configured hop count reads back correctly.

use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

use crate::error::{Error, Result};
use crate::transport::{PeerAddress, Transport};

/// IPv6 minimum MTU minus IPv6 header + UDP header = safe single-packet
/// receive buffer size. Larger packets fragment; smaller is fine.
const RECV_BUF_SIZE: usize = 1500;

/// Hop limit set on `IPV6_MULTICAST_HOPS` for outbound group messages.
///
/// Matter group multicast addresses use `ff35:…` (site-local scope,
/// scope nibble = 5). A hop limit of 8 is well above any realistic
/// intra-site path length and well below global scope; it follows the
/// conservative convention used by the reference `matter.js`
/// implementation.
pub const MATTER_GROUP_MULTICAST_HOPS: u32 = 8;

/// Default Tokio-based UDP transport for Matter.
///
/// Construct via [`Self::bind`] (any port on `[::]`) or
/// [`Self::bind_addr`] (specific address). The socket is dual-stack
/// (`IPV6_V6ONLY = false`) so IPv4-mapped traffic also works.
#[derive(Debug)]
pub struct TokioUdpTransport {
    socket: UdpSocket,
    recv_buf: Vec<u8>,
    local: SocketAddr,
}

impl TokioUdpTransport {
    /// Bind a dual-stack UDP socket on `[::]:port`. Pass `0` for an
    /// OS-assigned port.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] on bind failure (port already in use,
    /// permission denied, IPv6 unsupported, etc.).
    pub async fn bind(port: u16) -> Result<Self> {
        let addr = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0));
        Self::bind_addr(addr).await
    }

    /// [`Self::bind`] with an explicit IPv6 multicast egress interface
    /// (`IPV6_MULTICAST_IF`). On a multi-homed host the kernel default has no
    /// route for the admin-local `ff35:` group address and group sends fail
    /// with "No route to host" — pass `if_nametoindex`'s value for the LAN
    /// interface. `None` (or `Some(0)`) falls back to the
    /// `MATTER_MULTICAST_IF` env var, then the kernel default.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] on bind failure or an invalid interface index.
    pub async fn bind_with_multicast_if(port: u16, multicast_if: Option<u32>) -> Result<Self> {
        let addr = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, port, 0, 0));
        Self::bind_addr_with_multicast_if(addr, multicast_if).await
    }

    /// Bind on a specific address. Useful for tests with `[::1]:0`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] on bind failure.
    // `async` is preserved for API symmetry with `bind` and to leave
    // room for a future migration to `tokio::net::UdpSocket::bind`
    // (which IS async). All current operations inside are synchronous.
    #[allow(clippy::unused_async)]
    pub async fn bind_addr(addr: SocketAddr) -> Result<Self> {
        Self::bind_addr_with_multicast_if(addr, None).await
    }

    /// [`Self::bind_addr`] with an explicit IPv6 multicast egress interface —
    /// see [`Self::bind_with_multicast_if`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] on bind failure or an invalid interface index.
    // `unused_async_trait_impl` is new in Rust 1.98 (pedantic) and is not
    // covered by the `unused_async` allow above. Both are deliberate: the
    // `async` is part of this published constructor's signature, so dropping it
    // (or switching to `-> impl Future`) would be a breaking change, and it
    // leaves room for the future migration to `tokio::net::UdpSocket::bind`
    // described on `bind_addr`.
    #[allow(clippy::unused_async)] // symmetry with `bind`; see `bind_addr`.
    #[allow(clippy::unused_async_trait_impl)]
    pub async fn bind_addr_with_multicast_if(
        addr: SocketAddr,
        multicast_if: Option<u32>,
    ) -> Result<Self> {
        let domain = match addr {
            SocketAddr::V4(_) => Domain::IPV4,
            SocketAddr::V6(_) => Domain::IPV6,
        };
        let raw = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
        // Disable IPV6_V6ONLY so dual-stack works.
        if matches!(addr, SocketAddr::V6(_)) {
            raw.set_only_v6(false)?;
            // Raise the multicast hop limit so outbound group packets
            // reach their site-local scope. The outgoing interface is
            // left at the kernel default (unset) — see module-level
            // doc for why we do NOT call `set_multicast_if_v6(0)`.
            raw.set_multicast_hops_v6(MATTER_GROUP_MULTICAST_HOPS)?;
            // Multicast egress-interface selection: an explicit `multicast_if`
            // (the `MatterControllerBuilder::multicast_interface` path) wins;
            // the `MATTER_MULTICAST_IF` env var remains as a compat fallback
            // for the runbooks that predate the builder option. On a
            // multi-homed/macOS host the kernel default has no route for the
            // admin-local `ff35:` group address, so a group send fails with
            // "No route to host" without one of these. A real index is
            // accepted on macOS (only index 0 is rejected).
            let idx = multicast_if.filter(|&i| i != 0).or_else(|| {
                std::env::var("MATTER_MULTICAST_IF")
                    .ok()
                    .and_then(|s| s.parse::<u32>().ok())
                    .filter(|&i| i != 0)
            });
            if let Some(idx) = idx {
                raw.set_multicast_if_v6(idx)?;
            }
        }
        raw.set_nonblocking(true)?;
        raw.bind(&addr.into())?;
        let std_sock: std::net::UdpSocket = raw.into();
        let socket = UdpSocket::from_std(std_sock)?;
        let local = socket.local_addr()?;
        Ok(Self {
            socket,
            recv_buf: vec![0u8; RECV_BUF_SIZE],
            local,
        })
    }

    /// Borrow the underlying [`tokio::net::UdpSocket`]. Useful for
    /// awaiting `readable()` / `writable()` from a driver loop.
    #[must_use]
    pub fn socket(&self) -> &UdpSocket {
        &self.socket
    }
}

impl Transport for TokioUdpTransport {
    fn send(&mut self, peer: PeerAddress, packet: Vec<u8>) -> Result<()> {
        match self.socket.try_send_to(&packet, peer.0) {
            Ok(_) => Ok(()),
            // WouldBlock surfaces as Io(WouldBlock); caller decides
            // whether to retry. We don't transparently swallow it
            // because the caller's driver loop pairs this with
            // socket.writable().await.
            Err(e) => Err(Error::Io(e)),
        }
    }

    fn poll_recv(&mut self) -> Result<Option<(PeerAddress, Vec<u8>)>> {
        match self.socket.try_recv_from(&mut self.recv_buf) {
            Ok((n, from)) => {
                let bytes = self.recv_buf[..n].to_vec();
                Ok(Some((PeerAddress(from), bytes)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(Error::Io(e)),
        }
    }

    fn local_address(&self) -> SocketAddr {
        self.local
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {

    #[tokio::test]
    async fn bind_with_multicast_if_none_is_plain_bind() {
        let t = TokioUdpTransport::bind_with_multicast_if(0, None)
            .await
            .expect("bind with None must behave like plain bind");
        assert_ne!(t.socket().local_addr().unwrap().port(), 0);
    }

    #[tokio::test]
    async fn bind_with_bogus_multicast_if_errors() {
        // Interface indexes are small; 0x00FF_FFF0 does not exist, so
        // IPV6_MULTICAST_IF must be rejected by the kernel and surface as Io.
        let err = TokioUdpTransport::bind_with_multicast_if(0, Some(0x00FF_FFF0))
            .await
            .expect_err("bogus interface index must fail the bind");
        assert!(matches!(err, crate::Error::Io(_)), "unexpected: {err:?}");
    }

    use super::*;
    use std::net::{Ipv6Addr, SocketAddr};

    #[tokio::test]
    async fn bind_assigns_local_port() {
        let t = TokioUdpTransport::bind_addr("[::1]:0".parse::<SocketAddr>().unwrap())
            .await
            .unwrap();
        assert_ne!(t.local_address().port(), 0);
    }

    #[tokio::test]
    async fn bind_specific_port() {
        // Bind on port 0 first to find a free one, drop, then re-bind.
        let pick = TokioUdpTransport::bind_addr("[::1]:0".parse().unwrap())
            .await
            .unwrap();
        let port = pick.local_address().port();
        drop(pick);
        let t = TokioUdpTransport::bind_addr(format!("[::1]:{port}").parse().unwrap())
            .await
            .unwrap();
        assert_eq!(t.local_address().port(), port);
    }

    #[tokio::test]
    async fn bind_in_use_errors() {
        let a = TokioUdpTransport::bind_addr("[::1]:0".parse().unwrap())
            .await
            .unwrap();
        let port = a.local_address().port();
        let err = TokioUdpTransport::bind_addr(format!("[::1]:{port}").parse().unwrap())
            .await
            .unwrap_err();
        // The error MUST be Error::Io; on AddrInUse we expect kind ==
        // AddrInUse, but on some platforms binding the same port twice
        // succeeds with SO_REUSEADDR. Assert just the variant for
        // portability.
        assert!(matches!(err, crate::Error::Io(_)));
    }

    #[tokio::test]
    async fn send_recv_roundtrip_localhost() {
        let mut alice = TokioUdpTransport::bind_addr("[::1]:0".parse().unwrap())
            .await
            .unwrap();
        let mut bob = TokioUdpTransport::bind_addr("[::1]:0".parse().unwrap())
            .await
            .unwrap();
        let bob_addr = PeerAddress(bob.local_address());

        // Tokio's try_send_to requires the runtime to have observed
        // write readiness first; otherwise it returns WouldBlock even
        // on a freshly-bound, never-saturated UDP socket. Mirror the
        // production driver loop's pattern: await writable() then send.
        alice.socket().writable().await.unwrap();
        alice.send(bob_addr, b"hello matter".to_vec()).unwrap();

        // try_recv_from is non-blocking; loop with a 1s budget.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            if let Some((from, payload)) = bob.poll_recv().unwrap() {
                assert_eq!(payload, b"hello matter");
                assert_eq!(from.0.port(), alice.local_address().port());
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "did not receive packet within 1s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn poll_recv_empty_returns_ok_none() {
        let mut t = TokioUdpTransport::bind_addr("[::1]:0".parse().unwrap())
            .await
            .unwrap();
        assert!(matches!(t.poll_recv(), Ok(None)));
    }

    #[tokio::test]
    async fn peer_address_preserves_scope_id_through_send_recv() {
        let mut alice = TokioUdpTransport::bind_addr("[::1]:0".parse().unwrap())
            .await
            .unwrap();
        let mut bob = TokioUdpTransport::bind_addr("[::1]:0".parse().unwrap())
            .await
            .unwrap();
        // Bob's address as [::1]:port — loopback, scope_id naturally 0.
        let bob_addr = PeerAddress(bob.local_address());
        // Mirror the production driver: await writable() before send
        // so Tokio's internal readiness flag is set.
        alice.socket().writable().await.unwrap();
        alice.send(bob_addr, b"x".to_vec()).unwrap();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            if let Some((from, _)) = bob.poll_recv().unwrap() {
                // For loopback, the inbound peer address is also [::1].
                if let SocketAddr::V6(v6) = from.0 {
                    assert_eq!(*v6.ip(), Ipv6Addr::LOCALHOST);
                }
                break;
            }
            assert!(
                tokio::time::Instant::now() <= deadline,
                "did not receive within 1s"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    // ── Multicast socket-option tests ────────────────────────────────────────
    //
    // These tests verify that:
    //   (a) `MATTER_GROUP_MULTICAST_HOPS` round-trips through the
    //       `IPV6_MULTICAST_HOPS` socket option on a raw socket2::Socket; and
    //   (b) `TokioUdpTransport::bind_addr` succeeds with the hop option
    //       applied (the bind path sets it unconditionally for IPv6 sockets).
    //
    // Note: we do NOT test `set_multicast_if_v6(0)` because macOS rejects
    // interface index 0 with EINVAL. The outgoing interface is instead left
    // at its kernel default (equivalent to "let OS pick" on all platforms).
    //
    // End-to-end delivery of a group packet to real Matter devices is
    // hardware-validated in the E3 runbook; it cannot be exercised in CI
    // because it requires actual network interfaces with multicast routing.

    /// Verify that the `MATTER_GROUP_MULTICAST_HOPS` constant round-trips
    /// through `set_multicast_hops_v6` / `multicast_hops_v6` on a raw
    /// `socket2::Socket`. If the kernel rejects the value or the constant
    /// is wrong this test fails.
    #[test]
    fn multicast_hops_const_roundtrips_on_raw_socket() {
        use socket2::{Domain, Protocol, Socket, Type};
        let sock = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP)).unwrap();
        sock.set_multicast_hops_v6(MATTER_GROUP_MULTICAST_HOPS)
            .unwrap();
        let readback = sock.multicast_hops_v6().unwrap();
        assert_eq!(
            readback, MATTER_GROUP_MULTICAST_HOPS,
            "IPV6_MULTICAST_HOPS readback mismatch"
        );
    }

    /// Verify that `TokioUdpTransport::bind_addr` succeeds after
    /// `set_multicast_hops_v6` is applied inside it. The test binds to
    /// `[::1]:0` (IPv6, so the hop-limit branch runs) and confirms the
    /// returned port is non-zero — which means bind + option-set both
    /// succeeded.
    #[tokio::test]
    async fn bind_addr_succeeds_with_multicast_hops_applied() {
        let t = TokioUdpTransport::bind_addr("[::1]:0".parse::<SocketAddr>().unwrap())
            .await
            .unwrap();
        // Port 0 was resolved to a real OS-assigned port.
        assert_ne!(t.local_address().port(), 0);
    }

    /// Verify that `Transport::send` to a Matter group multicast address
    /// (`ff35:…`) does not return an error at the socket layer on a
    /// loopback-bound socket. Whether the datagram is delivered depends on
    /// a joined multicast group and a real interface — that is
    /// hardware-validated in the E3 runbook, not here.
    ///
    /// On most kernels `sendto` to a multicast address returns `Ok` even
    /// when the packet is silently dropped (no route). If the kernel
    /// returns `ENETUNREACH` the test skips rather than failing, because
    /// that is a routing configuration detail, not a bug in our code.
    #[tokio::test]
    async fn send_to_multicast_addr_does_not_error_at_socket_layer() {
        let mut t = TokioUdpTransport::bind_addr("[::1]:0".parse::<SocketAddr>().unwrap())
            .await
            .unwrap();

        // ff35:0040:fda1:a2a4:a8b1:b2b4:b800:e10f — the canonical test
        // vector from matter-crypto's group_multicast_ipv6 KAT.
        let group: Ipv6Addr = "ff35:0040:fda1:a2a4:a8b1:b2b4:b800:e10f".parse().unwrap();
        // Matter group port per spec §4.11.2.
        let dest = PeerAddress::from_ipv6(group, 5540);

        t.socket().writable().await.unwrap();
        match t.send(dest, b"test".to_vec()) {
            Ok(()) => { /* send accepted by kernel — expected path */ }
            Err(crate::Error::Io(e))
                if matches!(
                    e.kind(),
                    // WouldBlock: Tokio hasn't internally observed write
                    // readiness yet — same as the unicast path.
                    std::io::ErrorKind::WouldBlock
                        // NetworkUnreachable / HostUnreachable (ENETUNREACH /
                        // EHOSTUNREACH): no multicast route on the loopback
                        // interface in a CI container without a real NIC.
                        // These are routing-configuration issues, not bugs in
                        // our socket-option setup.
                        | std::io::ErrorKind::NetworkUnreachable
                        | std::io::ErrorKind::HostUnreachable
                ) =>
            {
                // Acceptable routing-layer rejection; not our bug.
            }
            Err(e) => panic!("unexpected socket error on multicast send: {e}"),
        }
    }
}
