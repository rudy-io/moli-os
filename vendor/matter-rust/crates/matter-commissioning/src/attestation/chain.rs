//! Device attestation chain validation.
//!
//! [`verify_chain`] runs the load-bearing X.509 path validation
//! through `rustls-webpki` 0.103 and layers Matter-specific overlay
//! checks (VID/PID equality per Matter §6.2.3) on top.
//!
//! Pure sans-I/O — no network, no clock reads, no internal state.
//! Callers supply [`matter_cert::time::MatterTime`] explicitly so
//! tests pin behaviour to fixture validity windows. The DAC public
//! key surfaced in [`ChainVerification`] is the same bytes
//! [`crate::attestation::Dac`]'s `public_key()` accessor returns;
//! M6.2.3 will feed it into `verify_attestation_response`.

#![forbid(unsafe_code)]

use core::time::Duration;

use matter_cert::time::MatterTime;
use rustls_pki_types::{CertificateDer, SignatureVerificationAlgorithm, TrustAnchor, UnixTime};
use webpki::{EndEntityCert, KeyUsage};

use crate::attestation::error::{map_webpki_error, AttestationError};
use crate::attestation::extensions::{ProductId, VendorId};
use crate::attestation::trust_store::PaaTrustStore;
use crate::attestation::x509::{Dac, Paa, Pai};

/// Signature algorithms accepted in Matter attestation chains.
///
/// Matter Core Spec §6.2 mandates ECDSA over the NIST P-256 curve
/// with SHA-256 for every signature in the DAC -> PAI -> PAA chain.
/// We list exactly that algorithm and no others: any cert signed
/// with a different scheme (e.g. RSA, `EdDSA`, P-384) is rejected by
/// webpki with `UnsupportedSignatureAlgorithm`, which our
/// [`map_webpki_error`] funnels into
/// [`AttestationError::InvalidChain`].
static MATTER_SIG_ALGS: &[&dyn SignatureVerificationAlgorithm] = &[webpki::ring::ECDSA_P256_SHA256];

/// Build a [`TrustAnchor`] from one of our [`Paa`]s.
///
/// webpki's anchor wants pre-parsed `Subject`, `SubjectPublicKeyInfo`,
/// and (optionally) `NameConstraints` byte slices. Rather than re-parse
/// the DER ourselves — and risk drifting from webpki's own notion of
/// each field's byte range — we hand the original DER to webpki's
/// dedicated anchor-extraction entry point and let it carve up the
/// slices.
///
/// # Why this returns `TrustAnchor<'static>` rather than `TrustAnchor<'_>`
///
/// webpki 0.103's [`webpki::anchor_from_trusted_cert`] is signed as
/// `fn(&'a CertificateDer<'a>) -> Result<TrustAnchor<'a>, _>` — the
/// returned anchor borrows from the `CertificateDer` wrapper, not the
/// underlying `&[u8]`. If we construct the `CertificateDer` locally
/// (which we must — `Paa` stores `Vec<u8>`, not `CertificateDer`),
/// the returned anchor would borrow from a stack local and the
/// function couldn't return it. So we [`TrustAnchor::to_owned`] the
/// result, copying the three small slices (subject DN, SPKI, optional
/// name constraints — together a few hundred bytes) onto the heap.
/// `verify_chain` calls this once per invocation,
/// so the cost is negligible (the path validator itself does far more
/// allocation per call).
///
/// # Errors
///
/// Returns [`AttestationError::Parse`] if webpki cannot parse the PAA
/// DER. Should be unreachable in practice — [`Paa::from_der`] already
/// validated the bytes as a self-signed Matter PAA in M6.2.1 — but
/// `x509-parser` (M6.2.1's parser) and webpki's internal parser are
/// distinct implementations, so we wrap rather than panic on any
/// divergence.
///
/// # Why webpki 0.103 doesn't expose `webpki::types::*`
///
/// Pre-0.103, webpki re-exported `rustls-pki-types` items under
/// `webpki::types::*`. 0.103 dropped the re-export — the types now
/// live at their canonical path (`rustls_pki_types::*`), and crates
/// like ours that name them in signatures pull `rustls-pki-types`
/// directly. The Cargo.toml comment on that dep records this.
//
// pub(crate) — the only legitimate caller is `verify_chain`.
// External callers don't need `TrustAnchor` in their hands; they
// see only [`AttestationError`] / [`ChainVerification`].
pub(crate) fn paa_to_trust_anchor(paa: &Paa) -> Result<TrustAnchor<'static>, AttestationError> {
    // `CertificateDer::from(&[u8])` is a zero-cost newtype wrap — no
    // copy of the PAA DER.
    let cert_der = CertificateDer::from(paa.der());
    webpki::anchor_from_trusted_cert(&cert_der)
        .map(|anchor| anchor.to_owned())
        .map_err(|e| AttestationError::Parse(Box::new(e)))
}

/// Outcome of a successful [`verify_chain`] call.
///
/// Returned by value (cheap — a few small fields plus an owned DER
/// public-key blob). Callers persist the [`VendorId`]/[`ProductId`]
/// for fabric records and pass `dac_public_key` to M6.2.3's
/// `verify_attestation_response`.
#[derive(Debug, Clone)]
pub struct ChainVerification {
    /// [`VendorId`] matched on both the DAC and PAI subject DNs.
    pub vendor_id: VendorId,
    /// [`ProductId`] matched on the DAC subject DN (and on the PAI if
    /// the PAI was product-scoped).
    pub product_id: ProductId,
    /// DAC subject public key — raw P-256 SEC1 uncompressed bytes
    /// (`0x04 || X || Y`, 65 bytes).
    pub dac_public_key: Vec<u8>,
    /// DER-encoded PAA subject Name. Opaque to most callers; kept for
    /// audit logging ("attested by PAA `<subject>`").
    pub paa_subject: Vec<u8>,
    /// `SubjectKeyIdentifier` of the PAA that anchored the chain, if it
    /// carries one. Used to enforce a Certification Declaration's
    /// `authorized_paa_list` (Matter §6.2.3): the CD may restrict which
    /// PAAs are allowed to attest the device, matched on this SKID.
    pub paa_skid: Option<Vec<u8>>,
}

/// Verify a Matter attestation chain.
///
/// Runs `rustls-webpki`'s RFC 5280 path validation (signature, name
/// chaining, validity windows, and `BasicConstraints` cA/path-length),
/// then layers Matter §6.2.3's VID/PID equality overlay on top. The DAC
/// is treated as the end-entity, the PAI as the sole intermediate, and
/// the trust store as the set of candidate PAAs.
///
/// **What webpki does *not* check.** `rustls-webpki` deliberately
/// **ignores the `KeyUsage` extension** for validation (see its
/// `verify_cert.rs`: *"For cert validation, we ignore the `KeyUsage`
/// extension"*), and it treats `ExtendedKeyUsage` as
/// *required-if-present* only — an absent EKU passes. We pass
/// [`KeyUsage::client_auth`] below, so a present EKU that lacks
/// `id-kp-clientAuth` is still rejected, but an EKU-less cert is not.
/// The full Matter attestation-certificate *profile* — the `KeyUsage`
/// bits, `SubjectKeyIdentifier`/`AuthorityKeyIdentifier` presence, the
/// certificate version, the signature algorithm, and the role-correct
/// `BasicConstraints` — is enforced separately by the crate-internal
/// `verify_attestation_cert_format`, which the commissioner runs
/// alongside this function (as chip's device attestation verifier does).
///
/// Pure sans-I/O: no clock reads, no network, no internal state.
/// Time is supplied via [`MatterTime`] so tests can pin behaviour to
/// fixture validity windows.
///
/// # Errors
///
/// - [`AttestationError::TimeBoundsViolation`] — a cert in the chain
///   was outside its validity window at `at`.
/// - [`AttestationError::BasicConstraintsViolation`] — a non-CA cert
///   was flagged as a CA, or the path-length constraint was violated.
/// - [`AttestationError::UntrustedRoot`] — no PAA in `trust_store`
///   anchors the PAI.
/// - [`AttestationError::InvalidChain`] — any other webpki rejection
///   (signature mismatch, unsupported algorithm, missing EKU, …).
/// - [`AttestationError::VidMismatch`] — DAC subject VID does not
///   equal PAI subject VID.
/// - [`AttestationError::PaiVidNotAuthorized`] — PAI is product-scoped
///   (carries a subject PID) and that PID does not equal the DAC's.
/// - [`AttestationError::PaaVidScopeMismatch`] — the anchoring PAA is
///   VID-scoped and its scoped VID does not equal the DAC/PAI subject
///   VID (Matter §6.2.2.1).
/// - [`AttestationError::Parse`] — a PAA in the trust store could not
///   be re-parsed by webpki (should be unreachable, since
///   [`Paa::from_der`] already validated the bytes).
pub fn verify_chain(
    dac: &Dac,
    pai: &Pai,
    trust_store: &PaaTrustStore,
    at: MatterTime,
) -> Result<ChainVerification, AttestationError> {
    // 1. Lift every PAA in the trust store into a webpki TrustAnchor.
    //    Each anchor borrows its bytes from a heap copy we own
    //    (paa_to_trust_anchor's `to_owned()` call), so the resulting
    //    Vec is `'static`-borrowed and can outlive any stack-local
    //    CertificateDer wrappers below.
    let anchors: Vec<TrustAnchor<'static>> = trust_store
        .iter()
        .map(paa_to_trust_anchor)
        .collect::<Result<Vec<_>, _>>()?;

    // 2. Wrap DAC + PAI DER as the webpki types. `CertificateDer::from`
    //    on a `&[u8]` is a zero-cost newtype wrap.
    let dac_der = CertificateDer::from(dac.der());
    let pai_der = CertificateDer::from(pai.der());
    let intermediates = [pai_der];
    let end_entity = EndEntityCert::try_from(&dac_der).map_err(map_webpki_error)?;

    // 3. Project MatterTime onto webpki's UnixTime. MatterTime stores
    //    seconds-since-Matter-epoch (2000-01-01); its `to_unix_secs`
    //    converts to seconds-since-Unix-epoch, which is the unit
    //    UnixTime takes.
    let now = UnixTime::since_unix_epoch(Duration::from_secs(at.to_unix_secs()));

    // 4. Path validation. webpki checks: signature on each cert with
    //    `MATTER_SIG_ALGS`; validity window vs `now`; BasicConstraints
    //    on every CA; KeyUsage matches the requested usage; EKU
    //    contains `id-kp-clientAuth` (Matter §6.5). No revocation
    //    (Matter doesn't define CRLs/OCSP for attestation in M6.2);
    //    no extra `verify_path` predicate (the Matter overlay below
    //    runs after webpki returns so we can produce typed errors
    //    rather than `Error::Other`).
    end_entity
        .verify_for_usage(
            MATTER_SIG_ALGS,
            &anchors,
            &intermediates,
            now,
            KeyUsage::client_auth(),
            None,
            None,
        )
        .map_err(map_webpki_error)?;

    // 5. Matter §6.2.3 overlay — VID/PID equality. webpki has already
    //    accepted the signatures and name-chain, so a mismatch here
    //    is a Matter-policy rejection rather than an X.509 one.
    let dac_vid = dac.subject_vid();
    let pai_vid = pai.subject_vid();
    if dac_vid != pai_vid {
        return Err(AttestationError::VidMismatch {
            dac: dac_vid,
            pai: pai_vid,
        });
    }
    if let Some(pai_pid) = pai.subject_pid() {
        if pai_pid != dac.subject_pid() {
            return Err(AttestationError::PaiVidNotAuthorized);
        }
    }

    // 6. Identify which PAA in the store actually anchored the chain
    //    so callers can audit-log "attested by PAA <subject>". Walk
    //    the trust store and find the PAA whose subject Name matches
    //    the PAI's issuer Name. Self-signed PAAs have
    //    issuer == subject (RFC 5280 §4.1.2.4), so this is the PAA
    //    webpki must have selected. If webpki accepted the chain
    //    above, exactly one such PAA exists; the `ok_or` is a safety
    //    net against subject-name encoding drift between webpki and
    //    x509-parser and should be unreachable.
    // x509-parser's `X509Name::as_raw()` returns the full DER-encoded
    // `Name` SEQUENCE (tag + length + contents), but webpki's
    // `TrustAnchor::subject` field stores only the SEQUENCE contents
    // (it strips the outer tag/length when extracting from the cert
    // — see `extract_trust_anchor_from_v1_cert_der` in webpki's
    // `trust_anchor.rs`). So we strip the SEQUENCE wrapper from the
    // PAI's issuer to put both sides on the same footing before
    // byte-comparing.
    let pai_issuer_contents =
        strip_sequence_wrapper(pai.issuer_raw()).ok_or(AttestationError::UntrustedRoot)?;
    let (anchoring_paa, paa_subject) = trust_store
        .iter()
        .find_map(|paa| {
            let anchor = paa_to_trust_anchor(paa).ok()?;
            if anchor.subject.as_ref() == pai_issuer_contents {
                Some((paa, anchor.subject.as_ref().to_vec()))
            } else {
                None
            }
        })
        .ok_or(AttestationError::UntrustedRoot)?;

    // 7. Matter §6.2.2.1 — VID-scoped PAA scope. webpki name-chained the
    //    PAI to this PAA on subject/issuer DN equality alone; it does
    //    NOT interpret the Matter VID OID as a NameConstraint. So when
    //    the anchoring PAA is itself VID-scoped, we must enforce that
    //    its scoped VID equals the DAC/PAI subject VID — otherwise a
    //    VID-scoped PAA could anchor a chain for a different vendor.
    //    A non-VID-scoped PAA (subject_vid == None) imposes no
    //    constraint. `dac_vid == pai_vid` was already established in
    //    step 5, so comparing against `dac_vid` covers both.
    if let Some(paa_vid) = anchoring_paa.subject_vid() {
        if paa_vid != dac_vid {
            return Err(AttestationError::PaaVidScopeMismatch { paa_vid, dac_vid });
        }
    }

    Ok(ChainVerification {
        vendor_id: dac_vid,
        product_id: dac.subject_pid(),
        dac_public_key: dac.public_key().to_vec(),
        paa_subject,
        paa_skid: anchoring_paa.subject_key_identifier(),
    })
}

/// Strip a DER `SEQUENCE` tag-and-length wrapper from `bytes`,
/// returning the inner contents.
///
/// Used to align an x509-parser-produced `Name` DER (full SEQUENCE
/// with tag + length + contents) against webpki's `TrustAnchor::subject`
/// (only the contents, the tag/length having been stripped during
/// anchor extraction). Returns `None` if `bytes` is not a definite-length
/// `SEQUENCE` or if the declared length runs past the slice — both
/// indicate input that already failed earlier DER parsing, so the
/// caller treats them as `UntrustedRoot`.
///
/// Handles the two length encodings observed in practice for Matter
/// `Name`s: short-form (single length byte, content < 128 bytes) and
/// long-form `0x81`/`0x82` (one or two length bytes, content up to
/// 65 535 bytes). Longer forms are rejected — a Matter `Name` would
/// never exceed a few hundred bytes.
fn strip_sequence_wrapper(bytes: &[u8]) -> Option<&[u8]> {
    // SEQUENCE constructed: tag byte 0x30.
    let (&tag, rest) = bytes.split_first()?;
    if tag != 0x30 {
        return None;
    }
    let (&first_len_byte, after_first) = rest.split_first()?;
    let (content_len, header_bytes) = match first_len_byte {
        // Short form: top bit clear, value is the length itself.
        n if n < 0x80 => (n as usize, 0_usize),
        // Long form: 0x81 = 1 length byte, 0x82 = 2 length bytes.
        0x81 => {
            let (&len, _) = after_first.split_first()?;
            (len as usize, 1)
        }
        0x82 => {
            let len_bytes: &[u8; 2] = after_first.get(..2)?.try_into().ok()?;
            (u16::from_be_bytes(*len_bytes) as usize, 2)
        }
        // 0x80 (indefinite-length) and 0x83+ (>= 16 MiB) are out of
        // scope for Matter Names.
        _ => return None,
    };
    let content_start = 2 + header_bytes;
    let content_end = content_start.checked_add(content_len)?;
    bytes.get(content_start..content_end)
}

#[cfg(test)]
mod tests {
    // The synthetic-chain helpers pair `dac_vid`/`paa_vid`,
    // `dac_der`/`pai_der`/`paa_der`, etc. — the near-identical names mirror
    // the PKI roles by design. Same carve-out as `tests/support/mod.rs`.
    #![allow(clippy::similar_names, clippy::struct_field_names)]

    use super::*;
    use crate::attestation::PaaTrustStore;

    const HAPPY_DAC: &[u8] = include_bytes!(
        "../../../../test-vectors/certs/attestation/happy-path/Chip-Test-DAC-FFF1-8000-0004-Cert.der"
    );
    const HAPPY_PAI: &[u8] = include_bytes!(
        "../../../../test-vectors/certs/attestation/happy-path/Chip-Test-PAI-FFF1-8000-Cert.der"
    );

    #[test]
    #[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn paa_to_trust_anchor_works_on_bundled_csa_root() {
        let store = PaaTrustStore::with_example_device_roots();
        let paa = store.iter().next().unwrap();
        // Must not error; webpki should accept any well-formed
        // X.509v3 self-signed cert that Paa::from_der accepted.
        let _anchor = paa_to_trust_anchor(paa).unwrap();
    }

    #[test]
    #[allow(clippy::expect_used, clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn verify_chain_happy_path_on_csa_test_vectors() {
        let dac = Dac::from_der(HAPPY_DAC).unwrap();
        let pai = Pai::from_der(HAPPY_PAI).unwrap();
        let store = PaaTrustStore::with_example_device_roots();

        // CSA test DAC issued ~2022 with multi-year validity; 2024
        // sits safely inside the window. Pinning the clock keeps the
        // test deterministic regardless of when it runs.
        let at = MatterTime::from_unix_secs(1_704_067_200); // 2024-01-01

        let result = verify_chain(&dac, &pai, &store, at).expect("happy-path verify_chain");
        assert_eq!(result.vendor_id, VendorId::new(0xFFF1));
        assert_eq!(result.product_id, ProductId::new(0x8000));
        assert_eq!(result.dac_public_key.len(), 65);
        assert_ne!(result.paa_subject, b"");
    }

    // ── VID-scoped PAA scope (Matter §6.2.2.1) ──────────────────────────────
    //
    // These tests synthesise a fresh DAC → PAI → PAA chain whose subject
    // VIDs we control independently, using the same recipe as the
    // integration-test `build_mock_device_pki` helper (the chip
    // `gen-negative-fixtures.py` extension layout). They exercise the
    // step-7 PAA VID-scope overlay:
    //
    //   - VID-scoped PAA whose scope MATCHES the DAC/PAI VID → accepted.
    //   - VID-scoped PAA whose scope DIFFERS from the DAC/PAI VID →
    //     PaaVidScopeMismatch (pre-fix this wrongly passed, because webpki
    //     name-chains on DN equality and never reads the Matter VID OID as
    //     a NameConstraint).
    //   - Non-VID-scoped PAA (subject_vid None) → accepted regardless of
    //     the DAC/PAI VID.

    use matter_cert::test_support::{build_x509_der, TestCertFields};
    use matter_cert::{
        BasicConstraints, DistinguishedName, DnAttribute, Extensions, KeyUsage, Signature,
    };
    use matter_crypto::{CaseSigner as _, RingSigner};

    /// EKU compact integer for `id-kp-clientAuth` (OID 1.3.6.1.5.5.7.3.2),
    /// the EKU `verify_chain` requires on the DAC. Matches the value the
    /// matter-cert X.509 encoder maps to clientAuth.
    const EKU_CLIENT_AUTH: u32 = 2;

    /// A synthetic DAC → PAI → PAA chain with independently chosen VIDs,
    /// returned as raw DER for feeding into `verify_chain`.
    struct SyntheticChain {
        dac_der: Vec<u8>,
        pai_der: Vec<u8>,
        paa_der: Vec<u8>,
    }

    /// Build a synthetic chain. `paa_vid == None` produces a non-VID-scoped
    /// PAA; `Some(v)` scopes the PAA subject to VID `v`. The PAI and DAC are
    /// always scoped to `device_vid`; the DAC also carries `device_pid`.
    ///
    /// Validity windows bracket `at_unix` exactly as `build_mock_device_pki`
    /// does, so the resulting chain validates at that instant.
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn build_synthetic_chain(
        at_unix: u64,
        paa_vid: Option<u16>,
        device_vid: u16,
        device_pid: u16,
    ) -> SyntheticChain {
        // PAA: self-signed root, optionally VID-scoped.
        let (paa_signer, paa_pkcs8) = RingSigner::generate().expect("PAA key");
        let mut paa_attrs = vec![DnAttribute::CommonName("Synthetic Test PAA".into())];
        if let Some(v) = paa_vid {
            paa_attrs.push(DnAttribute::VendorId(v));
        }
        let paa_dn = DistinguishedName::new(paa_attrs);
        let paa_der = build_x509_der(
            TestCertFields {
                serial: vec![0x01],
                issuer: paa_dn.clone(),
                not_before: MatterTime::from_unix_secs(at_unix.saturating_sub(365 * 86_400)),
                not_after: MatterTime::from_unix_secs(at_unix.saturating_add(3650 * 86_400)),
                subject: paa_dn.clone(),
                public_key: paa_signer.public_key().clone(),
                extensions: Extensions::builder()
                    .basic_constraints(Some(BasicConstraints::new(true, Some(1))))
                    .key_usage(Some(KeyUsage::KEY_CERT_SIGN | KeyUsage::CRL_SIGN))
                    .build(),
                signature: Signature::new([0u8; 64]),
            },
            &paa_pkcs8,
        )
        .expect("PAA DER");

        // PAI: signed by PAA, scoped to device_vid.
        let (pai_signer, pai_pkcs8) = RingSigner::generate().expect("PAI key");
        let pai_dn = DistinguishedName::new(vec![
            DnAttribute::CommonName("Synthetic Test PAI".into()),
            DnAttribute::VendorId(device_vid),
        ]);
        let pai_der = build_x509_der(
            TestCertFields {
                serial: vec![0x02],
                issuer: paa_dn,
                not_before: MatterTime::from_unix_secs(at_unix.saturating_sub(180 * 86_400)),
                not_after: MatterTime::from_unix_secs(at_unix.saturating_add(1825 * 86_400)),
                subject: pai_dn.clone(),
                public_key: pai_signer.public_key().clone(),
                extensions: Extensions::builder()
                    .basic_constraints(Some(BasicConstraints::new(true, Some(0))))
                    .key_usage(Some(KeyUsage::KEY_CERT_SIGN | KeyUsage::CRL_SIGN))
                    .build(),
                signature: Signature::new([0u8; 64]),
            },
            &paa_pkcs8,
        )
        .expect("PAI DER");

        // DAC: leaf, signed by PAI, scoped to device_vid + device_pid.
        let (dac_signer, _dac_pkcs8) = RingSigner::generate().expect("DAC key");
        let dac_dn = DistinguishedName::new(vec![
            DnAttribute::CommonName("Synthetic Test DAC".into()),
            DnAttribute::VendorId(device_vid),
            DnAttribute::ProductId(device_pid),
        ]);
        let dac_der = build_x509_der(
            TestCertFields {
                serial: vec![0x03],
                issuer: pai_dn,
                not_before: MatterTime::from_unix_secs(at_unix.saturating_sub(30 * 86_400)),
                not_after: MatterTime::from_unix_secs(at_unix.saturating_add(365 * 86_400)),
                subject: dac_dn,
                public_key: dac_signer.public_key().clone(),
                extensions: Extensions::builder()
                    .basic_constraints(Some(BasicConstraints::new(false, None)))
                    .key_usage(Some(KeyUsage::DIGITAL_SIGNATURE))
                    .extended_key_usage(Some(vec![EKU_CLIENT_AUTH]))
                    .build(),
                signature: Signature::new([0u8; 64]),
            },
            &pai_pkcs8,
        )
        .expect("DAC DER");

        SyntheticChain {
            dac_der,
            pai_der,
            paa_der,
        }
    }

    /// Build a single-PAA trust store from the synthetic PAA DER.
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn store_with(paa_der: &[u8]) -> PaaTrustStore {
        let mut store = PaaTrustStore::empty();
        store.add(Paa::from_der(paa_der).expect("synthetic PAA parses"));
        store
    }

    const SYNTH_AT_UNIX: u64 = 1_800_000_000; // ~2027-01-15, inside every window.

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn vid_scoped_paa_matching_device_vid_is_accepted() {
        // VID-scoped PAA (0xFFF1) anchoring a 0xFFF1 device → no mismatch.
        let chain = build_synthetic_chain(SYNTH_AT_UNIX, Some(0xFFF1), 0xFFF1, 0x8001);
        let dac = Dac::from_der(&chain.dac_der).expect("DAC parses");
        let pai = Pai::from_der(&chain.pai_der).expect("PAI parses");
        let store = store_with(&chain.paa_der);
        let at = MatterTime::from_unix_secs(SYNTH_AT_UNIX);

        let result =
            verify_chain(&dac, &pai, &store, at).expect("matching VID-scoped PAA accepted");
        assert_eq!(result.vendor_id, VendorId::new(0xFFF1));
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn vid_scoped_paa_mismatched_device_vid_is_rejected() {
        // VID-scoped PAA (0xFFF2) anchoring a 0xFFF1 device. webpki accepts
        // the DN-chained path (the VID OID is opaque to it), so the §6.2.2.1
        // overlay must reject. Pre-fix this wrongly passed.
        let chain = build_synthetic_chain(SYNTH_AT_UNIX, Some(0xFFF2), 0xFFF1, 0x8001);
        let dac = Dac::from_der(&chain.dac_der).expect("DAC parses");
        let pai = Pai::from_der(&chain.pai_der).expect("PAI parses");
        let store = store_with(&chain.paa_der);
        let at = MatterTime::from_unix_secs(SYNTH_AT_UNIX);

        let err = verify_chain(&dac, &pai, &store, at)
            .expect_err("VID-scoped PAA anchoring a different vendor must be rejected");
        assert!(
            matches!(
                err,
                AttestationError::PaaVidScopeMismatch {
                    paa_vid,
                    dac_vid,
                } if paa_vid == VendorId::new(0xFFF2) && dac_vid == VendorId::new(0xFFF1)
            ),
            "expected PaaVidScopeMismatch {{ paa_vid: FFF2, dac_vid: FFF1 }}, got {err:?}"
        );
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn non_vid_scoped_paa_imposes_no_vid_constraint() {
        // Non-VID-scoped PAA anchoring a 0xFFF1 device → accepted; the
        // overlay short-circuits on subject_vid() == None.
        let chain = build_synthetic_chain(SYNTH_AT_UNIX, None, 0xFFF1, 0x8001);
        let dac = Dac::from_der(&chain.dac_der).expect("DAC parses");
        let pai = Pai::from_der(&chain.pai_der).expect("PAI parses");
        let store = store_with(&chain.paa_der);
        let at = MatterTime::from_unix_secs(SYNTH_AT_UNIX);

        let result =
            verify_chain(&dac, &pai, &store, at).expect("non-VID-scoped PAA imposes no constraint");
        assert_eq!(result.vendor_id, VendorId::new(0xFFF1));
    }
}
