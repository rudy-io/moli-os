# matter-clusters

Typed Matter cluster definitions: per-cluster attribute/command/struct codecs,
feature flags, enums (with `Unknown(n)` forward-compat), bitmaps, and — for
clusters on the dump script's event allowlist — event-id constants plus
decode-only event payload structs (a cluster with events has an `event_id`
module in its `clusters::<cluster>` module). The modules under `clusters` are
generated from a pinned `@matter/model` dump by the `xtask` codegen tool.
(Before 0.6 the module was called `gen`, a reserved keyword from Rust edition
2024; `matter_clusters::gen` remains as a hidden alias.)

Part of [`matter-rust`](https://github.com/phunapps/matter-rust).

```toml
[dependencies]
matter-clusters = "0.6"
```

## What this crate does

- Provides encode/decode functions for the attributes, commands, and structs of
  113 Matter clusters (mandatory **and** optional attributes), as Matter TLV.
- Models cluster enums with an `Unknown(n)` variant (forward-compatible decode),
  feature maps as `bitflags`, and nullable fields as `Nullable<T>` (distinct
  from `Option<T>`).
- Takes every secret argument of a request (a PIN, a passcode verifier, a key,
  a Thread dataset, Wi-Fi credentials) as `Secret<T>`, whose `Debug` prints
  `"<redacted>"`, and keeps secrets out of the generated structs' `Debug`
  output (GroupKeyManagement's key set, whose epoch keys `KeySetWrite`
  sends, for example). The encoded payload still carries the secret in clear.
- Generates all of the above from the spec model, gated against drift in CI.

## What this crate does not do

- It covers every Matter 1.4 cluster except the two provisional JointFabric
  clusters (0x0752, 0x0753); Matter 1.5-only clusters are not generated.
- It does **not** provide generic or wildcard attribute access, or
  manufacturer-specific typed codecs. Reading arbitrary attributes a device
  publishes is the Interaction Model layer / high-level controller (see *Reading
  attributes beyond these clusters*).
- It is **codecs only**. Encoding a command is not the same as running the
  protocol around it: ACL evaluation, group multicast, commissioning-window
  orchestration, and OTA live in `matter-controller` and its siblings.
- It performs no IO and no session/transport work — it only encodes/decodes
  bytes.

## Status

**0.6.0**. Stability: a `0.x` crate, so a **minor** bump
may break API — and adding clusters is a routine minor bump.

## Clusters

113 clusters are generated today, covering their **mandatory and optional**
attributes, by area:

- **Core / identity** — BasicInformation, Descriptor, Identify, Groups, Binding,
  FixedLabel, UserLabel, PowerSource, GeneralDiagnostics,
  BridgedDeviceBasicInformation (per-bridged-endpoint identity behind a
  bridge/aggregator), Actions (a bridge's actions and endpoint lists, with its
  events).
- **Fabric Synchronization** — EcosystemInformation (other fabrics'
  `DeviceDirectory` and `LocationDirectory` entries decode with their
  fabric-sensitive fields `None`), CommissionerControl (with its event; the
  approval flow itself is not implemented). No chip example app fills
  EcosystemInformation's directories (fabric-bridge-app and fabric-sync add
  the cluster, empty, to their bridged-device endpoints): it is checked by
  decode smoke and `chip-xml-conformance.py` only.
- **Network infrastructure** — ThreadBorderRouterManagement,
  ThreadNetworkDirectory, WiFiNetworkManagement (codecs only: no dataset or
  credential handling). network-manager-app, chip's only example app that
  hosts all three (and the only one with ThreadNetworkDirectory or
  WiFiNetworkManagement), is not built by the integration harness: they are
  checked by decode smoke and `chip-xml-conformance.py` only.
- **Lighting and actuators** — OnOff, LevelControl, ColorControl, DoorLock
  (Aliro features excluded), WindowCovering (with the Matter 1.4 absolute
  position, provisional in Matter 1.4, removed in 1.5), Thermostat (with the
  Matter 1.4 weekly schedule, deprecated in Matter 1.4, removed in 1.5.1),
  ThermostatUserInterfaceConfiguration, FanControl,
  PumpConfigurationAndControl.
- **Sensing** — OccupancySensing, TemperatureMeasurement,
  RelativeHumidityMeasurement, IlluminanceMeasurement, PressureMeasurement,
  FlowMeasurement, BooleanState, Switch, AirQuality, and the ten
  ConcentrationMeasurement clusters (CarbonMonoxide 0x040C, CarbonDioxide
  0x040D, NitrogenDioxide 0x0413, Ozone 0x0415, Pm25 0x042A, Formaldehyde
  0x042B, Pm1 0x042C, Pm10 0x042D, TotalVolatileOrganicCompounds 0x042E, Radon
  0x042F).
- **Energy** — ElectricalPowerMeasurement, ElectricalEnergyMeasurement.
- **Energy management** — EnergyPreference (provisional: the specification
  marks the whole cluster provisional, so its shape may change between
  specification revisions), PowerTopology, WaterHeaterManagement,
  DeviceEnergyManagement, EnergyEvse (the last three with their events).
- **Messaging** — Messages (with its events; other fabrics' `Messages`
  entries decode with their fabric-sensitive fields `None`). chip does not
  fabric-scope `MessageStruct`: chip's SDK servers and tv-app's delegate do
  not filter entries by fabric, and no chip-SDK server can send FabricIndex
  (the generated types have no field 254), so every entry decodes in full,
  with `fabric_index` `None`.
- **Appliance modes** — ModeSelect, OvenMode, LaundryWasherMode,
  RefrigeratorAndTemperatureControlledCabinetMode, RvcRunMode, RvcCleanMode,
  DishwasherMode, MicrowaveOvenMode, EnergyEvseMode, WaterHeaterMode,
  DeviceEnergyManagementMode.
- **Appliance alarms** — DishwasherAlarm, RefrigeratorAlarm (with their
  `Notify` events).
- **Resource monitoring** — HepaFilterMonitoring,
  ActivatedCarbonFilterMonitoring, WaterTankLevelMonitoring.
- **Appliance operational state** — OperationalState,
  OvenCavityOperationalState, RvcOperationalState (with their
  `OperationalError` and `OperationCompletion` events).
- **Appliance controls** — TemperatureControl, LaundryWasherControls,
  LaundryDryerControls, MicrowaveOvenControl.
- **Robotic cleaners** — ServiceArea. No chip example app other than
  rvc-app serves it.
- **Safety sensors** — SmokeCoAlarm, BooleanStateConfiguration (with their
  events).
- **Valves** — ValveConfigurationAndControl (with its events).
- **Scenes** — ScenesManagement (other fabrics' `FabricSceneInfo` entries
  decode with their fabric-sensitive fields `None`).
- **Administration** — AccessControl, GroupKeyManagement,
  AdministratorCommissioning, OperationalCredentials, IcdManagement,
  TimeSynchronization, OtaSoftwareUpdateRequestor, OtaSoftwareUpdateProvider,
  GeneralCommissioning, NetworkCommissioning. The last two are codecs for use
  outside commissioning: `matter-commissioning` drives commissioning with its
  own hand-written subset of their commands, which encodes byte for byte the
  same for the same values (its `tests/generated_codec_parity.rs`).
  `matter-commissioning`'s response decoders accept a missing `DebugText`,
  `ErrorValue` or `MaxCumulativeFailsafeSeconds`; the generated ones follow
  the specification, where all three are mandatory.
- **Node configuration** — LocalizationConfiguration, TimeFormatLocalization,
  UnitLocalization, PowerSourceConfiguration.
- **Diagnostics** — SoftwareDiagnostics, WiFiNetworkDiagnostics,
  ThreadNetworkDiagnostics (with their events), EthernetNetworkDiagnostics,
  DiagnosticLogs (the request and its response; the BDX transfer is not
  implemented).
- **Media** — WakeOnLan, LowPower, KeypadInput, MediaInput, AudioOutput,
  ContentLauncher, Channel, ContentControl (provisional: the specification
  marks the whole cluster provisional, so its shape may change between
  specification revisions; a change is still a breaking release; the reset
  PIN is redacted in `Debug`), TargetNavigator and MediaPlayback (the last
  two with their events).
- **Content apps** — ApplicationBasic, ApplicationLauncher, AccountLogin (with
  its event; the setup PIN is redacted in `Debug`), ContentAppObserver. Of
  chip's example apps only tv-casting-app serves ContentAppObserver (tv-app's
  static Content App endpoint 3 is its client), and the harness does not
  drive it, so it is checked by decode smoke and `chip-xml-conformance.py`
  only.

Elements chip's Matter 1.4.2 data marks provisional (its 1.4.2 data model, or
its v1.4.2.0 controller codegen) carry a rustdoc note even where the
specification model does not: EnergyEvse's `SOC`, `PNC` and `V2X` features,
GeneralCommissioning's terms-and-conditions attributes and
GroupKeyManagement's `GroupKeyMulticastPolicy`, among others. The seven
clusters chip's v1.4.2.0 codegen marks provisional as a whole
(ScenesManagement, Messages, DeviceEnergyManagement,
DeviceEnergyManagementMode, WaterHeaterManagement, EcosystemInformation,
ContentAppObserver) say so in their module docs.

Verification varies by cluster, and the level is deliberate rather than
accidental. Every cluster has decode-smoke coverage. matter.js 0.16.11
byte-parity vectors (`test-vectors/clusters/`) cover the core, lighting, and
sensing sets, plus one vector for each novel wire shape a later cluster
introduced: the nested `MeasurementAccuracyStruct`, the list-typed
`AtomicRequest` command, GeneralDiagnostics' struct-with-byte-fields
`NetworkInterface`, the recursive list-of-struct in
`AccessControl.ReviewFabricRestrictions`, FLOAT32 attributes, LevelControl's
`MoveToLevelWithOnOff` (the inherited command fields), TemperatureControl's
`SetTemperature` (a negative signed request field), Thermostat's
`SetWeeklySchedule` (TLV null inside list entries), ValveConfigurationAndControl's
`Open(null, 50)` (the first `Option<Nullable<T>>` request field),
ScenesManagement's `AddScene` (nested request lists), and Messages'
`CancelMessagesRequest` (the first request `list<octstr>`) and
`PresentMessagesRequest` (the first optional request list), and
ContentLauncher's `LaunchURL` (request structs nesting structs, and the first
`double`) and `LaunchContent` (optional and nullable lists inside request
structs), and Channel's `GetProgramGuide` (the specification's field ids, which
chip's codegen does not follow). The read/write
actuator clusters additionally carry `decode(encode(x)) == x` roundtrips, and
floats get both a binary32-edge roundtrip (signed zero, subnormals, infinities,
NaN — compared by bits) and a `proptest` roundtrip drawn uniformly from the
whole binary32 bit space. A `single` attribute accepts a FLOAT32 element only,
matching chip's strict `TLVReader::Get(float&)`. No connectedhomeip example
app serves WaterTankLevelMonitoring, so it is validated by decode tests and the
chip 1.4.2 XML check (`scripts/chip-xml-conformance.py`) only. No chip example
app the harness drives serves Thermostat's Matter 1.4 weekly schedule (SCH, at
v1.4.2.0 or master), so it is validated by decode tests, the matter.js vector
and the chip 1.4.2 XML check only. ServiceArea,
MicrowaveOvenControl and the rvc-app paths (RvcOperationalState's GoHome and
its events; RvcRunMode / RvcCleanMode's refused changes) run live against
chip master 5cd2917a's rvc-app (8 of 8 tests) and microwave-oven-app (3 of 3),
built for Linux arm64 and run on a Raspberry Pi; they are not in the nightly
and do not run on the Mac, because rvc-app and microwave-oven-app need
Rosetta 2 to build on Apple silicon. Messages runs live against chip's
tv-app (its only chip host) with the media clusters, below; beyond that, it is
validated by decode tests, two matter.js vectors and the chip 1.4.2 XML check.
EnergyPreference runs live against chip's all-clusters at master (locally)
and v1.4.2.0 (the nightly). WaterHeaterManagement runs live only against chip
v1.4.2.0 (the nightly); master's server is unexercised, because master's
all-clusters no longer serves it and water-heater-app is not built.
EnergyEvse, DeviceEnergyManagement and PowerTopology commands and events run
live only against chip master's evse-app, locally; the nightly decodes their
attributes on v1.4.2.0's all-clusters. The node configuration clusters,
Actions (refusals only: all-clusters' stub has no real actions and emits no
events), GeneralCommissioning, NetworkCommissioning (Ethernet, reads only),
DiagnosticLogs and the Software, Wi-Fi, Thread and Ethernet diagnostics
clusters run live against chip's all-clusters at master 5cd2917a (locally)
and v1.4.2.0 (the nightly, run 38120287902); the four tests of the
diagnostics clusters and DiagnosticLogs also passed against v1.4.2.0 built
for Linux arm64 on a Raspberry Pi.
EcosystemInformation, CommissionerControl (hosted by fabric-bridge-app and
fabric-sync), ThreadBorderRouterManagement, ThreadNetworkDirectory and
WiFiNetworkManagement (hosted by network-manager-app) have not run live: they
are validated by decode smoke and `chip-xml-conformance.py` only.
The media and TV-platform clusters, and Messages, run live against chip's
tv-app built at master 5cd2917a and at v1.4.2.0 for Linux arm64 on a Raspberry
Pi (`just integration-tv`; in the nightly at v1.4.2.0 on linux-x64, green in
nightly 38175373715 on 2245a81; not on the Mac, because tv-app needs Rosetta 2
to build on Apple silicon). Every test passes at both refs with no pacing
between exchanges, although master's tv-app runs on a 15-buffer PacketBuffer
pool: the controller ACKs every response as its exchange closes. tv-app's
ContentControl delegate is a stub: the two rating lists fail to read and the
block-list commands are unsupported, so the live test covers attributes 0, 2
and 4–7 and the status answers only. WakeOnLan and LowPower also run against
chip's all-clusters, locally at master and on the Pi at v1.4.2.0, and in the
nightly at v1.4.2.0 (nightly 38175373715 on 2245a81). ContentAppObserver has
not run live (of chip's example apps only tv-casting-app serves it, which the
harness does not drive): decode smoke and `chip-xml-conformance.py` only. None
of these clusters' events has a stimulus that reaches tv-app's log, so they
are decode tests only.

For any attribute not covered by these typed codecs — manufacturer-specific, or
a cluster not in this list — the generic `Value` path in `matter-controller`
remains the universal answer. Hand-written support lives in `types`
(`Nullable<T>`), `error` (`ClusterError`), and `datatypes`
(`SemanticTagStruct`).

## Usage

```rust
use matter_clusters::clusters::{basic_information, on_off};

// Command payload — embed in an InvokeRequest.
let _toggle = on_off::encode_toggle();

// Attribute roundtrips: encode a value, decode it back.
let tlv = on_off::encode_on_time(30);
assert_eq!(on_off::decode_on_time(&tlv)?, 30);

let tlv = basic_information::encode_node_label(&"living room".to_string());
assert_eq!(basic_information::decode_node_label(&tlv)?, "living room");
# Ok::<(), matter_clusters::error::ClusterError>(())
```

See `crates/matter-commissioning/examples/control_onoff.rs` for an end-to-end
read / toggle / write against a real device (runbook:
`docs/runbooks/m7.5-control-onoff.md`).

## Generated code

`cargo xtask codegen` writes `src/gen/<cluster>.rs` (+ `globals.rs`, `mod.rs`)
from `xtask/model/clusters.json`. Do not edit the generated files by hand —
change the emitter (`xtask/src/codegen/`) and regenerate. `cargo xtask codegen
--check` gates drift in CI.

## Reading attributes beyond these clusters

Typed codecs cover these clusters' mandatory and optional attributes (a device
may not implement a given optional attribute — it then returns
`UNSUPPORTED_ATTRIBUTE`). To read attributes of other clusters, or
manufacturer-specific attributes, use the generic Interaction Model path:
`matter_interaction::parse_report_data` yields
`(AttributePath, matter_codec::Value)` for any attribute without a typed codec,
and `matter-controller` wraps that in a generic read/write/subscribe API with
wildcard paths.

## Correctness posture

- **Decode-smoke coverage** for every generated cluster.
- **Byte-parity** against matter.js 0.16.11 TLV combinators
  (`test-vectors/clusters/`) for the core, lighting, and sensing sets, plus one
  vector per novel wire shape — see [Clusters](#clusters) for exactly which.
- **`proptest` roundtrips** over attribute values.
- **A `cargo-fuzz` target** over the generated decoders (weekly CI).
- **`cargo xtask codegen --check`** fails CI if the committed `src/gen/` drifts
  from what the emitter + `clusters.json` produce.

## Cryptographic posture

`matter-clusters` performs no cryptography. It is pure data encoding.

## MSRV

Rust 1.89 (workspace MSRV). See the workspace `CHANGELOG.md`.

## License

Apache 2.0. See `LICENSE` at the workspace root.
