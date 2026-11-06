//! `control_onoff` — M7.5 example: commission an IP-reachable Matter device,
//! then drive `matter-clusters` codecs over a **fresh operational CASE session**.
//!
//! After commissioning (PASE → attestation → NOC → CASE → `CommissioningComplete`)
//! the device retains our fabric. This example then opens a *second*, fresh
//! operational CASE session and exercises the generated cluster codecs:
//!   1. read   `OnOff.OnOff` (endpoint 1)              → bool
//!   2. invoke `OnOff.Toggle` (endpoint 1)
//!   3. read   `OnOff.OnOff` again                     → flipped
//!   4. write  `BasicInformation.NodeLabel` (ep 0)     = "matter-rust"
//!   5. read   `BasicInformation.NodeLabel`            → echoes back
//!
//! Walkthrough + cross-verification: `docs/runbooks/m7.5-control-onoff.md`.
//!
//! Built behind the `driver` feature (mirrors `commission_ip`):
//! ```text
//! cargo run --example control_onoff --features driver -- --help
//! ```
//! To capture a decrypted wire-message trace for `cargo xtask trace-diff`:
//! ```text
//! cargo run --example control_onoff --features driver,tracing,wiretrace -- \
//!     --manual <code> --trace-out runs/rust-onoff.jsonl -vv
//! ```
//!
//! NOTE: this example does not decommission the device on exit (matching
//! `commission_ip`); each run consumes one device fabric slot. Persistent
//! fabric management + decommission is M8 `matter-controller` work.

use std::fs;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::Parser;

use matter_cert::{MatterCertificate, MatterTime, TrustAnchor, TrustedRoots};
use matter_clusters::clusters::{basic_information, on_off};
use matter_codec::{Tag, TlvWriter, Value};
use matter_commissioning::attestation::{CdSigningRoots, Paa, PaaTrustStore};
use matter_commissioning::driver::{
    commission, resolve_operational, run_case, secured_round_trip, DriverConfig,
};
use matter_commissioning::im::{
    build_invoke_request, build_read_request, build_write_request, parse_invoke_response,
    parse_report_data, parse_write_response, AttributePath, AttributeWriteRequest, CommandPath,
    ImStatus, InvokeResponse, ReportData,
};
use matter_commissioning::noc::{issue_noc, FabricRecord, NocRng, SystemNocRng, VerifiedCsr};
use matter_commissioning::setup::{parse_manual_code, parse_qr, SetupPayload};
use matter_commissioning::state_machine::CommissionerConfig;
use matter_crypto::{
    derive_compressed_fabric_id, derive_operational_ipk, CaseCredentials, RingSigner, Signer,
};
use matter_transport::{MdnsSdDiscovery, ProtocolId, SessionId, SessionManager, TokioUdpTransport};

// Fixed identity for this single commissioning run — identical to
// `commission_ip` so the two examples behave the same on the wire. A real
// controller (M8) persists these; the example mints a fresh fabric per run.
const FABRIC_ID: u64 = 1;
const RCAC_ID: u64 = 1;
const COMMISSIONER_NODE_ID: u64 = 0x1234_5678_9ABC_DEF0;
const ASSIGNED_NODE_ID: u64 = 0x0000_0000_0000_0002;
const ADMIN_VENDOR_ID: u16 = 0xFFF1; // CSA test VID
const IPK_EPOCH_KEY: [u8; 16] = [0x42; 16];

// Concrete (endpoint, cluster, attribute/command) targets for the demo.
const ONOFF_ENDPOINT: u16 = 1;
const ONOFF_CLUSTER: u32 = 0x0006;
const ONOFF_ATTR_ONOFF: u32 = 0x0000;
const ONOFF_CMD_TOGGLE: u32 = 0x02;
const BASICINFO_ENDPOINT: u16 = 0;
const BASICINFO_CLUSTER: u32 = 0x0028;
const BASICINFO_ATTR_NODE_LABEL: u32 = 0x0005;
const NODE_LABEL_VALUE: &str = "matter-rust";

// IM message opcodes (Matter Core Spec §10).
const OP_READ_REQUEST: u8 = 0x02;
const OP_WRITE_REQUEST: u8 = 0x06;
const OP_INVOKE_REQUEST: u8 = 0x08;

/// Commission an IP-reachable Matter device, then read/toggle `OnOff` and
/// write/read `BasicInformation.NodeLabel` over a fresh operational session.
#[derive(Debug, Parser)]
#[command(name = "control_onoff", about, long_about = None)]
struct Cli {
    /// Setup payload from a QR code (e.g. "MT:..."). Mutually exclusive with --manual.
    #[arg(long, conflicts_with = "manual")]
    qr: Option<String>,

    /// Setup payload from an 11- or 21-digit manual pairing code.
    #[arg(long)]
    manual: Option<String>,

    /// Dial this address directly (e.g. "`[fd11::2]:5540`"), skipping the mDNS browse.
    #[arg(long)]
    addr: Option<String>,

    /// Directory of PRODUCTION PAA root certs (loads every *.der file).
    /// When set, the bundled CSA test PAA roots are not used.
    #[arg(long)]
    paa_dir: Option<PathBuf>,

    /// PRODUCTION CD signing roots: a directory of CSA CD signing certs
    /// (loads every *.der), or a single *.der cert. When set, the bundled CSA
    /// test CD roots are not used.
    #[arg(long)]
    cd_root: Option<PathBuf>,

    /// Write the decrypted wire-message trace as JSON lines to this path
    /// (for `cargo xtask trace-diff`). Requires building with
    /// `--features driver,tracing,wiretrace`.
    #[arg(long)]
    trace_out: Option<PathBuf>,

    /// Increase log verbosity (-v = info spans, -vv = debug).
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

/// The attestation trust anchors the commissioner validates against.
struct TrustRoots {
    paa: PaaTrustStore,
    cd: CdSigningRoots,
    /// True when ANY bundled CSA *test* root is in use (drives the banner).
    using_test_roots: bool,
}

/// Build PAA + CD trust roots from the CLI. Defaults to the bundled CSA *test*
/// roots; `--paa-dir` / `--cd-root` independently swap in production roots.
fn build_trust_roots(cli: &Cli) -> anyhow::Result<TrustRoots> {
    let mut using_test_roots = false;

    let paa = if let Some(dir) = &cli.paa_dir {
        load_production_paa(dir)
            .with_context(|| format!("loading PAA roots from {}", dir.display()))?
    } else {
        using_test_roots = true;
        PaaTrustStore::with_example_device_roots()
    };

    let cd = if let Some(path) = &cli.cd_root {
        load_production_cd(path)
            .with_context(|| format!("loading CD signing roots from {}", path.display()))?
    } else {
        using_test_roots = true;
        CdSigningRoots::with_example_device_roots()
    };

    Ok(TrustRoots {
        paa,
        cd,
        using_test_roots,
    })
}

/// Load every `*.der` file in `dir` into a PAA trust store.
fn load_production_paa(dir: &std::path::Path) -> anyhow::Result<PaaTrustStore> {
    let mut store = PaaTrustStore::empty();
    let mut count = 0_usize;
    for entry in fs::read_dir(dir).with_context(|| format!("reading dir {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("der") {
            continue;
        }
        let der = fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let paa = Paa::from_der(&der).with_context(|| format!("parsing PAA {}", path.display()))?;
        store.add(paa);
        count += 1;
    }
    if count == 0 {
        bail!("no *.der PAA certs found in {}", dir.display());
    }
    Ok(store)
}

/// Load CSA CD signing roots from `path`: a directory of `*.der` CD signing
/// certificates, or a single `*.der` certificate.
fn load_production_cd(path: &std::path::Path) -> anyhow::Result<CdSigningRoots> {
    let mut ders: Vec<Vec<u8>> = Vec::new();
    if path.is_dir() {
        for entry in
            fs::read_dir(path).with_context(|| format!("reading dir {}", path.display()))?
        {
            let p = entry?.path();
            if p.extension().and_then(|e| e.to_str()) != Some("der") {
                continue;
            }
            ders.push(fs::read(&p).with_context(|| format!("reading {}", p.display()))?);
        }
        if ders.is_empty() {
            bail!("no *.der CD signing certs found in {}", path.display());
        }
    } else {
        ders.push(fs::read(path).with_context(|| format!("reading {}", path.display()))?);
    }
    let refs: Vec<&[u8]> = ders.iter().map(Vec::as_slice).collect();
    CdSigningRoots::from_cert_der(&refs).context("parsing CD signing certificates")
}

/// Resolve the setup payload from exactly one of `--qr` / `--manual`.
fn parse_setup_payload(cli: &Cli) -> anyhow::Result<SetupPayload> {
    match (&cli.qr, &cli.manual) {
        (Some(qr), None) => parse_qr(qr).context("parsing --qr setup payload"),
        (None, Some(manual)) => parse_manual_code(manual).context("parsing --manual pairing code"),
        (None, None) => bail!("one of --qr or --manual is required"),
        (Some(_), Some(_)) => bail!("--qr and --manual are mutually exclusive"),
    }
}

/// Parse a `--addr` value, supporting an IPv6 zone (scope) id for link-local
/// targets: `[fe80::1%11]:5540` (numeric interface index).
fn parse_dial_addr(s: &str) -> anyhow::Result<SocketAddr> {
    if let Some(pct) = s.find('%') {
        let close = s
            .find(']')
            .context("--addr with a zone id must be bracketed: [fe80::1%11]:5540")?;
        let ip: Ipv6Addr = s[1..pct].parse().context("--addr IPv6 address")?;
        let scope: u32 = s[pct + 1..close]
            .parse()
            .context("--addr zone id must be a numeric interface index, e.g. %11")?;
        let port: u16 = s[close + 2..].parse().context("--addr port")?;
        return Ok(SocketAddr::V6(SocketAddrV6::new(ip, port, 0, scope)));
    }
    s.parse::<SocketAddr>().context("parsing --addr")
}

/// Current wall-clock time as `MatterTime`. A binary may read the system clock.
fn current_matter_time() -> anyhow::Result<MatterTime> {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock before unix epoch")?
        .as_secs();
    Ok(MatterTime::from_unix_secs(secs))
}

/// Re-emit a decoded `ReportData` value as the standalone anonymous-tagged TLV
/// element the `matter-clusters` attribute decoders consume — the same
/// re-encode the commissioning read path uses (`driver::commission`'s
/// `extract_read_payload`). `write_value` handles every `Value` variant.
#[allow(clippy::expect_used)] // Vec-backed TlvWriter is infallible (project idiom; see matter-interaction builders).
fn value_to_tlv(value: &Value) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.write_value(Tag::Anonymous, value)
        .expect("infallible: Vec-backed TlvWriter");
    buf
}

/// Find one attribute value in a `ReportData` by (cluster, attribute).
fn find_attr(report: &ReportData, cluster: u32, attribute: u32) -> anyhow::Result<&Value> {
    report
        .attributes()
        .find(|(p, _)| p.cluster == cluster && p.attribute == attribute)
        .map(|(_, v)| v)
        .ok_or_else(|| {
            anyhow::anyhow!("attribute {cluster:#06x}/{attribute:#06x} absent from ReportData")
        })
}

/// Read `OnOff.OnOff` over the operational session and decode it via the
/// generated `matter-clusters` codec.
async fn read_on_off(
    transport: &TokioUdpTransport,
    sessions: &mut SessionManager,
    sid: SessionId,
    peer: SocketAddr,
) -> anyhow::Result<bool> {
    let req = build_read_request(&[AttributePath {
        endpoint: ONOFF_ENDPOINT,
        cluster: ONOFF_CLUSTER,
        attribute: ONOFF_ATTR_ONOFF,
    }]);
    let resp = secured_round_trip(
        transport,
        sessions,
        sid,
        peer,
        OP_READ_REQUEST,
        ProtocolId::INTERACTION_MODEL,
        &req,
    )
    .await
    .context("OnOff.OnOff read round-trip")?;
    let report = parse_report_data(&resp.payload).context("parsing OnOff ReportData")?;
    let value = find_attr(&report, ONOFF_CLUSTER, ONOFF_ATTR_ONOFF)?;
    on_off::decode_on_off(&value_to_tlv(value)).context("decoding OnOff.OnOff")
}

/// Invoke `OnOff.Toggle` over the operational session.
async fn invoke_toggle(
    transport: &TokioUdpTransport,
    sessions: &mut SessionManager,
    sid: SessionId,
    peer: SocketAddr,
) -> anyhow::Result<()> {
    let fields = on_off::encode_toggle();
    let req = build_invoke_request(
        CommandPath {
            endpoint: ONOFF_ENDPOINT,
            cluster: ONOFF_CLUSTER,
            command: ONOFF_CMD_TOGGLE,
        },
        &fields,
    );
    let resp = secured_round_trip(
        transport,
        sessions,
        sid,
        peer,
        OP_INVOKE_REQUEST,
        ProtocolId::INTERACTION_MODEL,
        &req,
    )
    .await
    .context("OnOff.Toggle invoke round-trip")?;
    match parse_invoke_response(&resp.payload).context("parsing Toggle InvokeResponse")? {
        // Toggle has no response payload; the device returns a bare success
        // status. A command-data response is unexpected but not a failure.
        InvokeResponse::Status(ImStatus::Success) | InvokeResponse::Command { .. } => Ok(()),
        InvokeResponse::Status(other) => bail!("OnOff.Toggle returned status {other:?}"),
    }
}

/// Write `BasicInformation.NodeLabel = "matter-rust"` over the operational session.
async fn write_node_label(
    transport: &TokioUdpTransport,
    sessions: &mut SessionManager,
    sid: SessionId,
    peer: SocketAddr,
) -> anyhow::Result<()> {
    let value_tlv = basic_information::encode_node_label(&NODE_LABEL_VALUE.to_string());
    let req = build_write_request(&[AttributeWriteRequest {
        path: AttributePath {
            endpoint: BASICINFO_ENDPOINT,
            cluster: BASICINFO_CLUSTER,
            attribute: BASICINFO_ATTR_NODE_LABEL,
        },
        value_tlv,
    }]);
    let resp = secured_round_trip(
        transport,
        sessions,
        sid,
        peer,
        OP_WRITE_REQUEST,
        ProtocolId::INTERACTION_MODEL,
        &req,
    )
    .await
    .context("NodeLabel write round-trip")?;
    let statuses = parse_write_response(&resp.payload).context("parsing WriteResponse")?;
    if statuses.is_empty() {
        bail!("NodeLabel write returned no per-path status");
    }
    for (path, status) in statuses {
        if !matches!(status, ImStatus::Success) {
            bail!(
                "NodeLabel write to {:#06x}/{:#06x} failed: {status:?}",
                path.cluster,
                path.attribute
            );
        }
    }
    Ok(())
}

/// Read `BasicInformation.NodeLabel` over the operational session.
async fn read_node_label(
    transport: &TokioUdpTransport,
    sessions: &mut SessionManager,
    sid: SessionId,
    peer: SocketAddr,
) -> anyhow::Result<String> {
    let req = build_read_request(&[AttributePath {
        endpoint: BASICINFO_ENDPOINT,
        cluster: BASICINFO_CLUSTER,
        attribute: BASICINFO_ATTR_NODE_LABEL,
    }]);
    let resp = secured_round_trip(
        transport,
        sessions,
        sid,
        peer,
        OP_READ_REQUEST,
        ProtocolId::INTERACTION_MODEL,
        &req,
    )
    .await
    .context("NodeLabel read round-trip")?;
    let report = parse_report_data(&resp.payload).context("parsing NodeLabel ReportData")?;
    let value = find_attr(&report, BASICINFO_CLUSTER, BASICINFO_ATTR_NODE_LABEL)?;
    basic_information::decode_node_label(&value_to_tlv(value)).context("decoding NodeLabel")
}

/// Initialize tracing: a stderr fmt layer driven by `-v/-vv`, plus — when
/// `--trace-out` is set and the `wiretrace` feature is compiled in — a
/// `JsonlLayer` writing the decrypted wire-message dialogue for
/// `cargo xtask trace-diff`.
fn init_tracing(verbose: u8, trace_out: Option<&std::path::Path>) -> anyhow::Result<()> {
    #[cfg(feature = "tracing")]
    {
        use tracing_subscriber::layer::SubscriberExt as _;
        use tracing_subscriber::util::SubscriberInitExt as _;
        use tracing_subscriber::Layer as _;

        let level = match verbose {
            0 => "warn",
            1 => "info",
            _ => "debug",
        };
        let fmt = tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_filter(tracing_subscriber::EnvFilter::new(level));

        #[cfg(feature = "wiretrace")]
        if let Some(path) = trace_out {
            let file = fs::File::create(path)
                .with_context(|| format!("creating trace file {}", path.display()))?;
            tracing_subscriber::registry()
                .with(fmt)
                .with(matter_commissioning::wiretrace::JsonlLayer::new(file))
                .init();
            return Ok(());
        }

        #[cfg(not(feature = "wiretrace"))]
        if trace_out.is_some() {
            anyhow::bail!("--trace-out requires building with --features driver,tracing,wiretrace");
        }

        tracing_subscriber::registry().with(fmt).init();
    }
    #[cfg(not(feature = "tracing"))]
    {
        let _ = verbose;
        if trace_out.is_some() {
            anyhow::bail!("--trace-out requires building with --features driver,tracing,wiretrace");
        }
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose, cli.trace_out.as_deref())?;
    let payload = parse_setup_payload(&cli)?;
    println!(
        "setup payload: vid={:?} pid={:?} discriminator={} passcode=<redacted>",
        payload.vendor_id,
        payload.product_id,
        // A manual code carries only the short discriminator; printing its
        // zero-extended form as though it were the device's 12-bit value is
        // how #120 started.
        if payload.discriminator.is_short() {
            format!("{:#x} (short)", payload.discriminator.short())
        } else {
            format!("{}", payload.discriminator.as_u16())
        },
    );
    let roots = build_trust_roots(&cli)?;
    if roots.using_test_roots {
        eprintln!(
            "\u{26A0}  TEST ATTESTATION ROOTS IN USE — this run trusts CSA *test* PAA/CD roots.\n   NOT valid for production trust decisions. Pass --paa-dir and --cd-root for real devices."
        );
    }

    // Self-generate an ephemeral fabric RCAC for this run. Kept in `rcac_fabric`
    // so it OUTLIVES commission() (the config only borrows it) and is available
    // to mint the commissioner's operational NOC below.
    // FLAG: per-run identity only — M8 owns a stable persistent commissioner identity.
    let (root_signer, _pkcs8) = RingSigner::generate().context("generating fabric root key")?;
    let root_signer: Arc<dyn Signer> = Arc::new(root_signer);
    let now = current_matter_time()?;
    let rcac_fabric = FabricRecord::new_root_only(
        FABRIC_ID,
        root_signer,
        now,
        MatterTime::NO_EXPIRY,
        RCAC_ID,
        &SystemNocRng,
    )
    .context("building fabric RCAC")?;

    let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
    let commissioner = CommissionerConfig {
        // commission() overwrites this with the live PASE-derived attestation key.
        pase_attestation_challenge: [0u8; 16],
        fabric: &rcac_fabric,
        setup_payload: &payload,
        paa_trust_store: &roots.paa,
        cd_signing_roots: &roots.cd,
        commissioner_node_id: COMMISSIONER_NODE_ID,
        assigned_node_id: ASSIGNED_NODE_ID,
        ipk_epoch_key: IPK_EPOCH_KEY,
        case_admin_subject: COMMISSIONER_NODE_ID,
        admin_vendor_id: ADMIN_VENDOR_ID,
        now,
        rng,
        network: matter_commissioning::NetworkCredentials::AlreadyOnNetwork, // ECM/Ethernet path; see runbook.
    };

    // Mint the commissioner's operational NOC ONCE here — used both for the
    // commissioning CASE (passed into DriverConfig) and the subsequent
    // operational CASE (passed into operational_phase). One stable identity
    // for both, as M8's persistent CommissionerIdentity will provide.
    // FLAG: per-run identity only — M8 owns a stable persistent commissioner identity.
    let (commissioner_signer, commissioner_pkcs8) =
        RingSigner::generate().context("generating commissioner operational key")?;
    let commissioner_noc = issue_noc(
        &rcac_fabric,
        &VerifiedCsr {
            public_key: commissioner_signer.public_key().clone(),
        },
        COMMISSIONER_NODE_ID,
        &[],
        (now, MatterTime::NO_EXPIRY),
        &SystemNocRng,
    )
    .context("minting commissioner operational NOC")?;

    let commissionable_addr = match &cli.addr {
        Some(s) => Some(parse_dial_addr(s)?),
        None => None,
    };
    let dial_ipv4 = commissionable_addr.is_some_and(|a| a.is_ipv4());

    let config = DriverConfig {
        commissioner,
        commissionable_addr,
        passcode: payload.passcode.as_u32(),
        commissioner_noc: &commissioner_noc,
        commissioner_signer_pkcs8: &commissioner_pkcs8,
    };

    let transport = if dial_ipv4 {
        TokioUdpTransport::bind_addr(SocketAddr::from(([0u8, 0, 0, 0], 0)))
            .await
            .context("binding IPv4 UDP socket")?
    } else {
        TokioUdpTransport::bind(0)
            .await
            .context("binding UDP socket")?
    };
    let mut discovery = MdnsSdDiscovery::new().context("starting mDNS discovery")?;

    println!("commissioning… (PASE → attestation → NOC → CASE → CommissioningComplete)");
    let device = commission(&transport, &mut discovery, config)
        .await
        .context("commission() failed")?;
    println!(
        "✅ commissioned: peer_node_id={:#018x}",
        device.peer_node_id
    );

    operational_phase(
        &transport,
        &mut discovery,
        &rcac_fabric,
        commissioner_signer,
        &commissioner_noc,
    )
    .await?;

    println!("✅ control_onoff complete");
    Ok(())
}

/// Open a fresh operational CASE session to the just-commissioned device and
/// drive the five `matter-clusters` operations over it.
///
/// Accepts the pre-minted commissioner identity (the same NOC and private key
/// passed to `DriverConfig` during commissioning) so that one stable identity
/// is used for both the commissioning CASE and the operational CASE, matching
/// the M8 persistent `CommissionerIdentity` model.
async fn operational_phase(
    transport: &TokioUdpTransport,
    discovery: &mut MdnsSdDiscovery,
    rcac_fabric: &FabricRecord,
    commissioner_signer: RingSigner,
    commissioner_noc: &MatterCertificate,
) -> anyhow::Result<()> {
    println!("opening operational CASE session…");

    let compressed = derive_compressed_fabric_id(rcac_fabric.root_public_key.as_bytes(), FABRIC_ID)
        .context("deriving compressed fabric id")?;
    let peer = resolve_operational(discovery, compressed, ASSIGNED_NODE_ID)
        .await
        .context("resolving operational device address via mDNS")?;

    // The *operational* IPK (what Sigma1's destination id is HMAC'd with) is
    // derived from the SAME epoch key AddNOC distributed, salted with the
    // compressed fabric id (spec §4.15.2) — NOT the raw epoch key.
    let operational_ipk =
        derive_operational_ipk(&IPK_EPOCH_KEY, &compressed).context("deriving operational IPK")?;
    let credentials = CaseCredentials {
        noc: commissioner_noc.clone(),
        icac: rcac_fabric.icac_cert.clone(), // None for a root-only fabric.
        signer: Box::new(commissioner_signer),
        fabric_id: FABRIC_ID,
        node_id: COMMISSIONER_NODE_ID,
        ipk: operational_ipk,
        rcac_public_key: *rcac_fabric.root_public_key.as_bytes(),
    };
    let mut trusted_roots = TrustedRoots::new();
    trusted_roots.add(TrustAnchor::from_root_cert(&rcac_fabric.root_cert));

    let mut sessions = SessionManager::new();
    let sid = run_case(
        transport,
        &mut sessions,
        peer,
        credentials,
        trusted_roots,
        ASSIGNED_NODE_ID,       // peer_node_id = the device
        FABRIC_ID,              // peer_fabric_id
        current_matter_time()?, // validate the device NOC at real wall-clock
    )
    .await
    .context("operational CASE handshake (run_case)")?;
    println!("   operational session established (local id {})", sid.0);

    // 1. Read OnOff.OnOff.
    let before = read_on_off(transport, &mut sessions, sid, peer).await?;
    println!("   read   OnOff.OnOff            = {before}");

    // 2. Invoke OnOff.Toggle.
    invoke_toggle(transport, &mut sessions, sid, peer).await?;
    println!("   invoke OnOff.Toggle           -> OK");

    // 3. Read OnOff.OnOff again — expect it flipped.
    let after = read_on_off(transport, &mut sessions, sid, peer).await?;
    println!("   read   OnOff.OnOff            = {after} (was {before})");
    if after == before {
        bail!("OnOff did not change after Toggle (before={before}, after={after})");
    }

    // 4. Write BasicInformation.NodeLabel.
    write_node_label(transport, &mut sessions, sid, peer).await?;
    println!("   write  BasicInformation.NodeLabel = {NODE_LABEL_VALUE:?}");

    // 5. Read NodeLabel back — expect the echo.
    let echoed = read_node_label(transport, &mut sessions, sid, peer).await?;
    println!("   read   BasicInformation.NodeLabel = {echoed:?}");
    if echoed != NODE_LABEL_VALUE {
        bail!("NodeLabel did not echo: wrote {NODE_LABEL_VALUE:?}, read {echoed:?}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        // clap's own consistency checker: catches conflicting/duplicate args.
        Cli::command().debug_assert();
    }

    #[test]
    fn qr_and_manual_conflict() {
        let err = Cli::try_parse_from(["control_onoff", "--qr", "MT:X", "--manual", "123"]);
        assert!(err.is_err(), "--qr and --manual must be mutually exclusive");
    }

    #[test]
    fn value_to_tlv_roundtrips_through_onoff_codec() {
        // The bool a ReportData carries must survive value_to_tlv → cluster decode.
        let tlv = value_to_tlv(&Value::Bool(true));
        assert!(on_off::decode_on_off(&tlv).expect("decode true"));
        let tlv = value_to_tlv(&Value::Bool(false));
        assert!(!on_off::decode_on_off(&tlv).expect("decode false"));
    }

    #[test]
    fn value_to_tlv_roundtrips_through_node_label_codec() {
        let tlv = value_to_tlv(&Value::Utf8("matter-rust".to_string()));
        assert_eq!(
            basic_information::decode_node_label(&tlv).expect("decode label"),
            "matter-rust"
        );
    }
}
