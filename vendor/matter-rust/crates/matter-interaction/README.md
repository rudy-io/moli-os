# matter-interaction

Matter Interaction Model (IM) message framing — Matter Core Specification §10.
Builds the request messages a controller puts on the wire and parses the
responses that come back. You supply already-encoded cluster TLV (from
`matter-clusters`, or your own) and the paths; this crate frames them.

Part of [`matter-rust`](https://github.com/phunapps/matter-rust). Most users
want [`matter-controller`](https://crates.io/crates/matter-controller), which
drives these codecs over a live CASE session. Reach for `matter-interaction`
directly when you own the transport and want the IM message layer on its own.

> Status: **0.4.3**, published on crates.io. Depends only on `matter-codec`,
> and performs no IO. Stability: a `0.x` crate, so a **minor** bump may break
> API.

```toml
[dependencies]
matter-interaction = "0.4"
```

## What it does

- **Invoke** — a single command (`build_invoke_request`), several commands in
  one message (`build_invoke_request_batch` /
  `parse_invoke_response_batch`), a group/broadcast invoke
  (`build_invoke_request_group`), and the timed variant
  (`build_invoke_request_timed`). Responses parse to a command payload or a
  per-command `ImStatus`.
- **Read** — concrete *and* wildcard paths (`ReadPath` leaves endpoint,
  cluster, or attribute unset for "all"), plus `build_read_request_full` for a
  request carrying attribute paths, event paths, and event filters together.
- **Subscribe** — `SubscribeRequest` / `build_subscribe_request`,
  `parse_subscribe_response`, and the `StatusResponse` the subscriber sends to
  acknowledge each report (`build_status_response`).
- **Chunked reports** — `ReportAccumulator` reassembles a report split across
  messages, including a list attribute split across chunk boundaries, with
  configurable element and byte ceilings.
- **Write** — one-message writes (`build_write_request`), the timed variant,
  and `build_list_write_chunks` for a list attribute too large to send in a
  single message.
- **Events** — `EventPath` and `EventFilter` on both reads and subscriptions,
  and `EventReportIB` parsing (priority, timestamp, payload) via
  `ReportData::events`.
- **Timed interactions** — the `TimedRequest` message
  (`build_timed_request`), which precedes a write or invoke that carries the
  timed flag.
- **Server side** — `parse_invoke_request` and the `build_invoke_response_*`
  builders, for answering an invoke rather than issuing one. This workspace's
  OTA provider is built on them.

## What it does not do

- No IO, no sessions, no exchanges, no MRP — that is
  [`matter-transport`](https://crates.io/crates/matter-transport).
- No cluster semantics. Command and attribute payload bytes are opaque here;
  typed codecs live in
  [`matter-clusters`](https://crates.io/crates/matter-clusters).
- No controller-side policy. The batch builders are here, but deciding what to
  batch and honouring a device's `MaxPathsPerInvoke` is the caller's job —
  `matter-controller` does it.

## Verification

Byte-parity against matter.js fixtures in `test-vectors/commissioning/im/`
(see `tests/im_byte_parity.rs`), captured via `cargo xtask capture-im`.

## License

Apache 2.0. See [LICENSE](../../LICENSE).
