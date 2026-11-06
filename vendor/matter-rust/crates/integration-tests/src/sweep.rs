//! Helpers for the per-batch cluster sweeps: read every
//! attribute of a cluster with one wildcard read and decode each with the
//! generated `matter-clusters` decoder, and invoke a command and decode its
//! response.

use anyhow::{bail, Context, Result};
use matter_clusters::error::ClusterError;
use matter_codec::TlvReader;
use matter_controller::{AttributePath, CommandPath, ImStatus, InvokeResult, Node, ReadPath};

use crate::dut::DutConfig;
use crate::events::payload_tlv;

/// The all-clusters app's endpoint configuration (ZAP's `.matter` IDL),
/// relative to the connectedhomeip checkout.
const ALL_CLUSTERS_MATTER: &str =
    "examples/all-clusters-app/all-clusters-common/all-clusters-app.matter";

/// Whether the all-clusters app built from `cfg.chip_root` serves cluster
/// `name` (its `.matter` name, e.g. `WaterHeaterMode`) on `endpoint`,
/// according to that checkout's `all-clusters-app.matter`.
///
/// For a cluster whose presence differs between the chip releases the
/// harness runs against: v1.4.2.0 (the nightly's pin) serves
/// `EnergyEvseMode`, `WaterHeaterMode` and `DeviceEnergyManagementMode` on
/// endpoint 1, master does not. A test that asks the source can require the
/// cluster where it is served, instead of passing vacuously whenever the
/// Descriptor leaves it out. Like
/// [`crate::events::all_clusters_pipe_supports`], this assumes the binary was
/// built from that checkout.
///
/// # Errors
///
/// If the file cannot be read or has no `endpoint <endpoint> {` block: a
/// moved or reshaped file must fail the test, not quietly drop coverage.
pub fn all_clusters_serves(cfg: &DutConfig, endpoint: u16, name: &str) -> Result<bool> {
    let path = cfg.chip_root.join(ALL_CLUSTERS_MATTER);
    let source = std::fs::read_to_string(&path)
        .with_context(|| format!("read all-clusters endpoint config {}", path.display()))?;
    endpoint_serves(&source, endpoint, name).with_context(|| format!("in {}", path.display()))
}

/// Whether `.matter` `source` declares `server cluster <name>` inside its
/// `endpoint <endpoint> {` block. The block runs to the `}` that closes it at
/// column 0; the clusters inside are indented, so their own `}` never ends it.
fn endpoint_serves(source: &str, endpoint: u16, name: &str) -> Result<bool> {
    let header = format!("endpoint {endpoint} {{");
    let mut lines = source.lines().skip_while(|l| l.trim_end() != header);
    if lines.next().is_none() {
        bail!("no `{header}` block");
    }
    Ok(lines.take_while(|l| !l.starts_with('}')).any(|l| {
        l.trim_start()
            .strip_prefix("server cluster ")
            .and_then(|rest| rest.split([' ', '{', ';']).next())
            == Some(name)
    }))
}

/// Whether the all-clusters app built from `cfg.chip_root` serves attribute
/// `attribute` (its `.matter` name, e.g. `unmounted`) of cluster `cluster` on
/// `endpoint`, according to that checkout's `all-clusters-app.matter`.
///
/// For an attribute whose presence differs between the chip releases the
/// harness runs against: master's all-clusters serves `SmokeCoAlarm`
/// `unmounted` (0x000D, cluster revision 2), v1.4.2.0's does not. An
/// exact-id sweep asks the source instead of accepting either list. Like
/// [`all_clusters_serves`], this assumes the binary was built from that
/// checkout.
///
/// # Errors
///
/// If the file cannot be read, has no `endpoint <endpoint> {` block, or the
/// cluster is not served there.
pub fn all_clusters_serves_attribute(
    cfg: &DutConfig,
    endpoint: u16,
    cluster: &str,
    attribute: &str,
) -> Result<bool> {
    let path = cfg.chip_root.join(ALL_CLUSTERS_MATTER);
    let source = std::fs::read_to_string(&path)
        .with_context(|| format!("read all-clusters endpoint config {}", path.display()))?;
    cluster_serves_attribute(&source, endpoint, cluster, attribute)
        .with_context(|| format!("in {}", path.display()))
}

/// Whether the `server cluster <cluster> {` block inside `.matter` `source`'s
/// `endpoint <endpoint> {` block declares `attribute <attribute>` (any storage:
/// `ram`, `persist`, `callback`; with or without `default = ...`, so the name
/// may end in the line's `;`). The cluster block ends at its own `}`,
/// indented by two.
fn cluster_serves_attribute(
    source: &str,
    endpoint: u16,
    cluster: &str,
    attribute: &str,
) -> Result<bool> {
    let header = format!("endpoint {endpoint} {{");
    let mut lines = source.lines().skip_while(|l| l.trim_end() != header);
    if lines.next().is_none() {
        bail!("no `{header}` block");
    }
    let opening = format!("server cluster {cluster} {{");
    let mut block = lines
        .take_while(|l| !l.starts_with('}'))
        .skip_while(|l| l.trim() != opening);
    if block.next().is_none() {
        bail!("endpoint {endpoint} serves no cluster {cluster}");
    }
    Ok(block.take_while(|l| l.trim() != "}").any(|l| {
        // The name is the word after the first `attribute`; without a
        // `default = ...` the line's `;` is glued to it.
        let mut words = l.split_whitespace().skip_while(|w| *w != "attribute");
        words.next().is_some()
            && words
                .next()
                .map(|name| name.strip_suffix(';').unwrap_or(name))
                == Some(attribute)
    }))
}

/// Global attribute ids (`AcceptedCommandList`, `AttributeList`, ...,
/// 0xF000..=0xFFFE) are left out of a sweep: `gen/globals.rs` covers them for
/// every cluster.
fn is_global(attribute: u32) -> bool {
    (0xF000..=0xFFFE).contains(&attribute)
}

/// Every non-global attribute of `cluster` on `endpoint`, from one wildcard
/// read, as `(attribute id, value TLV)` sorted by id: the input the generated
/// `decode_<attribute>` functions take.
///
/// # Errors
///
/// A transport or IM error from the read.
pub async fn read_cluster_attributes(
    node: &Node,
    endpoint: u16,
    cluster: u32,
) -> Result<Vec<(u32, Vec<u8>)>> {
    let mut attrs: Vec<(u32, Vec<u8>)> = node
        .read(&[ReadPath::cluster(endpoint, cluster)])
        .await
        .with_context(|| format!("wildcard read of cluster {cluster:#06x} on ep{endpoint}"))?
        .into_iter()
        .filter(|(p, _)| p.cluster == cluster && !is_global(p.attribute))
        .map(|(p, v)| (p.attribute, payload_tlv(&v)))
        .collect();
    attrs.sort_by_key(|(id, _)| *id);
    Ok(attrs)
}

/// Read every non-global attribute of `cluster` on `endpoint` and decode each
/// with `decode(attribute_id, tlv)`. Returns what was read (as
/// [`read_cluster_attributes`]), so the caller can assert the sweep was not
/// vacuous and inspect decoded values without a second read.
///
/// # Panics
///
/// On a read error or the first attribute that fails to decode, naming it.
pub async fn decode_every_attribute(
    node: &Node,
    endpoint: u16,
    cluster: u32,
    decode: impl Fn(u32, &[u8]) -> Result<(), String>,
) -> Vec<(u32, Vec<u8>)> {
    let attrs = read_cluster_attributes(node, endpoint, cluster)
        .await
        .unwrap_or_else(|e| panic!("{e:#}"));
    for (id, tlv) in &attrs {
        decode(*id, tlv).unwrap_or_else(|e| {
            panic!("cluster {cluster:#06x} attribute {id:#06x} failed to decode: {e}; {tlv:02x?}")
        });
    }
    eprintln!(
        "[sweep] ep{endpoint} cluster {cluster:#06x}: decoded attributes {:04x?}",
        attribute_ids(&attrs)
    );
    attrs
}

/// The attribute ids of a sweep's results, in order.
#[must_use]
pub fn attribute_ids(attrs: &[(u32, Vec<u8>)]) -> Vec<u32> {
    attrs.iter().map(|(id, _)| *id).collect()
}

/// Whether `attribute` is manufacturer-specific: its MEI carries a non-zero
/// vendor prefix in the upper 16 bits, where every standard attribute id has
/// prefix 0. chip v1.4.2.0 all-clusters' `ModeSelect` serves one,
/// `manufacturerExtension` (`0xFFF1_0001`).
#[must_use]
pub fn is_vendor_attribute(attribute: u32) -> bool {
    attribute > 0xFFFF
}

/// The standard attribute ids of a sweep's results, in order: what
/// [`attribute_ids`] returns minus vendor attributes
/// ([`is_vendor_attribute`]), for an exact comparison with the ids the
/// codegen knows. Which vendor attributes a device adds is its own business.
#[must_use]
pub fn standard_attribute_ids(attrs: &[(u32, Vec<u8>)]) -> Vec<u32> {
    attrs
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| !is_vendor_attribute(*id))
        .collect()
}

/// Assert that a sweep read exactly the standard attribute ids `want`, in
/// order ([`standard_attribute_ids`]: vendor attributes are left out). No
/// fewer (a vacuous or partial sweep) and no more (an attribute the test does
/// not know the host serves).
///
/// # Panics
///
/// When the standard ids read differ from `want`, naming `cluster`.
pub fn assert_exact_attribute_ids(cluster: &str, attrs: &[(u32, Vec<u8>)], want: &[u32]) {
    let got = standard_attribute_ids(attrs);
    assert_eq!(
        got, want,
        "{cluster}: served attribute ids {got:04x?}, expected {want:04x?}"
    );
}

/// The value TLV of attribute `id` in a sweep's results.
///
/// # Panics
///
/// If the wildcard read did not return `id`.
#[must_use]
pub fn attribute_tlv(attrs: &[(u32, Vec<u8>)], id: u32) -> &[u8] {
    &attrs
        .iter()
        .find(|(a, _)| *a == id)
        .unwrap_or_else(|| panic!("attribute {id:#06x} not in the wildcard read"))
        .1
}

/// Map a generated decoder's result to the [`decode_every_attribute`] shape.
///
/// # Errors
///
/// The decoder's error, rendered.
pub fn ok<T>(r: Result<T, ClusterError>) -> Result<(), String> {
    r.map(|_| ()).map_err(|e| e.to_string())
}

/// An attribute id the 1.4 codegen does not know: a vendor attribute
/// ([`is_vendor_attribute`]), or a standard one newer than the codegen (a
/// 1.5-era chip may serve one). Logged as which it is and skipped, never a
/// failure. The callers assert the ids they do know were all read.
///
/// # Errors
///
/// Never; the `Result` matches the decode-closure signature.
#[allow(clippy::unnecessary_wraps)]
pub fn newer_than_codegen(cluster: &str, attribute: u32) -> Result<(), String> {
    if is_vendor_attribute(attribute) {
        eprintln!("[sweep] {cluster}: skipping vendor attribute {attribute:#010x}");
    } else {
        eprintln!(
            "[sweep] {cluster}: skipping attribute {attribute:#06x} unknown to the 1.4 codegen"
        );
    }
    Ok(())
}

/// Invoke `path` with pre-encoded `fields` and return the response command's
/// payload TLV, for the generated `<Response>::decode`.
///
/// # Errors
///
/// A transport error, a bare status instead of a response, or a response
/// that is not `response_command` on `path`'s endpoint and cluster.
pub async fn invoke_for_response(
    node: &Node,
    path: CommandPath,
    fields: Vec<u8>,
    response_command: u32,
) -> Result<Vec<u8>> {
    let result = node.invoke_tlv(path, fields).await.context("invoke")?;
    response_payload(path, response_command, result)
}

/// The payload TLV of `result` when it is the response command
/// `response_command` on the request `path`'s endpoint and cluster.
fn response_payload(
    path: CommandPath,
    response_command: u32,
    result: InvokeResult,
) -> Result<Vec<u8>> {
    match result {
        InvokeResult::Data { path: got, fields }
            if (got.endpoint, got.cluster, got.command)
                == (path.endpoint, path.cluster, response_command) =>
        {
            Ok(payload_tlv(&fields))
        }
        other => bail!(
            "ep{} command {:#06x}/{:#04x}: expected response {:#06x}/{response_command:#04x} \
             on ep{}, got {other:?}",
            path.endpoint,
            path.cluster,
            path.command,
            path.cluster,
            path.endpoint
        ),
    }
}

/// Write one attribute with the value TLV a generated `encode_<attribute>`
/// returned, and return the status the device answered for that path.
///
/// # Errors
///
/// A TLV the codec cannot read back, a transport error, or a response that is
/// not exactly one status for `path`.
pub async fn write_attribute(node: &Node, path: AttributePath, tlv: Vec<u8>) -> Result<ImStatus> {
    let (_, value) = TlvReader::new(&tlv)
        .read_value()
        .context("read back the generated attribute TLV")?;
    let statuses = node
        .write(&[(path, value)])
        .await
        .with_context(|| format!("write {path:?}"))?;
    match statuses.as_slice() {
        [(p, s)] if *p == path => Ok(*s),
        other => bail!("write {path:?}: expected one status for the path, got {other:?}"),
    }
}

/// Invoke `path` with pre-encoded `fields` and return the bare status the
/// device answered (a command with no response command).
///
/// # Errors
///
/// A transport error, or a response command where a status was expected.
pub async fn invoke_for_status(
    node: &Node,
    path: CommandPath,
    fields: Vec<u8>,
) -> Result<ImStatus> {
    match node.invoke_tlv(path, fields).await.context("invoke")? {
        InvokeResult::Status(s) => Ok(s),
        other => bail!(
            "command {:#06x}/{:#04x}: expected a status, got {other:?}",
            path.cluster,
            path.command
        ),
    }
}

/// Sweep one `ModeBase` derivative on `$endpoint`: decode every
/// attribute from a wildcard read (asserting `SupportedModes` and
/// `CurrentMode` are among them, and that `CurrentMode` is a supported mode),
/// then invoke `ChangeToMode(CurrentMode)` and decode the response.
///
/// `ChangeToMode(CurrentMode)` is the safe command: chip answers `Success`
/// before it consults the app's delegate and changes nothing
/// (`ModeBaseCluster.cpp` `HandleChangeToMode`), and it leaves `StatusText`
/// out of that reply, which the server builds itself; the macro
/// asserts both. `$m` is the generated
/// module, in scope at the call site (`use matter_clusters::clusters::rvc_run_mode;`).
///
/// Panics (it is a test helper) on any failure, naming the cluster.
#[macro_export]
macro_rules! sweep_mode_base {
    ($node:expr, $endpoint:expr, $m:ident) => {{
        use $crate::sweep::{
            attribute_ids, attribute_tlv, decode_every_attribute, invoke_for_response,
            newer_than_codegen, ok,
        };
        use $m::attribute_id::{CURRENT_MODE, SUPPORTED_MODES};
        let name = stringify!($m);
        let attrs = decode_every_attribute($node, $endpoint, $m::CLUSTER_ID, |id, t| match id {
            SUPPORTED_MODES => ok($m::decode_supported_modes(t)),
            CURRENT_MODE => ok($m::decode_current_mode(t)),
            other => newer_than_codegen(name, other),
        })
        .await;
        let ids = attribute_ids(&attrs);
        assert!(
            ids.contains(&SUPPORTED_MODES) && ids.contains(&CURRENT_MODE),
            "{name}: {ids:04x?}"
        );
        let modes = $m::decode_supported_modes(attribute_tlv(&attrs, SUPPORTED_MODES)).unwrap();
        let current = $m::decode_current_mode(attribute_tlv(&attrs, CURRENT_MODE)).unwrap();
        assert!(
            modes.iter().any(|m| m.mode == current),
            "{name}: CurrentMode {current} not in SupportedModes"
        );
        let path = ::matter_controller::CommandPath {
            endpoint: $endpoint,
            cluster: $m::CLUSTER_ID,
            command: $m::command_id::CHANGE_TO_MODE,
        };
        let resp = invoke_for_response(
            $node,
            path,
            $m::encode_change_to_mode(current),
            $m::command_id::CHANGE_TO_MODE_RESPONSE,
        )
        .await
        .unwrap_or_else(|e| panic!("{name}: {e:#}"));
        let r = $m::ChangeToModeResponse::decode(&resp)
            .unwrap_or_else(|e| panic!("{name}: ChangeToModeResponse: {e}"));
        assert_eq!(r.status, $m::ModeChangeStatus::Success, "{name}");
        assert_eq!(
            r.status_text, None,
            "{name}: chip's server omits StatusText from this reply"
        );
    }};
}

/// Sweep one `OperationalState`-family cluster (`OperationalState`,
/// `OvenCavityOperationalState`, `RvcOperationalState`) on `$endpoint`: decode
/// every attribute from a wildcard read, assert the standard ids served are
/// exactly `$ids`, and evaluate to the sweep's `(id, tlv)` pairs for value
/// checks. `$m` is the generated module, in scope at the call site.
///
/// Panics (it is a test helper) on any failure, naming the cluster.
#[macro_export]
macro_rules! sweep_operational_state {
    ($node:expr, $endpoint:expr, $m:ident, $ids:expr) => {{
        use $crate::sweep::{
            assert_exact_attribute_ids, decode_every_attribute, newer_than_codegen, ok,
        };
        use $m::attribute_id as op;
        let name = stringify!($m);
        let attrs = decode_every_attribute($node, $endpoint, $m::CLUSTER_ID, |id, t| match id {
            op::PHASE_LIST => ok($m::decode_phase_list(t)),
            op::CURRENT_PHASE => ok($m::decode_current_phase(t)),
            op::COUNTDOWN_TIME => ok($m::decode_countdown_time(t)),
            op::OPERATIONAL_STATE_LIST => ok($m::decode_operational_state_list(t)),
            op::OPERATIONAL_STATE => ok($m::decode_operational_state(t)),
            op::OPERATIONAL_ERROR => ok($m::decode_operational_error(t)),
            other => newer_than_codegen(name, other),
        })
        .await;
        assert_exact_attribute_ids(name, &attrs, $ids);
        attrs
    }};
}

/// Invoke one `OperationalState`-family command (`$cmd`, a `command_id`
/// constant; `$encode`, its generated encoder) on `$endpoint` and evaluate to
/// the decoded `OperationalCommandResponse.CommandResponseState`. Every such
/// command answers with that response, never a bare status
/// (`OperationalStateCluster.cpp`; `operational-state-server.cpp` at
/// v1.4.2.0).
///
/// Panics (it is a test helper) on a transport error, a bare status or an
/// undecodable response, naming the command.
#[macro_export]
macro_rules! operational_command {
    ($node:expr, $endpoint:expr, $m:ident, $cmd:ident, $encode:ident) => {{
        let path = ::matter_controller::CommandPath {
            endpoint: $endpoint,
            cluster: $m::CLUSTER_ID,
            command: $m::command_id::$cmd,
        };
        let resp = $crate::sweep::invoke_for_response(
            $node,
            path,
            $m::$encode(),
            $m::command_id::OPERATIONAL_COMMAND_RESPONSE,
        )
        .await
        .unwrap_or_else(|e| panic!("{}.{}: {e:#}", stringify!($m), stringify!($cmd)));
        $m::OperationalCommandResponse::decode(&resp)
            .unwrap_or_else(|e| panic!("{}.{}: {e}", stringify!($m), stringify!($cmd)))
            .command_response_state
    }};
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use matter_codec::Value;

    /// The shape of `all-clusters-app.matter`: cluster definitions first,
    /// then one block per endpoint whose clusters are indented by two.
    const MATTER: &str = "\
cluster WaterHeaterMode = 158 {
  revision 1;
}

endpoint 0 {
  device type ma_rootdevice = 22, version 1;

  server cluster DeviceEnergyManagementMode {
    callback attribute supportedModes;
  }
}
endpoint 1 {
  device type ma_onofflight = 256, version 1;

  binding cluster OnOff;

  server cluster EnergyEvseModeX {
    callback attribute supportedModes;
  }
  server cluster WaterHeaterMode {
    callback attribute supportedModes;
    callback attribute currentMode;
  }
  client cluster EnergyEvseMode {
  }
}
endpoint 2 {
  server cluster EnergyEvseMode {
  }
}
";

    #[test]
    fn a_server_cluster_on_the_endpoint_is_served() {
        assert!(endpoint_serves(MATTER, 1, "WaterHeaterMode").unwrap());
    }

    #[test]
    fn a_cluster_absent_from_the_endpoint_is_not_served() {
        // Served on endpoint 0 only.
        assert!(!endpoint_serves(MATTER, 1, "DeviceEnergyManagementMode").unwrap());
        // A client on endpoint 1, a server only on endpoint 2, and a longer
        // name that starts with it: none is a server on endpoint 1.
        assert!(!endpoint_serves(MATTER, 1, "EnergyEvseMode").unwrap());
        // Declared as a cluster definition, not served on endpoint 0.
        assert!(!endpoint_serves(MATTER, 0, "WaterHeaterMode").unwrap());
    }

    #[test]
    fn the_last_endpoint_block_is_searched_too() {
        assert!(endpoint_serves(MATTER, 2, "EnergyEvseMode").unwrap());
    }

    const REQUEST: CommandPath = CommandPath {
        endpoint: 1,
        cluster: 0x0059,
        command: 0x00,
    };

    fn response_at(endpoint: u16, cluster: u32, command: u32) -> InvokeResult {
        InvokeResult::Data {
            path: CommandPath {
                endpoint,
                cluster,
                command,
            },
            fields: Value::Bool(true),
        }
    }

    #[test]
    fn the_matching_response_yields_its_payload() {
        let got = response_payload(REQUEST, 0x01, response_at(1, 0x0059, 0x01)).unwrap();
        assert_eq!(got, payload_tlv(&Value::Bool(true)));
    }

    #[test]
    fn a_response_with_another_command_id_is_refused() {
        assert!(response_payload(REQUEST, 0x01, response_at(1, 0x0059, 0x02)).is_err());
    }

    #[test]
    fn a_response_from_another_cluster_is_refused() {
        let err = response_payload(REQUEST, 0x01, response_at(1, 0x0051, 0x01)).unwrap_err();
        assert!(
            err.to_string().contains("expected response 0x0059/0x01"),
            "{err:#}"
        );
    }

    #[test]
    fn a_response_from_another_endpoint_is_refused() {
        let err = response_payload(REQUEST, 0x01, response_at(2, 0x0059, 0x01)).unwrap_err();
        assert!(err.to_string().contains("on ep1"), "{err:#}");
    }

    #[test]
    fn a_bare_status_is_refused() {
        let r = InvokeResult::Status(ImStatus::Success);
        assert!(response_payload(REQUEST, 0x01, r).is_err());
    }

    #[test]
    fn a_vendor_prefixed_attribute_is_a_vendor_attribute() {
        assert!(is_vendor_attribute(0xFFF1_0001));
        assert!(is_vendor_attribute(0x0001_0000));
        assert!(!is_vendor_attribute(0x0005));
        assert!(!is_vendor_attribute(0xFFFC));
    }

    #[test]
    fn standard_ids_leave_out_vendor_attributes() {
        let attrs: Vec<(u32, Vec<u8>)> = [0x0000, 0x0005, 0xFFF1_0001]
            .into_iter()
            .map(|id| (id, Vec::new()))
            .collect();
        assert_eq!(standard_attribute_ids(&attrs), [0x0000, 0x0005]);
    }

    /// A `server cluster` block shaped like all-clusters' `SmokeCoAlarm`.
    const SMOKE: &str = "\
endpoint 1 {
  server cluster SmokeCoAlarm {
    emits event SmokeAlarm;
    persist  attribute expressedState default = 0;
    ram      attribute expiryDate default = 3976214400;
    ram      attribute unmounted default = 0;
    callback attribute currentSensitivityLevel;
    ram      attribute openDuration;
  }
  server cluster OnOff {
    ram      attribute unmountedx default = 0;
  }
}
";

    #[test]
    fn an_attribute_in_the_cluster_block_is_served() {
        assert!(cluster_serves_attribute(SMOKE, 1, "SmokeCoAlarm", "unmounted").unwrap());
        assert!(cluster_serves_attribute(SMOKE, 1, "SmokeCoAlarm", "expiryDate").unwrap());
    }

    #[test]
    fn a_declaration_without_a_default_is_served() {
        // No `default = ...`, so the `;` is glued to the name; `callback`
        // attributes never carry a default.
        assert!(
            cluster_serves_attribute(SMOKE, 1, "SmokeCoAlarm", "currentSensitivityLevel").unwrap()
        );
        assert!(cluster_serves_attribute(SMOKE, 1, "SmokeCoAlarm", "openDuration").unwrap());
    }

    #[test]
    fn a_near_miss_of_a_declaration_without_a_default_is_not_served() {
        for name in [
            "currentSensitivity",
            "currentSensitivityLevelX",
            "openDuration;",
            "default",
            "=",
        ] {
            assert!(
                !cluster_serves_attribute(SMOKE, 1, "SmokeCoAlarm", name).unwrap(),
                "{name}"
            );
        }
    }

    #[test]
    fn an_attribute_elsewhere_or_a_longer_name_is_not_served() {
        // `unmountedx` is another cluster's (and a longer name); an event is not
        // an attribute.
        assert!(!cluster_serves_attribute(SMOKE, 1, "SmokeCoAlarm", "unmountedx").unwrap());
        assert!(!cluster_serves_attribute(SMOKE, 1, "OnOff", "unmounted").unwrap());
        assert!(!cluster_serves_attribute(SMOKE, 1, "SmokeCoAlarm", "SmokeAlarm").unwrap());
    }

    #[test]
    fn a_cluster_not_served_on_the_endpoint_is_an_error() {
        let err = cluster_serves_attribute(SMOKE, 1, "Thermostat", "x").unwrap_err();
        assert!(
            err.to_string().contains("serves no cluster Thermostat"),
            "{err:#}"
        );
        assert!(cluster_serves_attribute(SMOKE, 2, "SmokeCoAlarm", "unmounted").is_err());
    }

    #[test]
    fn a_missing_endpoint_block_is_an_error() {
        let err = endpoint_serves(MATTER, 9, "WaterHeaterMode").unwrap_err();
        assert!(err.to_string().contains("endpoint 9 {"), "{err:#}");
    }
}
