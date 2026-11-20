//! Unit tests for the M6.5.2 `NetworkCommissioning` sub-cursor.
//!
//! Covers the `Stage::ReadNetworkCommissioningInfo` dispatch arm and
//! the `Expectation::NetworkCommissioningInfo` response handler's
//! Wi-Fi / Ethernet / Thread / malformed branching logic.
//!
//! Requires the `__test_shortcuts` cargo feature (see `Cargo.toml`
//! `[[test]]` section). The feature gates
//! `Commissioner::position_at_stage_for_test`, which jumps
//! the cursor past M6.4 attestation + NOC crypto that cannot be driven
//! with synthetic data. This is intentional test isolation: the real
//! crypto path is exercised in the in-source glass-box tests in
//! `src/state_machine/commissioner.rs::tests`.

// Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, OnceLock};

use matter_cert::time::MatterTime;
use matter_commissioning::attestation::CdSigningRoots;
use matter_commissioning::noc::{FabricRecord, NocRng, SystemNocRng};
use matter_commissioning::setup::{
    CommissioningFlow, DiscoveryCapabilities, Discriminator, Passcode, SetupPayload,
};
use matter_commissioning::{
    Action, Commissioner, CommissionerConfig, CommissioningError, Expectation, NetworkCredentials,
    NetworkKind, PaaTrustStore, RemediationHint, SessionContext, Stage, TestStateSeeds,
    ThreadDataset, WiFiCredentials,
};
use matter_crypto::{RingSigner, Signer};

// ---------------------------------------------------------------------------
// Static fixtures (OnceLock pattern from sample_valid_config in commissioner.rs)
// ---------------------------------------------------------------------------

fn static_fabric() -> &'static FabricRecord {
    static FABRIC: OnceLock<FabricRecord> = OnceLock::new();
    FABRIC.get_or_init(|| {
        let (signer, _pkcs8) = RingSigner::generate().expect("ring keypair");
        let signer: Arc<dyn Signer> = Arc::new(signer);
        FabricRecord::new_root_only(
            0x0000_0000_0000_0001,
            signer,
            MatterTime::from_unix_secs(1_704_067_200),
            MatterTime::from_unix_secs(1_735_689_600),
            42,
            &SystemNocRng,
        )
        .expect("valid root fabric")
    })
}

fn static_setup() -> &'static SetupPayload {
    static SETUP: OnceLock<SetupPayload> = OnceLock::new();
    SETUP.get_or_init(|| SetupPayload {
        version: 0,
        vendor_id: Some(0xFFF1),
        product_id: Some(0x8000),
        commissioning_flow: CommissioningFlow::Standard,
        discovery_capabilities: DiscoveryCapabilities::ON_NETWORK,
        discriminator: Discriminator::new(0x0F00).expect("valid discriminator"),
        passcode: Passcode::new(20_202_021).expect("valid passcode"),
    })
}

fn static_paa() -> &'static PaaTrustStore {
    static PAA: OnceLock<PaaTrustStore> = OnceLock::new();
    PAA.get_or_init(PaaTrustStore::with_example_device_roots)
}

fn static_cd() -> &'static CdSigningRoots {
    static CD: OnceLock<CdSigningRoots> = OnceLock::new();
    CD.get_or_init(CdSigningRoots::with_example_device_roots)
}

/// Returns a fully-populated `CommissionerConfig<'static>` with Wi-Fi
/// credentials set (ssid=`b"matter"`, credentials=`b"hunter22"`).
fn make_wifi_config() -> CommissionerConfig<'static> {
    let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
    CommissionerConfig {
        pase_attestation_challenge: [0u8; 16],
        fabric: static_fabric(),
        setup_payload: static_setup(),
        paa_trust_store: static_paa(),
        cd_signing_roots: static_cd(),
        commissioner_node_id: 0x1,
        assigned_node_id: 0x2,
        ipk_epoch_key: [0x42_u8; 16],
        case_admin_subject: 0x1,
        admin_vendor_id: 0xFFF1,
        now: MatterTime::from_unix_secs(1_704_067_200),
        rng,
        network: NetworkCredentials::WiFi(WiFiCredentials {
            ssid: b"matter".to_vec(),
            credentials: b"hunter22".to_vec(),
        }),
    }
}

/// Returns a config without Wi-Fi credentials (for Ethernet-only tests).
fn make_ethernet_config() -> CommissionerConfig<'static> {
    let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
    CommissionerConfig {
        pase_attestation_challenge: [0u8; 16],
        fabric: static_fabric(),
        setup_payload: static_setup(),
        paa_trust_store: static_paa(),
        cd_signing_roots: static_cd(),
        commissioner_node_id: 0x1,
        assigned_node_id: 0x2,
        ipk_epoch_key: [0x42_u8; 16],
        case_admin_subject: 0x1,
        admin_vendor_id: 0xFFF1,
        now: MatterTime::from_unix_secs(1_704_067_200),
        rng,
        network: NetworkCredentials::AlreadyOnNetwork,
    }
}

// ---------------------------------------------------------------------------
// FeatureMap TLV encoder
// ---------------------------------------------------------------------------

/// Encode a bare `u32` `FeatureMap` value as the minimal-width TLV scalar
/// that `decode_feature_map` expects.
///
/// Matter attribute read responses return a bare TLV scalar (not wrapped
/// in a struct). The two encoding widths used in tests:
/// - `bits ≤ 0xFF`: 1-byte unsigned (TLV type byte `0x04`) followed by the
///   value byte.
/// - `bits ≤ 0xFFFF`: 2-byte unsigned (type `0x05`) followed by LE bytes.
#[allow(clippy::cast_possible_truncation)]
fn feature_map_tlv(bits: u32) -> Vec<u8> {
    if bits <= 0xFF {
        vec![0x04, bits as u8]
    } else {
        vec![0x05, (bits & 0xFF) as u8, ((bits >> 8) & 0xFF) as u8]
    }
}

// ---------------------------------------------------------------------------
// Cursor helper
// ---------------------------------------------------------------------------

/// Create a `Commissioner` positioned at `Stage::ReadNetworkCommissioningInfo`
/// using `Commissioner::position_at_stage_for_test` (gated by the
/// `__test_shortcuts` cargo feature).
///
/// This skips M6.4 attestation + NOC crypto; the full crypto path is covered
/// by the in-source glass-box tests in `commissioner.rs::tests`.
fn drive_to_read_network_info(config: CommissionerConfig<'static>) -> Commissioner {
    Commissioner::new(config)
        .expect("valid config must produce a Commissioner")
        .position_at_stage_for_test(
            Stage::ReadNetworkCommissioningInfo,
            TestStateSeeds::default(),
        )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn wifi_only_feature_map_advances_to_wifi_setup() {
    let mut sm = drive_to_read_network_info(make_wifi_config());
    let action = sm.poll().expect("emit ReadAttribute");
    // Verify the emitted action has the right expectation.
    match action {
        matter_commissioning::Action::ReadAttribute { expect, .. } => {
            assert_eq!(expect, Expectation::NetworkCommissioningInfo);
        }
        other => panic!("expected ReadAttribute, got {other:?}"),
    }
    sm.on_response(
        Expectation::NetworkCommissioningInfo,
        &feature_map_tlv(0b001),
    )
    .expect("WiFi-only FeatureMap accepted");
    assert_eq!(sm.stage(), Stage::NetworkSetup);
}

#[test]
fn ethernet_only_feature_map_skips_to_evict_case() {
    let mut sm = drive_to_read_network_info(make_ethernet_config());
    let _ = sm.poll().expect("emit ReadAttribute");
    sm.on_response(
        Expectation::NetworkCommissioningInfo,
        &feature_map_tlv(0b100),
    )
    .expect("Ethernet-only FeatureMap accepted");
    assert_eq!(sm.stage(), Stage::EvictPreviousCaseSessions);
}

#[test]
fn wifi_creds_thread_only_featuremap_rejects_mismatch() {
    // Wi-Fi credentials against a Thread-only device: routing keys off the
    // *supplied* credential type, so the mismatch is reported as the
    // supplied type being unsupported by the device — `needed: WiFi`, NOT
    // `Thread`. (Before M9-C2 the routing reported the device's offered
    // type here.)
    let mut sm = drive_to_read_network_info(make_wifi_config());
    let _ = sm.poll().expect("emit ReadAttribute");
    let Err(err) = sm.on_response(
        Expectation::NetworkCommissioningInfo,
        &feature_map_tlv(0b010),
    ) else {
        panic!("Wi-Fi creds on a Thread-only device should fail");
    };
    assert!(
        matches!(
            err,
            CommissioningError::NetworkFeatureUnsupported {
                needed: NetworkKind::WiFi,
            },
        ),
        "got {err:?}",
    );
}

#[test]
fn wifi_device_without_credentials_skips_network_setup() {
    // A Wi-Fi FeatureMap with NO wifi_credentials configured: the device is
    // already on the network (IP commissioning reached it there — e.g. a
    // second-fabric commission of an already-provisioned device; observed:
    // Tapo P110M, M6.6.5 validation). Mirror chip's AutoCommissioner: include
    // the network-setup stages ONLY when credentials are supplied; otherwise
    // skip straight past the Wi-Fi sub-cursor.
    let mut sm = drive_to_read_network_info(make_ethernet_config());
    let _ = sm.poll().expect("emit ReadAttribute");
    sm.on_response(
        Expectation::NetworkCommissioningInfo,
        &feature_map_tlv(0b001),
    )
    .expect("WiFi FeatureMap with no creds must skip network setup");
    assert_eq!(sm.stage(), Stage::EvictPreviousCaseSessions);
}

#[test]
fn empty_feature_map_is_malformed() {
    let mut sm = drive_to_read_network_info(make_wifi_config());
    let _ = sm.poll().expect("emit ReadAttribute");
    let Err(err) = sm.on_response(Expectation::NetworkCommissioningInfo, &feature_map_tlv(0))
    else {
        panic!("Empty FeatureMap should fail");
    };
    assert!(
        matches!(
            err,
            CommissioningError::MalformedResponse(Stage::ReadNetworkCommissioningInfo),
        ),
        "got {err:?}",
    );
}

// ---------------------------------------------------------------------------
// NetworkSetup cursor helper (Wi-Fi path)
// ---------------------------------------------------------------------------

/// Drive the state machine from `ReadNetworkCommissioningInfo` all the
/// way to `Stage::NetworkSetup` by composing `drive_to_read_network_info`
/// with a real `FeatureMap` response.
///
/// Avoids adding a new `position_at_stage_for_test(Stage::NetworkSetup, …)` call:
/// the `__test_shortcuts` surface stays minimal and the full flow through
/// `Expectation::NetworkCommissioningInfo` is exercised as a by-product.
fn drive_to_wifi_network_setup() -> Commissioner {
    let mut sm = drive_to_read_network_info(make_wifi_config());
    let _ = sm.poll().expect("emit ReadAttribute");
    sm.on_response(
        Expectation::NetworkCommissioningInfo,
        &feature_map_tlv(0b001),
    )
    .expect("WiFi FeatureMap accepted");
    sm
}

// ---------------------------------------------------------------------------
// NetworkSetup dispatch (Wi-Fi) + NetworkConfigResponse handler
// ---------------------------------------------------------------------------

#[test]
fn wifi_network_setup_happy_path_emits_add_or_update() {
    let mut sm = drive_to_wifi_network_setup();
    let action = sm.poll().expect("poll");
    match action {
        matter_commissioning::Action::Invoke {
            session,
            cluster,
            command,
            payload,
            expect,
            ..
        } => {
            assert_eq!(session, matter_commissioning::SessionContext::Pase);
            assert_eq!(cluster, 0x0031);
            assert_eq!(command, 0x02);
            assert_eq!(expect, Expectation::NetworkConfigResponse);
            // Payload should contain the SSID "matter" literal.
            assert!(
                payload.windows(6).any(|w| w == b"matter"),
                "payload should contain SSID bytes: {payload:02x?}",
            );
        }
        other => panic!("expected Invoke, got {other:?}"),
    }
}

#[test]
fn wifi_network_setup_ok_response_advances_to_failsafe_before_wifi_enable() {
    let mut sm = drive_to_wifi_network_setup();
    let _ = sm.poll().expect("emit Invoke");
    let response = vec![0x15, 0x24, 0x00, 0x00, 0x18]; // { 0: 0_u8 } — OK
    sm.on_response(Expectation::NetworkConfigResponse, &response)
        .expect("ok response accepted");
    assert_eq!(sm.stage(), Stage::FailsafeBeforeNetworkEnable);
}

#[test]
fn wifi_network_setup_auth_failure_carries_remediation_hint() {
    let mut sm = drive_to_wifi_network_setup();
    let _ = sm.poll().expect("emit Invoke");
    // { 0: 7_u8 } = AuthFailure → CheckPassphrase
    let response = vec![0x15, 0x24, 0x00, 0x07, 0x18];
    let Err(err) = sm.on_response(Expectation::NetworkConfigResponse, &response) else {
        panic!("AuthFailure should fail");
    };
    match err {
        CommissioningError::NetworkRejected {
            stage,
            networking_status,
            remediation_hint,
            ..
        } => {
            assert_eq!(stage, Stage::NetworkSetup);
            assert_eq!(networking_status, 7);
            assert_eq!(remediation_hint, RemediationHint::CheckPassphrase);
        }
        other => panic!("expected NetworkRejected, got {other:?}"),
    }
}

#[test]
fn wifi_network_setup_bounds_exceeded_maps_to_slots_full() {
    let mut sm = drive_to_wifi_network_setup();
    let _ = sm.poll().expect("emit Invoke");
    let response = vec![0x15, 0x24, 0x00, 0x02, 0x18]; // BoundsExceeded
    let Err(err) = sm.on_response(Expectation::NetworkConfigResponse, &response) else {
        panic!("BoundsExceeded should fail");
    };
    match err {
        CommissioningError::NetworkRejected {
            remediation_hint, ..
        } => {
            assert_eq!(remediation_hint, RemediationHint::DeviceNetworkSlotsFull);
        }
        other => panic!("got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// FailsafeBeforeNetworkEnable dispatch + second ArmFailSafe
// ---------------------------------------------------------------------------

fn drive_to_failsafe_before_wifi_enable() -> Commissioner {
    let mut sm = drive_to_wifi_network_setup();
    let _ = sm.poll().expect("AddOrUpdateWiFiNetwork");
    let ok = vec![0x15, 0x24, 0x00, 0x00, 0x18];
    sm.on_response(Expectation::NetworkConfigResponse, &ok)
        .expect("ok accepted");
    sm
}

#[test]
fn failsafe_before_wifi_enable_emits_second_arm_failsafe() {
    let mut sm = drive_to_failsafe_before_wifi_enable();
    let action = sm.poll().expect("emit second ArmFailSafe");
    match action {
        matter_commissioning::Action::Invoke {
            cluster,
            command,
            expect,
            ..
        } => {
            assert_eq!(cluster, 0x0030);
            assert_eq!(command, 0x00);
            assert_eq!(expect, Expectation::ArmFailsafeResponse);
        }
        other => panic!("expected Invoke, got {other:?}"),
    }
    let ok = vec![0x15, 0x24, 0x00, 0x00, 0x18];
    sm.on_response(Expectation::ArmFailsafeResponse, &ok)
        .expect("ok accepted");
    assert_eq!(sm.stage(), Stage::NetworkEnable);
}

// ---------------------------------------------------------------------------
// NetworkEnable dispatch (Wi-Fi) + ConnectNetworkResponse handler
// ---------------------------------------------------------------------------

fn drive_to_wifi_network_enable() -> Commissioner {
    let mut sm = drive_to_failsafe_before_wifi_enable();
    let _ = sm.poll().expect("emit second ArmFailSafe");
    let ok = vec![0x15, 0x24, 0x00, 0x00, 0x18];
    sm.on_response(Expectation::ArmFailsafeResponse, &ok)
        .expect("ok accepted");
    sm
}

#[test]
fn wifi_network_enable_happy_path_emits_connect_network() {
    let mut sm = drive_to_wifi_network_enable();
    let action = sm.poll().expect("emit ConnectNetwork");
    match action {
        matter_commissioning::Action::Invoke {
            cluster,
            command,
            payload,
            expect,
            ..
        } => {
            assert_eq!(cluster, 0x0031);
            assert_eq!(command, 0x06);
            assert_eq!(expect, Expectation::ConnectNetworkResponse);
            assert!(payload.windows(6).any(|w| w == b"matter"));
        }
        other => panic!("expected Invoke, got {other:?}"),
    }
}

#[test]
fn wifi_network_enable_ok_response_advances_to_evict_case() {
    let mut sm = drive_to_wifi_network_enable();
    let _ = sm.poll().expect("emit Invoke");
    let response = vec![0x15, 0x24, 0x00, 0x00, 0x18]; // { 0: 0 } OK
    sm.on_response(Expectation::ConnectNetworkResponse, &response)
        .expect("ok accepted");
    assert_eq!(sm.stage(), Stage::EvictPreviousCaseSessions);
}

#[test]
fn wifi_network_enable_network_not_found_maps_to_check_ssid() {
    let mut sm = drive_to_wifi_network_enable();
    let _ = sm.poll().expect("emit Invoke");
    let response = vec![0x15, 0x24, 0x00, 0x05, 0x18]; // NetworkNotFound
    let Err(err) = sm.on_response(Expectation::ConnectNetworkResponse, &response) else {
        panic!("NetworkNotFound should fail");
    };
    match err {
        CommissioningError::NetworkRejected {
            stage,
            networking_status,
            remediation_hint,
            ..
        } => {
            assert_eq!(stage, Stage::NetworkEnable);
            assert_eq!(networking_status, 5);
            assert_eq!(remediation_hint, RemediationHint::CheckSsid);
        }
        other => panic!("got {other:?}"),
    }
}

#[test]
fn wifi_network_enable_unknown_status_maps_to_none() {
    let mut sm = drive_to_wifi_network_enable();
    let _ = sm.poll().expect("emit Invoke");
    let response = vec![0x15, 0x24, 0x00, 0x0C, 0x18]; // UnknownError
    let Err(err) = sm.on_response(Expectation::ConnectNetworkResponse, &response) else {
        panic!("UnknownError should fail");
    };
    match err {
        CommissioningError::NetworkRejected {
            remediation_hint, ..
        } => {
            assert_eq!(remediation_hint, RemediationHint::None);
        }
        other => panic!("got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Ethernet-only end-to-end walk
// ---------------------------------------------------------------------------

/// Full end-to-end walk for an Ethernet-only device (no Wi-Fi or Thread):
/// `EvictPreviousCaseSessions` (sentinel NOC key pre-set) →
/// `FindOperationalForComplete` (`EstablishCase`) → `on_case_established` →
/// `SendComplete` (`CommissioningComplete`) → `Cleanup` (Done).
///
/// The cursor is placed at `EvictPreviousCaseSessions` via
/// `position_at_stage_for_test` with a synthetic NOC public key so that
/// `Stage::Cleanup` can emit `Action::Done` (which requires
/// `issued_noc_public_key` set by NOC issuance in M6.4.4 — skipped here).
/// The `ReadNetworkCommissioningInfo` → Ethernet `FeatureMap` → `EvictPreviousCaseSessions`
/// transition is covered by `ethernet_only_feature_map_skips_to_evict_case`
/// and the proptest totality suite.
#[test]
fn ethernet_only_e2e_reaches_done() {
    let mut sm = Commissioner::new(make_ethernet_config())
        .expect("valid config")
        .position_at_stage_for_test(
            Stage::EvictPreviousCaseSessions,
            TestStateSeeds {
                synthetic_noc_pubkey: Some([0xCC; 65]),
            },
        );

    // EvictPreviousCaseSessions is a no-op in M6.4/5 (advances internally);
    // next poll routes through FindOperationalForComplete and emits EstablishCase.
    let action = sm.poll().expect("emit EstablishCase");
    match action {
        Action::EstablishCase {
            fabric_id: _,
            peer_node_id: _,
        } => {}
        other => panic!("expected EstablishCase, got {other:?}"),
    }

    // Driver-layer signal: CASE handshake succeeded.
    sm.on_case_established().expect("CASE established");

    // SendComplete — emit CommissioningComplete invoke.
    let action = sm.poll().expect("emit CommissioningComplete");
    match action {
        Action::Invoke {
            cluster,
            command,
            expect,
            session,
            ..
        } => {
            assert_eq!(cluster, 0x0030);
            assert_eq!(command, 0x04);
            assert_eq!(expect, Expectation::CommissioningCompleteResponse);
            assert_eq!(session, SessionContext::Case);
        }
        other => panic!("expected Invoke, got {other:?}"),
    }

    // CommissioningComplete OK response.
    let ok = vec![0x15, 0x24, 0x00, 0x00, 0x18]; // { 0: 0 }
    sm.on_response(Expectation::CommissioningCompleteResponse, &ok)
        .expect("ok CommissioningCompleteResponse");

    // Cleanup — terminal Done.
    let action = sm.poll().expect("emit Done");
    match action {
        Action::Done(_) => {}
        other => panic!("expected Done, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Thread network provisioning (route + dispatch)
// ---------------------------------------------------------------------------

/// Reference OTBR operational dataset (111 bytes, `ot-ctl dataset active -x`).
/// Vector: `test-vectors/thread/network_commissioning.json`
/// (`reference_operational_dataset`). Extended PAN ID (TLV `02 08`) =
/// `7896217f787f6ebe`.
const THREAD_DS_HEX: &str = "0e08000000000001000000030000184a0300001235060004001fffe0\
02087896217f787f6ebe0708fdec3f34f3cd2020051071dccee3f164f15da92254e0b9c8a3a5030f4f706\
56e5468726561642d38396437010289d70410dc4b544c7a58671a2ce4f876f5d6dcd90c0402a0f7f8";

/// Extended PAN ID extracted from `THREAD_DS_HEX` — the `ConnectNetwork`
/// `network_id` for the Thread path (NOT an SSID).
const THREAD_EXT_PAN_ID: [u8; 8] = [0x78, 0x96, 0x21, 0x7f, 0x78, 0x7f, 0x6e, 0xbe];

/// A `CommissionerConfig<'static>` carrying a validated Thread operational
/// dataset (`NetworkCredentials::Thread`).
fn make_thread_config() -> CommissionerConfig<'static> {
    let dataset = ThreadDataset::new(hex::decode(THREAD_DS_HEX).expect("valid dataset hex"))
        .expect("valid Thread dataset");
    let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
    CommissionerConfig {
        pase_attestation_challenge: [0u8; 16],
        fabric: static_fabric(),
        setup_payload: static_setup(),
        paa_trust_store: static_paa(),
        cd_signing_roots: static_cd(),
        commissioner_node_id: 0x1,
        assigned_node_id: 0x2,
        ipk_epoch_key: [0x42_u8; 16],
        case_admin_subject: 0x1,
        admin_vendor_id: 0xFFF1,
        now: MatterTime::from_unix_secs(1_704_067_200),
        rng,
        network: NetworkCredentials::Thread(dataset),
    }
}

/// Thread creds + a Thread-only `FeatureMap` route to `NetworkSetup`, emit
/// `AddOrUpdateThreadNetwork` bytes matching the captured vector, then
/// `ConnectNetwork` with the Extended PAN ID as the `network_id`.
#[test]
fn thread_creds_thread_featuremap_routes_and_provisions() {
    let mut sm = drive_to_read_network_info(make_thread_config());
    let _ = sm.poll().expect("emit ReadAttribute");
    // THREAD bit only (0b010).
    sm.on_response(
        Expectation::NetworkCommissioningInfo,
        &feature_map_tlv(0b010),
    )
    .expect("Thread FeatureMap accepted for Thread creds");
    assert_eq!(sm.stage(), Stage::NetworkSetup);

    // NetworkSetup emits AddOrUpdateThreadNetwork == the vector (breadcrumb 1,
    // since position_at_stage_for_test skipped the breadcrumb-bearing M6.4
    // stages, so NetworkSetup is the first breadcrumb consumer).
    let action = sm.poll().expect("emit AddOrUpdateThreadNetwork");
    match action {
        Action::Invoke {
            session,
            cluster,
            command,
            payload,
            expect,
            ..
        } => {
            assert_eq!(session, SessionContext::Pase);
            assert_eq!(cluster, 0x0031);
            assert_eq!(command, 0x03, "ADD_OR_UPDATE_THREAD_NETWORK");
            assert_eq!(expect, Expectation::NetworkConfigResponse);
            // Vector: test-vectors/thread/network_commissioning.json
            // "add_or_update_thread_network" (dataset + breadcrumb=1).
            let expected = "1530006f0e08000000000001000000030000184a0300001235060004001\
fffe002087896217f787f6ebe0708fdec3f34f3cd2020051071dccee3f164f15da92254e0b9c8a3a5030f4\
f70656e5468726561642d38396437010289d70410dc4b544c7a58671a2ce4f876f5d6dcd90c0402a0f7f82\
4010118";
            assert_eq!(hex::encode(&payload), expected, "payload: {payload:02x?}");
        }
        other => panic!("expected Invoke, got {other:?}"),
    }

    // Accept the NetworkConfigResponse → FailsafeBeforeNetworkEnable.
    let ok = vec![0x15, 0x24, 0x00, 0x00, 0x18];
    sm.on_response(Expectation::NetworkConfigResponse, &ok)
        .expect("ok NetworkConfigResponse");
    assert_eq!(sm.stage(), Stage::FailsafeBeforeNetworkEnable);

    // Second ArmFailSafe → NetworkEnable.
    let _ = sm.poll().expect("emit second ArmFailSafe");
    sm.on_response(Expectation::ArmFailsafeResponse, &ok)
        .expect("ok ArmFailsafeResponse");
    assert_eq!(sm.stage(), Stage::NetworkEnable);

    // NetworkEnable emits ConnectNetwork with network_id = Extended PAN ID.
    let action = sm.poll().expect("emit ConnectNetwork");
    match action {
        Action::Invoke {
            cluster,
            command,
            payload,
            expect,
            ..
        } => {
            assert_eq!(cluster, 0x0031);
            assert_eq!(command, 0x06, "CONNECT_NETWORK");
            assert_eq!(expect, Expectation::ConnectNetworkResponse);
            // network_id is the Ext-PAN-ID (the "connect_network_thread"
            // vector's network_id), NOT an SSID. The breadcrumb differs from
            // that vector (it is the 3rd breadcrumb here), so assert the
            // network_id field is present rather than the whole payload.
            assert!(
                payload.windows(8).any(|w| w == THREAD_EXT_PAN_ID),
                "ConnectNetwork network_id must be the Ext-PAN-ID: {payload:02x?}",
            );
        }
        other => panic!("expected Invoke, got {other:?}"),
    }
}

/// Thread creds against a Wi-Fi-only `FeatureMap` must fail fast with
/// `NetworkFeatureUnsupported{ needed: Thread }` — NOT silently skip
/// provisioning.
#[test]
fn thread_creds_wifi_only_featuremap_rejects_mismatch() {
    let mut sm = drive_to_read_network_info(make_thread_config());
    let _ = sm.poll().expect("emit ReadAttribute");
    // WIFI bit only (0b001) — Thread not offered.
    let Err(err) = sm.on_response(
        Expectation::NetworkCommissioningInfo,
        &feature_map_tlv(0b001),
    ) else {
        panic!("Thread creds on a Wi-Fi-only device must fail");
    };
    assert!(
        matches!(
            err,
            CommissioningError::NetworkFeatureUnsupported {
                needed: NetworkKind::Thread,
            },
        ),
        "got {err:?}",
    );
}
