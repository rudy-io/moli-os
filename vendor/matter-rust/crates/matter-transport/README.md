# matter-transport

Matter network transport — secured-message framing, MRP reliability,
session management, and default Tokio UDP + mdns-sd adapters. Part of
the [matter-rust](https://github.com/phunapps/matter-rust) workspace.

## Scope

Implements Matter Core Specification §4 (network transport) plus the
MRP reliability layer (§4.11) and the application protocol header
(§4.4.5).

- **Framing:** secured-message header encode/decode, AES-CCM-128 payload
  encryption, and sliding-window replay protection — for both unicast
  sessions and **group (multicast) messages**, including message
  privacy obfuscation.
- **Sessions:** `SessionManager` owns per-session counters, replay
  windows, and MRP state, and is the seam through which messages are
  encoded outbound and decoded inbound. The session table is bounded
  with idle-first eviction (DoS defence).
- **MRP:** a per-session sans-IO state machine (pending acks, piggyback
  queue, exchange table, recent-reliable cache). Retransmit timing is
  sized to the **peer** — the active/idle base is chosen from the peer's
  activity within its Session Active Threshold (re-evaluated per
  retransmit, chip `GetMRPBaseTimeout`) using the peer's advertised
  `SII`/`SAI`/`SAT` (`MrpConfig::for_peer`), so a sleepy/ICD device is
  never hammered with active-interval spacing. Closing an exchange hands
  out the ACK it still owes at once (`SessionManager::finish_exchange`,
  as chip's `FlushAcks` on close); the 200 ms standalone-ACK timer is
  only the fallback.
- **Secure Channel CloseSession:** `secure_channel::encode_close_session`
  closes a session the way the spec asks (new exchange, no R flag), and
  `secure_channel::is_close_session` recognises a peer's, byte-checked
  against matter.js.
- **Protocol header:** the Matter application protocol header codec,
  with skip-and-ignore handling of SX/V extensions.
- **Transport + Discovery:** sans-IO `Transport` / `Discovery` traits
  and the service-record types for Matter's commissionable and
  operational mDNS records, plus default Tokio UDP and `mdns-sd`
  adapters behind Cargo features.

## Status

**0.7.3**, published on crates.io. Feature-complete and validated
against real silicon (ESP32-C6 over Wi-Fi and Thread) via the
higher-level crates. Stability: a `0.x` crate, so a **minor** bump may
break API.

## Cargo features

- `tokio` (default): enables `TokioUdpTransport` and the `Error::Io`
  variant. Pulls `tokio` 1.x with features `net + rt + io-util`.
- `mdns-sd` (default): enables `MdnsSdDiscovery` and the `Error::Mdns`
  variant. Pulls `mdns-sd` 0.21.

Embedded callers disable defaults:

```toml
matter-transport = { version = "0.7", default-features = false }
```

…and implement `Transport` + `Discovery` themselves against their HAL.

## Minimal example

```rust,no_run
use std::time::Instant;
use matter_transport::{
    protocol_header::ProtocolId,
    session::{PeerHint, SessionManager, SessionRole},
    MrpFlags, PeerAddress, TokioUdpTransport, Transport,
};
use matter_crypto::pase::PaseSessionKeys;

# async fn run() -> matter_transport::Result<()> {
let mut tx = TokioUdpTransport::bind(5540).await?;
let mut mgr = SessionManager::new();

// Register a session whose keys came from a completed PASE handshake.
let keys = PaseSessionKeys {
    ke: [0; 16], i2r_key: [0x11; 16], r2i_key: [0x22; 16],
    attestation_key: [0; 16],
};
let sid = mgr.register_pase(keys, SessionRole::Initiator, 1, PeerHint::default());

let peer = PeerAddress::from_ipv6("::1".parse().unwrap(), 5541);
let out = mgr.encode_outbound(
    sid, None, 0x02, ProtocolId::INTERACTION_MODEL,
    b"hello matter", MrpFlags { reliable: true }, Instant::now(),
)?;
tx.send(peer, out.wire_bytes)?;
# Ok(()) }
```

See `tests/loopback.rs` for a complete two-side example.

## Cross-verification

Framing and protocol-header layers are verified byte-for-byte against
captured fixtures in `test-vectors/transport/`: three framing and three
protocol-header vectors from matter.js, a matter.js group-message
vector, and a `connectedhomeip` vector for group message privacy.
MRP behaviour (including peer-activity classification and peer-config
sizing) is covered by simulated-clock state-machine tests. Real-device
interop is validated end-to-end by the higher-level crates against an
ESP32-C6 over Wi-Fi and Thread.

## License

Apache 2.0. See [LICENSE](../../LICENSE).
