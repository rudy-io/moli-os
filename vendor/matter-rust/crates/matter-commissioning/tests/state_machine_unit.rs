//! M6.4.1 — public-API integration tests for the commissioning state
//! machine's negative-path matrix.
//!
//! Each test exercises a single failure mode and asserts the right
//! `CommissioningError` variant + post-failure cursor position via the
//! public re-exports in `matter_commissioning::state_machine`.

// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use matter_cert::time::MatterTime;
use matter_commissioning::attestation::CdSigningRoots;
use matter_commissioning::noc::{FabricRecord, NocRng, SystemNocRng};
use matter_commissioning::setup::{
    CommissioningFlow, DiscoveryCapabilities, Discriminator, Passcode, SetupPayload,
};
use matter_commissioning::state_machine::{
    Action, Commissioner, CommissionerConfig, CommissioningError, Expectation, Stage,
};
use matter_commissioning::PaaTrustStore;
use matter_crypto::{RingSigner, Signer};

fn make_fabric() -> FabricRecord {
    let (signer, _pkcs8) = RingSigner::generate().expect("ring keypair");
    let signer: Arc<dyn Signer> = Arc::new(signer);
    let rng = SystemNocRng;
    FabricRecord::new_root_only(
        0x0000_0000_0000_0001,
        signer,
        MatterTime::from_unix_secs(1_704_067_200),
        MatterTime::from_unix_secs(1_735_689_600),
        0xDEAD_BEEF_CAFE_F00D,
        &rng,
    )
    .expect("valid root fabric")
}

fn make_setup() -> SetupPayload {
    SetupPayload {
        version: 0,
        vendor_id: Some(0xFFF1),
        product_id: Some(0x8000),
        commissioning_flow: CommissioningFlow::Standard,
        discovery_capabilities: DiscoveryCapabilities::ON_NETWORK,
        discriminator: Discriminator::new(0x0F00).expect("valid discriminator"),
        passcode: Passcode::new(20_202_021).expect("valid passcode"),
    }
}

fn build_sm(
    fabric: &FabricRecord,
    setup: &SetupPayload,
    paa: &PaaTrustStore,
    cd: &CdSigningRoots,
) -> Commissioner {
    let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
    let cfg = CommissionerConfig {
        pase_attestation_challenge: [0u8; 16],
        fabric,
        setup_payload: setup,
        paa_trust_store: paa,
        cd_signing_roots: cd,
        commissioner_node_id: 0x1,
        assigned_node_id: 0x2,
        ipk_epoch_key: [0x42_u8; 16],
        case_admin_subject: 0x1,
        admin_vendor_id: 0xFFF1,
        now: MatterTime::from_unix_secs(1_704_067_200),
        rng,
        network: matter_commissioning::NetworkCredentials::AlreadyOnNetwork,
    };
    Commissioner::new(cfg).expect("valid config")
}

#[test]
fn arm_failsafe_returns_busy() {
    let fabric = make_fabric();
    let setup = make_setup();
    let paa = PaaTrustStore::with_example_device_roots();
    let cd = CdSigningRoots::with_example_device_roots();
    let mut sm = build_sm(&fabric, &setup, &paa, &cd);
    let _ = sm.poll().unwrap();
    sm.on_response(Expectation::CommissioningInfo, &[0x15, 0x18])
        .unwrap();
    let _ = sm.poll().unwrap();
    let err = sm
        .on_response(
            Expectation::ArmFailsafeResponse,
            &[0x15, 0x24, 0x00, 0x04, 0x18], // error_code = 4 (busy)
        )
        .unwrap_err();
    assert!(matches!(
        err,
        CommissioningError::DeviceImStatus {
            stage: Stage::ArmFailsafe,
            im_status: 4,
        }
    ));
    assert_eq!(sm.stage(), Stage::Failed);
}

#[test]
fn arm_failsafe_response_is_malformed() {
    let fabric = make_fabric();
    let setup = make_setup();
    let paa = PaaTrustStore::with_example_device_roots();
    let cd = CdSigningRoots::with_example_device_roots();
    let mut sm = build_sm(&fabric, &setup, &paa, &cd);
    let _ = sm.poll().unwrap();
    sm.on_response(Expectation::CommissioningInfo, &[0x15, 0x18])
        .unwrap();
    let _ = sm.poll().unwrap();
    let err = sm
        .on_response(Expectation::ArmFailsafeResponse, &[0xFF])
        .unwrap_err();
    assert!(matches!(
        err,
        CommissioningError::MalformedResponse(Stage::ArmFailsafe)
    ));
    assert_eq!(sm.stage(), Stage::Failed);
}

#[test]
fn config_regulatory_returns_value_outside_range() {
    let fabric = make_fabric();
    let setup = make_setup();
    let paa = PaaTrustStore::with_example_device_roots();
    let cd = CdSigningRoots::with_example_device_roots();
    let mut sm = build_sm(&fabric, &setup, &paa, &cd);
    let _ = sm.poll().unwrap();
    sm.on_response(Expectation::CommissioningInfo, &[0x15, 0x18])
        .unwrap();
    let _ = sm.poll().unwrap();
    sm.on_response(
        Expectation::ArmFailsafeResponse,
        &[0x15, 0x24, 0x00, 0x00, 0x18],
    )
    .unwrap();
    let _ = sm.poll().unwrap();
    let err = sm
        .on_response(
            Expectation::SetRegulatoryConfigResponse,
            &[0x15, 0x24, 0x00, 0x02, 0x18], // error_code = 2 (ValueOutsideRange)
        )
        .unwrap_err();
    assert!(matches!(
        err,
        CommissioningError::DeviceImStatus {
            stage: Stage::ConfigRegulatory,
            im_status: 2,
        }
    ));
    assert_eq!(sm.stage(), Stage::Failed);
}

#[test]
fn failed_stage_subsequent_poll_emits_abort() {
    let fabric = make_fabric();
    let setup = make_setup();
    let paa = PaaTrustStore::with_example_device_roots();
    let cd = CdSigningRoots::with_example_device_roots();
    let mut sm = build_sm(&fabric, &setup, &paa, &cd);
    let _ = sm.poll().unwrap();
    let _ = sm
        .on_response(Expectation::CommissioningInfo, &[0xFF])
        .unwrap_err();
    assert_eq!(sm.stage(), Stage::Failed);
    match sm.poll().unwrap() {
        Action::Abort {
            send_disarm_failsafe,
            reason: _,
        } => {
            assert!(send_disarm_failsafe);
        }
        other => panic!("expected Abort, got {other:?}"),
    }
}

#[test]
fn unexpected_response_kind_after_arm_failsafe_poll() {
    let fabric = make_fabric();
    let setup = make_setup();
    let paa = PaaTrustStore::with_example_device_roots();
    let cd = CdSigningRoots::with_example_device_roots();
    let mut sm = build_sm(&fabric, &setup, &paa, &cd);
    let _ = sm.poll().unwrap();
    sm.on_response(Expectation::CommissioningInfo, &[0x15, 0x18])
        .unwrap();
    let _ = sm.poll().unwrap();
    let err = sm
        .on_response(Expectation::AttestationResponse, &[])
        .unwrap_err();
    assert!(matches!(
        err,
        CommissioningError::UnexpectedResponseKind {
            expected: Expectation::ArmFailsafeResponse,
            got: Expectation::AttestationResponse,
        }
    ));
    // Wrong-kind does NOT transition to Failed.
    assert_eq!(sm.stage(), Stage::ArmFailsafe);
}

#[test]
fn tampered_pai_der_returns_attestation_error() {
    use matter_codec::{Tag, TlvWriter};

    fn wrap_octet_string_in_anonymous_struct(payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).expect("infallible");
        w.put_bytes(Tag::Context(0), payload).expect("infallible");
        w.end_container().expect("infallible");
        buf
    }

    fn synthetic_attestation_response(elements: &[u8], sig: &[u8; 64]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).expect("infallible");
        w.put_bytes(Tag::Context(0), elements).expect("infallible");
        w.put_bytes(Tag::Context(1), sig).expect("infallible");
        w.end_container().expect("infallible");
        buf
    }

    let fabric = make_fabric();
    let setup = make_setup();
    let paa = PaaTrustStore::with_example_device_roots();
    let cd = CdSigningRoots::with_example_device_roots();
    let mut sm = build_sm(&fabric, &setup, &paa, &cd);

    // Drive through ReadCommissioningInfo → ArmFailsafe → ConfigRegulatory.
    let _ = sm.poll().unwrap();
    sm.on_response(Expectation::CommissioningInfo, &[0x15, 0x18])
        .unwrap();
    let _ = sm.poll().unwrap();
    sm.on_response(
        Expectation::ArmFailsafeResponse,
        &[0x15, 0x24, 0x00, 0x00, 0x18],
    )
    .unwrap();
    let _ = sm.poll().unwrap();
    sm.on_response(
        Expectation::SetRegulatoryConfigResponse,
        &[0x15, 0x24, 0x00, 0x00, 0x18],
    )
    .unwrap();

    // SendPaiCertRequest — feed bogus DER bytes (not a valid X.509 cert).
    let _ = sm.poll().unwrap();
    let bogus_chain_response = wrap_octet_string_in_anonymous_struct(&[0x00, 0x01, 0x02]);
    sm.on_response(Expectation::PaiCertChainResponse, &bogus_chain_response)
        .unwrap();

    // SendDacCertRequest — feed the same bogus DER so we reach
    // AttestationVerification.
    let _ = sm.poll().unwrap();
    sm.on_response(Expectation::DacCertChainResponse, &bogus_chain_response)
        .unwrap();

    // SendAttestationRequest — feed a synthetic AttestationResponse.
    let _ = sm.poll().unwrap();
    let synthetic = synthetic_attestation_response(&[0x15, 0x18], &[0u8; 64]);
    sm.on_response(Expectation::AttestationResponse, &synthetic)
        .unwrap();

    // AttestationVerification — verifier rejects on bogus PAI/DAC.
    let err = sm.poll().expect_err("verifier should reject bogus PAI DER");
    assert!(
        matches!(err, CommissioningError::Attestation(_)),
        "expected Attestation(_), got {err:?}"
    );
    assert_eq!(sm.stage(), Stage::Failed);
}

use proptest::prelude::*;

fn any_expectation() -> impl Strategy<Value = Expectation> {
    prop_oneof![
        Just(Expectation::CommissioningInfo),
        Just(Expectation::ArmFailsafeResponse),
        Just(Expectation::SetRegulatoryConfigResponse),
        Just(Expectation::PaiCertChainResponse),
        Just(Expectation::DacCertChainResponse),
        Just(Expectation::AttestationResponse),
        Just(Expectation::CsrResponse),
        Just(Expectation::AddTrustedRootResponse),
        Just(Expectation::NocResponse),
        Just(Expectation::CommissioningCompleteResponse),
        Just(Expectation::NetworkCommissioningInfo),
        Just(Expectation::NetworkConfigResponse),
        Just(Expectation::ConnectNetworkResponse),
        Just(Expectation::CaseFailed),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// Calling `on_response` with any random `Expectation` + payload at
    /// any point in the state machine must never panic — it must always
    /// return either `Ok` or a typed `CommissioningError`.
    #[test]
    fn on_response_never_panics(
        exp in any_expectation(),
        payload in prop::collection::vec(any::<u8>(), 0..32),
    ) {
        let fabric = make_fabric();
        let setup = make_setup();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let mut sm = build_sm(&fabric, &setup, &paa, &cd);
        // Poll once to put the state machine into a "waiting for response" position.
        let _ = sm.poll();
        let _ = sm.on_response(exp, &payload);
    }

    /// Calling `poll()` at any point — including after a random
    /// `on_response` — must never panic.
    #[test]
    fn poll_never_panics(
        exp in any_expectation(),
        payload in prop::collection::vec(any::<u8>(), 0..32),
    ) {
        let fabric = make_fabric();
        let setup = make_setup();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let mut sm = build_sm(&fabric, &setup, &paa, &cd);
        let _ = sm.poll();
        let _ = sm.on_response(exp, &payload);
        let _ = sm.poll();
    }

    /// For every `(Stage, Expectation, payload)` combination
    /// the state machine handles cleanly: returns `Ok` or a typed
    /// `CommissioningError` variant, no panic.
    ///
    /// The `target_stage_index` parameter is currently unused because the
    /// public `Commissioner` API can't jump the cursor — it's a
    /// forward-compat parameter that becomes meaningful when M6.4.6
    /// fixtures let the proptest drive to deeper cursor positions. For
    /// M6.4.5 it provides coverage at the `SecurePairing` →
    /// `ReadCommissioningInfo` waiting state on top of the existing
    /// `on_response_never_panics` + `poll_never_panics` coverage from
    /// M6.4.1. Updated to 21 stages in M6.5.2 (adds
    /// `ReadNetworkCommissioningInfo`, `NetworkSetup`,
    /// `FailsafeBeforeNetworkEnable`, and `NetworkEnable`).
    #[test]
    fn transitions_are_total(
        target_stage_index in 0_usize..21,
        exp in any_expectation(),
        payload in prop::collection::vec(any::<u8>(), 0..64),
    ) {
        let fabric = make_fabric();
        let setup = make_setup();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let mut sm = build_sm(&fabric, &setup, &paa, &cd);
        // target_stage_index is unused by this test (the public API
        // can't jump the cursor) — it's there as a forward-compat
        // parameter for when an in-test cursor-setter is added.
        let _ = target_stage_index;
        // Drive the cursor to a position where it's awaiting a response.
        // The state machine starts at SecurePairing → on first poll it
        // transitions to ReadCommissioningInfo and awaits
        // CommissioningInfo.
        let _ = sm.poll();
        // The fuzz harness — on_response must never panic.
        let _ = sm.on_response(exp, &payload);
        // poll() after a random response must never panic either.
        let _ = sm.poll();
    }
}
