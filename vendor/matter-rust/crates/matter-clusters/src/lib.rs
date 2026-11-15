//! Typed Matter cluster definitions — generated from the Matter spec.
//!
//! Per-cluster attribute / command / struct **codecs** (encode/decode to Matter
//! TLV), feature bitflags, enums (with an `Unknown(n)` variant for
//! forward-compatibility), bitmaps, and — for clusters on the dump script's
//! event allowlist — `event_id` consts plus decode-only `<Name>Event` payload
//! structs (a cluster with events has an `event_id` module in its
//! `clusters::<cluster>` module). The cluster modules live under
//! [`clusters`]; the hand-written foundation is [`Nullable<T>`](types::Nullable)
//! (distinct from `Option`), [`Secret<T>`](types::Secret) (a request argument
//! that carries a PIN, key or credential: its `Debug` prints `"<redacted>"`),
//! [`ClusterError`](error::ClusterError), and [`datatypes::SemanticTagStruct`].
//!
//! # Pipeline
//!
//! The [`clusters`] modules are generated, not hand-written: a pinned `@matter/model`
//! dump becomes the committed `xtask/model/clusters.json`, which
//! `cargo xtask codegen` turns into the committed `src/gen/*.rs`. CI gates drift
//! with `cargo xtask codegen --check`. **Do not edit `src/gen/` by hand** —
//! change the emitter in `xtask/src/codegen/` and regenerate.
//!
//! Correctness: the generated codecs are checked against matter.js 0.16.11
//! byte-parity vectors (`test-vectors/clusters/`), with `proptest` roundtrips
//! and a `cargo-fuzz` target. See [Clusters](#clusters) for what is covered
//! at which level.
//!
//! # Clusters
//!
//! 113 clusters are generated today. The full list is [`clusters`]; by area:
//!
//! - **Core / identity:** `BasicInformation`, `Descriptor`, `Identify`,
//!   `Groups`, `Binding`, `FixedLabel`, `UserLabel`, `PowerSource`,
//!   `GeneralDiagnostics`, `BridgedDeviceBasicInformation` (per-bridged-
//!   endpoint identity behind a bridge/aggregator), `Actions` (a bridge's
//!   actions and endpoint lists, with its events).
//! - **Fabric Synchronization:** `EcosystemInformation` (other fabrics'
//!   `DeviceDirectory` and `LocationDirectory` entries decode with their
//!   fabric-sensitive fields `None`; the global location struct is generated
//!   as `ecosystem_information::LocationDescriptorStruct`),
//!   `CommissionerControl` (with its event; the approval flow itself is not
//!   implemented).
//! - **Network infrastructure:** `ThreadBorderRouterManagement`,
//!   `ThreadNetworkDirectory`, `WiFiNetworkManagement` (codecs only: no
//!   dataset or credential handling).
//! - **Lighting and actuators:** `OnOff`, `LevelControl`, `ColorControl`,
//!   `DoorLock` (Aliro features excluded), `WindowCovering` (with the Matter
//!   1.4 absolute position), `Thermostat` (with the Matter 1.4 weekly
//!   schedule), `ThermostatUserInterfaceConfiguration`, `FanControl`,
//!   `PumpConfigurationAndControl`.
//! - **Sensing:** `OccupancySensing`, `TemperatureMeasurement`,
//!   `RelativeHumidityMeasurement`, `IlluminanceMeasurement`,
//!   `PressureMeasurement`, `FlowMeasurement`, `BooleanState`, `Switch`,
//!   `AirQuality`, and the ten `ConcentrationMeasurement` clusters
//!   (`CarbonMonoxide`, `CarbonDioxide`, `NitrogenDioxide`, `Ozone`, `Pm25`,
//!   `Formaldehyde`, `Pm1`, `Pm10`, `TotalVolatileOrganicCompounds`,
//!   `Radon`).
//! - **Energy:** `ElectricalPowerMeasurement`, `ElectricalEnergyMeasurement`.
//! - **Energy management:** `EnergyPreference` (provisional: the
//!   specification marks the whole cluster provisional, so its shape may
//!   change), `PowerTopology`, `WaterHeaterManagement`,
//!   `DeviceEnergyManagement`, `EnergyEvse` (the last three with their
//!   events).
//! - **Messaging:** `Messages` (with its events; other fabrics' `Messages`
//!   entries decode with their fabric-sensitive fields `None`).
//! - **Appliance modes:** `ModeSelect`, `OvenMode`, `LaundryWasherMode`,
//!   `RefrigeratorAndTemperatureControlledCabinetMode`, `RvcRunMode`,
//!   `RvcCleanMode`, `DishwasherMode`, `MicrowaveOvenMode`, `EnergyEvseMode`,
//!   `WaterHeaterMode`, `DeviceEnergyManagementMode`.
//! - **Appliance alarms:** `DishwasherAlarm`, `RefrigeratorAlarm` (with their
//!   `Notify` events).
//! - **Resource monitoring:** `HepaFilterMonitoring`,
//!   `ActivatedCarbonFilterMonitoring`, `WaterTankLevelMonitoring`.
//! - **Appliance operational state:** `OperationalState`,
//!   `OvenCavityOperationalState`, `RvcOperationalState` (with their
//!   `OperationalError` and `OperationCompletion` events).
//! - **Appliance controls:** `TemperatureControl`, `LaundryWasherControls`,
//!   `LaundryDryerControls`, `MicrowaveOvenControl`.
//! - **Robotic cleaners:** `ServiceArea` (with the global location struct,
//!   generated as `service_area::LocationDescriptorStruct`).
//! - **Safety sensors:** `SmokeCoAlarm`, `BooleanStateConfiguration` (with
//!   their events).
//! - **Valves:** `ValveConfigurationAndControl` (with its events).
//! - **Scenes:** `ScenesManagement` (its `FabricSceneInfo` entries of other
//!   fabrics decode with their fabric-sensitive fields `None`).
//! - **Administration:** `AccessControl`, `GroupKeyManagement`,
//!   `AdministratorCommissioning`, `OperationalCredentials`,
//!   `IcdManagement`, `TimeSynchronization`, `OtaSoftwareUpdateRequestor`,
//!   `OtaSoftwareUpdateProvider`, `GeneralCommissioning`,
//!   `NetworkCommissioning` (codecs for use outside commissioning:
//!   `matter-commissioning` drives commissioning with its own hand-written
//!   subset of these commands, which encodes the same values identically).
//!   The generated response decoders follow the specification, so they are
//!   stricter than `matter-commissioning`'s, which the commissioning flow
//!   uses: `DebugText`, `ErrorValue` and `MaxCumulativeFailsafeSeconds` are
//!   required here and may be missing there.
//! - **Node configuration:** `LocalizationConfiguration`,
//!   `TimeFormatLocalization`, `UnitLocalization`, `PowerSourceConfiguration`.
//! - **Diagnostics:** `SoftwareDiagnostics`, `WiFiNetworkDiagnostics`,
//!   `ThreadNetworkDiagnostics` (with their events),
//!   `EthernetNetworkDiagnostics`, `DiagnosticLogs` (the request and its
//!   response; the BDX transfer is not implemented).
//! - **Media:** `WakeOnLan`, `LowPower`, `KeypadInput`, `MediaInput`,
//!   `AudioOutput`, `ContentLauncher`, `Channel`, `ContentControl`
//!   (provisional: the specification marks the whole cluster provisional, so
//!   its shape may change; the reset PIN is redacted in `Debug`),
//!   `TargetNavigator` and `MediaPlayback` (the last two with their events).
//! - **Content apps:** `ApplicationBasic`, `ApplicationLauncher`,
//!   `AccountLogin` (with its event; the setup PIN is redacted in `Debug`),
//!   `ContentAppObserver`.
//!
//! Elements chip's Matter 1.4.2 data marks provisional (its 1.4.2 data model,
//! or its v1.4.2.0 controller codegen) carry a rustdoc note even where the
//! specification model does not: `EnergyEvse`'s `SOC`, `PNC` and `V2X`
//! features, `GeneralCommissioning`'s terms-and-conditions attributes and
//! `GroupKeyManagement`'s `GroupKeyMulticastPolicy`, among others. The seven
//! clusters chip's v1.4.2.0 codegen marks provisional as a whole
//! (`ScenesManagement`, `Messages`, `DeviceEnergyManagement`,
//! `DeviceEnergyManagementMode`, `WaterHeaterManagement`,
//! `EcosystemInformation`, `ContentAppObserver`) say so in their module docs.
//!
//! Note that this crate holds **codecs only**. For the administration
//! clusters in particular, encoding a command is not the same as running the
//! protocol around it: ACL evaluation, group multicast, commissioning-window
//! orchestration, and OTA live in `matter-controller` and its siblings.
//!
//! Verification varies by cluster. Every cluster has decode-smoke coverage;
//! matter.js byte-parity vectors cover the core, lighting, and sensing sets
//! plus one vector for each novel wire shape the later batches introduced
//! (nested measurement-accuracy structs, list-typed commands,
//! struct-with-byte-fields, recursive list-of-struct, and floats).
//!
//! For any attribute not covered by these typed codecs — a cluster not in
//! this list, or a manufacturer-specific attribute — the generic `Value`
//! path in `matter-controller` remains the universal answer.
//!
//! # Usage
//!
//! Codecs are free functions per attribute/command. Encoders return a standalone
//! anonymous-tagged TLV element (ready to embed in an Interaction Model
//! request); decoders take the attribute value bytes from a report.
//!
//! ```
//! use matter_clusters::clusters::{basic_information, on_off};
//!
//! // Command payload — embed in an InvokeRequest (see the `control_onoff` example).
//! let _toggle = on_off::encode_toggle();
//!
//! // Attribute roundtrips: encode a value, decode it back.
//! let tlv = on_off::encode_on_time(30);
//! assert_eq!(on_off::decode_on_time(&tlv)?, 30);
//!
//! let tlv = basic_information::encode_node_label(&"living room".to_string());
//! assert_eq!(basic_information::decode_node_label(&tlv)?, "living room");
//! # Ok::<(), matter_clusters::error::ClusterError>(())
//! ```
//!
//! See `crates/matter-commissioning/examples/control_onoff.rs` for an
//! end-to-end read / toggle / write against a real device.
//!
//! # Scope — reading attributes beyond these clusters
//!
//! Typed codecs exist for these clusters' **mandatory and optional** attributes
//! (a device may not implement a given optional attribute — it then returns
//! `UNSUPPORTED_ATTRIBUTE`). To read attributes of clusters NOT in this set, or
//! manufacturer-specific attributes, use the generic Interaction Model path:
//! `matter_interaction::parse_report_data` decodes any attribute to a
//! `(AttributePath, matter_codec::Value)` pair without a typed codec, and
//! `matter-controller` wraps that in a generic read/write/subscribe API with
//! wildcard paths.

#![forbid(unsafe_code)]

pub mod datatypes;
pub mod error;
pub mod types;

pub use datatypes::SemanticTagStruct;

// The generated cluster modules. The files stay in `src/gen/` (the directory
// `cargo xtask codegen` writes and `codegen --check` gates); the module is
// `clusters` because `gen` is a reserved keyword from Rust edition 2024, where
// `matter_clusters::gen::…` no longer parses (only `r#gen` would).
#[path = "gen/mod.rs"]
pub mod clusters;

/// The generated cluster modules under their pre-0.6 name: an alias of
/// [`clusters`], kept so `matter_clusters::gen::…` paths in edition 2015–2021
/// code keep compiling. Hidden from the docs; use [`clusters`].
#[doc(hidden)]
pub use self::clusters as gen;

#[cfg(test)]
mod golden;
#[cfg(test)]
mod golden_tests;

/// Compile-checks the Rust examples in this crate's `README.md`.
///
/// `#[cfg(doctest)]` means the item exists only while rustdoc is collecting
/// doctests, so the README is compiled by `cargo test --doc` without being
/// duplicated into the rendered crate docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

/// Edition 2024: `gen` is a reserved keyword there, so the
/// generated modules must be reachable under another name. These blocks
/// compile as edition 2024 crates.
///
/// ```edition2024
/// use matter_clusters::clusters::on_off;
/// assert_eq!(on_off::CLUSTER_ID, 0x0006);
/// ```
///
/// The old path does not parse in edition 2024, which is why the module was
/// renamed:
///
/// ```compile_fail,edition2024
/// use matter_clusters::gen::on_off;
/// ```
#[cfg(doctest)]
struct Edition2024Doctests;
