//! `Stage` — every cursor position the state machine can occupy.
//!
//! Stages match `project-chip/connectedhomeip`'s `CommissioningStage`
//! enum (translated to Rust style and trimmed to the M6.5 subset).
//! Stages we defer past M6.5 are noted inline as `// deferred: kFoo`
//! so future expansion is mechanical.

#![forbid(unsafe_code)]

/// Cursor position inside the commissioning sequence.
///
/// Variants are ordered top-to-bottom in transition order. The transition
/// function lives in `next_stage` (crate-internal).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Stage {
    /// Entry state — caller has just constructed the [`super::Commissioner`]
    /// with a valid PASE session. No action emitted; advances on first
    /// `poll()` to [`Self::ReadCommissioningInfo`].
    SecurePairing,
    /// Read `BasicCommissioningInfo`, `RegulatoryConfig`, etc. from the
    /// `GeneralCommissioning` cluster (id `0x0030`) so the commissioner
    /// knows `failsafe_expiry_length_seconds` before arming the failsafe.
    ReadCommissioningInfo,
    /// `GeneralCommissioning::ArmFailSafe` (command id `0x00`).
    ArmFailsafe,
    /// `GeneralCommissioning::SetRegulatoryConfig` (command id `0x02`).
    ConfigRegulatory,
    /// `OperationalCredentials::CertificateChainRequest` with `type=PAI`
    /// (cluster `0x003E`, command `0x02`, type enum `0x01`).
    SendPaiCertRequest,
    /// `OperationalCredentials::CertificateChainRequest` with `type=DAC`
    /// (type enum `0x02`).
    SendDacCertRequest,
    /// `OperationalCredentials::AttestationRequest` (command `0x00`).
    SendAttestationRequest,
    /// Off-wire: chain + signature + CD verification.
    AttestationVerification,
    /// `OperationalCredentials::CSRRequest` (command `0x04`).
    SendOpCertSigningRequest,
    /// Off-wire: PKCS#10 self-signature + DAC attestation + nonce echo.
    ValidateCsr,
    /// Off-wire: build + sign the NOC under the commissioner's RCAC.
    GenerateNocChain,
    /// `OperationalCredentials::AddTrustedRootCertificate` (command `0x0B`).
    SendTrustedRootCert,
    /// `OperationalCredentials::AddNOC` (command `0x06`).
    SendNoc,
    /// Read `NetworkCommissioning::FeatureMap` (attribute `0xFFFC`) and
    /// `ConnectMaxTimeSeconds` (attribute `0x0003`) on endpoint 0.
    /// `FeatureMap` determines whether the device supports Wi-Fi,
    /// Ethernet, or Thread (or some combination). Branching at this
    /// stage's response routes by the *supplied*
    /// [`super::NetworkCredentials`] variant, cross-checked against the
    /// device `FeatureMap`: matching credentials advance to
    /// `NetworkSetup`, `AlreadyOnNetwork` skips to
    /// `EvictPreviousCaseSessions`, and a credential type absent from the
    /// `FeatureMap` fails with `NetworkFeatureUnsupported`.
    ReadNetworkCommissioningInfo,
    /// Network provisioning: `NetworkCommissioning::AddOrUpdateWiFiNetwork`
    /// (cluster `0x0031` command `0x02`) for Wi-Fi credentials, or
    /// `AddOrUpdateThreadNetwork` (command `0x03`) for a Thread dataset.
    /// The stage is generic; the command is selected by the supplied
    /// [`super::NetworkCredentials`] variant. Skipped for `AlreadyOnNetwork`
    /// / Ethernet-only devices.
    NetworkSetup,
    /// Second `GeneralCommissioning::ArmFailSafe` (cluster `0x0030`
    /// command `0x00`). Extends the failsafe window before
    /// `ConnectNetwork` so the device has room to associate with the
    /// operational network and re-discover the commissioner via mDNS.
    /// The extension is sized from the device's `ConnectMaxTimeSeconds`
    /// (Thread attach is slower than Wi-Fi association), falling back to
    /// a generous default. Re-uses the existing
    /// `Expectation::ArmFailsafeResponse`.
    FailsafeBeforeNetworkEnable,
    /// `NetworkCommissioning::ConnectNetwork` (cluster `0x0031`
    /// command `0x06`). The device associates with the operational
    /// network and (typically) returns `ConnectNetworkResponse` over
    /// PASE before switching networks. The `network_id` is the SSID for
    /// Wi-Fi and the Extended PAN ID for Thread.
    NetworkEnable,
    /// Evict any prior CASE session for this fabric/peer pair. M6.4
    /// only supports new-fabric commissioning — no eviction needed —
    /// so the stage advances immediately. Slot reserved for M8
    /// multi-fabric work.
    EvictPreviousCaseSessions,
    /// Caller establishes a CASE session via mDNS find-operational +
    /// SIGMA handshake (M6.6 mechanics). State machine emits
    /// `Action::EstablishCase` and waits for `on_case_established()`.
    FindOperationalForComplete,
    /// `GeneralCommissioning::CommissioningComplete` (command `0x04`),
    /// sent over the freshly-established CASE session.
    SendComplete,
    /// Terminal success. Emits `Action::Done(CommissionedFabric)`.
    Cleanup,
    /// Terminal failure. Emits `Action::Abort`.
    Failed,
    // deferred: kReadCommissioningInfo2 (post-NOC capability re-read)
    // deferred: kConfigureUTCTime, kConfigureTimeZone, kConfigureDSTOffset, kConfigureDefaultNTP
    // deferred: kAttestationRevocationCheck
    // deferred: kJCMTrustVerification
    // deferred: kICDGetRegistrationInfo, kICDRegistration
    // deferred: kConfigureTCAcknowledgments
    // deferred: kPrimaryOperationalNetworkFailed, kRemoveWiFiNetworkConfig, kRemoveThreadNetworkConfig
}

/// Happy-path successor of `current`. Returns `None` for terminal
/// stages (`Cleanup`, `Failed`).
///
/// Encodes the happy-path order (exercised by this module's tests);
/// `Commissioner::advance` is passed its next stage explicitly. Errors at
/// any stage transition the cursor directly to [`Stage::Failed`].
// Only this module's tests call it, hence the dead-code allow.
#[allow(dead_code)]
#[allow(unreachable_pub)]
#[must_use]
pub fn next_stage(current: Stage) -> Option<Stage> {
    Some(match current {
        Stage::SecurePairing => Stage::ReadCommissioningInfo,
        Stage::ReadCommissioningInfo => Stage::ArmFailsafe,
        Stage::ArmFailsafe => Stage::ConfigRegulatory,
        Stage::ConfigRegulatory => Stage::SendPaiCertRequest,
        Stage::SendPaiCertRequest => Stage::SendDacCertRequest,
        Stage::SendDacCertRequest => Stage::SendAttestationRequest,
        Stage::SendAttestationRequest => Stage::AttestationVerification,
        Stage::AttestationVerification => Stage::SendOpCertSigningRequest,
        Stage::SendOpCertSigningRequest => Stage::ValidateCsr,
        Stage::ValidateCsr => Stage::GenerateNocChain,
        Stage::GenerateNocChain => Stage::SendTrustedRootCert,
        Stage::SendTrustedRootCert => Stage::SendNoc,
        Stage::SendNoc => Stage::ReadNetworkCommissioningInfo,
        Stage::ReadNetworkCommissioningInfo => Stage::NetworkSetup,
        Stage::NetworkSetup => Stage::FailsafeBeforeNetworkEnable,
        Stage::FailsafeBeforeNetworkEnable => Stage::NetworkEnable,
        Stage::NetworkEnable => Stage::EvictPreviousCaseSessions,
        Stage::EvictPreviousCaseSessions => Stage::FindOperationalForComplete,
        Stage::FindOperationalForComplete => Stage::SendComplete,
        Stage::SendComplete => Stage::Cleanup,
        Stage::Cleanup | Stage::Failed => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_advances_through_all_stages() {
        // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
        #![allow(clippy::unwrap_used)]
        let expected = [
            Stage::SecurePairing,
            Stage::ReadCommissioningInfo,
            Stage::ArmFailsafe,
            Stage::ConfigRegulatory,
            Stage::SendPaiCertRequest,
            Stage::SendDacCertRequest,
            Stage::SendAttestationRequest,
            Stage::AttestationVerification,
            Stage::SendOpCertSigningRequest,
            Stage::ValidateCsr,
            Stage::GenerateNocChain,
            Stage::SendTrustedRootCert,
            Stage::SendNoc,
            Stage::ReadNetworkCommissioningInfo,
            Stage::NetworkSetup,
            Stage::FailsafeBeforeNetworkEnable,
            Stage::NetworkEnable,
            Stage::EvictPreviousCaseSessions,
            Stage::FindOperationalForComplete,
            Stage::SendComplete,
            Stage::Cleanup,
        ];
        for pair in expected.windows(2) {
            assert_eq!(
                next_stage(pair[0]),
                Some(pair[1]),
                "next_stage({:?}) should be Some({:?})",
                pair[0],
                pair[1],
            );
        }
        assert_eq!(next_stage(Stage::Cleanup), None, "Cleanup is terminal");
        assert_eq!(next_stage(Stage::Failed), None, "Failed is terminal");
    }
}
