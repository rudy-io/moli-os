# matter-controller

The high-level Matter controller API — the single crate a consumer depends on
to commission and control Matter devices from pure Rust. It wraps every other
`matter-*` crate behind a small, async (Tokio) surface.

Part of [`matter-rust`](https://github.com/phunapps/matter-rust).

> Status: **0.18.3**. The v1.0 controller (M8) plus Matter-1.4 completeness work
> (M9): BLE→Wi-Fi/Thread commissioning, full interaction model, groups, OTA
> provider, ICD client, multi-admin/ACL — extensively validated against real
> silicon (ESP32-C6 over Wi-Fi and Thread).

## What it does

- **Fabric & identity** — `create_fabric` mints and persists the controller's
  stable operational identity once per fabric, through a pluggable
  `ControllerStore` (a default `FileStore` ships). Opt-in per-fabric ICAC for
  a 3-tier RCAC→ICAC→NOC chain. `fabrics() -> Vec<FabricInfo>` enumerates
  fabrics already created — check it before calling `create_fabric` again;
  a second call with an existing `fabric_id` is refused.
- **Commissioning** — `commission("MT:…" | "<manual-code>", label)` brings a
  device onto the fabric over IP, verifying device attestation against an
  `AttestationTrust` (`example_device_roots()`, or production PAA/CD roots via
  `from_dirs`). `commission_ble` (feature `ble`) commissions a fresh device
  over BLE onto Wi-Fi or Thread. Both return a typed `NodeInfo`.
- **Node lifecycle** — `nodes() -> Vec<NodeInfo>` enumerates commissioned
  devices (node id, fabric id, vendor/product id, label) with no snapshot
  deserialization; `forget_node(node_id)` drops all local state for a device
  without needing it to cooperate (reclaim an unreachable/reset node).
- **Interaction** — `Node::read` / `write` / `invoke` over raw
  `matter_codec::Value` (plus `invoke_tlv` for pre-encoded
  `matter-clusters` command TLV), **wildcard + chunked reads**, events, and
  timed interactions.
- **Subscriptions** — `Node::subscribe` returns a `Subscription` stream of
  attribute/event reports that **transparently auto-resubscribes** across
  session loss or a device reboot (validated on hardware).
- **Groups** — provision group keys and `invoke_group` over IPv6 multicast.
- **OTA provider** — `serve_ota` announces + serves a `.ota` image over BDX to
  a commissioned requestor.
- **ICD client** — register as a check-in client and receive a Long-Idle-Time
  device's periodic Check-In.

The operational CASE session is established, cached, and reused transparently —
callers address a device by node id and never manage sessions.

## Quickstart

```rust,no_run
use std::path::Path;
use std::sync::Arc;
use matter_controller::{AttestationTrust, FabricConfig, FileStore, MatterController, MatterTime, ReadPath};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let store = Arc::new(FileStore::new("controller-state.bin"));
    let controller = MatterController::builder(store)
        // Arbitrary certified devices need real CSA roots. The bundled
        // `example_device_roots()` covers only CSA-test / example devices
        // (chip's example apps, esp-matter) — not a production trust set.
        .attestation_trust(AttestationTrust::from_dirs(
            Path::new("paa-roots"),
            Path::new("cd-roots"),
        )?)
        .build()
        .await?;

    // Only create a fabric that doesn't exist yet — `create_fabric` refuses a
    // `fabric_id` it already has, so check `fabrics()` first (or gate on a fresh
    // store). `not_before` must be a real wall-clock time, backdated a little (an
    // hour is plenty) for device clock skew: MatterTime(0) / from_unix_secs(0)
    // (the Matter epoch) is rejected, and so is a time far in the future — see
    // `FabricConfig::validity` and issue #111.
    if controller.fabrics().await?.is_empty() {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        controller.create_fabric(FabricConfig::new(
            /* fabric_id */ 1,
            /* rcac_id */ 1,
            /* commissioner_node_id */ 1,
            (
                MatterTime::from_unix_secs(now_unix.saturating_sub(3600)),
                MatterTime::NO_EXPIRY,
            ),
        )).await?;
    }

    let info = controller
        .commission("MT:Y.K90AFN00KA0648G00", Some("kitchen plug".into()))
        .await?;
    let node = controller.node(info.node_id);

    // Read all OnOff attributes; subscribe to changes.
    let report = node.read(&[ReadPath::cluster(1, 0x0006)]).await?;
    println!("read {} attributes", report.len());
    let mut sub = node.subscribe(&[ReadPath::cluster(1, 0x0006)], &[], 1, 30).await?;
    // `next()` yields `SubscriptionEvent` — attribute/event reports plus
    // (re-)establishment status changes.
    while let Some(event) = sub.next().await {
        println!("{event:?}");
    }

    // Enumerate and manage commissioned nodes.
    for n in controller.nodes().await? {
        println!("node 0x{:016X} — {:?}", n.node_id, n.label);
    }
    Ok(())
}
```

See `examples/` (`controller_quickstart`, `list_nodes`, `e3_group_multicast`,
`serve_ota`, …) for end-to-end runs, and
[`docs/matter-js-migration-guide.md`](../../docs/matter-js-migration-guide.md)
if you're coming from matter.js.

## Known limitations

- **BLE commissioning on macOS cannot complete.** Root-caused to `btleplug`
  0.12.0 / CoreBluetooth: the CHIPoBLE GATT characteristics draw
  `CBError.uuidNotAllowed` on descriptor discovery and the C1 write, and btleplug
  drops the errored delegate events. The former infinite hang is now bounded to a
  fast, clear failure, but success needs an upstream fix — drive live BLE
  commissioning from Linux. IP commissioning and everything else is unaffected on
  all platforms. Instrumentation: `MATTER_BLE_PUMP_TRACE=1`.
- Thread network commissioning and BLE transport require the `ble` feature.

## License

Apache-2.0.
