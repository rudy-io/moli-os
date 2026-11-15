//! M6.6.4 headline gate: the REAL `commission()` Commissioner commissions a
//! self-contained `MockDevice` end to end, hardware-free.
//!
//! This is the milestone green check. The controller half is the production
//! [`matter_commissioning::driver::commission`] driver; the device half is the
//! `support::run_mock_device` twin (`tests/support/mod.rs`). Both run under one
//! `tokio::join!` over a single [`InMemoryDatagram`] pair. No part of
//! `commission()` is mocked or stubbed — only the network transport and mDNS
//! discovery are in-process.
//!
//! ## Mutually-consistent assembly (where loopbacks live or die)
//!
//! - **VID/PID:** mock DAC + CD fixture + SetupPayload all use 0xFFF1 / 0x8001
//!   (`support::VID` / `support::PID`). The controller's `paa_trust_store` is the
//!   mock PKI's store; `cd_signing_roots` is `CdSigningRoots::with_example_device_roots()`.
//! - **Passcode / PASE:** the SetupPayload passcode, `DriverConfig.passcode`, and
//!   the mock's `pase_pin` are the SAME value. The PBKDF params (`PasePbkdfParams`)
//!   are supplied by the device verifier and negotiated to the controller's
//!   prover via PBKDFParamResponse (see `pase.rs::run_pase`), so any valid params
//!   are fine as long as both halves use the same ones.
//! - **Attestation challenge:** sourced from the LIVE PASE session on both sides
//!   (`commission()` takes it from its session; `run_mock_device` uses
//!   `pase_keys.attestation_key`).
//!   The controller's `pase_attestation_challenge` config field is overwritten by
//!   `commission()`, so we set it to `[0u8; 16]` here to prove that.
//! - **Fabric / CASE / NOC:** ONE fabric RCAC (`FabricRecord::new_root_only`).
//!   The controller commissions under it (and mints its own operational NOC under
//!   the RCAC inside `commission()`). The DEVICE's CASE NOC is minted HERE under
//!   the SAME RCAC for `ASSIGNED_NODE_ID`, with `trusted_roots` = that RCAC. CASE
//!   then validates both NOCs against the shared root.
//! - **Node ids:** `COMMISSIONER_NODE_ID` (controller) != `ASSIGNED_NODE_ID`
//!   (device); the device CASE NOC's node id == `ASSIGNED_NODE_ID` == the
//!   `EstablishCase{peer_node_id}` the Commissioner emits.
//! - **Discovery:** the `commissionable_addr` is supplied directly (no mDNS for
//!   discovery). `commission()` still browses `_matter._tcp` operational records
//!   during `EstablishCase`; `FakeDiscovery` returns `dev_addr` for the matching
//!   operational instance name (compressed-fabric-id + assigned node id).

// This integration test drives the `driver`-gated `commission()` orchestrator
// and the `support` module (which uses driver types), so it only compiles when
// the `driver` feature is on. Without it the file is empty (CI runs
// `--all-features`; plain `cargo test` then skips it cleanly).
#![cfg(feature = "driver")]
// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]
// Domain acronyms (VID, PID, PASE, CASE, NOC, RCAC, PBKDF, mDNS) are prose, not
// code items; and `commission`/`commissioner`/`commissioned` are intentionally
// related names that mirror the API surface under test.
#![allow(clippy::doc_markdown, clippy::similar_names)]

mod support;

use std::sync::Arc;

use matter_cert::{MatterTime, TrustAnchor, TrustedRoots};
use matter_commissioning::attestation::CdSigningRoots;
use matter_commissioning::driver::{
    commission, operational_instance_name, DriverConfig, InMemoryDatagram,
};
use matter_commissioning::noc::{
    issue_icac, issue_noc, FabricRecord, NocRng, SystemNocRng, VerifiedCsr,
};
use matter_commissioning::setup::{
    CommissioningFlow, DiscoveryCapabilities, Discriminator, Passcode, SetupPayload,
};
use matter_commissioning::state_machine::{CommissionerConfig, Stage};
use matter_crypto::{derive_compressed_fabric_id, CaseCredentials, RingSigner, Signer};
use matter_transport::{Discovery, MatterService, QueryHandle, ServiceKind};

use support::{build_mock_device_pki, run_mock_device, MockDeviceCaseSetup, PID, VID};

// ── Fixed run parameters ──────────────────────────────────────────────────────

/// Device passcode. The SetupPayload, the driver config, and the mock verifier
/// PIN are all this value (Matter Core Spec §5.1.7 — a SPAKE2+ passcode).
const PASSCODE: u32 = 20_202_021;

/// 12-bit commissionable discriminator. Only used to build the SetupPayload; the
/// commissionable address is supplied directly so mDNS discovery is bypassed.
const DISCRIMINATOR: u16 = 0x0F00;

/// Fabric id the controller commissions under. Both the controller's and the
/// device's operational NOCs carry this fabric id.
const FABRIC_ID: u64 = 0x0000_0000_0000_0001;

/// The controller's own operational node id on this fabric.
const COMMISSIONER_NODE_ID: u64 = 0x0000_0000_0000_0001;

/// The node id assigned to the device. The device CASE NOC's node id MUST equal
/// this, and it is the `peer_node_id` the Commissioner emits in `EstablishCase`.
const ASSIGNED_NODE_ID: u64 = 0x0000_0000_0000_0002;

/// Responder PASE session id the DEVICE advertises in PBKDFParamResponse. The
/// controller learns it via the handshake and addresses its secured PASE replies
/// to it. Mirrors the `0x00BB` the `pase.rs` loopback uses.
const PASE_RESPONDER_SESSION_ID: u16 = 0x00BB;

/// Responder CASE session id the DEVICE advertises in Sigma2. Mirrors the
/// `0x00D2` the `case.rs` `run_case` loopback uses.
const CASE_RESPONDER_SESSION_ID: u16 = 0x00D2;

/// Wall-clock anchor for the run (Unix 1_800_000_000 ≈ 2027-01-15). Matches the
/// `now()` used by the mock PKI builder so the DAC/PAI/PAA validity windows
/// bracket it.
fn now() -> MatterTime {
    MatterTime::from_unix_secs(1_800_000_000)
}

// ── FakeDiscovery (operational browse only) ────────────────────────────────────

/// Minimal [`Discovery`] that returns one canned operational record for every
/// query. `commission()` only browses discovery for the operational record
/// during `EstablishCase` (the commissionable address is supplied directly), and
/// `resolve_operational` filters by `instance_name`, so the canned record's
/// instance name is the operational instance name for our fabric + assigned node.
struct FakeDiscovery {
    service: MatterService,
}

impl Discovery for FakeDiscovery {
    fn publish(&mut self, _s: &MatterService) -> matter_transport::Result<()> {
        Ok(())
    }
    fn unpublish(&mut self, _n: &str, _k: ServiceKind) -> matter_transport::Result<()> {
        Ok(())
    }
    fn query(&mut self, _k: ServiceKind) -> matter_transport::Result<QueryHandle> {
        Ok(QueryHandle(1))
    }
    fn stop_query(&mut self, _h: QueryHandle) {}
    fn poll_results(&mut self, _h: QueryHandle) -> Vec<MatterService> {
        vec![self.service.clone()]
    }
}

/// Mint the device's operational CASE NOC under the fabric RCAC and assemble the
/// `MockDeviceCaseSetup` the responder needs.
///
/// The device generates a fresh operational keypair; we issue its NOC under the
/// same `fabric` the controller commissions under (RCAC -> NOC, no ICAC), for
/// `ASSIGNED_NODE_ID`. The responder validates the controller's NOC against the
/// fabric RCAC and vice versa.
fn build_device_case_setup(fabric: &FabricRecord, ipk_epoch_key: [u8; 16]) -> MockDeviceCaseSetup {
    let (device_op_signer, _pkcs8) = RingSigner::generate().expect("device op key");
    let device_op_pub = device_op_signer.public_key().clone();
    let verified_csr = VerifiedCsr {
        public_key: device_op_pub,
    };
    let device_noc = issue_noc(
        fabric,
        &verified_csr,
        ASSIGNED_NODE_ID,
        &[],
        (now(), MatterTime::NO_EXPIRY),
        &SystemNocRng,
    )
    .expect("device NOC issuance under fabric RCAC");

    // Like a real device, derive the *operational* IPK from the epoch key
    // AddNOC distributed (spec §4.15.2) — `CaseCredentials.ipk` is the
    // operational key the Sigma1 destination id is checked against.
    let compressed_fabric_id = matter_crypto::derive_compressed_fabric_id(
        fabric.root_public_key.as_bytes(),
        fabric.fabric_id,
    )
    .expect("compressed fabric id");
    let operational_ipk =
        matter_crypto::derive_operational_ipk(&ipk_epoch_key, &compressed_fabric_id)
            .expect("operational IPK derivation");

    let credentials = CaseCredentials {
        noc: device_noc,
        icac: fabric.icac_cert.clone(),
        signer: Box::new(device_op_signer),
        fabric_id: fabric.fabric_id,
        node_id: ASSIGNED_NODE_ID,
        ipk: operational_ipk,
        rcac_public_key: *fabric.root_public_key.as_bytes(),
    };

    let mut trusted_roots = TrustedRoots::new();
    trusted_roots.add(TrustAnchor::from_root_cert(&fabric.root_cert));

    MockDeviceCaseSetup {
        credentials,
        trusted_roots,
        responder_session_id: CASE_RESPONDER_SESSION_ID,
        // Validate the controller's NOC at the same instant the commissioner
        // mints it (its `not_before`), so the chain is within its window.
        now: now(),
    }
}

// ── Shared loopback helper ─────────────────────────────────────────────────────

/// Run the full mock-device loopback commission (the M6.6.4 gate),
/// returning normally on success. Shared by the headline test and the
/// wiretrace capture test.
#[allow(clippy::too_many_lines)] // Commissioning setup is inherently verbose; extracting sub-functions would obscure the mutually-consistent assembly.
async fn run_loopback_commission() {
    // ── 1. Mock-device PKI: PAA/PAI/DAC chain + the PAA trust store the
    //       controller validates the device attestation against. ──────────────
    let mock_pki = build_mock_device_pki(now());

    // ── 2. One fabric RCAC the controller commissions under. ──────────────────
    let (root_signer, _pkcs8) = RingSigner::generate().expect("fabric root key");
    let root_signer: Arc<dyn Signer> = Arc::new(root_signer);
    let fabric = FabricRecord::new_root_only(
        FABRIC_ID,
        root_signer,
        now(),
        MatterTime::NO_EXPIRY,
        1, // rcac_id
        &SystemNocRng,
    )
    .expect("fabric RCAC");

    // ── 3. Setup payload (VID/PID/passcode/discriminator) matching the mock. ──
    let setup = SetupPayload {
        version: 0,
        vendor_id: Some(VID),
        product_id: Some(PID),
        commissioning_flow: CommissioningFlow::Standard,
        discovery_capabilities: DiscoveryCapabilities::ON_NETWORK,
        discriminator: Discriminator::new(DISCRIMINATOR).unwrap(),
        passcode: Passcode::new(PASSCODE).unwrap(),
    };

    let cd_signing_roots = CdSigningRoots::with_example_device_roots();
    let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);

    // ── 4. Transport pair + discovery returning the operational record. ───────
    let (ctrl_io, dev_io) = InMemoryDatagram::pair();
    let dev_addr = dev_io.local_addr();
    let ctrl_addr = ctrl_io.local_addr();

    // The operational instance name the controller's resolve_operational expects:
    // compressed-fabric-id(root_public_key, fabric_id) + assigned node id.
    let compressed = derive_compressed_fabric_id(fabric.root_public_key.as_bytes(), FABRIC_ID)
        .expect("compressed fabric id");
    let op_instance_name = operational_instance_name(compressed, ASSIGNED_NODE_ID);
    let mut fake_disc = FakeDiscovery {
        service: MatterService::new(
            op_instance_name,
            ServiceKind::Operational,
            vec![dev_addr.ip()],
            dev_addr.port(),
            std::collections::HashMap::new(),
        ),
    };

    // ── 5. CommissionerConfig + DriverConfig (the controller under test). ─────
    //
    // pase_attestation_challenge is intentionally [0u8; 16]: commission()
    // overrides it with the live PASE-derived attestation_key, so the value
    // here is irrelevant — proving commission() sources the challenge from the
    // session.
    let commissioner = CommissionerConfig {
        pase_attestation_challenge: [0u8; 16],
        fabric: &fabric,
        setup_payload: &setup,
        paa_trust_store: &mock_pki.paa_trust_store,
        cd_signing_roots: &cd_signing_roots,
        commissioner_node_id: COMMISSIONER_NODE_ID,
        assigned_node_id: ASSIGNED_NODE_ID,
        ipk_epoch_key: [0x42_u8; 16],
        case_admin_subject: COMMISSIONER_NODE_ID,
        admin_vendor_id: VID,
        now: now(),
        rng,
        network: matter_commissioning::NetworkCredentials::AlreadyOnNetwork, // Ethernet path
    };
    // Persistent commissioner operational identity (replaces the former
    // per-call throwaway mint inside commission()).
    let (commissioner_signer, commissioner_pkcs8) =
        matter_crypto::RingSigner::generate().expect("commissioner keypair");
    let commissioner_noc = matter_commissioning::issue_noc(
        &fabric,
        &matter_commissioning::VerifiedCsr {
            public_key: matter_crypto::CaseSigner::public_key(&commissioner_signer).clone(),
        },
        COMMISSIONER_NODE_ID,
        &[],
        (now(), matter_cert::MatterTime::NO_EXPIRY),
        &matter_commissioning::SystemNocRng,
    )
    .expect("commissioner NOC");

    let config = DriverConfig {
        commissioner,
        commissionable_addr: Some(dev_addr),
        passcode: PASSCODE,
        commissioner_noc: &commissioner_noc,
        commissioner_signer_pkcs8: &commissioner_pkcs8,
    };

    // ── 6. Device CASE setup (NOC under the SAME fabric RCAC; operational IPK
    //       derived from the SAME epoch key the config distributes). ──────────
    let case_setup = build_device_case_setup(&fabric, [0x42_u8; 16]);

    // ── 7. Run BOTH halves concurrently: the real commission() driver against
    //       the mock device. ────────────────────────────────────────────────
    let device = run_mock_device(
        &dev_io,
        ctrl_addr, // replies go back to the controller's loopback endpoint
        &mock_pki,
        PASSCODE,
        matter_crypto::pase::PasePbkdfParams {
            iterations: 1000,
            salt: vec![0x55; 16],
        },
        PASE_RESPONDER_SESSION_ID,
        case_setup,
    );

    let (commission_result, device_result) =
        tokio::join!(commission(&ctrl_io, &mut fake_disc, config), device);

    // ── 8. Assert the green check. ────────────────────────────────────────────
    let device_outcome = device_result.expect("mock device side completed without error");
    assert!(
        device_outcome.complete_response_acked,
        "the controller must ACK the CommissioningCompleteResponse before commission() returns"
    );
    assert!(
        device_outcome.case_session_closed,
        "after the ACK, the controller must close its CASE session with a CloseSession \
         (new exchange, R clear) before commission() returns"
    );
    let commissioned = commission_result.expect("commission() reached Done");

    assert_eq!(
        commissioned.terminated_at,
        Stage::Cleanup,
        "commissioning must terminate at the Cleanup stage"
    );
    assert_eq!(
        commissioned.peer_node_id, ASSIGNED_NODE_ID,
        "commissioned peer node id must equal the assigned node id"
    );
    assert_eq!(
        commissioned.fabric.fabric_id, FABRIC_ID,
        "commissioned fabric id must match the fabric we commissioned under"
    );
}

// ── Headline gate ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn commission_reaches_done_against_mock_device() {
    run_loopback_commission().await;
}

// ── ICAC loopback gate ─────────────────────────────────────────────────────────

/// Sibling of [`run_loopback_commission`]: mints a 3-tier-chain ICAC and
/// attaches it to the fabric BEFORE any NOC issuance, then runs the same
/// commission loopback. `FabricRecord::issue_noc`/the free `issue_noc`
/// function both consult `fabric.icac_signer`/`fabric.icac_cert` and sign
/// under the ICAC whenever both are present (see
/// `noc::issuer::issue_noc_signs_under_icac_when_present`), so once the
/// fabric carries an ICAC, both the commissioner's own operational NOC and
/// the device's CASE NOC (minted in `build_device_case_setup`, which already
/// threads `icac: fabric.icac_cert.clone()` into `CaseCredentials`) are
/// signed under the ICAC automatically — no other code path changes.
///
/// This asserts the same success criteria as the flat gate
/// (`terminated_at == Cleanup`, correct peer node id and fabric id, CASE
/// therefore established) plus that the ICAC actually threaded through: the
/// device's `CaseCredentials.icac` is `Some`, and the device NOC's issuer DN
/// equals the ICAC's subject DN (not the RCAC's) — the same negative-control
/// shape as `case_establishes_over_three_tier_chain` in
/// `matter-crypto/tests/case_roundtrip.rs`.
#[allow(clippy::too_many_lines)] // mirrors run_loopback_commission's mutually-consistent assembly
#[tokio::test]
async fn run_loopback_commission_with_icac() {
    // ── 1. Mock-device PKI (identical to the flat gate). ───────────────────────
    let mock_pki = build_mock_device_pki(now());

    // ── 2. Fabric RCAC, then an ICAC minted under it and attached BEFORE any
    //       NOC issuance so every NOC minted from here on signs under the ICAC. ─
    let (root_signer, _pkcs8) = RingSigner::generate().expect("fabric root key");
    let root_signer: Arc<dyn Signer> = Arc::new(root_signer);
    let mut fabric = FabricRecord::new_root_only(
        FABRIC_ID,
        root_signer,
        now(),
        MatterTime::NO_EXPIRY,
        1, // rcac_id
        &SystemNocRng,
    )
    .expect("fabric RCAC");

    let (icac_signer, _icac_pkcs8) = RingSigner::generate().expect("icac key");
    let icac_public_key = icac_signer.public_key().clone();
    let icac_cert = issue_icac(
        &fabric,
        1, // icac_id
        &icac_public_key,
        (now(), MatterTime::NO_EXPIRY),
        &SystemNocRng,
    )
    .expect("issue icac under fabric RCAC"); // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    let icac_subject = icac_cert.subject().clone();

    fabric.icac_signer = Some(Arc::new(icac_signer));
    fabric.icac_cert = Some(icac_cert);

    // ── 3. Setup payload (identical to the flat gate). ─────────────────────────
    let setup = SetupPayload {
        version: 0,
        vendor_id: Some(VID),
        product_id: Some(PID),
        commissioning_flow: CommissioningFlow::Standard,
        discovery_capabilities: DiscoveryCapabilities::ON_NETWORK,
        discriminator: Discriminator::new(DISCRIMINATOR).unwrap(),
        passcode: Passcode::new(PASSCODE).unwrap(),
    };

    let cd_signing_roots = CdSigningRoots::with_example_device_roots();
    let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);

    // ── 4. Transport pair + discovery returning the operational record. ────────
    let (ctrl_io, dev_io) = InMemoryDatagram::pair();
    let dev_addr = dev_io.local_addr();
    let ctrl_addr = ctrl_io.local_addr();

    let compressed = derive_compressed_fabric_id(fabric.root_public_key.as_bytes(), FABRIC_ID)
        .expect("compressed fabric id");
    let op_instance_name = operational_instance_name(compressed, ASSIGNED_NODE_ID);
    let mut fake_disc = FakeDiscovery {
        service: MatterService::new(
            op_instance_name,
            ServiceKind::Operational,
            vec![dev_addr.ip()],
            dev_addr.port(),
            std::collections::HashMap::new(),
        ),
    };

    // ── 5. CommissionerConfig + DriverConfig. ───────────────────────────────────
    let commissioner = CommissionerConfig {
        pase_attestation_challenge: [0u8; 16],
        fabric: &fabric,
        setup_payload: &setup,
        paa_trust_store: &mock_pki.paa_trust_store,
        cd_signing_roots: &cd_signing_roots,
        commissioner_node_id: COMMISSIONER_NODE_ID,
        assigned_node_id: ASSIGNED_NODE_ID,
        ipk_epoch_key: [0x42_u8; 16],
        case_admin_subject: COMMISSIONER_NODE_ID,
        admin_vendor_id: VID,
        now: now(),
        rng,
        network: matter_commissioning::NetworkCredentials::AlreadyOnNetwork,
    };
    // Persistent commissioner operational identity — `issue_noc` below signs
    // under the ICAC automatically since `fabric.icac_signer`/`icac_cert` are
    // both `Some` at this point.
    let (commissioner_signer, commissioner_pkcs8) =
        matter_crypto::RingSigner::generate().expect("commissioner keypair");
    let commissioner_noc = matter_commissioning::issue_noc(
        &fabric,
        &matter_commissioning::VerifiedCsr {
            public_key: matter_crypto::CaseSigner::public_key(&commissioner_signer).clone(),
        },
        COMMISSIONER_NODE_ID,
        &[],
        (now(), matter_cert::MatterTime::NO_EXPIRY),
        &matter_commissioning::SystemNocRng,
    )
    .expect("commissioner NOC");

    let config = DriverConfig {
        commissioner,
        commissionable_addr: Some(dev_addr),
        passcode: PASSCODE,
        commissioner_noc: &commissioner_noc,
        commissioner_signer_pkcs8: &commissioner_pkcs8,
    };

    // ── 6. Device CASE setup — `build_device_case_setup` already threads
    //       `icac: fabric.icac_cert.clone()` into `CaseCredentials` and mints
    //       the device NOC via `issue_noc(fabric, ...)`, which now signs under
    //       the ICAC we just attached. Capture the issuer DN and the
    //       `icac.is_some()` flag BEFORE `case_setup` is moved into
    //       `run_mock_device` below. ──────────────────────────────────────────
    let case_setup = build_device_case_setup(&fabric, [0x42_u8; 16]);
    let device_noc_issuer = case_setup.credentials.noc.issuer().clone();
    let device_credentials_has_icac = case_setup.credentials.icac.is_some();

    // ── 7. Run both halves concurrently. ────────────────────────────────────
    let device = run_mock_device(
        &dev_io,
        ctrl_addr,
        &mock_pki,
        PASSCODE,
        matter_crypto::pase::PasePbkdfParams {
            iterations: 1000,
            salt: vec![0x55; 16],
        },
        PASE_RESPONDER_SESSION_ID,
        case_setup,
    );

    let (commission_result, device_result) =
        tokio::join!(commission(&ctrl_io, &mut fake_disc, config), device);

    // ── 8. Assert the same success criteria as the flat gate. ──────────────────
    let device_outcome = device_result.expect("mock device side completed without error");
    assert!(
        device_outcome.complete_response_acked,
        "the controller must ACK the CommissioningCompleteResponse before commission() returns"
    );
    assert!(
        device_outcome.case_session_closed,
        "after the ACK, the controller must close its CASE session with a CloseSession \
         (new exchange, R clear) before commission() returns"
    );
    let commissioned = commission_result.expect("commission() reached Done");

    assert_eq!(
        commissioned.terminated_at,
        Stage::Cleanup,
        "commissioning must terminate at the Cleanup stage"
    );
    assert_eq!(
        commissioned.peer_node_id, ASSIGNED_NODE_ID,
        "commissioned peer node id must equal the assigned node id"
    );
    assert_eq!(
        commissioned.fabric.fabric_id, FABRIC_ID,
        "commissioned fabric id must match the fabric we commissioned under"
    );

    // ── 9. ICAC-specific assertions: the ICAC actually threaded through. ───────
    assert!(
        commissioned.fabric.icac_cert.is_some(),
        "the fabric returned by commission() must still carry the ICAC"
    );
    assert!(
        device_credentials_has_icac,
        "device-side CaseCredentials.icac must be Some once the fabric carries an ICAC"
    );
    assert_eq!(
        device_noc_issuer, icac_subject,
        "device NOC issuer must be the ICAC subject, proving the NOC was signed under \
         the ICAC and not the RCAC"
    );
}

/// Install, once per process, a trivial global subscriber that reports
/// `Sometimes` interest for every callsite, so a test's thread-local
/// `set_default` subscriber receives events. See the note on
/// `loopback_commission_emits_wire_trace` for why this is needed.
#[cfg(feature = "tracing")]
fn prime_callsite_interest() {
    use std::sync::OnceLock;

    // Install a trivial global subscriber that reports `Sometimes` interest
    // for all callsites. This primes `tracing-core`'s callsite-interest cache
    // so that events flow through `get_default` (which dispatches to the
    // test's thread-local subscriber). Without this, thread-local-only subscribers
    // do not participate in the global `DISPATCHERS` registry, causing all
    // callsite interests to be permanently cached as `Never` at first use.
    static GLOBAL_INIT: OnceLock<()> = OnceLock::new();
    GLOBAL_INIT.get_or_init(|| {
        // A minimal `Subscriber` that tells every callsite it's `Sometimes`
        // interesting. The actual per-thread dispatch goes to whatever
        // `set_default` subscriber is active on that thread.
        struct AlwaysSometimes;
        impl tracing::Subscriber for AlwaysSometimes {
            fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
                true
            }
            fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
                tracing::span::Id::from_u64(1)
            }
            fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
            fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
            fn event(&self, _: &tracing::Event<'_>) {}
            fn enter(&self, _: &tracing::span::Id) {}
            fn exit(&self, _: &tracing::span::Id) {}
            fn register_callsite(
                &self,
                _: &'static tracing::Metadata<'static>,
            ) -> tracing::subscriber::Interest {
                tracing::subscriber::Interest::sometimes()
            }
        }
        // Ignore error: if another test already installed a global subscriber
        // (not expected in this binary, but possible in theory), the existing
        // subscriber is fine as long as it reports `Sometimes` interest.
        let _ = tracing::subscriber::set_global_default(AlwaysSometimes);
    });
}

// ── Wire-trace capture test ────────────────────────────────────────────────────

/// M6 cross-verification: the same loopback run, captured through
/// `JsonlLayer`, must produce a well-formed JSONL dialogue with the
/// expected message sequence.
///
/// ## Subscriber installation note
///
/// `tracing::subscriber::set_default` is thread-local, which is the right
/// tool here: `#[tokio::test]` uses a current-thread runtime so every future
/// in `run_loopback_commission().await` is polled on the same OS thread where
/// the guard was installed.
///
/// However, `tracing-core` caches callsite *interest* (Never / Sometimes /
/// Always) at first use by consulting only **globally-registered** dispatchers
/// (`DISPATCHERS`). A thread-local subscriber installed via `set_default` is
/// NOT in `DISPATCHERS`, so all callsites whose first `interest()` call fires
/// while only a thread-local subscriber is active get permanently cached as
/// `Interest::Never` — silencing every subsequent event on that callsite for
/// the process lifetime.
///
/// Fix: before running the loopback we install a **trivial global subscriber**
/// once (via `OnceLock`) whose sole job is to tell tracing "this callsite is
/// `Sometimes` interesting", triggering a `rebuild_interest_cache` that
/// ensures events flow through `get_default`. The actual per-test capture is
/// still done via `set_default` (thread-local), so events from the concurrent
/// baseline test — which runs on a different OS thread — go to the global
/// subscriber (effectively discarded) while our thread's events go to our
/// `JsonlLayer`.
///
/// This workaround can be deleted if/when `tracing-core` lets thread-local-only
/// subscribers participate in callsite-interest rebuilds, or if this test moves
/// to its own binary (where no global subscriber races against the first callsite
/// registration).
#[cfg(all(feature = "tracing", feature = "wiretrace"))]
#[tokio::test]
async fn loopback_commission_emits_wire_trace() {
    use std::sync::{Arc, Mutex};

    use matter_commissioning::wiretrace::JsonlLayer;
    use tracing_subscriber::layer::SubscriberExt as _;

    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    prime_callsite_interest();

    let buf = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(JsonlLayer::new(SharedBuf(buf.clone())));
    // set_default installs our JsonlLayer as the thread-local dispatcher.
    // Because `AlwaysSometimes` above primed all callsite interests to
    // `Sometimes`, events reach `get_default` which dispatches to our
    // thread-local subscriber. The concurrent baseline test's events on its
    // own thread go to `get_default` on that thread, which (absent a
    // thread-local subscriber there) falls back to `AlwaysSometimes` whose
    // `event()` is a no-op.
    let guard = tracing::subscriber::set_default(subscriber);
    run_loopback_commission().await;
    drop(guard);

    let text = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    let records: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad JSONL line {l:?}: {e}")))
        .collect();
    assert!(
        !records.is_empty(),
        "no wire events captured — has the AlwaysSometimes interest-priming regressed?"
    );

    // seq strictly increasing from 0.
    for (i, r) in records.iter().enumerate() {
        assert_eq!(r["seq"], i as u64, "record {i} seq out of order");
    }

    // The dialogue opens with PBKDFParamRequest (tx, unsecured, SC 0x20).
    let first = records
        .iter()
        .find(|r| r["opcode"] != 0x10) // skip any leading standalone ack
        .expect("a non-ack first message");
    assert_eq!(first["dir"], "tx");
    assert_eq!(first["session_id"], 0);
    assert_eq!(first["protocol"], 0);
    assert_eq!(first["opcode"], 0x20);

    // PASE handshake opcodes all present, in order, on the unsecured session.
    let sc_unsecured: Vec<u64> = records
        .iter()
        .filter(|r| r["session_id"] == 0 && r["protocol"] == 0 && r["opcode"] != 0x10)
        .map(|r| r["opcode"].as_u64().unwrap())
        .collect();
    let pase: Vec<u64> = sc_unsecured
        .iter()
        .copied()
        .filter(|op| (0x20..=0x24).contains(op))
        .collect();
    assert_eq!(pase, vec![0x20, 0x21, 0x22, 0x23, 0x24], "PASE sequence");

    // CASE sigma exchange present.
    let sigmas: Vec<u64> = sc_unsecured
        .iter()
        .copied()
        .filter(|op| (0x30..=0x32).contains(op))
        .collect();
    assert_eq!(sigmas, vec![0x30, 0x31, 0x32], "CASE sigma sequence");

    // Secured IM traffic exists on at least two distinct secured sessions
    // (PASE-encrypted stages, then CASE-encrypted CommissioningComplete).
    let secured_sessions: std::collections::BTreeSet<u64> = records
        .iter()
        .filter(|r| r["session_id"] != 0 && r["protocol"] == 1)
        .map(|r| r["session_id"].as_u64().unwrap())
        .collect();
    assert!(
        secured_sessions.len() >= 2,
        "expected PASE + CASE secured sessions, got {secured_sessions:?}"
    );

    // The dialogue ends with our CloseSession: the last record that is not a
    // standalone ack is a tx Secure Channel StatusReport on a secured session.
    let last = records
        .iter()
        .rev()
        .find(|r| r["opcode"] != 0x10)
        .expect("a last non-ack record");
    assert_eq!(last["dir"], "tx");
    assert_eq!(last["protocol"], 0);
    assert_eq!(last["opcode"], 0x40);
    assert_ne!(last["session_id"], 0);
}

// ── No payload bytes under plain `tracing` ────────────────────────────────────

/// The loopback run's IPK epoch key (`[0x42; 16]`, see
/// `run_loopback_commission`) as the lowercase hex the driver's dumps use.
/// `AddNOC` carries it in plaintext inside the PASE session, so it appears in
/// a wire dump exactly when that dump records the command's payload.
#[cfg(feature = "tracing")]
const IPK_HEX: &str = "42424242424242424242424242424242";

/// Every event the run emits, as `(target, [(field, rendered value)])`.
#[cfg(feature = "tracing")]
type Captured = Vec<(String, Vec<(String, String)>)>;

/// Runs the loopback commission under a thread-local subscriber that records
/// every field of every event, from any target, at any level.
#[cfg(feature = "tracing")]
async fn capture_loopback_events() -> Captured {
    use std::sync::{Arc, Mutex};

    use tracing::field::{Field, Visit};
    use tracing_subscriber::layer::{Context, SubscriberExt as _};
    use tracing_subscriber::Layer;

    struct Fields(Vec<(String, String)>);
    impl Visit for Fields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0.push((field.name().to_owned(), format!("{value:?}")));
        }
    }

    struct CaptureAll(Arc<Mutex<Captured>>);
    impl<S: tracing::Subscriber> Layer<S> for CaptureAll {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let mut fields = Fields(Vec::new());
            event.record(&mut fields);
            self.0
                .lock()
                .unwrap()
                .push((event.metadata().target().to_owned(), fields.0));
        }
    }

    prime_callsite_interest();
    let events = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(CaptureAll(events.clone()));
    let guard = tracing::subscriber::set_default(subscriber);
    run_loopback_commission().await;
    drop(guard);
    let captured = events.lock().unwrap().clone();
    captured
}

/// With `tracing` on and `wiretrace` off, no event carries a message payload:
/// the `matter_wire` events keep their direction, session, exchange, protocol
/// and opcode, but neither they nor the driver's other debug events record the
/// plaintext bytes. Checked two ways: the IPK that `AddNOC` carries appears in
/// no field of any event, and no event records a payload-dump field at all
/// (which also covers the inbound direction, whose payloads hold no marker,
/// and the raw packet head the unsecured receive loop used to dump).
#[cfg(all(feature = "tracing", not(feature = "wiretrace")))]
#[tokio::test]
async fn tracing_without_wiretrace_logs_no_payload_bytes() {
    let events = capture_loopback_events().await;

    let wire: Vec<&Vec<(String, String)>> = events
        .iter()
        .filter(|(target, _)| target == "matter_wire")
        .map(|(_, fields)| fields)
        .collect();
    assert!(
        !wire.is_empty(),
        "no matter_wire events captured — has the interest priming regressed?"
    );
    for fields in &wire {
        for name in ["dir", "session_id", "exchange_id", "protocol", "opcode"] {
            assert!(
                fields.iter().any(|(n, _)| n == name),
                "a matter_wire event lost its {name} field"
            );
        }
    }

    for (target, fields) in &events {
        for (name, value) in fields {
            assert!(
                !value.contains(IPK_HEX),
                "event on {target} leaked the IPK in field {name}"
            );
            assert!(
                !matches!(
                    name.as_str(),
                    "payload" | "wire" | "report_data_tlv" | "head"
                ),
                "event on {target} recorded a payload dump in field {name}"
            );
        }
    }
}

/// Positive control for the test above: with `wiretrace` on, the same capture
/// DOES see the IPK in a `matter_wire` payload. This proves the capture reaches
/// the driver's events and the marker is really on the wire path, so the
/// absence check above cannot pass vacuously. It also pins that `wiretrace`
/// still records full payloads for `JsonlLayer` / `cargo xtask trace-diff`.
#[cfg(feature = "wiretrace")]
#[tokio::test]
async fn wiretrace_logs_the_payload_bytes() {
    let events = capture_loopback_events().await;
    assert!(
        events.iter().any(|(target, fields)| {
            target == "matter_wire"
                && fields
                    .iter()
                    .any(|(n, v)| n == "payload" && v.contains(IPK_HEX))
        }),
        "wiretrace should record the AddNOC payload, IPK included"
    );
}
