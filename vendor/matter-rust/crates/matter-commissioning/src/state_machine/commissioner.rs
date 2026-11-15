//! `Commissioner` — the state-machine cursor.

#![forbid(unsafe_code)]

use std::sync::Arc;

use matter_cert::time::MatterTime;

use crate::attestation::PaaTrustStore;
use crate::noc::{FabricRecord, NocRng};
use crate::setup::SetupPayload;
use crate::state_machine::action::{Action, Expectation};
use crate::state_machine::error::CommissioningError;
use crate::state_machine::stage::Stage;

#[cfg(feature = "tracing")]
use tracing::instrument;

/// Fallback failsafe extension (seconds) applied at
/// `Stage::FailsafeBeforeNetworkEnable` when the device did not report a
/// usable `ConnectMaxTimeSeconds`. Chosen generously (Thread attach +
/// SRP registration is slower than Wi-Fi association); the C1 Wi-Fi path
/// adopts it harmlessly. Matter Core Spec §11.9.5.4 defines the attribute
/// but does not mandate a minimum, so a conservative default is safest.
pub(crate) const DEFAULT_CONNECT_MAX_TIME_SECONDS: u16 = 90;

/// Wi-Fi station credentials supplied to `AddOrUpdateWiFiNetwork`.
///
/// `ssid` must be 1–32 bytes (Matter Core Spec §11.9 constraints).
/// `credentials` must be 0–64 bytes — empty means open network, ≤64
/// bytes covers WPA2/WPA3 PSK lengths.
///
/// `Debug` is hand-written to redact `credentials` (renders only the
/// length). `Clone` is derived. Validation runs in
/// `Commissioner::new`.
#[derive(Clone, PartialEq, Eq)]
pub struct WiFiCredentials {
    /// SSID bytes, 1–32 bytes.
    pub ssid: Vec<u8>,
    /// Pre-shared key / passphrase bytes, 0–64 bytes. Empty means
    /// open network.
    pub credentials: Vec<u8>,
}

impl core::fmt::Debug for WiFiCredentials {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("WiFiCredentials")
            .field("ssid", &format_args!("<{} bytes>", self.ssid.len()))
            .field(
                "credentials",
                &format_args!("<redacted, {} bytes>", self.credentials.len()),
            )
            .finish()
    }
}

/// Operational-network credentials for the commissionee, selecting which
/// network-provisioning sub-cursor the state machine runs after `AddNOC`.
///
/// Mirrors chip's `AutoCommissioner`: network provisioning runs only for
/// the concrete network type whose credentials are supplied.
///
/// - [`NetworkCredentials::WiFi`] provisions Wi-Fi via
///   `AddOrUpdateWiFiNetwork` + `ConnectNetwork`.
/// - [`NetworkCredentials::Thread`] provisions Thread from an operational
///   dataset via `AddOrUpdateThreadNetwork` + `ConnectNetwork` (the
///   Extended PAN ID is the `ConnectNetwork` `network_id`); the dataset is
///   self-validated at [`ThreadDataset`](crate::ThreadDataset)
///   construction.
/// - [`NetworkCredentials::AlreadyOnNetwork`] skips network provisioning
///   entirely — correct both for Ethernet-only devices and for devices
///   already reachable on their operational network (the usual
///   IP-commissioning case, e.g. a second-fabric commission).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetworkCredentials {
    /// Provision Wi-Fi using the supplied station credentials.
    WiFi(WiFiCredentials),
    /// Provision Thread using the supplied operational dataset.
    Thread(crate::thread_dataset::ThreadDataset),
    /// Device is already on an operational network; skip provisioning.
    AlreadyOnNetwork,
}

/// Configuration passed to [`Commissioner::new`].
///
/// All fields are by-reference where possible so the state machine
/// can share long-lived caller-owned resources (the fabric record, the
/// trust store, the setup payload) without copying.
///
/// **Not `#[non_exhaustive]`** — callers build this as a struct literal
/// with all public fields populated. Adding a field is a breaking change,
/// accepted for a pre-1.0 unpublished crate. `#[non_exhaustive]` stays on
/// [`Action`], [`Expectation`], [`Stage`], and [`CommissioningError`] —
/// those are read by callers, not constructed by them.
// Do not derive `Debug`: `ipk_epoch_key` is the fabric's Identity Protection
// Key, `pase_attestation_challenge` is derived from the PASE session keys,
// and `setup_payload` holds the device's passcode (`Passcode` redacts its own
// `Debug`, but the IPK and challenge are plain arrays a derive would print).
// If `Debug` is ever needed, write it by hand and redact them.
pub struct CommissionerConfig<'a> {
    /// 16-byte attestation challenge derived from the active PASE
    /// session. Matter Core Spec §3.6.4: bytes `[32..48]` of the
    /// 48-byte PASE session key blob (exposed as
    /// `PaseSessionKeys::attestation_key`).
    pub pase_attestation_challenge: [u8; 16],
    /// The commissioner's fabric record (RCAC keypair + signer + IPK).
    /// Constructed via [`FabricRecord::new_root_only`] from M6.3.
    pub fabric: &'a FabricRecord,
    /// The setup payload parsed from QR or manual code (M6.1). Used
    /// to cross-check VID/PID against the DAC's subject during
    /// attestation verification.
    pub setup_payload: &'a SetupPayload,
    /// Trusted PAA roots for attestation chain validation (M6.2).
    pub paa_trust_store: &'a PaaTrustStore,
    /// Trusted CSA Certification Declaration signing roots (M6.4.3).
    /// Tests can use `CdSigningRoots::with_example_device_roots()`; production
    /// callers supply CSA-published roots via `CdSigningRoots::from_pem`.
    pub cd_signing_roots: &'a crate::attestation::CdSigningRoots,
    /// The commissioner's own operational node ID on this fabric.
    /// Must be non-zero.
    pub commissioner_node_id: u64,
    /// The operational node ID being assigned to the device on this
    /// fabric. Must be non-zero and distinct from
    /// `commissioner_node_id`.
    pub assigned_node_id: u64,
    /// 16-byte Identity Protection Key (IPK) epoch key for `AddNOC`.
    /// Matter Core Spec §4.15.2. Must not be all-zero (rejected by
    /// the device-side `AddNOC` handler).
    pub ipk_epoch_key: [u8; 16],
    /// CASE admin subject for `AddNOC` (typically the commissioner's
    /// own operational node ID).
    pub case_admin_subject: u64,
    /// Admin vendor ID for `AddNOC`.
    pub admin_vendor_id: u16,
    /// Wall-clock time at construction. Used for NOC + RCAC validity
    /// windows and for chain verification's `not_before` / `not_after`
    /// checks.
    pub now: MatterTime,
    /// RNG for nonces (`CSRNonce`, `AttestationNonce`) and NOC serials.
    pub rng: Arc<dyn NocRng>,
    /// Operational-network credentials for the commissionee.
    ///
    /// [`NetworkCredentials::AlreadyOnNetwork`] skips the network
    /// sub-cursor entirely, mirroring chip's `AutoCommissioner`: network
    /// provisioning runs ONLY when concrete credentials are supplied. It is
    /// correct both for Ethernet-only devices and for devices already
    /// reachable on their operational network (the usual case for IP
    /// commissioning, e.g. a second-fabric commission). Supplying
    /// [`NetworkCredentials::WiFi`] or [`NetworkCredentials::Thread`]
    /// forces provisioning via `Stage::NetworkSetup`.
    pub network: NetworkCredentials,
}

/// The commissioning state machine cursor.
///
/// One `Commissioner` per in-flight commissioning. `Send` but `!Sync`.
/// See module docs in [`crate::state_machine`] for the driver-loop
/// example.
// `commissioner_node_id` mirrors Matter Core Spec terminology
// (commissioner node ID vs. assigned node ID). Renaming to satisfy
// the lint would obscure the spec mapping.
#[allow(clippy::struct_field_names)]
pub struct Commissioner {
    stage: Stage,

    // Configuration captured at construction time. The per-stage storage
    // slots (`pai_der`, `dac_der`, `attestation_response`, CSR / NOC
    // artefacts, the CASE-awaiting flag, etc.) follow below.
    #[allow(dead_code)] // Used by attestation/CSR verification in M6.4.2+.
    pase_attestation_challenge: [u8; 16],
    #[allow(dead_code)] // Used by NOC issuance + chain validation in M6.4.4.
    fabric: FabricRecord,
    #[allow(dead_code)] // Used by chain validation in M6.4.2.
    paa_trust_store: PaaTrustStore,
    cd_signing_roots: crate::attestation::CdSigningRoots,
    #[allow(dead_code)] // Used by VID/PID cross-check in M6.4.2.
    setup_payload: SetupPayload,
    #[allow(dead_code)] // Used by NOC subject in M6.4.4.
    commissioner_node_id: u64,
    #[allow(dead_code)] // Used by NOC subject in M6.4.4.
    assigned_node_id: u64,
    #[allow(dead_code)] // Used by AddNOC payload in M6.4.4.
    ipk_epoch_key: [u8; 16],
    #[allow(dead_code)] // Used by AddNOC payload in M6.4.4.
    case_admin_subject: u64,
    #[allow(dead_code)] // Used by AddNOC payload in M6.4.4.
    admin_vendor_id: u16,
    #[allow(dead_code)] // Used by cert validity windows in M6.4.2 + M6.4.4.
    now: MatterTime,
    #[allow(dead_code)] // Used for nonce generation in M6.4.2 + M6.4.4.
    rng: Arc<dyn NocRng>,

    // Attestation slots — populated by SendPaiCertRequest /
    // SendDacCertRequest / SendAttestationRequest, consumed by
    // AttestationVerification.
    pai_der: Option<Vec<u8>>,
    dac_der: Option<Vec<u8>>,
    attestation_nonce: Option<[u8; 32]>,
    attestation_response: Option<crate::attestation::AttestationResponse>,

    // CSR + NOC slots — populated by SendOpCertSigningRequest /
    // ValidateCsr / GenerateNocChain, consumed by SendTrustedRootCert
    // and SendNoc.
    csr_nonce: Option<[u8; 32]>,
    csr_response: Option<crate::noc::CsrResponse>,
    verified_csr: Option<crate::noc::VerifiedCsr>,
    issued_noc: Option<matter_cert::MatterCertificate>,
    issued_noc_public_key: Option<[u8; 65]>,

    /// Operational-network credentials captured from config at
    /// construction; consumed by the network-provisioning sub-cursor
    /// (`Stage::NetworkSetup`, `AddOrUpdateWiFiNetwork` for Wi-Fi or
    /// `AddOrUpdateThreadNetwork` for Thread).
    /// [`NetworkCredentials::AlreadyOnNetwork`] skips provisioning.
    network: NetworkCredentials,

    /// Maximum failsafe expiry the device accepts, in seconds.
    /// Initialised to 60 (the M6.4 fallback) and updated from
    /// `BasicCommissioningInfo::failsafe_expiry_length_seconds` once
    /// the `Expectation::CommissioningInfo` response arrives. The first
    /// `Stage::ArmFailsafe` consumes this.
    failsafe_expiry_seconds: u16,

    /// Device-declared `ConnectMaxTimeSeconds` (`NetworkCommissioning`
    /// attribute `0x0003`), captured from the
    /// `Expectation::NetworkCommissioningInfo` read. `0` means unread /
    /// absent, in which case [`Self::network_enable_failsafe_seconds`]
    /// falls back to [`DEFAULT_CONNECT_MAX_TIME_SECONDS`]. Sizes the
    /// `Stage::FailsafeBeforeNetworkEnable` failsafe extension so Thread
    /// attach (slower than Wi-Fi association) has room to complete before
    /// the failsafe expires.
    connect_max_time_seconds: u16,

    /// Monotonically-increasing breadcrumb attached to every
    /// breadcrumb-bearing cluster command. Matter Core Spec §11.10
    /// uses breadcrumb so an interrupted commissioning can be resumed
    /// from the last acknowledged step. Initialised to `1` in
    /// `Commissioner::new`; incremented after every breadcrumb emit.
    breadcrumb_counter: u64,

    /// `true` after [`Stage::FindOperationalForComplete`] emits
    /// `Action::EstablishCase`; cleared by
    /// [`Commissioner::on_case_established`] (success) or by
    /// `on_response(Expectation::CaseFailed, _)` (failure).
    awaiting_case_session: bool,

    /// The Expectation the state machine last emitted with `poll()`.
    /// `None` while not waiting for a response (terminal stages, or
    /// pre-poll).
    awaiting: Option<Expectation>,

    /// Cached pending Action so repeated `poll()` calls between
    /// `on_response`s are idempotent. Cleared when the cursor advances.
    pending_action: Option<Action>,

    /// Rendered summary of why the state machine entered `Failed`,
    /// stashed by `on_response`'s error path and read by the
    /// `Stage::Failed` arm of `dispatch_stage` so `Action::Abort.reason`
    /// surfaces the real failure (not a hard-coded placeholder).
    last_failure: Option<String>,
}

impl Commissioner {
    /// Construct a new commissioner from a validated config.
    ///
    /// # Errors
    ///
    /// Returns [`CommissioningError::InvalidConfig`] if any field fails
    /// basic validation: zero `commissioner_node_id`, zero
    /// `assigned_node_id`, `commissioner_node_id == assigned_node_id`,
    /// or all-zero `ipk_epoch_key`.
    pub fn new(cfg: CommissionerConfig<'_>) -> Result<Self, CommissioningError> {
        if cfg.commissioner_node_id == 0 {
            return Err(CommissioningError::InvalidConfig(
                "commissioner_node_id must be non-zero",
            ));
        }
        if cfg.assigned_node_id == 0 {
            return Err(CommissioningError::InvalidConfig(
                "assigned_node_id must be non-zero",
            ));
        }
        if cfg.assigned_node_id == cfg.commissioner_node_id {
            return Err(CommissioningError::InvalidConfig(
                "assigned_node_id must differ from commissioner_node_id",
            ));
        }
        if cfg.ipk_epoch_key == [0u8; 16] {
            return Err(CommissioningError::InvalidConfig(
                "ipk_epoch_key must not be all-zero",
            ));
        }
        // Only Wi-Fi credentials carry length bounds here; `Thread`
        // datasets are self-validated at `ThreadDataset` construction and
        // `AlreadyOnNetwork` carries no data to check.
        if let NetworkCredentials::WiFi(creds) = &cfg.network {
            if creds.ssid.is_empty() {
                return Err(CommissioningError::InvalidConfig(
                    "network: Wi-Fi ssid must not be empty",
                ));
            }
            if creds.ssid.len() > 32 {
                return Err(CommissioningError::InvalidConfig(
                    "network: Wi-Fi ssid must be ≤32 bytes",
                ));
            }
            if creds.credentials.len() > 64 {
                return Err(CommissioningError::InvalidConfig(
                    "network: Wi-Fi credentials must be ≤64 bytes",
                ));
            }
        }
        Ok(Self {
            stage: Stage::SecurePairing,
            pase_attestation_challenge: cfg.pase_attestation_challenge,
            fabric: cfg.fabric.clone(),
            paa_trust_store: cfg.paa_trust_store.clone(),
            cd_signing_roots: cfg.cd_signing_roots.clone(),
            setup_payload: cfg.setup_payload.clone(),
            commissioner_node_id: cfg.commissioner_node_id,
            assigned_node_id: cfg.assigned_node_id,
            ipk_epoch_key: cfg.ipk_epoch_key,
            case_admin_subject: cfg.case_admin_subject,
            admin_vendor_id: cfg.admin_vendor_id,
            now: cfg.now,
            rng: cfg.rng,
            pai_der: None,
            dac_der: None,
            attestation_nonce: None,
            attestation_response: None,
            csr_nonce: None,
            csr_response: None,
            verified_csr: None,
            issued_noc: None,
            issued_noc_public_key: None,
            network: cfg.network,
            failsafe_expiry_seconds: 60,
            connect_max_time_seconds: 0,
            breadcrumb_counter: 1,
            awaiting_case_session: false,
            awaiting: None,
            pending_action: None,
            last_failure: None,
        })
    }

    /// Current cursor position. Useful for logging + tests.
    #[must_use]
    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// The operational-network credentials captured at construction.
    /// Read by tests; the network-provisioning dispatch and routing read
    /// the field directly.
    #[allow(dead_code)] // Only tests call this accessor.
    pub(crate) fn network(&self) -> &NetworkCredentials {
        &self.network
    }

    /// **Test-only.** Jumps the cursor to `stage` and applies any opt-in
    /// seeds in `seeds`. Consumes `self` and returns the repositioned
    /// `Commissioner`.
    ///
    /// Use this in integration tests when a real M6.4 attestation +
    /// NOC-issuance flow is not yet available (the M6.4.6 real-fixture
    /// e2e driver is still operator-touch deferred — see
    /// `TODO-1.0.md`). Never use in production code.
    ///
    /// Behind the `__test_shortcuts` feature flag.
    #[cfg(feature = "__test_shortcuts")]
    #[must_use]
    pub fn position_at_stage_for_test(mut self, stage: Stage, seeds: TestStateSeeds) -> Self {
        self.stage = stage;
        if let Some(pk) = seeds.synthetic_noc_pubkey {
            self.issued_noc_public_key = Some(pk);
        }
        self
    }

    /// Drive the state machine forward.
    ///
    /// Returns the next [`Action`] the caller must perform. Idempotent:
    /// calling `poll` twice without an intervening `on_response` returns
    /// the same `Action`.
    ///
    /// # Errors
    ///
    /// Returns the typed error that caused a transition into
    /// [`Stage::Failed`] — when this happens, the cursor advances to
    /// `Failed` and the next `poll()` call emits an
    /// [`Action::Abort`] with a rendered summary of the failure.
    #[cfg_attr(feature = "tracing", instrument(skip(self), fields(stage = ?self.stage)))]
    pub fn poll(&mut self) -> Result<Action, CommissioningError> {
        if let Some(act) = self.pending_action.clone() {
            return Ok(act);
        }
        let action = self.dispatch_stage()?;
        self.pending_action = Some(action.clone());
        Ok(action)
    }

    /// Failsafe extension (seconds) for `Stage::FailsafeBeforeNetworkEnable`.
    ///
    /// Uses the device-reported `ConnectMaxTimeSeconds` when non-zero,
    /// else [`DEFAULT_CONNECT_MAX_TIME_SECONDS`]. Sized to give the device
    /// room to associate with the operational network (Thread attach is
    /// slower than Wi-Fi association) before the failsafe expires.
    fn network_enable_failsafe_seconds(&self) -> u16 {
        if self.connect_max_time_seconds > 0 {
            self.connect_max_time_seconds
        } else {
            DEFAULT_CONNECT_MAX_TIME_SECONDS
        }
    }

    /// Record the device's `ConnectMaxTimeSeconds` (`NetworkCommissioning`
    /// attribute `0x0003`), read alongside the `FeatureMap` at
    /// `Stage::ReadNetworkCommissioningInfo`. Consumed by
    /// [`Self::network_enable_failsafe_seconds`] to size the
    /// `FailsafeBeforeNetworkEnable` extension. Called by the driver's
    /// read-dispatch after the `FeatureMap` response is applied; a `0`
    /// value (unread/absent) leaves the default in force.
    ///
    /// The driver is the only caller, so this is gated on its feature; without
    /// it the field simply keeps its default and
    /// [`Self::network_enable_failsafe_seconds`] falls back to
    /// [`DEFAULT_CONNECT_MAX_TIME_SECONDS`].
    #[cfg(feature = "driver")]
    pub(crate) fn set_connect_max_time_seconds(&mut self, seconds: u16) {
        self.connect_max_time_seconds = seconds;
    }

    /// The device-reported `ConnectMaxTimeSeconds`, or `0` if the device
    /// hasn't reported it yet (default before
    /// `Stage::ReadNetworkCommissioningInfo` completes, or the device
    /// reported `0`).
    ///
    /// Consumed by the driver ([`crate::driver::commission`]) to size the
    /// BLE-path `ConnectNetwork` response deadline from the same value that
    /// [`Self::network_enable_failsafe_seconds`] uses for the failsafe
    /// extension (both are sized from it when the device reports it, so the
    /// deadline never fires before the extension would expire; an unreported
    /// value leaves the fixed 60 s deadline).
    ///
    /// Driver-only, like its setter.
    #[cfg(feature = "driver")]
    #[must_use]
    pub(crate) fn connect_max_time_seconds(&self) -> u16 {
        self.connect_max_time_seconds
    }

    /// Helper: emit an `ArmFailsafe` action at the current stage.
    ///
    /// Used by both `Stage::ArmFailsafe` and
    /// `Stage::FailsafeBeforeNetworkEnable`, which share identical action
    /// logic. The failsafe expiry differs: the first arm uses the
    /// device's `failsafe_expiry_length_seconds`; the pre-`ConnectNetwork`
    /// extension uses [`Self::network_enable_failsafe_seconds`].
    fn arm_failsafe_action(&mut self) -> Action {
        use crate::clusters::general_commissioning as gc;
        use crate::state_machine::action::SessionContext;
        let expiry_seconds = if self.stage == Stage::FailsafeBeforeNetworkEnable {
            self.network_enable_failsafe_seconds()
        } else {
            self.failsafe_expiry_seconds
        };
        let breadcrumb = self.next_breadcrumb();
        let payload = gc::encode_arm_fail_safe(expiry_seconds, breadcrumb);
        self.awaiting = Some(Expectation::ArmFailsafeResponse);
        Action::Invoke {
            session: SessionContext::Pase,
            endpoint: 0,
            cluster: gc::CLUSTER_ID,
            command: gc::command_id::ARM_FAIL_SAFE,
            payload,
            expect: Expectation::ArmFailsafeResponse,
        }
    }

    /// Compute the next [`Action`] for the current [`Stage`].
    ///
    /// Called by [`Self::poll`] only when there is no cached
    /// `pending_action`. Walks `Stage::SecurePairing` forward to the
    /// first wire stage by self-recursion.
    // Lint carve-out: the per-stage arms each carry their own
    // payload-shape comments, so collapsing them into smaller helpers
    // would obscure the cluster-command mapping the function
    // documents. Each new stage adds a small fixed arm.
    #[allow(clippy::too_many_lines)]
    fn dispatch_stage(&mut self) -> Result<Action, CommissioningError> {
        use crate::clusters::general_commissioning as gc;
        use crate::state_machine::action::SessionContext;
        match self.stage {
            Stage::SecurePairing => {
                // Entry → first wire stage. Advance and re-dispatch.
                self.stage = Stage::ReadCommissioningInfo;
                self.dispatch_stage()
            }
            Stage::ReadCommissioningInfo => {
                self.awaiting = Some(Expectation::CommissioningInfo);
                Ok(Action::ReadAttribute {
                    session: SessionContext::Pase,
                    endpoint: 0,
                    cluster: gc::CLUSTER_ID,
                    attributes: &[
                        // GeneralCommissioning attribute ids per spec §11.10.6
                        // (confirmed against a real device's report).
                        0x0000, // Breadcrumb
                        0x0001, // BasicCommissioningInfo (failsafe_expiry_length_seconds, …)
                        0x0002, // RegulatoryConfig
                        0x0004, // SupportsConcurrentConnection
                    ],
                    expect: Expectation::CommissioningInfo,
                })
            }
            Stage::ArmFailsafe | Stage::FailsafeBeforeNetworkEnable => {
                Ok(self.arm_failsafe_action())
            }
            Stage::ConfigRegulatory => {
                let breadcrumb = self.next_breadcrumb();
                // FIXME(temp, uncommitted): hardcoding IndoorOutdoor(2) exceeds
                // stricter devices' LocationCapability → SetRegulatoryConfig
                // returns ValueOutsideRange (errorCode 1). The chip-faithful fix
                // is to read attr 0x03 (LocationCapability) in ReadCommissioningInfo
                // and echo it here. Indoor(0) is the safe universal value (accepted
                // by Indoor-only AND IndoorOutdoor devices) — minimal unblock.
                let payload = gc::encode_set_regulatory_config(
                    gc::RegulatoryLocation::Indoor,
                    "XX",
                    breadcrumb,
                );
                self.awaiting = Some(Expectation::SetRegulatoryConfigResponse);
                Ok(Action::Invoke {
                    session: SessionContext::Pase,
                    endpoint: 0,
                    cluster: gc::CLUSTER_ID,
                    command: gc::command_id::SET_REGULATORY_CONFIG,
                    payload,
                    expect: Expectation::SetRegulatoryConfigResponse,
                })
            }
            Stage::SendPaiCertRequest => {
                use crate::noc::{encode_certificate_chain_request, CertChainType};
                let payload = encode_certificate_chain_request(CertChainType::Pai);
                self.awaiting = Some(Expectation::PaiCertChainResponse);
                Ok(Action::Invoke {
                    session: SessionContext::Pase,
                    endpoint: 0,
                    cluster: 0x003E,
                    command: 0x02,
                    payload,
                    expect: Expectation::PaiCertChainResponse,
                })
            }
            Stage::SendDacCertRequest => {
                use crate::noc::{encode_certificate_chain_request, CertChainType};
                let payload = encode_certificate_chain_request(CertChainType::Dac);
                self.awaiting = Some(Expectation::DacCertChainResponse);
                Ok(Action::Invoke {
                    session: SessionContext::Pase,
                    endpoint: 0,
                    cluster: 0x003E,
                    command: 0x02,
                    payload,
                    expect: Expectation::DacCertChainResponse,
                })
            }
            Stage::SendAttestationRequest => {
                use crate::noc::encode_attestation_request;
                let mut nonce = [0u8; 32];
                self.rng
                    .fill(&mut nonce)
                    .map_err(CommissioningError::from)?;
                let payload = encode_attestation_request(&nonce);
                self.attestation_nonce = Some(nonce);
                self.awaiting = Some(Expectation::AttestationResponse);
                Ok(Action::Invoke {
                    session: SessionContext::Pase,
                    endpoint: 0,
                    cluster: 0x003E,
                    command: 0x00,
                    payload,
                    expect: Expectation::AttestationResponse,
                })
            }
            Stage::AttestationVerification => {
                match self.run_attestation_verification() {
                    Ok(()) => {
                        self.advance(Stage::SendOpCertSigningRequest);
                        self.dispatch_stage()
                    }
                    Err(err) => {
                        // Poll-time failure: align with the contract documented
                        // on `poll()` — cursor advances to `Failed`, the next
                        // `poll()` emits `Action::Abort` with a rendered reason.
                        self.last_failure = Some(err.to_string());
                        self.stage = Stage::Failed;
                        self.awaiting = None;
                        self.pending_action = None;
                        Err(err)
                    }
                }
            }
            Stage::SendOpCertSigningRequest => {
                use crate::noc::encode_csr_request;
                let mut nonce = [0u8; 32];
                self.rng
                    .fill(&mut nonce)
                    .map_err(CommissioningError::from)?;
                // Spec §11.18.5.5 `CSRRequest`. `is_for_update_noc` is
                // hard-coded false: M6.4 only commissions new fabrics.
                let payload = encode_csr_request(&nonce, false);
                self.csr_nonce = Some(nonce);
                self.awaiting = Some(Expectation::CsrResponse);
                Ok(Action::Invoke {
                    session: SessionContext::Pase,
                    endpoint: 0,
                    cluster: 0x003E,
                    command: 0x04,
                    payload,
                    expect: Expectation::CsrResponse,
                })
            }
            Stage::ValidateCsr => {
                // Off-wire: M6.3's three-check verify_csr_response gate.
                self.run_validate_csr()?;
                self.advance(Stage::GenerateNocChain);
                self.dispatch_stage()
            }
            Stage::GenerateNocChain => {
                // Off-wire: build + sign the NOC under the fabric's RCAC.
                self.run_generate_noc_chain()?;
                self.advance(Stage::SendTrustedRootCert);
                self.dispatch_stage()
            }
            Stage::SendTrustedRootCert => {
                use crate::noc::encode_add_trusted_root;
                // RCAC is already TLV-serialisable via matter-cert's
                // `to_tlv`. Surfaces as NocError::CertBuild on the rare
                // re-serialisation failure path (codec / extension shape
                // regression). Sanity: `FabricRecord::new_root_only` round-
                // tripped the cert through `verify_signed_by` at
                // construction, so the bytes are well-formed by here.
                let rcac_tlv =
                    self.fabric.root_cert.to_tlv().map_err(|e| {
                        CommissioningError::from(crate::noc::NocError::CertBuild(e))
                    })?;
                #[cfg(feature = "tracing")]
                tracing::debug!(
                    rcac_tlv = %crate::hexdump::hex(&rcac_tlv),
                    "sending AddTrustedRootCertificate"
                );
                let payload = encode_add_trusted_root(&rcac_tlv);
                self.awaiting = Some(Expectation::AddTrustedRootResponse);
                Ok(Action::Invoke {
                    session: SessionContext::Pase,
                    endpoint: 0,
                    cluster: 0x003E,
                    command: 0x0B,
                    payload,
                    expect: Expectation::AddTrustedRootResponse,
                })
            }
            Stage::SendNoc => {
                use crate::noc::encode_add_noc;
                let noc = self
                    .issued_noc
                    .as_ref()
                    .ok_or(CommissioningError::OutOfOrderResponse(self.stage))?;
                let noc_tlv = noc
                    .to_tlv()
                    .map_err(|e| CommissioningError::from(crate::noc::NocError::CertBuild(e)))?;
                // If this fabric runs a 3-tier RCAC -> ICAC -> NOC chain, the
                // device NOC was signed under the ICAC (see
                // `run_generate_noc_chain` -> `issue_noc`, which signs under
                // `fabric.icac_signer` whenever the fabric carries one). The
                // device therefore needs the ICAC certificate to assemble and
                // validate the chain, so we transmit it in AddNOC's optional
                // ICACValue field (spec §11.18.5.9 field 1). Flat RCAC -> NOC
                // fabrics carry no ICAC and the field is omitted, leaving the
                // M6.3 wire bytes unchanged.
                let icac_tlv = match self.fabric.icac_cert.as_ref() {
                    Some(icac) => Some(icac.to_tlv().map_err(|e| {
                        CommissioningError::from(crate::noc::NocError::CertBuild(e))
                    })?),
                    None => None,
                };
                let payload = encode_add_noc(
                    &noc_tlv,
                    icac_tlv.as_deref(),
                    &self.ipk_epoch_key,
                    self.case_admin_subject,
                    self.admin_vendor_id,
                );
                self.awaiting = Some(Expectation::NocResponse);
                Ok(Action::Invoke {
                    session: SessionContext::Pase,
                    endpoint: 0,
                    cluster: 0x003E,
                    command: 0x06,
                    payload,
                    expect: Expectation::NocResponse,
                })
            }
            Stage::ReadNetworkCommissioningInfo => {
                self.awaiting = Some(Expectation::NetworkCommissioningInfo);
                Ok(Action::ReadAttribute {
                    session: SessionContext::Pase,
                    endpoint: 0,
                    cluster: crate::clusters::network_commissioning::CLUSTER_ID,
                    attributes: &[
                        crate::clusters::network_commissioning::attribute_id::FEATURE_MAP,
                        // ConnectMaxTimeSeconds (spec §11.9.5.4) — sizes the
                        // FailsafeBeforeNetworkEnable extension. Thread
                        // attach is slower than Wi-Fi association.
                        crate::clusters::network_commissioning::attribute_id::CONNECT_MAX_TIME_SECONDS,
                    ],
                    expect: Expectation::NetworkCommissioningInfo,
                })
            }
            Stage::NetworkSetup => {
                use crate::clusters::network_commissioning as nc;
                // Select the provisioning command by the supplied
                // credential type. The FeatureMap-cross-check at
                // `Expectation::NetworkCommissioningInfo` guarantees the
                // device actually supports this network type before we
                // reach here, so `AlreadyOnNetwork` never lands in this
                // arm — treat it as an out-of-order state, not a silent
                // skip.
                let breadcrumb = self.next_breadcrumb();
                let (command, payload) = match &self.network {
                    NetworkCredentials::WiFi(creds) => (
                        nc::command_id::ADD_OR_UPDATE_WIFI_NETWORK,
                        nc::encode_add_or_update_wifi_network(
                            &creds.ssid,
                            &creds.credentials,
                            breadcrumb,
                        ),
                    ),
                    NetworkCredentials::Thread(dataset) => (
                        nc::command_id::ADD_OR_UPDATE_THREAD_NETWORK,
                        nc::encode_add_or_update_thread_network(dataset.as_bytes(), breadcrumb),
                    ),
                    NetworkCredentials::AlreadyOnNetwork => {
                        return Err(CommissioningError::OutOfOrderResponse(self.stage));
                    }
                };
                self.awaiting = Some(Expectation::NetworkConfigResponse);
                Ok(Action::Invoke {
                    session: SessionContext::Pase,
                    endpoint: 0,
                    cluster: nc::CLUSTER_ID,
                    command,
                    payload,
                    expect: Expectation::NetworkConfigResponse,
                })
            }
            Stage::NetworkEnable => {
                use crate::clusters::network_commissioning as nc;
                // `ConnectNetwork` takes an opaque `network_id`: the SSID
                // for Wi-Fi, the Extended PAN ID for Thread (spec §11.9.6.6).
                let breadcrumb = self.next_breadcrumb();
                let payload = match &self.network {
                    NetworkCredentials::WiFi(creds) => {
                        nc::encode_connect_network(&creds.ssid, breadcrumb)
                    }
                    NetworkCredentials::Thread(dataset) => {
                        nc::encode_connect_network(&dataset.ext_pan_id(), breadcrumb)
                    }
                    NetworkCredentials::AlreadyOnNetwork => {
                        return Err(CommissioningError::OutOfOrderResponse(self.stage));
                    }
                };
                self.awaiting = Some(Expectation::ConnectNetworkResponse);
                Ok(Action::Invoke {
                    session: SessionContext::Pase,
                    endpoint: 0,
                    cluster: nc::CLUSTER_ID,
                    command: nc::command_id::CONNECT_NETWORK,
                    payload,
                    expect: Expectation::ConnectNetworkResponse,
                })
            }
            Stage::EvictPreviousCaseSessions => {
                // New-fabric commissioning has no prior CASE session
                // to evict. M8 multi-fabric work will emit
                // Action::EvictCase here.
                self.advance(Stage::FindOperationalForComplete);
                self.dispatch_stage()
            }
            Stage::FindOperationalForComplete => {
                self.awaiting_case_session = true;
                Ok(Action::EstablishCase {
                    fabric_id: self.fabric.fabric_id,
                    peer_node_id: self.assigned_node_id,
                })
            }
            Stage::SendComplete => {
                let payload = gc::encode_commissioning_complete();
                self.awaiting = Some(Expectation::CommissioningCompleteResponse);
                Ok(Action::Invoke {
                    session: SessionContext::Case,
                    endpoint: 0,
                    cluster: gc::CLUSTER_ID,
                    command: gc::command_id::COMMISSIONING_COMPLETE,
                    payload,
                    expect: Expectation::CommissioningCompleteResponse,
                })
            }
            Stage::Cleanup => {
                let public_key = self
                    .issued_noc_public_key
                    .ok_or(CommissioningError::OutOfOrderResponse(self.stage))?;
                Ok(Action::Done(crate::state_machine::CommissionedFabric {
                    fabric: self.fabric.clone(),
                    peer_node_id: self.assigned_node_id,
                    peer_root_public_key: public_key,
                    terminated_at: Stage::Cleanup,
                }))
            }
            Stage::Failed => {
                // Subsequent poll() after a failure surfaces the Abort.
                // The state machine stays in Failed.
                self.awaiting = None;
                let reason = self
                    .last_failure
                    .clone()
                    .unwrap_or_else(|| "commissioning aborted".to_string());
                Ok(Action::Abort {
                    send_disarm_failsafe: true,
                    reason,
                })
            } // Every `Stage` variant has its own arm above. `Stage` is
              // `#[non_exhaustive]` for cross-crate consumers, but within
              // this crate the match is exhaustive — no `_ =>` arm needed.
        }
    }

    /// Feed a response payload back into the state machine.
    ///
    /// `expect` MUST match the [`Expectation`] from the last `poll()`'s
    /// emitted `Action`.
    ///
    /// # Errors
    ///
    /// - [`CommissioningError::OutOfOrderResponse`] if the state machine
    ///   isn't currently waiting for a response.
    /// - [`CommissioningError::UnexpectedResponseKind`] if `expect`
    ///   doesn't match the last `Action`'s `Expectation`. The cursor
    ///   does not advance.
    /// - [`CommissioningError::MalformedResponse`] if `payload` fails
    ///   to decode at the cluster-command level.
    /// - [`CommissioningError::DeviceImStatus`] if the device returned a
    ///   non-OK Interaction Model status.
    ///
    /// Any error other than `OutOfOrderResponse` and
    /// `UnexpectedResponseKind` transitions the cursor to
    /// [`Stage::Failed`]; the next `poll()` call emits
    /// [`Action::Abort`] with a rendered summary.
    #[cfg_attr(feature = "tracing", instrument(skip(self, payload), fields(stage = ?self.stage, expectation = ?expect)))]
    pub fn on_response(
        &mut self,
        expect: Expectation,
        payload: &[u8],
    ) -> Result<(), CommissioningError> {
        if expect == Expectation::CaseFailed {
            // CaseFailed bypasses the awaiting check — the caller
            // signals failure of the EstablishCase action explicitly,
            // and EstablishCase tracks readiness via
            // `awaiting_case_session`, not `awaiting`.
            if !self.awaiting_case_session {
                return Err(CommissioningError::OutOfOrderResponse(self.stage));
            }
            self.awaiting_case_session = false;
            self.stage = Stage::Failed;
            self.awaiting = None;
            self.pending_action = None;
            self.last_failure = Some(CommissioningError::CaseEstablishmentFailed.to_string());
            return Err(CommissioningError::CaseEstablishmentFailed);
        }
        let Some(awaiting) = self.awaiting else {
            return Err(CommissioningError::OutOfOrderResponse(self.stage));
        };
        if awaiting != expect {
            return Err(CommissioningError::UnexpectedResponseKind {
                expected: awaiting,
                got: expect,
            });
        }
        match self.handle_response(expect, payload) {
            Ok(()) => Ok(()),
            Err(err) => {
                self.last_failure = Some(err.to_string());
                self.stage = Stage::Failed;
                self.awaiting = None;
                self.pending_action = None;
                Err(err)
            }
        }
    }

    /// Signal that CASE establishment (mDNS find-operational + the
    /// SIGMA-I handshake — both M6.6 mechanics, owned by the driver)
    /// has succeeded. The state machine advances from
    /// [`Stage::FindOperationalForComplete`] to [`Stage::SendComplete`].
    ///
    /// # Errors
    ///
    /// Returns [`CommissioningError::OutOfOrderResponse`] if the state
    /// machine isn't currently awaiting CASE establishment (i.e., the
    /// cursor is not at `FindOperationalForComplete` or the
    /// `EstablishCase` action hasn't been emitted yet).
    #[cfg_attr(feature = "tracing", instrument(skip(self)))]
    pub fn on_case_established(&mut self) -> Result<(), CommissioningError> {
        if !self.awaiting_case_session {
            return Err(CommissioningError::OutOfOrderResponse(self.stage));
        }
        self.awaiting_case_session = false;
        self.advance(Stage::SendComplete);
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn handle_response(
        &mut self,
        expect: Expectation,
        payload: &[u8],
    ) -> Result<(), CommissioningError> {
        use crate::clusters::general_commissioning as gc;
        match expect {
            Expectation::CommissioningInfo => {
                Self::assert_tlv_well_formed(self.stage, payload)?;
                // Best-effort: scan the response for a BasicCommissioningInfo
                // struct and update failsafe_expiry_seconds. Malformed or
                // missing → keep the M6.4 fallback (60s) silently.
                if let Some(info) = gc::decode_basic_commissioning_info(payload) {
                    if info.failsafe_expiry_length_seconds > 0 {
                        self.failsafe_expiry_seconds = info.failsafe_expiry_length_seconds;
                    }
                    // Cap against the device's hard cumulative failsafe limit
                    // (Matter §11.10.5.1): an `ArmFailSafe` whose expiry exceeds
                    // `MaxCumulativeFailsafeSeconds` is guaranteed to be rejected
                    // (BoundsExceeded), so never round-trip such a value.
                    if info.max_cumulative_failsafe_seconds > 0 {
                        self.failsafe_expiry_seconds = self
                            .failsafe_expiry_seconds
                            .min(info.max_cumulative_failsafe_seconds);
                    }
                }
                self.advance(Stage::ArmFailsafe);
                Ok(())
            }
            Expectation::ArmFailsafeResponse => {
                let resp = gc::decode_arm_fail_safe_response(payload)?;
                if resp.error_code != 0 {
                    return Err(CommissioningError::DeviceImStatus {
                        stage: self.stage,
                        im_status: u16::from(resp.error_code),
                    });
                }
                let next = match self.stage {
                    Stage::ArmFailsafe => Stage::ConfigRegulatory,
                    Stage::FailsafeBeforeNetworkEnable => Stage::NetworkEnable,
                    other => {
                        return Err(CommissioningError::OutOfOrderResponse(other));
                    }
                };
                self.advance(next);
                Ok(())
            }
            Expectation::SetRegulatoryConfigResponse => {
                let resp = gc::decode_set_regulatory_config_response(payload)?;
                if resp.error_code != 0 {
                    return Err(CommissioningError::DeviceImStatus {
                        stage: Stage::ConfigRegulatory,
                        im_status: u16::from(resp.error_code),
                    });
                }
                self.advance(Stage::SendPaiCertRequest);
                Ok(())
            }
            Expectation::PaiCertChainResponse => {
                let resp = crate::noc::decode_certificate_chain_response(payload)?;
                self.pai_der = Some(resp.certificate);
                self.advance(Stage::SendDacCertRequest);
                Ok(())
            }
            Expectation::DacCertChainResponse => {
                let resp = crate::noc::decode_certificate_chain_response(payload)?;
                self.dac_der = Some(resp.certificate);
                self.advance(Stage::SendAttestationRequest);
                Ok(())
            }
            Expectation::AttestationResponse => {
                let resp = crate::noc::decode_attestation_response(payload)?;
                self.attestation_response = Some(resp);
                self.advance(Stage::AttestationVerification);
                Ok(())
            }
            Expectation::CsrResponse => {
                let resp = crate::noc::decode_csr_response(payload)?;
                self.csr_response = Some(resp);
                self.advance(Stage::ValidateCsr);
                Ok(())
            }
            Expectation::AddTrustedRootResponse => {
                // `AddTrustedRootCertificate` has no typed response —
                // success is a status-only ack at the Interaction Model
                // layer. The caller surfaces the IM status as a 1-byte
                // payload: `0x00` = success, anything else = error.
                if payload.first() != Some(&0u8) {
                    return Err(CommissioningError::DeviceImStatus {
                        stage: Stage::SendTrustedRootCert,
                        im_status: u16::from(payload.first().copied().unwrap_or(0xFF)),
                    });
                }
                self.advance(Stage::SendNoc);
                Ok(())
            }
            Expectation::NocResponse => {
                let resp = crate::noc::decode_noc_response(payload)?;
                if resp.status != 0 {
                    return Err(CommissioningError::DeviceImStatus {
                        stage: Stage::SendNoc,
                        im_status: u16::from(resp.status),
                    });
                }
                self.advance(Stage::ReadNetworkCommissioningInfo);
                Ok(())
            }
            Expectation::NetworkCommissioningInfo => {
                use crate::clusters::network_commissioning as nc;
                use crate::state_machine::NetworkKind;
                // EMPTY payload = the driver's absent-cluster sentinel: the
                // device answered the read with unsupported-path statuses
                // instead of data (it exposes no NetworkCommissioning cluster
                // — observed on a real device: eufy E31 lock commissioned
                // over IP). With no credentials to provision that is fine —
                // the device is already reachable on its operational network
                // — so skip provisioning exactly like the AlreadyOnNetwork
                // feature-map path below. With Wi-Fi/Thread credentials the
                // device CANNOT be provisioned: surface the typed error, not
                // a framing error.
                if payload.is_empty() {
                    match &self.network {
                        NetworkCredentials::AlreadyOnNetwork => {
                            self.advance(Stage::EvictPreviousCaseSessions);
                            return Ok(());
                        }
                        NetworkCredentials::WiFi(_) => {
                            return Err(CommissioningError::NetworkFeatureUnsupported {
                                needed: NetworkKind::WiFi,
                            });
                        }
                        NetworkCredentials::Thread(_) => {
                            return Err(CommissioningError::NetworkFeatureUnsupported {
                                needed: NetworkKind::Thread,
                            });
                        }
                    }
                }
                let features = nc::decode_feature_map(payload)?;
                // A FeatureMap with no recognised interface bit is
                // malformed regardless of the supplied credentials — a
                // NetworkCommissioning cluster always exposes at least one
                // of Wi-Fi / Thread / Ethernet.
                if features.is_empty() {
                    return Err(CommissioningError::MalformedResponse(
                        Stage::ReadNetworkCommissioningInfo,
                    ));
                }
                // Route by the *supplied* credential type, cross-checked
                // against the device FeatureMap. This resolves the
                // dual-stack ordering ambiguity (a Wi-Fi+Thread device is
                // provisioned per the caller's chosen credential type, not
                // by feature-bit order) and rejects a mismatch (credential
                // type absent from the FeatureMap) instead of silently
                // skipping provisioning.
                match &self.network {
                    NetworkCredentials::WiFi(_) => {
                        if !features.contains(nc::NetworkCommissioningFeature::WIFI) {
                            return Err(CommissioningError::NetworkFeatureUnsupported {
                                needed: NetworkKind::WiFi,
                            });
                        }
                        self.advance(Stage::NetworkSetup);
                    }
                    NetworkCredentials::Thread(_) => {
                        if !features.contains(nc::NetworkCommissioningFeature::THREAD) {
                            return Err(CommissioningError::NetworkFeatureUnsupported {
                                needed: NetworkKind::Thread,
                            });
                        }
                        self.advance(Stage::NetworkSetup);
                    }
                    NetworkCredentials::AlreadyOnNetwork => {
                        // No credentials to provision: the device is
                        // already reachable on its operational network (IP
                        // commissioning reached it there — e.g. a
                        // second-fabric commission of an already-provisioned
                        // device, or an Ethernet-only device). Mirror
                        // chip's AutoCommissioner: skip the network
                        // sub-cursor entirely (observed necessary on a real
                        // device: Tapo P110M, M6.6.5 validation).
                        self.advance(Stage::EvictPreviousCaseSessions);
                    }
                }
                Ok(())
            }
            Expectation::NetworkConfigResponse => {
                use crate::clusters::network_commissioning as nc;
                let resp = nc::decode_network_config_response(Stage::NetworkSetup, payload)?;
                if resp.networking_status != 0 {
                    return Err(CommissioningError::NetworkRejected {
                        stage: Stage::NetworkSetup,
                        networking_status: resp.networking_status,
                        debug_text: resp.debug_text,
                        remediation_hint: nc::remediation_for(resp.networking_status),
                    });
                }
                self.advance(Stage::FailsafeBeforeNetworkEnable);
                Ok(())
            }
            Expectation::ConnectNetworkResponse => {
                use crate::clusters::network_commissioning as nc;
                let resp = nc::decode_connect_network_response(Stage::NetworkEnable, payload)?;
                if resp.networking_status != 0 {
                    return Err(CommissioningError::NetworkRejected {
                        stage: Stage::NetworkEnable,
                        networking_status: resp.networking_status,
                        debug_text: resp.debug_text,
                        remediation_hint: nc::remediation_for(resp.networking_status),
                    });
                }
                self.advance(Stage::EvictPreviousCaseSessions);
                Ok(())
            }
            Expectation::CommissioningCompleteResponse => {
                let (error_code, _debug) =
                    gc::decode_commissioning_error_response(Stage::SendComplete, payload)?;
                if error_code != 0 {
                    return Err(CommissioningError::DeviceImStatus {
                        stage: Stage::SendComplete,
                        im_status: u16::from(error_code),
                    });
                }
                self.advance(Stage::Cleanup);
                Ok(())
            }
            // `Expectation::CaseFailed` is handled by `on_response`'s
            // pre-awaiting fast path and never reaches handle_response.
            _ => Err(CommissioningError::OutOfOrderResponse(self.stage)),
        }
    }

    fn next_breadcrumb(&mut self) -> u64 {
        let b = self.breadcrumb_counter;
        self.breadcrumb_counter = b.saturating_add(1);
        b
    }

    fn advance(&mut self, next: Stage) {
        self.stage = next;
        self.awaiting = None;
        self.pending_action = None;
    }

    /// Off-wire attestation verification chain.
    ///
    /// Consumes the PAI/DAC DER + `AttestationResponse` + nonce captured
    /// by [`Stage::SendPaiCertRequest`] / [`Stage::SendDacCertRequest`]
    /// / [`Stage::SendAttestationRequest`] and runs M6.2's verifier
    /// chain end-to-end:
    ///
    /// 1. Parse PAI/DAC DER.
    /// 2. `verify_chain` — webpki path validation + Matter VID/PID
    ///    overlay (M6.2.2).
    /// 3. `verify_attestation_response` — ECDSA signature over
    ///    `attestation_elements || attestation_challenge` (M6.2.3).
    /// 4. `extract_attestation_elements_fields` — pull the
    ///    `attestation_nonce` echo + CD bytes out of the TLV blob.
    /// 5. Confirm the device echoed the nonce we sent.
    /// 6. `verify_certification_declaration` — verify the CSA-signed CD
    ///    embedded in `attestation_elements` against
    ///    [`crate::attestation::CdSigningRoots`] and confirm the
    ///    declared VID/PID match what the DAC subject claimed.
    fn run_attestation_verification(&mut self) -> Result<(), CommissioningError> {
        use crate::attestation::profile::{verify_attestation_cert_format, CertRole};
        use crate::attestation::{
            extract_attestation_elements_fields, verify_attestation_response, verify_chain,
            AttestationError, Dac, Pai,
        };

        let pai_der = self
            .pai_der
            .as_ref()
            .ok_or(CommissioningError::OutOfOrderResponse(self.stage))?;
        let dac_der = self
            .dac_der
            .as_ref()
            .ok_or(CommissioningError::OutOfOrderResponse(self.stage))?;
        let response = self
            .attestation_response
            .as_ref()
            .ok_or(CommissioningError::OutOfOrderResponse(self.stage))?;
        let expected_nonce = self
            .attestation_nonce
            .ok_or(CommissioningError::OutOfOrderResponse(self.stage))?;

        // 1. Parse chain certs.
        let pai = Pai::from_der(pai_der)?;
        let dac = Dac::from_der(dac_der)?;
        #[cfg(feature = "tracing")]
        tracing::debug!(
            dac_der = %crate::hexdump::hex(dac_der),
            pai_der = %crate::hexdump::hex(pai_der),
            "verifying attestation chain"
        );

        // 1b. Certificate format profile (Matter §6.2.2) — mirrors chip's
        //     `VerifyAttestationCertificateFormat`. `rustls-webpki` ignores
        //     the KeyUsage extension and never requires SKID/AKID, so a
        //     counterfeit DAC carrying `keyUsage = keyCertSign` (a signing
        //     key posing as a device leaf) would pass path validation. We
        //     enforce the DAC/PAI X.509 profile ourselves, before trusting
        //     the chain. PAI first, then DAC — chip's order.
        verify_attestation_cert_format(pai.der(), CertRole::Pai)?;
        verify_attestation_cert_format(dac.der(), CertRole::Dac)?;

        // 2. Chain validation (M6.2.2 — webpki path validation + VID/PID overlay).
        //    The returned `ChainVerification` carries the VID/PID that
        //    both webpki and the Matter overlay agreed on; we re-use
        //    those for the CD check below so a single source of truth
        //    drives both the chain validation and the CD VID/PID
        //    equality check.
        let chain = verify_chain(&dac, &pai, &self.paa_trust_store, self.now)?;

        // 3. AttestationResponse signature (M6.2.3).
        verify_attestation_response(response, &self.pase_attestation_challenge, dac.public_key())?;

        // 4. Extract attestation_elements fields: CD bytes (M6.4.3 will verify),
        //    nonce echo, timestamp.
        let fields = extract_attestation_elements_fields(&response.attestation_elements)?;
        if fields.attestation_nonce != expected_nonce {
            return Err(CommissioningError::Attestation(
                AttestationError::ResponseElementsMalformed,
            ));
        }

        // 5. CD verification — verify the device's declared VID/PID
        //    against the CSA-signed Certification Declaration extracted
        //    from `attestation_elements`.
        #[cfg(feature = "tracing")]
        tracing::debug!(
            cd_cms = %crate::hexdump::hex(&fields.certification_declaration),
            "verifying certification declaration"
        );
        crate::attestation::verify_certification_declaration_with_paa(
            &fields.certification_declaration,
            chain.vendor_id,
            chain.product_id,
            &self.cd_signing_roots,
            chain.paa_skid.as_deref(),
        )?;
        Ok(())
    }

    /// Off-wire CSR verification (M6.4.4 `Stage::ValidateCsr`).
    ///
    /// Consumes the `CsrResponse` captured by `Stage::SendOpCertSigningRequest`
    /// plus the DAC DER captured earlier by `Stage::SendDacCertRequest`,
    /// and runs M6.3's `verify_csr_response` three-check atomic gate:
    ///
    /// 1. PKCS#10 self-signature on the embedded CSR.
    /// 2. The device's `CSRNonce` echo equals the commissioner-issued nonce.
    /// 3. The DAC's attestation signature over
    ///    `nocsr_elements || attestation_challenge`.
    fn run_validate_csr(&mut self) -> Result<(), CommissioningError> {
        use crate::attestation::Dac;
        use crate::noc::verify_csr_response;

        let resp = self
            .csr_response
            .as_ref()
            .ok_or(CommissioningError::OutOfOrderResponse(self.stage))?;
        let dac_der = self
            .dac_der
            .as_ref()
            .ok_or(CommissioningError::OutOfOrderResponse(self.stage))?;
        let csr_nonce = self
            .csr_nonce
            .ok_or(CommissioningError::OutOfOrderResponse(self.stage))?;

        let dac = Dac::from_der(dac_der)?;
        let verified = verify_csr_response(
            &resp.nocsr_elements,
            &resp.attestation_signature,
            &csr_nonce,
            &self.pase_attestation_challenge,
            dac.public_key(),
        )?;
        self.verified_csr = Some(verified);
        Ok(())
    }

    /// Off-wire NOC issuance (M6.4.4 `Stage::GenerateNocChain`).
    ///
    /// Consumes the [`crate::noc::VerifiedCsr`] populated by
    /// [`Self::run_validate_csr`] and mints a NOC signed by the fabric's
    /// RCAC via M6.3's `issue_noc`.
    ///
    /// Validity window: M6.4 uses `(self.now, MatterTime::NO_EXPIRY)` —
    /// the same convention `issue_noc`'s own unit test uses. M8 may
    /// tighten this to a bounded operational-cert lifetime per Matter
    /// Core Spec §6.4 once persistence + rotation policy lands.
    ///
    /// CATs (CASE Authenticated Tags) are empty in M6.4; tag-based
    /// access control comes later.
    fn run_generate_noc_chain(&mut self) -> Result<(), CommissioningError> {
        use crate::noc::issue_noc;

        let verified = self
            .verified_csr
            .as_ref()
            .ok_or(CommissioningError::OutOfOrderResponse(self.stage))?;
        let noc = issue_noc(
            &self.fabric,
            verified,
            self.assigned_node_id,
            &[],
            (self.now, MatterTime::NO_EXPIRY),
            self.rng.as_ref(),
        )?;
        // Cache the NOC public key (the same bytes the verified CSR
        // committed to) for later use — currently only consumed by
        // M6.4.5's PASE -> CASE handoff in `CommissionedFabric`. Stored
        // here so the SendNoc stage doesn't have to re-derive it.
        self.issued_noc_public_key = Some(*verified.public_key.as_bytes());
        self.issued_noc = Some(noc);
        Ok(())
    }

    /// Require `payload` to be exactly one anonymous structure: closed by its
    /// own end-of-container, with nothing after it. The members are skipped,
    /// not decoded.
    fn assert_tlv_well_formed(stage: Stage, payload: &[u8]) -> Result<(), CommissioningError> {
        use matter_codec::{ContainerKind, Element, Tag, TlvReader};
        let mut reader = TlvReader::new(payload);
        match reader
            .next()
            .map_err(|_| CommissioningError::MalformedResponse(stage))?
        {
            Some(Element::ContainerStart {
                tag: Tag::Anonymous,
                kind: ContainerKind::Structure,
            }) => {}
            _ => return Err(CommissioningError::MalformedResponse(stage)),
        }
        // Skip the members (contents are ignored since M6.4.1) through the
        // struct's own end-of-container. Stopping at the first
        // `ContainerEnd`, as this used to, accepted a struct truncated after
        // any closed nested container, and bytes after the close.
        reader
            .finish()
            .map_err(|_| CommissioningError::MalformedResponse(stage))
    }
}

/// Test-only state seeds for [`Commissioner::position_at_stage_for_test`].
///
/// Each field is `None` by default — the caller opts in to each seed
/// explicitly. **Never use in production.**
#[cfg(feature = "__test_shortcuts")]
#[derive(Default, Debug, Clone, Copy)]
pub struct TestStateSeeds {
    /// Override `issued_noc_public_key` (normally populated when the
    /// state machine actually issues a NOC). Set to a synthetic SEC1-
    /// uncompressed P-256 byte pattern (e.g. `[0xCC; 65]`) when
    /// fast-forwarding past the NOC-issuance stages.
    pub synthetic_noc_pubkey: Option<[u8; 65]>,
}

#[cfg(test)]
mod tests {
    // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::attestation::CdSigningRoots;
    use crate::noc::{FabricRecord, NocRng, SystemNocRng};
    use crate::setup::{
        CommissioningFlow, DiscoveryCapabilities, Discriminator, Passcode, SetupPayload,
    };
    use crate::state_machine::{Action, Expectation};
    use crate::PaaTrustStore;
    use matter_cert::time::MatterTime;
    use matter_crypto::{RingSigner, Signer};
    use std::sync::Arc;

    fn make_setup_payload() -> SetupPayload {
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

    fn make_fabric_record() -> FabricRecord {
        let (signer, _pkcs8) = RingSigner::generate().unwrap();
        let signer: Arc<dyn Signer> = Arc::new(signer);
        FabricRecord::new_root_only(
            /* fabric_id */ 0x0000_0000_0000_0001,
            signer,
            /* not_before */ MatterTime::from_unix_secs(1_704_067_200),
            /* not_after */ MatterTime::from_unix_secs(1_735_689_600),
            /* rcac_id */ 42,
            &SystemNocRng,
        )
        .unwrap()
    }

    /// A fabric that issues a 3-tier RCAC -> ICAC -> NOC chain: the RCAC from
    /// [`make_fabric_record`] with a freshly-minted ICAC attached (so
    /// `issue_noc` signs the device NOC under the ICAC and `SendNoc` must
    /// transmit the ICAC alongside it).
    fn make_fabric_record_with_icac() -> FabricRecord {
        let mut fabric = make_fabric_record();
        let (icac_signer, _pkcs8) = RingSigner::generate().unwrap();
        let icac_public_key =
            matter_cert::PublicKey::from_slice(icac_signer.public_key().as_bytes()).unwrap();
        let icac = crate::noc::issue_icac(
            &fabric,
            /* icac_id */ 7,
            &icac_public_key,
            (
                MatterTime::from_unix_secs(1_704_067_200),
                MatterTime::NO_EXPIRY,
            ),
            &SystemNocRng,
        )
        .unwrap();
        fabric.icac_signer = Some(Arc::new(icac_signer));
        fabric.icac_cert = Some(icac);
        fabric
    }

    /// True iff the encoded `AddNOC` payload carries an `ICACValue` (field 1,
    /// spec §11.18.5.9). Scans the top-level struct members for a
    /// context-1 element; every `AddNOC` field is a scalar/byte-string, so
    /// no descent past the outer container is needed.
    fn add_noc_payload_has_icac(payload: &[u8]) -> bool {
        use matter_codec::{Element, Tag, TlvReader};
        let mut r = TlvReader::new(payload);
        match r.next().unwrap() {
            Some(Element::ContainerStart { .. }) => {}
            other => panic!("expected an AddNOC struct, got {other:?}"),
        }
        while let Some(el) = r.next().unwrap() {
            match el {
                Element::ContainerEnd => break,
                Element::Scalar {
                    tag: Tag::Context(1),
                    ..
                } => return true,
                _ => {}
            }
        }
        false
    }

    /// `assert_tlv_well_formed` requires the struct's own end-of-container and
    /// nothing after it. It used to stop at the first `ContainerEnd` it saw,
    /// so a struct cut off after a closed nested container passed.
    #[test]
    fn assert_tlv_well_formed_requires_the_top_level_close() {
        let check =
            |b: &[u8]| Commissioner::assert_tlv_well_formed(Stage::ReadCommissioningInfo, b);
        // { ctx1: { ctx0: 7 } }
        let full = [0x15, 0x35, 0x01, 0x24, 0x00, 0x07, 0x18, 0x18];
        assert!(check(&full).is_ok());
        for n in 0..full.len() {
            assert!(check(&full[..n]).is_err(), "prefix of {n} bytes accepted");
        }
        for suffix in [&[0x00][..], &[0x15, 0x18], &[0x18]] {
            let mut tailed = full.to_vec();
            tailed.extend_from_slice(suffix);
            assert!(
                check(&tailed).is_err(),
                "accepted with trailing {suffix:02x?}"
            );
        }
    }

    #[test]
    fn send_noc_includes_icac_for_three_tier_fabric() {
        let fabric = make_fabric_record_with_icac();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
        let mut sm =
            Commissioner::new(base_config(&fabric, &setup, &paa, &cd, rng)).expect("valid config");
        // `SendNoc` reads `issued_noc` + `fabric.icac_cert`; the NOC bytes are
        // irrelevant to whether the ICAC is attached, so stub the NOC and jump
        // straight to the stage under test.
        sm.issued_noc = Some(fabric.root_cert.clone());
        sm.stage = Stage::SendNoc;
        match sm.dispatch_stage().expect("SendNoc dispatch") {
            Action::Invoke {
                cluster,
                command,
                payload,
                ..
            } => {
                assert_eq!(cluster, 0x003E);
                assert_eq!(command, 0x06);
                assert!(
                    add_noc_payload_has_icac(&payload),
                    "AddNOC must carry the ICAC (ctx1) when the fabric runs a 3-tier chain"
                );
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn send_noc_omits_icac_for_flat_fabric() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
        let mut sm =
            Commissioner::new(base_config(&fabric, &setup, &paa, &cd, rng)).expect("valid config");
        sm.issued_noc = Some(fabric.root_cert.clone());
        sm.stage = Stage::SendNoc;
        match sm.dispatch_stage().expect("SendNoc dispatch") {
            Action::Invoke { payload, .. } => {
                assert!(
                    !add_noc_payload_has_icac(&payload),
                    "flat RCAC->NOC AddNOC must omit the ICAC field"
                );
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    fn base_config<'a>(
        fabric: &'a FabricRecord,
        setup: &'a SetupPayload,
        paa: &'a PaaTrustStore,
        cd: &'a crate::attestation::CdSigningRoots,
        rng: Arc<dyn NocRng>,
    ) -> CommissionerConfig<'a> {
        CommissionerConfig {
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
            network: NetworkCredentials::AlreadyOnNetwork,
        }
    }

    #[test]
    fn new_rejects_zero_commissioner_node_id() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
        let mut cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        cfg.commissioner_node_id = 0;
        // Cannot use `expect_err`: `Commissioner` does not impl Debug
        // because `FabricRecord` (a stored field) is not Debug.
        let Err(err) = Commissioner::new(cfg) else {
            panic!("zero commissioner_node_id should fail");
        };
        assert!(
            matches!(err, CommissioningError::InvalidConfig(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn new_rejects_zero_assigned_node_id() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
        let mut cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        cfg.assigned_node_id = 0;
        let Err(err) = Commissioner::new(cfg) else {
            panic!("zero assigned_node_id should fail");
        };
        assert!(
            matches!(err, CommissioningError::InvalidConfig(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn new_rejects_equal_commissioner_and_assigned_ids() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
        let mut cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        cfg.commissioner_node_id = 0x42;
        cfg.assigned_node_id = 0x42;
        let Err(err) = Commissioner::new(cfg) else {
            panic!("equal IDs should fail");
        };
        assert!(
            matches!(err, CommissioningError::InvalidConfig(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn new_rejects_zero_ipk_epoch_key() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
        let mut cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        cfg.ipk_epoch_key = [0u8; 16];
        let Err(err) = Commissioner::new(cfg) else {
            panic!("zero IPK should fail");
        };
        assert!(
            matches!(err, CommissioningError::InvalidConfig(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn new_returns_secure_pairing_stage() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let sm = Commissioner::new(cfg).expect("valid config should construct");
        assert_eq!(sm.stage(), Stage::SecurePairing);
    }

    #[test]
    fn poll_from_secure_pairing_emits_read_commissioning_info() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");
        let act = sm.poll().expect("poll succeeds");
        match act {
            Action::ReadAttribute {
                session,
                endpoint,
                cluster,
                attributes,
                expect,
            } => {
                assert_eq!(session, crate::state_machine::SessionContext::Pase);
                assert_eq!(endpoint, 0);
                assert_eq!(cluster, 0x0030);
                assert_eq!(expect, Expectation::CommissioningInfo);
                assert_ne!(attributes, &[] as &[u32]);
            }
            other => panic!("expected ReadAttribute, got {other:?}"),
        }
        assert_eq!(sm.stage(), Stage::ReadCommissioningInfo);
    }

    #[test]
    fn poll_is_idempotent_between_responses() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");
        let act1 = sm.poll().expect("first poll");
        let act2 = sm.poll().expect("second poll");
        match (act1, act2) {
            (
                Action::ReadAttribute {
                    cluster: c1,
                    expect: e1,
                    ..
                },
                Action::ReadAttribute {
                    cluster: c2,
                    expect: e2,
                    ..
                },
            ) => {
                assert_eq!(c1, c2);
                assert_eq!(e1, e2);
            }
            other => panic!("idempotent poll returned different variants: {other:?}"),
        }
    }

    #[test]
    fn full_happy_path_through_config_regulatory_lands_on_send_pai_cert_request() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");

        // SecurePairing → ReadCommissioningInfo
        let _ = sm.poll().expect("poll #1");
        let canned_info = encode_read_commissioning_info_response();
        sm.on_response(Expectation::CommissioningInfo, &canned_info)
            .expect("commissioning info accepted");
        assert_eq!(sm.stage(), Stage::ArmFailsafe);

        // ArmFailsafe
        let _ = sm.poll().expect("poll #2");
        sm.on_response(
            Expectation::ArmFailsafeResponse,
            &[0x15, 0x24, 0x00, 0x00, 0x18],
        )
        .expect("arm failsafe ok");
        assert_eq!(sm.stage(), Stage::ConfigRegulatory);

        // ConfigRegulatory
        let _ = sm.poll().expect("poll #3");
        sm.on_response(
            Expectation::SetRegulatoryConfigResponse,
            &[0x15, 0x24, 0x00, 0x00, 0x18],
        )
        .expect("config regulatory ok");
        assert_eq!(sm.stage(), Stage::SendPaiCertRequest);

        // M6.4.2: SendPaiCertRequest now actually emits an Invoke.
        match sm.poll().expect("poll #4") {
            Action::Invoke {
                cluster,
                command,
                expect,
                ..
            } => {
                assert_eq!(cluster, 0x003E);
                assert_eq!(command, 0x02);
                assert_eq!(expect, Expectation::PaiCertChainResponse);
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn arm_failsafe_busy_response_aborts_with_device_im_status() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");
        let _ = sm.poll().expect("poll info");
        sm.on_response(
            Expectation::CommissioningInfo,
            &encode_read_commissioning_info_response(),
        )
        .expect("commissioning info ok");
        let _ = sm.poll().expect("poll arm failsafe");
        // Device returns BusyWithOtherAdmin: error_code = 4 (spec §11.10.5.1).
        let err = sm
            .on_response(
                Expectation::ArmFailsafeResponse,
                &[0x15, 0x24, 0x00, 0x04, 0x18],
            )
            .expect_err("busy should fail");
        assert!(matches!(
            err,
            CommissioningError::DeviceImStatus {
                stage: Stage::ArmFailsafe,
                im_status: 4,
            }
        ));
        assert_eq!(sm.stage(), Stage::Failed);
        match sm.poll().expect("abort emission") {
            Action::Abort {
                send_disarm_failsafe,
                reason,
            } => {
                assert!(send_disarm_failsafe);
                assert!(reason.contains("ArmFailsafe"), "reason was {reason}");
                assert!(reason.contains("0x4"), "reason was {reason}");
            }
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn out_of_order_response_returns_error_without_advancing() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");
        // No poll called — state machine isn't waiting on anything.
        let err = sm
            .on_response(Expectation::ArmFailsafeResponse, &[])
            .expect_err("should reject out-of-order");
        assert!(matches!(err, CommissioningError::OutOfOrderResponse(_)));
        assert_eq!(sm.stage(), Stage::SecurePairing);
    }

    #[test]
    fn wrong_expectation_returns_unexpected_response_kind() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");
        let _ = sm.poll().expect("poll");
        let err = sm
            .on_response(Expectation::ArmFailsafeResponse, &[])
            .expect_err("wrong kind should fail");
        assert!(matches!(
            err,
            CommissioningError::UnexpectedResponseKind {
                expected: Expectation::CommissioningInfo,
                got: Expectation::ArmFailsafeResponse,
            }
        ));
        // Wrong-kind does NOT advance the cursor.
        assert_eq!(sm.stage(), Stage::ReadCommissioningInfo);
    }

    fn encode_read_commissioning_info_response() -> Vec<u8> {
        // Minimal well-formed anonymous struct. M6.4.1 doesn't parse
        // individual attributes yet.
        vec![0x15, 0x18]
    }

    // --- attestation flow tests ---

    fn drive_to_send_pai_cert_request(sm: &mut Commissioner) {
        let _ = sm.poll().expect("poll info");
        sm.on_response(Expectation::CommissioningInfo, &[0x15, 0x18])
            .expect("info ok");
        let _ = sm.poll().expect("poll arm failsafe");
        sm.on_response(
            Expectation::ArmFailsafeResponse,
            &[0x15, 0x24, 0x00, 0x00, 0x18],
        )
        .expect("arm ok");
        let _ = sm.poll().expect("poll config regulatory");
        sm.on_response(
            Expectation::SetRegulatoryConfigResponse,
            &[0x15, 0x24, 0x00, 0x00, 0x18],
        )
        .expect("regulatory ok");
    }

    fn synthetic_cert_chain_response(cert: &[u8]) -> Vec<u8> {
        use matter_codec::{Tag, TlvWriter};
        let mut buf = Vec::new();
        let mut w = TlvWriter::new(&mut buf);
        w.start_structure(Tag::Anonymous).expect("infallible");
        w.put_bytes(Tag::Context(0), cert).expect("infallible");
        w.end_container().expect("infallible");
        buf
    }

    fn nonce_from_attestation_invoke(act: &Action) -> [u8; 32] {
        match act {
            Action::Invoke { payload, .. } => {
                use matter_codec::{Element, Tag, TlvReader, Value};
                let mut r = TlvReader::new(payload);
                let _ = r.next().expect("reader").expect("anon-struct-start");
                loop {
                    match r.next().expect("reader") {
                        Some(Element::Scalar {
                            tag: Tag::Context(0),
                            value: Value::Bytes(b),
                        }) => {
                            return b.as_slice().try_into().expect("32 bytes");
                        }
                        Some(_) => {}
                        None => panic!("no nonce found"),
                    }
                }
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn poll_at_send_pai_emits_certificate_chain_request_pai() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");
        drive_to_send_pai_cert_request(&mut sm);
        assert_eq!(sm.stage(), Stage::SendPaiCertRequest);
        match sm.poll().expect("poll PAI") {
            Action::Invoke {
                cluster,
                command,
                expect,
                payload,
                ..
            } => {
                assert_eq!(cluster, 0x003E);
                assert_eq!(command, 0x02);
                assert_eq!(expect, Expectation::PaiCertChainResponse);
                // CertificateChainTypeEnum (spec §11.18.5.2): 2 = PAI.
                assert_eq!(payload, vec![0x15, 0x24, 0x00, 0x02, 0x18]);
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn poll_at_send_dac_emits_certificate_chain_request_dac() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");
        drive_to_send_pai_cert_request(&mut sm);
        let _ = sm.poll().expect("poll PAI");
        let pai_response = synthetic_cert_chain_response(&[0xAA, 0xBB, 0xCC]);
        sm.on_response(Expectation::PaiCertChainResponse, &pai_response)
            .expect("PAI accepted");
        assert_eq!(sm.stage(), Stage::SendDacCertRequest);
        match sm.poll().expect("poll DAC") {
            Action::Invoke {
                cluster,
                command,
                expect,
                payload,
                ..
            } => {
                assert_eq!(cluster, 0x003E);
                assert_eq!(command, 0x02);
                assert_eq!(expect, Expectation::DacCertChainResponse);
                // CertificateChainTypeEnum (spec §11.18.5.2): 1 = DAC.
                assert_eq!(payload, vec![0x15, 0x24, 0x00, 0x01, 0x18]);
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn send_attestation_request_uses_fresh_random_nonce_each_time() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();

        let rng_a: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg_a = base_config(&fabric, &setup, &paa, &cd, rng_a);
        let mut sm_a = Commissioner::new(cfg_a).expect("valid config");
        drive_to_send_pai_cert_request(&mut sm_a);
        let _ = sm_a.poll().expect("poll PAI a");
        let pai_response = synthetic_cert_chain_response(&[0xAA]);
        sm_a.on_response(Expectation::PaiCertChainResponse, &pai_response)
            .expect("ok");
        let _ = sm_a.poll().expect("poll DAC a");
        let dac_response = synthetic_cert_chain_response(&[0xBB]);
        sm_a.on_response(Expectation::DacCertChainResponse, &dac_response)
            .expect("ok");
        let nonce_a = nonce_from_attestation_invoke(&sm_a.poll().expect("poll att a"));

        let rng_b: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg_b = base_config(&fabric, &setup, &paa, &cd, rng_b);
        let mut sm_b = Commissioner::new(cfg_b).expect("valid config");
        drive_to_send_pai_cert_request(&mut sm_b);
        let _ = sm_b.poll().expect("poll PAI b");
        sm_b.on_response(Expectation::PaiCertChainResponse, &pai_response)
            .expect("ok");
        let _ = sm_b.poll().expect("poll DAC b");
        sm_b.on_response(Expectation::DacCertChainResponse, &dac_response)
            .expect("ok");
        let nonce_b = nonce_from_attestation_invoke(&sm_b.poll().expect("poll att b"));

        assert_ne!(
            nonce_a, nonce_b,
            "two independent runs should use different random nonces"
        );
    }

    // --- CSR + NOC issuance flow tests ---

    /// Extract the 32-byte `CSRNonce` from a `CSRRequest` Invoke payload.
    /// Mirrors `nonce_from_attestation_invoke` — same TLV shape, both
    /// pull the bytes at context tag 0 inside the anonymous outer struct.
    fn nonce_from_csr_invoke(act: &Action) -> [u8; 32] {
        match act {
            Action::Invoke { payload, .. } => {
                use matter_codec::{Element, Tag, TlvReader, Value};
                let mut r = TlvReader::new(payload);
                let _ = r.next().expect("reader").expect("anon-struct-start");
                loop {
                    match r.next().expect("reader") {
                        Some(Element::Scalar {
                            tag: Tag::Context(0),
                            value: Value::Bytes(b),
                        }) => {
                            return b.as_slice().try_into().expect("32 bytes");
                        }
                        Some(_) => {}
                        None => panic!("no nonce found"),
                    }
                }
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    /// Glass-box test: jumps the cursor straight to
    /// `Stage::SendOpCertSigningRequest` (bypassing PAI/DAC/Att, which
    /// would otherwise demand real fixtures M6.4.2's verifier accepts)
    /// and checks the emitted `CSRRequest` Invoke's nonce randomness
    /// across two independent commissioner instances. The full
    /// integration drive with real matter.js fixtures is
    /// `tests/state_machine_noc.rs`.
    #[test]
    fn send_op_cert_signing_request_emits_csr_with_random_nonce() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();

        let rng_a: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg_a = base_config(&fabric, &setup, &paa, &cd, rng_a);
        let mut sm_a = Commissioner::new(cfg_a).expect("valid config");
        // Jump the cursor + plant the prerequisite DAC slot. Glass-box
        // crate-private access is fine inside the in-module `tests`
        // submodule.
        sm_a.stage = Stage::SendOpCertSigningRequest;
        sm_a.dac_der = Some(vec![0xAA, 0xBB]);
        let act_a = sm_a.poll().expect("poll csr a");
        let nonce_a = nonce_from_csr_invoke(&act_a);

        let rng_b: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg_b = base_config(&fabric, &setup, &paa, &cd, rng_b);
        let mut sm_b = Commissioner::new(cfg_b).expect("valid config");
        sm_b.stage = Stage::SendOpCertSigningRequest;
        sm_b.dac_der = Some(vec![0xAA, 0xBB]);
        let act_b = sm_b.poll().expect("poll csr b");
        let nonce_b = nonce_from_csr_invoke(&act_b);

        assert_ne!(
            nonce_a, nonce_b,
            "two independent runs should use different CSR nonces"
        );

        match act_a {
            Action::Invoke {
                cluster,
                command,
                expect,
                ..
            } => {
                assert_eq!(cluster, 0x003E);
                assert_eq!(command, 0x04);
                assert_eq!(expect, Expectation::CsrResponse);
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    /// Glass-box test: with the CSR + NOC artefacts pre-populated and
    /// the cursor placed at `Stage::SendNoc`, `poll()` must emit an
    /// `AddNOC` Invoke targeting cluster `0x003E` / command `0x06`.
    /// Then drive the synthetic `NOCResponse { status: 0 }` through
    /// `on_response` and assert the cursor lands on
    /// `Stage::ReadNetworkCommissioningInfo`.
    #[test]
    fn drive_through_send_noc_with_synthetic_noc_response() {
        use matter_cert::{
            BasicConstraints, DistinguishedName, DnAttribute, Extensions, MatterCertificate,
            PublicKey,
        };

        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");

        // Skip past attestation/CSR machinery — we want to test the
        // SendNoc dispatch arm + the NOC response handler in isolation.
        // Plant a structurally-valid synthetic NOC the AddNOC encoder
        // can re-serialise. (The device side wouldn't accept it, but
        // we're not talking to a device — we feed a canned response.)
        let mut key_bytes = [0u8; 65];
        key_bytes[0] = 0x04;
        let synthetic_noc = MatterCertificate::builder()
            .serial(vec![1, 2, 3])
            .issuer(fabric.root_cert.subject().clone())
            .subject(DistinguishedName::new(vec![
                DnAttribute::FabricId(fabric.fabric_id),
                DnAttribute::NodeId(0x2),
            ]))
            .validity(
                MatterTime::from_unix_secs(1_704_067_200),
                MatterTime::NO_EXPIRY,
            )
            .public_key(PublicKey::new(key_bytes).expect("valid sec1 prefix"))
            .extensions(
                Extensions::builder()
                    .basic_constraints(Some(BasicConstraints::new(false, None)))
                    .build(),
            )
            .build_unsigned()
            .expect("builder")
            .assemble([0u8; 64]);

        sm.stage = Stage::SendNoc;
        sm.issued_noc = Some(synthetic_noc);
        sm.issued_noc_public_key = Some(key_bytes);

        match sm.poll().expect("poll SendNoc") {
            Action::Invoke {
                cluster,
                command,
                expect,
                ..
            } => {
                assert_eq!(cluster, 0x003E);
                assert_eq!(command, 0x06);
                assert_eq!(expect, Expectation::NocResponse);
            }
            other => panic!("expected Invoke, got {other:?}"),
        }

        // Synthetic NOCResponse: anonymous struct with status=0 + fabric_index=1.
        let mut noc_response = Vec::new();
        {
            use matter_codec::{Tag, TlvWriter};
            let mut w = TlvWriter::new(&mut noc_response);
            w.start_structure(Tag::Anonymous).expect("infallible");
            w.put_uint(Tag::Context(0), 0).expect("infallible"); // status = OK
            w.put_uint(Tag::Context(1), 1).expect("infallible"); // fabric_index = 1
            w.end_container().expect("infallible");
        }
        sm.on_response(Expectation::NocResponse, &noc_response)
            .expect("NocResponse accepted");
        assert_eq!(sm.stage(), Stage::ReadNetworkCommissioningInfo);
    }

    /// Glass-box test: a non-zero NOC status surfaces as
    /// `CommissioningError::DeviceImStatus { stage: SendNoc, ... }`
    /// and transitions the cursor to `Failed`.
    #[test]
    fn send_noc_failure_status_aborts_with_device_im_status() {
        use matter_cert::{
            BasicConstraints, DistinguishedName, DnAttribute, Extensions, MatterCertificate,
            PublicKey,
        };

        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");

        let mut key_bytes = [0u8; 65];
        key_bytes[0] = 0x04;
        let synthetic_noc = MatterCertificate::builder()
            .serial(vec![9])
            .issuer(fabric.root_cert.subject().clone())
            .subject(DistinguishedName::new(vec![
                DnAttribute::FabricId(fabric.fabric_id),
                DnAttribute::NodeId(0x2),
            ]))
            .validity(
                MatterTime::from_unix_secs(1_704_067_200),
                MatterTime::NO_EXPIRY,
            )
            .public_key(PublicKey::new(key_bytes).expect("valid sec1 prefix"))
            .extensions(
                Extensions::builder()
                    .basic_constraints(Some(BasicConstraints::new(false, None)))
                    .build(),
            )
            .build_unsigned()
            .expect("builder")
            .assemble([0u8; 64]);

        sm.stage = Stage::SendNoc;
        sm.issued_noc = Some(synthetic_noc);
        sm.issued_noc_public_key = Some(key_bytes);

        let _ = sm.poll().expect("poll SendNoc");

        // status = 9 (InvalidNOC, spec §11.18.6.1).
        let mut bad_response = Vec::new();
        {
            use matter_codec::{Tag, TlvWriter};
            let mut w = TlvWriter::new(&mut bad_response);
            w.start_structure(Tag::Anonymous).expect("infallible");
            w.put_uint(Tag::Context(0), 9).expect("infallible");
            w.end_container().expect("infallible");
        }
        let err = sm
            .on_response(Expectation::NocResponse, &bad_response)
            .expect_err("non-zero NOC status should fail");
        assert!(matches!(
            err,
            CommissioningError::DeviceImStatus {
                stage: Stage::SendNoc,
                im_status: 9,
            }
        ));
        assert_eq!(sm.stage(), Stage::Failed);
    }

    /// Glass-box test: `SendTrustedRootCert` emits an `AddTrustedRootCertificate`
    /// Invoke whose payload TLV starts with anonymous-struct + context-0
    /// octet-string carrying the RCAC TLV bytes. A subsequent `[0x00]`
    /// status-ack advances the cursor to `Stage::SendNoc`.
    #[test]
    fn send_trusted_root_cert_emits_invoke_and_status_ack_advances() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");

        sm.stage = Stage::SendTrustedRootCert;

        match sm.poll().expect("poll SendTrustedRootCert") {
            Action::Invoke {
                cluster,
                command,
                expect,
                payload,
                ..
            } => {
                assert_eq!(cluster, 0x003E);
                assert_eq!(command, 0x0B);
                assert_eq!(expect, Expectation::AddTrustedRootResponse);
                // Sanity: payload is at least the anonymous-struct
                // wrapper + a non-trivial octet-string of RCAC TLV.
                assert!(payload.len() > 16, "RCAC TLV too short: {}", payload.len());
                assert_eq!(payload[0], 0x15); // anonymous struct start
            }
            other => panic!("expected Invoke, got {other:?}"),
        }

        // Status-ack of 0x00 (success) advances to SendNoc.
        sm.on_response(Expectation::AddTrustedRootResponse, &[0x00])
            .expect("status-ack accepted");
        assert_eq!(sm.stage(), Stage::SendNoc);
    }

    // --- PASE -> CASE handoff + CommissioningComplete tests ---

    #[test]
    fn find_operational_for_complete_emits_establish_case() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = crate::attestation::CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");
        sm.stage = Stage::FindOperationalForComplete;
        match sm.poll().expect("poll establish case") {
            Action::EstablishCase {
                fabric_id,
                peer_node_id,
            } => {
                assert_eq!(fabric_id, fabric.fabric_id);
                assert_eq!(peer_node_id, 0x2); // matches base_config's assigned_node_id
            }
            other => panic!("expected EstablishCase, got {other:?}"),
        }
        assert!(sm.awaiting_case_session);
    }

    #[test]
    fn on_case_established_advances_to_send_complete() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = crate::attestation::CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");
        sm.stage = Stage::FindOperationalForComplete;
        let _ = sm.poll().expect("emit EstablishCase");
        sm.on_case_established().expect("case established");
        assert_eq!(sm.stage(), Stage::SendComplete);
        assert!(!sm.awaiting_case_session);
    }

    #[test]
    fn on_case_established_without_pending_emits_out_of_order() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = crate::attestation::CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");
        let err = sm.on_case_established().expect_err("no pending establish");
        assert!(matches!(err, CommissioningError::OutOfOrderResponse(_)));
    }

    #[test]
    fn send_complete_emits_invoke_over_case_session() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = crate::attestation::CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");
        sm.stage = Stage::SendComplete;
        match sm.poll().expect("poll send complete") {
            Action::Invoke {
                session,
                cluster,
                command,
                expect,
                payload,
                ..
            } => {
                assert_eq!(session, crate::state_machine::SessionContext::Case);
                assert_eq!(cluster, 0x0030);
                assert_eq!(command, 0x04); // CommissioningComplete
                assert_eq!(expect, Expectation::CommissioningCompleteResponse);
                // CommissioningComplete carries no payload fields — empty struct.
                assert_eq!(payload, vec![0x15, 0x18]);
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn send_complete_success_advances_to_cleanup() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = crate::attestation::CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");
        sm.stage = Stage::SendComplete;
        let _ = sm.poll().expect("emit invoke");
        sm.on_response(
            Expectation::CommissioningCompleteResponse,
            &[0x15, 0x24, 0x00, 0x00, 0x18], // error_code = 0
        )
        .expect("complete ok");
        assert_eq!(sm.stage(), Stage::Cleanup);
    }

    #[test]
    fn cleanup_emits_done_with_noc_public_key() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = crate::attestation::CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");
        sm.stage = Stage::Cleanup;
        sm.issued_noc_public_key = Some([0xCA; 65]);
        match sm.poll().expect("poll cleanup") {
            Action::Done(cf) => {
                assert_eq!(cf.peer_node_id, 0x2);
                assert_eq!(cf.peer_root_public_key, [0xCA; 65]);
                assert_eq!(cf.terminated_at, Stage::Cleanup);
                assert_eq!(cf.fabric.fabric_id, fabric.fabric_id);
            }
            other => panic!("expected Done, got {other:?}"),
        }
    }

    // --- CaseFailed negative coverage ---

    #[test]
    fn case_failed_response_aborts_with_case_establishment_failed() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = crate::attestation::CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");

        // Glass-box: jump to FindOperationalForComplete (skipping the
        // attestation + CSR + NOC stages that need real fixtures).
        sm.stage = Stage::FindOperationalForComplete;
        let _ = sm.poll().expect("emit EstablishCase");
        assert!(sm.awaiting_case_session);

        // Caller signals CASE establishment failure.
        let err = sm
            .on_response(Expectation::CaseFailed, &[])
            .expect_err("CaseFailed should error");
        assert!(matches!(err, CommissioningError::CaseEstablishmentFailed));
        assert_eq!(sm.stage(), Stage::Failed);
        assert!(!sm.awaiting_case_session);

        // Subsequent poll emits Action::Abort with send_disarm_failsafe=true.
        match sm.poll().expect("emit abort") {
            Action::Abort {
                send_disarm_failsafe,
                reason,
            } => {
                assert!(send_disarm_failsafe);
                assert!(
                    reason.contains("CASE"),
                    "abort reason should mention CASE: {reason}"
                );
            }
            other => panic!("expected Abort, got {other:?}"),
        }
    }

    #[test]
    fn case_failed_when_not_awaiting_returns_out_of_order() {
        let fabric = make_fabric_record();
        let setup = make_setup_payload();
        let paa = PaaTrustStore::with_example_device_roots();
        let cd = crate::attestation::CdSigningRoots::with_example_device_roots();
        let rng: Arc<dyn crate::noc::NocRng> = Arc::new(SystemNocRng);
        let cfg = base_config(&fabric, &setup, &paa, &cd, rng);
        let mut sm = Commissioner::new(cfg).expect("valid config");
        let err = sm
            .on_response(Expectation::CaseFailed, &[])
            .expect_err("CaseFailed without pending should error");
        assert!(matches!(err, CommissioningError::OutOfOrderResponse(_)));
    }

    /// Returns a fully-populated, valid [`CommissionerConfig`] for use in
    /// unit tests that only need to mutate one field. All held references
    /// are leaked so the config is `'static`; acceptable for test code.
    fn sample_valid_config() -> CommissionerConfig<'static> {
        use std::sync::OnceLock;
        static FABRIC: OnceLock<FabricRecord> = OnceLock::new();
        static SETUP: OnceLock<SetupPayload> = OnceLock::new();
        static PAA: OnceLock<PaaTrustStore> = OnceLock::new();
        static CD: OnceLock<crate::attestation::CdSigningRoots> = OnceLock::new();

        let fabric = FABRIC.get_or_init(make_fabric_record);
        let setup = SETUP.get_or_init(make_setup_payload);
        let paa = PAA.get_or_init(PaaTrustStore::with_example_device_roots);
        let cd = CD.get_or_init(crate::attestation::CdSigningRoots::with_example_device_roots);
        let rng: Arc<dyn NocRng> = Arc::new(SystemNocRng);

        CommissionerConfig {
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
            network: NetworkCredentials::AlreadyOnNetwork,
        }
    }

    #[test]
    fn empty_ssid_is_rejected() {
        let mut config = sample_valid_config();
        config.network = NetworkCredentials::WiFi(WiFiCredentials {
            ssid: vec![],
            credentials: vec![],
        });
        let Err(err) = Commissioner::new(config) else {
            panic!("empty ssid should fail");
        };
        assert!(
            matches!(err, CommissioningError::InvalidConfig(m) if m.contains("ssid")),
            "got {err:?}",
        );
    }

    #[test]
    fn oversize_ssid_is_rejected() {
        let mut config = sample_valid_config();
        config.network = NetworkCredentials::WiFi(WiFiCredentials {
            ssid: vec![b'a'; 33],
            credentials: vec![],
        });
        let Err(err) = Commissioner::new(config) else {
            panic!("33-byte ssid should fail");
        };
        assert!(
            matches!(err, CommissioningError::InvalidConfig(m) if m.contains("≤32")),
            "got {err:?}",
        );
    }

    #[test]
    fn oversize_credentials_is_rejected() {
        let mut config = sample_valid_config();
        config.network = NetworkCredentials::WiFi(WiFiCredentials {
            ssid: b"matter".to_vec(),
            credentials: vec![0u8; 65],
        });
        let Err(err) = Commissioner::new(config) else {
            panic!("65-byte credentials should fail");
        };
        assert!(
            matches!(err, CommissioningError::InvalidConfig(m) if m.contains("≤64")),
            "got {err:?}",
        );
    }

    #[test]
    fn wifi_credentials_none_is_accepted() {
        let mut config = sample_valid_config();
        config.network = NetworkCredentials::AlreadyOnNetwork;
        Commissioner::new(config).expect("AlreadyOnNetwork should pass validation");
    }

    /// A device that omits the `NetworkCommissioning` cluster (observed: eufy
    /// E31 lock over IP — `UNSUPPORTED_CLUSTER` for both read paths) reaches
    /// the state machine as an EMPTY `NetworkCommissioningInfo` payload (the
    /// driver's absent-cluster sentinel). With `AlreadyOnNetwork` there is
    /// nothing to provision, so the absent cluster is acceptable: skip
    /// straight to `EvictPreviousCaseSessions`, exactly like the
    /// feature-map-present path.
    #[test]
    fn network_commissioning_cluster_absent_skips_provisioning_when_already_on_network() {
        let mut config = sample_valid_config();
        config.network = NetworkCredentials::AlreadyOnNetwork;
        let mut sm = Commissioner::new(config).expect("valid config");
        sm.stage = Stage::ReadNetworkCommissioningInfo;
        match sm.poll().expect("poll emits the read") {
            Action::ReadAttribute { expect, .. } => {
                assert_eq!(expect, Expectation::NetworkCommissioningInfo);
            }
            other => panic!("expected ReadAttribute, got {other:?}"),
        }

        sm.on_response(Expectation::NetworkCommissioningInfo, &[])
            .expect("absent cluster is acceptable when already on network");
        assert_eq!(sm.stage(), Stage::EvictPreviousCaseSessions);
    }

    /// The empty sentinel with Wi-Fi credentials supplied is a REAL error —
    /// the device cannot be provisioned onto a network it exposes no
    /// `NetworkCommissioning` cluster for. Surface the typed
    /// `NetworkFeatureUnsupported`, not a framing error.
    #[test]
    fn network_commissioning_cluster_absent_rejects_wifi_provisioning() {
        use crate::state_machine::NetworkKind;

        let mut config = sample_valid_config();
        config.network = NetworkCredentials::WiFi(WiFiCredentials {
            ssid: b"matter".to_vec(),
            credentials: b"hunter22".to_vec(),
        });
        let mut sm = Commissioner::new(config).expect("valid config");
        sm.stage = Stage::ReadNetworkCommissioningInfo;
        let _ = sm.poll().expect("poll emits the read");

        let Err(err) = sm.on_response(Expectation::NetworkCommissioningInfo, &[]) else {
            panic!("absent cluster with Wi-Fi credentials must fail");
        };
        assert!(
            matches!(
                err,
                CommissioningError::NetworkFeatureUnsupported {
                    needed: NetworkKind::WiFi,
                }
            ),
            "got {err:?}",
        );
    }

    #[test]
    fn network_credentials_thread_variant_accepted() {
        // Minimal well-formed dataset: a single Extended PAN ID TLV
        // (type 0x02, length 8). ThreadDataset::new self-validates.
        let ds =
            crate::thread_dataset::ThreadDataset::new(vec![0x02, 0x08, 0, 0, 0, 0, 0, 0, 0, 0])
                .expect("minimal ext-pan-id dataset is valid");
        let mut config = sample_valid_config();
        config.network = NetworkCredentials::Thread(ds);
        let c = Commissioner::new(config).expect("Thread network should pass validation");
        assert!(matches!(c.network(), NetworkCredentials::Thread(_)));
    }

    #[test]
    fn wifi_credentials_debug_redacts_passphrase() {
        let creds = WiFiCredentials {
            ssid: b"matter".to_vec(),
            credentials: b"hunter22".to_vec(),
        };
        let rendered = format!("{creds:?}");
        assert!(
            !rendered.contains("hunter22"),
            "Debug must not contain credentials bytes: {rendered}",
        );
        assert!(rendered.contains("redacted"), "got {rendered}");
        assert!(
            rendered.contains('8'),
            "credentials length should appear: {rendered}"
        );
        assert!(
            rendered.contains('6'),
            "ssid length should appear: {rendered}"
        );
    }

    // --- failsafe expiry derivation from BasicCommissioningInfo ---

    #[test]
    fn failsafe_expiry_derives_from_basic_commissioning_info() {
        let mut sm = Commissioner::new(sample_valid_config()).expect("valid config");
        // Advance to ReadCommissioningInfo and feed a BasicCommissioningInfo with 120s.
        let _initial = sm.poll().expect("initial poll");
        let response = vec![
            0x15, 0x25, 0x00, 0x78, 0x00, // u16 = 120
            0x18,
        ];
        sm.on_response(Expectation::CommissioningInfo, &response)
            .expect("commissioning info accepted");
        // Now ArmFailsafe should emit with expiry=120.
        let action = sm.poll().expect("arm-failsafe poll");
        match action {
            Action::Invoke {
                payload,
                cluster,
                command,
                ..
            } => {
                assert_eq!(cluster, 0x0030);
                assert_eq!(command, 0x00);
                // ArmFailSafe payload byte for expiry: TLV-encoded u8/u16 at context tag 0.
                // For value 120 the smallest-width encoding is u8 = 0x24 0x00 0x78.
                assert!(
                    payload.windows(3).any(|w| w == [0x24, 0x00, 0x78]),
                    "ArmFailSafe payload should carry expiry=120: {payload:02x?}",
                );
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn failsafe_expiry_capped_at_max_cumulative_failsafe_seconds() {
        let mut sm = Commissioner::new(sample_valid_config()).expect("valid config");
        let _initial = sm.poll().expect("initial poll");
        // BasicCommissioningInfo: failsafe_expiry_length=120 (tag 0),
        // max_cumulative=90 (tag 1). Our requested expiry must cap at 90 so the
        // device never rejects the ArmFailSafe with BoundsExceeded.
        let response = vec![
            0x15, //
            0x25, 0x00, 0x78, 0x00, // tag0 u16 = 120
            0x25, 0x01, 0x5A, 0x00, // tag1 u16 = 90 (max cumulative)
            0x18,
        ];
        sm.on_response(Expectation::CommissioningInfo, &response)
            .expect("commissioning info accepted");
        let action = sm.poll().expect("arm-failsafe poll");
        match action {
            Action::Invoke { payload, .. } => {
                assert!(
                    payload.windows(3).any(|w| w == [0x24, 0x00, 0x5A]),
                    "ArmFailSafe expiry must be capped to 90: {payload:02x?}",
                );
                assert!(
                    !payload.windows(3).any(|w| w == [0x24, 0x00, 0x78]),
                    "the uncapped 120 must NOT be sent: {payload:02x?}",
                );
            }
            other => panic!("expected Invoke, got {other:?}"),
        }
    }

    #[test]
    fn failsafe_expiry_falls_back_to_60_on_empty_basic_commissioning_info() {
        let mut sm = Commissioner::new(sample_valid_config()).expect("valid config");
        let _initial = sm.poll().expect("initial poll");
        // Feed a well-formed empty struct — decode_basic_commissioning_info
        // returns None when the failsafe field is missing, so the M6.4
        // fallback of 60s applies.
        sm.on_response(Expectation::CommissioningInfo, &[0x15, 0x18])
            .expect("empty struct accepted");
        let action = sm.poll().expect("arm-failsafe poll");
        if let Action::Invoke { payload, .. } = action {
            // 60 = 0x3C, anonymous struct with context-tag-0 u8.
            assert!(
                payload.windows(3).any(|w| w == [0x24, 0x00, 0x3C]),
                "ArmFailSafe payload should carry expiry=60 fallback: {payload:02x?}",
            );
        }
    }

    // --- breadcrumb monotonicity ---

    #[test]
    fn breadcrumb_increases_across_commands() {
        fn extract_uint_at_tag(payload: &[u8], tag_num: u8) -> Option<u64> {
            use matter_codec::{Element, Tag, TlvReader, Value};
            let mut reader = TlvReader::new(payload);
            while let Ok(Some(elem)) = reader.next() {
                if let Element::Scalar {
                    tag: Tag::Context(t),
                    value: Value::Uint(v),
                } = elem
                {
                    if t == tag_num {
                        return Some(v);
                    }
                }
            }
            None
        }

        let mut sm = Commissioner::new(sample_valid_config()).expect("valid config");
        let mut breadcrumbs: Vec<u64> = Vec::new();

        // Poll #1: ReadCommissioningInfo (no breadcrumb).
        let _ = sm.poll().expect("read commissioning info");
        sm.on_response(Expectation::CommissioningInfo, &[0x15, 0x18])
            .expect("info accepted");

        // Poll #2: ArmFailsafe — first breadcrumb-bearing command.
        let action = sm.poll().expect("arm-failsafe");
        if let Action::Invoke { payload, .. } = action {
            if let Some(b) = extract_uint_at_tag(&payload, 1) {
                breadcrumbs.push(b);
            }
        }
        sm.on_response(
            Expectation::ArmFailsafeResponse,
            &[0x15, 0x24, 0x00, 0x00, 0x18],
        )
        .expect("arm-failsafe ok");

        // Poll #3: SetRegulatoryConfig — second breadcrumb-bearing command.
        let action = sm.poll().expect("set-regulatory");
        if let Action::Invoke { payload, .. } = action {
            if let Some(b) = extract_uint_at_tag(&payload, 2) {
                breadcrumbs.push(b);
            }
        }

        assert_eq!(
            breadcrumbs.len(),
            2,
            "should have extracted exactly two breadcrumbs, got {breadcrumbs:?}",
        );
        assert!(
            breadcrumbs[0] < breadcrumbs[1],
            "breadcrumbs should be strictly increasing: {breadcrumbs:?}",
        );
    }
}
