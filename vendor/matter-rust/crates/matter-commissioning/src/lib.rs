//! Matter commissioning: setup payloads, device attestation, NOC issuance,
//! network commissioning, and the state machine that sequences them.
//!
//! The commissioning flow here has been driven against real Matter hardware —
//! over IP and over BLE, onto Wi-Fi and onto Thread.
//!
//! If you want a complete controller — commissioning plus reading, writing,
//! invoking, and subscribing — use [`matter-controller`], which is built on
//! this crate. Reach for `matter-commissioning` directly when you want the
//! commissioning pieces on their own, or want to drive the state machine from
//! your own IO layer.
//!
//! [`matter-controller`]: https://crates.io/crates/matter-controller
//!
//! ## What's here
//!
//! - [`setup`] — QR and manual pairing codes, decode and encode.
//! - [`attestation`] — typed [`Dac`] / [`Pai`] / [`Paa`] wrappers,
//!   chain validation against a [`PaaTrustStore`] via [`verify_chain`]
//!   (`rustls-webpki` path validation plus a Matter VID/PID overlay),
//!   [`verify_attestation_response`] for the device's signed attestation, and
//!   CSA Certification Declaration (CMS) verification against
//!   [`CdSigningRoots`].
//! - [`noc`] — Node Operational Certificate issuance: [`FabricRecord`],
//!   CSR verification, RCAC/NOC minting, and the `OperationalCredentials`
//!   command codecs.
//! - [`state_machine`] — a sans-IO cursor over the whole flow, from
//!   `Stage::SecurePairing` through `Action::Done(CommissionedFabric)`:
//!   attestation, CSR and NOC installation, the network-commissioning
//!   subgraph, and the PASE→CASE handoff. It emits [`Action`]s and consumes
//!   responses; it performs no IO of its own.
//! - [`clusters`] and [`thread_dataset`] — the `GeneralCommissioning` and
//!   `NetworkCommissioning` command codecs, and Thread Operational Dataset
//!   parsing.
//! - [`im`] — Interaction Model framing, re-exported from
//!   [`matter_interaction`].
//! - `driver` (behind the off-by-default `driver` feature) — the async Tokio
//!   IO layer that runs the state machine for real: PASE, mDNS discovery,
//!   CASE, and the Invoke/Read round-trips in between.
//!
//! Network commissioning covers Wi-Fi and Thread; a device already on its
//! operational network (Ethernet, or Wi-Fi it has already joined) skips the
//! network sub-cursor entirely. A device whose `NetworkCommissioning` feature
//! map does not match the credentials you supplied fails fast with a typed
//! `NetworkFeatureUnsupported` error, and [`RemediationHint`] categorises
//! `NetworkRejected` failures into actionable causes.
//!
//! ## Quick-start: parse a setup payload
//!
//! ```
//! use matter_commissioning::setup::{parse_qr, parse_manual_code};
//! # fn run() -> Result<(), matter_commissioning::setup::Error> {
//! let from_qr = parse_qr("MT:Y.K90AFN00KA0648G00")?;
//! let from_manual = parse_manual_code("11693312331")?;
//! assert_eq!(from_qr.vendor_id, Some(0xFFF1));
//! assert_eq!(from_manual.passcode.as_u32(), 20_202_021);
//! # Ok(())
//! # }
//! # let _ = run;
//! ```
//!
//! Those are the spec's example codes; substitute the ones printed on your
//! own device.
//!
//! ## Optional `tracing` feature
//!
//! Enable the `tracing` crate feature to get per-method spans on
//! `Commissioner::poll`, `Commissioner::on_response`, and
//! `Commissioner::on_case_established`. Span fields (`stage`,
//! `expectation`) align best-effort with matter.js's log-event format
//! so operators can grep across both implementations. Compatibility
//! is not guaranteed across matter.js minor versions.
//!
//! With `driver` also on, `tracing` adds debug events for each message the
//! driver sends or receives. They record the direction, session and
//! exchange ids, protocol and opcode (some also a length), but **no message
//! payload bytes**. The only bytes the crate's debug events dump are public or
//! encrypted: the attestation certificates and the RCAC being installed,
//! and the CASE Sigma2 messages (whose secret parts are ciphertext).
//!
//! ## Optional `wiretrace` feature: debugging only
//!
//! `wiretrace` implies `tracing` and adds `wiretrace::JsonlLayer`, which
//! captures the `matter_wire` events for `cargo xtask trace-diff`. With it
//! on, those events also record **the full decrypted plaintext of every
//! commissioning exchange** as hex. That includes the IPK in `AddNOC`, the
//! Wi-Fi passphrase in `AddOrUpdateWiFiNetwork` and the Thread operational
//! dataset (network key included) in `AddOrUpdateThreadNetwork`. Enable it
//! only to debug or cross-verify against matter.js, on a test network, and
//! never in production.

#![forbid(unsafe_code)]

pub mod attestation;
pub mod clusters;
#[cfg(test)]
mod debug_redaction_tests;
#[cfg(feature = "driver")]
pub mod driver;
pub mod error;
/// Lowercase-hex rendering for `tracing` debug dumps of wire bytes.
#[cfg(feature = "tracing")]
pub(crate) mod hexdump {
    use std::fmt::Write;

    /// Render `bytes` as a contiguous lowercase-hex string.
    pub(crate) fn hex(bytes: &[u8]) -> String {
        bytes
            .iter()
            .fold(String::with_capacity(bytes.len() * 2), |mut out, b| {
                // Vec-backed String writes are infallible.
                let _ = write!(out, "{b:02x}");
                out
            })
    }

    /// A message-payload field for a driver debug event: the bytes as hex
    /// under `wiretrace`, and nothing at all under plain `tracing`.
    ///
    /// Message payloads are the decrypted plaintext of the commissioning
    /// exchanges, and `AddNOC`, `AddOrUpdateWiFiNetwork` and
    /// `AddOrUpdateThreadNetwork` carry the IPK, the Wi-Fi passphrase and
    /// the Thread dataset. So the hex is recorded only when the caller opted
    /// into `wiretrace`, the debugging-only capture for
    /// `cargo xtask trace-diff`.
    ///
    /// Use it as `payload = hexdump::payload(bytes)`, without a `%` sigil.
    /// `tracing` records an `Option` field only when it is `Some`, so under
    /// plain `tracing` the field is never recorded and the hex is never
    /// computed: no subscriber, formatter or layer can see the bytes.
    #[cfg(all(feature = "driver", feature = "wiretrace"))]
    #[allow(
        clippy::unnecessary_wraps,
        reason = "same signature as the non-wiretrace variant, which returns None"
    )]
    pub(crate) fn payload(bytes: &[u8]) -> Option<tracing::field::DisplayValue<String>> {
        Some(tracing::field::display(hex(bytes)))
    }

    /// See the `wiretrace` variant: without that feature the field is
    /// always `None`, so it is never recorded.
    #[cfg(all(feature = "driver", not(feature = "wiretrace")))]
    pub(crate) fn payload(_bytes: &[u8]) -> Option<tracing::field::DisplayValue<String>> {
        None
    }
}
/// Interaction Model message framing — re-exported from [`matter_interaction`],
/// which this crate used to host. All `im::` paths still resolve.
pub use matter_interaction as im;
pub mod noc;
pub mod setup;
pub mod state_machine;
#[cfg(feature = "test-support")]
pub mod test_support;
pub mod thread_dataset;
#[cfg(feature = "wiretrace")]
pub mod wiretrace;

pub use setup::{
    encode_manual_code, encode_qr, parse_manual_code, parse_qr, CommissioningFlow,
    DiscoveryCapabilities, Discriminator, Error as SetupError, Passcode, SetupPayload,
};

pub use attestation::{
    extract_attestation_elements_fields, verify_attestation_response,
    verify_certification_declaration, verify_certification_declaration_with_paa, verify_chain,
    verify_dac_signed_elements, AttestationElementsFields, AttestationError, AttestationResponse,
    CdSigningRoots, ChainVerification, Dac, Paa, PaaTrustStore, Pai, ProductId, VendorId,
};

pub use noc::{
    decode_attestation_response, decode_certificate_chain_response, decode_csr_response,
    decode_noc_response, encode_add_noc, encode_add_trusted_root, encode_attestation_request,
    encode_certificate_chain_request, encode_csr_request, encode_update_noc, issue_icac, issue_noc,
    parse_and_verify_csr, parse_nocsr, verify_csr_response, CertChainType,
    CertificateChainResponse, CsrResponse, FabricRecord, NocError, NocResponse, NocRng,
    NocsrElements, ParsedCsr, SystemNocRng, VerifiedCsr,
};

pub use clusters::network_commissioning::{
    decode_connect_network_response, decode_feature_map, decode_network_config_response,
    encode_add_or_update_wifi_network, encode_connect_network, remediation_for,
    ConnectNetworkResponse, NetworkCommissioningFeature, NetworkConfigResponse,
};

pub use im::{
    build_invoke_request, build_read_request, parse_invoke_response, parse_report_data,
    AttributePath, CommandPath, ImError, ImStatus, InvokeResponse, ReportData, IM_REVISION,
};

#[cfg(feature = "__test_shortcuts")]
pub use state_machine::TestStateSeeds;
pub use state_machine::{
    Action, CommissionedFabric, Commissioner, CommissionerConfig, CommissioningError, Expectation,
    NetworkCredentials, NetworkKind, RemediationHint, SessionContext, Stage, WiFiCredentials,
};

pub use thread_dataset::{ThreadDataset, ThreadDatasetError};

/// Compile-checks the Rust examples in this crate's `README.md`.
///
/// `#[cfg(doctest)]` means the item exists only while rustdoc is collecting
/// doctests, so the README is compiled by `cargo test --doc` without being
/// duplicated into the rendered crate docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
