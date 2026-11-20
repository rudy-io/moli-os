//! `CommissioningError` — error variants surfaced by the state machine.

#![forbid(unsafe_code)]

use crate::attestation::AttestationError;
use crate::noc::NocError;
use crate::state_machine::action::Expectation;
use crate::state_machine::stage::Stage;

/// Errors emitted by the commissioning state machine.
///
/// All variants are `#[non_exhaustive]` — future sub-phases or future
/// milestones (M6.5 network commissioning, etc.) can add variants
/// without breaking `SemVer`.
///
/// `CommissioningError` is intentionally **not** `Clone`. The summary
/// emitted in [`super::Action::Abort`] is a pre-rendered `String`, so
/// callers never need to clone the full error.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CommissioningError {
    /// `CommissionerConfig` failed validation in `Commissioner::new`.
    /// Carries a `&'static str` describing which field is bad — no
    /// alloc on the error path.
    #[error("invalid commissioner config: {0}")]
    InvalidConfig(&'static str),

    /// Caller invoked `on_response` with an `Expectation` that does
    /// not match the last `poll()`'s emitted `Expectation`.
    #[error("unexpected response kind: expected {expected:?}, got {got:?}")]
    UnexpectedResponseKind {
        /// The Expectation the state machine emitted with the last
        /// Action.
        expected: Expectation,
        /// The Expectation the caller passed in.
        got: Expectation,
    },

    /// Caller invoked `on_response` or `on_case_established` in a
    /// stage where the state machine is not waiting for input (e.g.
    /// `Stage::Cleanup` or `Stage::SecurePairing`).
    #[error("response delivered out of order at stage {0:?}")]
    OutOfOrderResponse(Stage),

    /// Device returned a non-OK Interaction Model status for a cluster
    /// command at `stage`. The 16-bit `im_status` is the canonical
    /// Matter status code from the response envelope.
    #[error("device rejected stage {stage:?}: IM status {im_status:#x}")]
    DeviceImStatus {
        /// Where the rejection happened.
        stage: Stage,
        /// IM status code (Matter Core Spec §8.10).
        im_status: u16,
    },

    /// Response TLV failed to decode at the cluster command level.
    #[error("malformed response at stage {0:?}")]
    MalformedResponse(Stage),

    /// Attestation verification failed (chain / signature / CD).
    #[error("attestation verification failed: {0}")]
    Attestation(#[from] AttestationError),

    /// CSR verification or NOC issuance failed.
    #[error("NOC issuance failed: {0}")]
    Noc(#[from] NocError),

    /// CASE establishment failed (caller called
    /// `on_response(Expectation::CaseFailed, &[])`).
    #[error("CASE session establishment failed")]
    CaseEstablishmentFailed,

    /// The device's `NetworkCommissioning::FeatureMap` does not declare
    /// the network type the caller supplied credentials for (e.g.
    /// `NetworkCredentials::Thread` was supplied but the device's
    /// `FeatureMap` lacks the Thread bit). Both Wi-Fi and Thread are
    /// supported network types as of M9-C2 — this variant signals a
    /// device/credential *mismatch*, not an unsupported network type.
    ///
    /// **Wording pinned:** `WeaveHome` substring-matches
    /// `does not support Thread network type` to route Wi-Fi-only devices off
    /// its automatic Thread path. Do not reword without coordinating — the
    /// typed replacement is `matter_controller::Error::network_feature_unsupported()`.
    #[error("device does not support {needed:?} network type (credential/device mismatch)")]
    NetworkFeatureUnsupported {
        /// Which network type the supplied credentials required.
        needed: NetworkKind,
    },

    /// Device rejected `AddOrUpdateWiFiNetwork` or `ConnectNetwork`
    /// with a non-OK `NetworkCommissioningStatusEnum` value
    /// (spec §11.9.5.1).
    #[error(
        "network commissioning rejected at stage {stage:?}: \
             networking_status {networking_status:#x}, \
             debug_text={}, hint={remediation_hint:?}",
        display_debug_text(debug_text.as_ref())
    )]
    NetworkRejected {
        /// Which stage the device rejected.
        stage: Stage,
        /// Raw `NetworkCommissioningStatusEnum` value from the
        /// response.
        networking_status: u8,
        /// Optional human-readable debug text echoed by the device,
        /// capped at the spec's 512-octet bound at decode.
        /// **Device-controlled free text** — it may name networks (e.g.
        /// an SSID); log deliberately.
        debug_text: Option<String>,
        /// Mapped remediation category for downstream UI rendering.
        remediation_hint: RemediationHint,
    },
}

/// Render `debug_text` for `Display`, capped at 64 chars with an ellipsis.
/// The device controls this string and it can echo an SSID; the full (still
/// 512-byte-capped) value stays on the field for deliberate consumers.
///
/// Rendered with `Debug` (`{:?}`), not `Display` — deliberately. The device
/// fully controls these bytes (decoded as UTF-8, so `\n`, `\r`, and ANSI
/// escapes like `\u{1b}` are all legal content), and this string lands
/// directly in whatever a consumer logs. `Debug` escapes control characters
/// into their `\n`/`\r`/`\u{1b}` textual form; passing the raw string
/// through `Display` would let a malicious or buggy device inject newlines
/// or terminal escape sequences straight into consumer logs.
fn display_debug_text(text: Option<&String>) -> String {
    match text {
        None => "None".to_owned(),
        Some(s) => {
            let capped: String = s.chars().take(64).collect();
            if capped.len() < s.len() {
                format!("Some({capped:?}…)")
            } else {
                format!("Some({s:?})")
            }
        }
    }
}

/// Which Matter network-commissioning type a device declared in its
/// `NetworkCommissioning::FeatureMap`.
///
/// `#[non_exhaustive]` — future Matter-spec network interfaces
/// (e.g. Thread Border Router relay) can be added without a breaking
/// change.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum NetworkKind {
    /// Wi-Fi network interface (`FeatureMap` bit 0).
    WiFi,
    /// Thread network interface (`FeatureMap` bit 1).
    Thread,
    /// Ethernet network interface (`FeatureMap` bit 2).
    Ethernet,
}

/// Hint describing what a downstream UI could suggest to remediate a
/// `CommissioningError::NetworkRejected` (lands in M6.5.2).
///
/// Maps from a Matter `NetworkCommissioningStatusEnum` value (spec
/// §11.9.5.1) into a category callers can render meaningfully without
/// parsing the raw status code. The mapping table lives in
/// `crate::clusters::network_commissioning::remediation_for`.
///
/// # Stability
///
/// `#[non_exhaustive]` from inception. New variants may be added in any
/// release. Existing variants will never be renamed or reordered.
/// Changes to the `status_code` → variant mapping are documented in the
/// CHANGELOG as semi-public behavioural changes.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum RemediationHint {
    /// Password/passphrase likely wrong. From `AuthFailure` (7).
    CheckPassphrase,
    /// SSID not found. From `NetworkNotFound` (5), `NetworkIDNotFound` (3).
    CheckSsid,
    /// Country code / regulatory location mismatch. From
    /// `RegulatoryError` (6).
    CheckRegulatoryRegion,
    /// Wi-Fi security cipher unsupported (e.g. WEP-only device). From
    /// `UnsupportedSecurity` (8).
    UpgradeSecurityMode,
    /// Device reached its `MaxNetworks` limit. From `BoundsExceeded` (2).
    DeviceNetworkSlotsFull,
    /// IP-stack-layer failure on the device side. From `IPV6Failed` (10),
    /// `IPBindFailed` (11).
    DeviceIpStackFailure,
    /// No specific guidance available. From `OtherConnectionFailure` (9),
    /// `UnknownError` (12), or any status code not yet mapped.
    None,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_machine::action::Expectation;
    use crate::state_machine::stage::Stage;

    #[test]
    fn invalid_config_carries_message() {
        let e = CommissioningError::InvalidConfig("missing IPK epoch key");
        let msg = e.to_string();
        assert!(msg.contains("missing IPK"), "{msg}");
    }

    #[test]
    fn unexpected_response_kind_shows_both_sides() {
        let e = CommissioningError::UnexpectedResponseKind {
            expected: Expectation::ArmFailsafeResponse,
            got: Expectation::AttestationResponse,
        };
        let msg = e.to_string();
        assert!(msg.contains("ArmFailsafeResponse"), "{msg}");
        assert!(msg.contains("AttestationResponse"), "{msg}");
    }

    #[test]
    fn out_of_order_response_names_the_stage() {
        let e = CommissioningError::OutOfOrderResponse(Stage::ArmFailsafe);
        let msg = e.to_string();
        assert!(msg.contains("ArmFailsafe"), "{msg}");
    }

    #[test]
    fn device_im_status_includes_stage_and_status_code() {
        let e = CommissioningError::DeviceImStatus {
            stage: Stage::ArmFailsafe,
            im_status: 0x0098,
        };
        let msg = e.to_string();
        assert!(msg.contains("ArmFailsafe"), "{msg}");
        assert!(msg.contains("0x98"), "{msg}");
    }

    #[test]
    fn remediation_hint_is_copy_eq_hash() {
        fn assert_copy<T: Copy + Eq + std::hash::Hash>() {}
        assert_copy::<RemediationHint>();
        assert_eq!(RemediationHint::None, RemediationHint::None);
        assert_ne!(RemediationHint::None, RemediationHint::CheckPassphrase);
    }

    #[test]
    fn network_rejected_display_caps_debug_text() {
        let e = CommissioningError::NetworkRejected {
            stage: Stage::NetworkSetup,
            networking_status: 5,
            debug_text: Some("s".repeat(300)),
            remediation_hint: RemediationHint::CheckSsid,
        };
        let msg = e.to_string();
        // 64 chars + ellipsis, not the whole 300. Rendered via `{:?}`, so the
        // capped run of plain ASCII 's' is unescaped but still quoted.
        assert!(msg.contains(&format!("{:?}", "s".repeat(64))));
        assert!(!msg.contains(&"s".repeat(65)));
        assert!(msg.contains('…'));

        // Short text renders in full, no ellipsis, still Debug-quoted.
        let short = CommissioningError::NetworkRejected {
            stage: Stage::NetworkSetup,
            networking_status: 5,
            debug_text: Some("bad ssid".into()),
            remediation_hint: RemediationHint::CheckSsid,
        };
        assert!(short.to_string().contains("\"bad ssid\""));
        assert!(!short.to_string().contains('…'));
    }

    #[test]
    fn network_rejected_display_escapes_control_chars() {
        // The critical property this Display impl exists for: a
        // device-controlled debug_text containing a newline or an ANSI
        // escape must never appear raw in the rendered message — only in
        // its escaped `Debug` form (`\n`, `\u{1b}`). Otherwise a malicious
        // device can inject fake log lines or terminal control sequences
        // into whatever logs this error's Display output.
        let e = CommissioningError::NetworkRejected {
            stage: Stage::NetworkSetup,
            networking_status: 5,
            debug_text: Some("evil\nFAKE LOG LINE\u{1b}[31mred".to_owned()),
            remediation_hint: RemediationHint::CheckSsid,
        };
        let msg = e.to_string();
        assert!(!msg.contains('\n'), "raw newline leaked into: {msg}");
        assert!(!msg.contains('\u{1b}'), "raw ESC leaked into: {msg}");
        assert!(msg.contains("\\n"), "newline must render escaped: {msg}");
        assert!(msg.contains("\\u{1b}"), "ESC must render escaped: {msg}");
    }

    #[test]
    fn network_feature_unsupported_wording_is_pinned() {
        // WeaveHome routes Wi-Fi-only devices off its Thread path by
        // substring-matching this exact wording (their state_machine/error.rs
        // consumer). A reword compiles cleanly downstream and silently breaks
        // the fall-through — this pin makes a reword a visible test failure.
        // Coordinate any change with WeaveHome and their typed replacement,
        // matter_controller::Error::network_feature_unsupported().
        let e = CommissioningError::NetworkFeatureUnsupported {
            needed: NetworkKind::Thread,
        };
        assert!(
            e.to_string()
                .contains("does not support Thread network type"),
            "pinned substring changed: {e}"
        );
    }
}
