//! `commission_ip` — operator binary that commissions an IP-reachable Matter
//! device end to end using the M6.6 [`commission`] orchestrator.
//!
//! Full walkthrough: `docs/runbooks/m6.6-first-commission.md`.
//!
//! Built behind the `driver` feature:
//! ```text
//! cargo run --example commission_ip --features driver -- --help
//! ```
//! For per-step tracing spans, also enable `tracing`:
//! ```text
//! cargo run --example commission_ip --features driver,tracing -- --manual <code> -vv
//! ```
//! To capture a decrypted wire-message trace for `cargo xtask trace-diff`:
//! ```text
//! cargo run --example commission_ip --features driver,tracing,wiretrace -- \
//!     --manual <code> --trace-out runs/rust-p110m.jsonl -vv
//! ```

use std::fs;
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::Parser;

use matter_cert::MatterTime;
use matter_commissioning::attestation::{CdSigningRoots, Paa, PaaTrustStore};
use matter_commissioning::driver::{commission, DriverConfig};
use matter_commissioning::noc::{issue_noc, FabricRecord, NocRng, SystemNocRng, VerifiedCsr};
use matter_commissioning::setup::{parse_manual_code, parse_qr, SetupPayload};
use matter_commissioning::state_machine::{CommissionedFabric, CommissionerConfig};
use matter_crypto::{derive_compressed_fabric_id, RingSigner, Signer};
use matter_transport::{MdnsSdDiscovery, TokioUdpTransport};

// Fixed identity for this single commissioning run. A real controller (M8)
// persists these; the example mints a fresh fabric per run.
const FABRIC_ID: u64 = 1;
const RCAC_ID: u64 = 1;
const COMMISSIONER_NODE_ID: u64 = 0x1234_5678_9ABC_DEF0;
const ASSIGNED_NODE_ID: u64 = 0x0000_0000_0000_0002;
const ADMIN_VENDOR_ID: u16 = 0xFFF1; // CSA test VID
const IPK_EPOCH_KEY: [u8; 16] = [0x42; 16];

/// Commission an IP-reachable Matter device that is in commissioning mode.
#[derive(Debug, Parser)]
#[command(name = "commission_ip", about, long_about = None)]
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
    /// (loads every *.der, e.g. connectedhomeip credentials/production/cd-certs),
    /// or a single *.der cert. When set, the bundled CSA test CD roots are not used.
    #[arg(long)]
    cd_root: Option<PathBuf>,

    /// Write the resulting fabric summary to this path as JSON.
    #[arg(long)]
    out: Option<PathBuf>,

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

/// Load every `*.der` file in `dir` into a PAA trust store. The connectedhomeip
/// snapshot (`credentials/production/paa-root-certs/`) ships paired `.der`+`.pem`
/// for each PAA, so loading DER-only avoids double-counting and PEM parsing.
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
/// certificates (e.g. connectedhomeip `credentials/production/cd-certs/`, which
/// ships several distinct CSA keys), or a single `*.der` certificate.
///
/// Real-world CD signing roots are X.509 certificates, so this uses
/// [`CdSigningRoots::from_cert_der`] (not the `from_pem` `SubjectPublicKeyInfo`
/// path the bundled test root uses).
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
        // clap's `conflicts_with` prevents both, but guard anyway.
        (Some(_), Some(_)) => bail!("--qr and --manual are mutually exclusive"),
    }
}

/// Parse a `--addr` value, supporting an IPv6 zone (scope) id for link-local
/// targets: `[fe80::1%11]:5540` (numeric interface index). Matter devices in a
/// commissioning window are typically reachable on their `fe80::` link-local
/// address, which requires a scope id that `SocketAddr`'s own parser rejects.
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

/// JSON-serializable summary of a commissioning result.
///
/// NOTE: the fabric's RCAC *private key* is intentionally NOT persisted — this
/// example mints an ephemeral per-run identity. Durable fabric persistence
/// (including the operational signing key) is M8 `matter-controller`'s job.
#[derive(serde::Serialize)]
struct FabricSummary {
    fabric_id: u64,
    compressed_fabric_id: String,
    peer_node_id: u64,
    peer_public_key: String,
    terminated_at: String,
}

impl FabricSummary {
    fn from_fabric(fabric: &CommissionedFabric) -> anyhow::Result<Self> {
        let compressed = derive_compressed_fabric_id(
            fabric.fabric.root_public_key.as_bytes(),
            fabric.fabric.fabric_id,
        )
        .context("deriving compressed fabric id")?;
        Ok(Self {
            fabric_id: fabric.fabric.fabric_id,
            compressed_fabric_id: hex::encode(compressed),
            peer_node_id: fabric.peer_node_id,
            peer_public_key: hex::encode(fabric.peer_root_public_key),
            terminated_at: format!("{:?}", fabric.terminated_at),
        })
    }
}

/// Print a human-readable summary of the commissioned fabric.
fn print_summary(fabric: &CommissionedFabric) -> anyhow::Result<()> {
    let compressed = derive_compressed_fabric_id(
        fabric.fabric.root_public_key.as_bytes(),
        fabric.fabric.fabric_id,
    )
    .context("deriving compressed fabric id")?;
    println!("✅ commissioned");
    println!("   fabric_id            = {}", fabric.fabric.fabric_id);
    println!("   compressed_fabric_id = {}", hex::encode(compressed));
    println!("   peer_node_id         = {:#018x}", fabric.peer_node_id);
    println!(
        "   peer_public_key      = {}",
        hex::encode(fabric.peer_root_public_key)
    );
    println!("   terminated_at        = {:?}", fabric.terminated_at);
    Ok(())
}

/// Initialize tracing: a stderr fmt layer driven by `-v/-vv`, plus — when
/// `--trace-out` is set and the `wiretrace` feature is compiled in — a
/// [`matter_commissioning::wiretrace::JsonlLayer`] writing the decrypted
/// wire-message dialogue for `cargo xtask trace-diff`. The
/// [`tracing_subscriber::EnvFilter`] is attached to the fmt layer only, so the
/// wire trace always captures every `matter_wire` event regardless of `-v`.
///
/// Returns an error if `--trace-out` is given but the required features are
/// not compiled in, or if the trace file cannot be created.
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

        // wiretrace absent: --trace-out was given but the feature is off → hard
        // error (mutually exclusive with the cfg(wiretrace) arm above).
        #[cfg(not(feature = "wiretrace"))]
        if trace_out.is_some() {
            anyhow::bail!("--trace-out requires building with --features driver,tracing,wiretrace");
        }

        tracing_subscriber::registry().with(fmt).init();
    }
    #[cfg(not(feature = "tracing"))]
    {
        // `-v/-vv` only takes effect when built with the `tracing` feature.
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

    // Self-generate an ephemeral fabric RCAC for this run.
    // FLAG: per-run identity only — M8 owns a stable persistent commissioner identity.
    let (root_signer, _pkcs8) = RingSigner::generate().context("generating fabric root key")?;
    let root_signer: Arc<dyn Signer> = Arc::new(root_signer);
    let now = current_matter_time()?;
    let fabric = FabricRecord::new_root_only(
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
        fabric: &fabric,
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

    // Optional direct-dial address (skips the mDNS commissionable browse).
    let commissionable_addr = match &cli.addr {
        Some(s) => Some(parse_dial_addr(s)?),
        None => None,
    };

    // Match the local socket's family to a direct-dial IPv4 target (the default
    // bind(0) is IPv6 dual-stack, which cannot route to a plain IPv4 peer).
    let dial_ipv4 = commissionable_addr.is_some_and(|a| a.is_ipv4());

    // Persistent commissioner operational identity (replaces the former
    // per-call throwaway mint inside commission()).
    // FLAG: per-run identity only — M8 owns a stable persistent commissioner identity.
    let (commissioner_signer, commissioner_pkcs8) =
        RingSigner::generate().context("generating commissioner operational key")?;
    let commissioner_noc = issue_noc(
        &fabric,
        &VerifiedCsr {
            public_key: commissioner_signer.public_key().clone(),
        },
        COMMISSIONER_NODE_ID,
        &[],
        (now, matter_cert::MatterTime::NO_EXPIRY),
        &SystemNocRng,
    )
    .context("minting commissioner operational NOC")?;

    let config = DriverConfig {
        commissioner,
        commissionable_addr,
        passcode: payload.passcode.as_u32(),
        commissioner_noc: &commissioner_noc,
        commissioner_signer_pkcs8: &commissioner_pkcs8,
    };

    // Real IO: UDP socket (IPv6 dual-stack by default; IPv4 for a v4 --addr) + mDNS.
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

    println!("commissioning… (this performs PASE → attestation → NOC → CASE)");
    let fabric = commission(&transport, &mut discovery, config)
        .await
        .context("commission() failed")?;

    print_summary(&fabric)?;

    if let Some(out) = &cli.out {
        let summary = FabricSummary::from_fabric(&fabric)?;
        let json = serde_json::to_string_pretty(&summary).context("serializing summary")?;
        std::fs::write(out, json).with_context(|| format!("writing {}", out.display()))?;
        println!("   wrote summary -> {}", out.display());
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
        let err = Cli::try_parse_from(["commission_ip", "--qr", "MT:X", "--manual", "123"]);
        assert!(err.is_err(), "--qr and --manual must be mutually exclusive");
    }

    #[test]
    fn fabric_summary_serializes() {
        let summary = FabricSummary {
            fabric_id: 1,
            compressed_fabric_id: "aabb".into(),
            peer_node_id: 2,
            peer_public_key: "04ff".into(),
            terminated_at: "Cleanup".into(),
        };
        let json = serde_json::to_string(&summary).expect("serialize");
        assert!(json.contains("\"fabric_id\":1"));
        assert!(json.contains("\"peer_node_id\":2"));
    }
}
