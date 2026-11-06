//! X.509 DER cert wrappers for the Matter attestation chain
//! ([`Dac`], [`Pai`], [`Paa`]).
//!
//! Each wrapper owns its DER bytes plus the few fields the verifier
//! needs (subject VID/PID per role, SPKI). Parsing happens once at
//! construction so the accessors are infallible and cheap.

use x509_parser::prelude::{FromDer, X509Certificate};

use crate::attestation::error::AttestationError;
use crate::attestation::extensions::{self, ProductId, VendorId};

/// Device Attestation Certificate.
///
/// Matter Core Spec §6.2.3: a conventional X.509v3 leaf certificate
/// issued by a PAI. Subject DN MUST contain both a [`VendorId`] and a
/// [`ProductId`] attribute (Matter §6.5.6).
#[derive(Debug, Clone)]
pub struct Dac {
    der: Vec<u8>,
    subject_vid: VendorId,
    subject_pid: ProductId,
    public_key: Vec<u8>,
}

impl Dac {
    /// Parse and validate the structural shape of a DAC from its DER
    /// bytes. Does NOT validate the signature chain — that's the job
    /// of `verify_chain` in M6.2.2.
    ///
    /// # Errors
    ///
    /// Returns [`AttestationError::Parse`] if the DER is malformed,
    /// or if the subject DN is missing the required Matter
    /// [`VendorId`] or [`ProductId`] attribute, or if either of those
    /// attributes' values is itself malformed (not 4 UPPERCASE hex
    /// chars per Matter §6.5.6.1).
    pub fn from_der(bytes: &[u8]) -> Result<Self, AttestationError> {
        let (_, cert) = X509Certificate::from_der(bytes)
            .map_err(|e| AttestationError::Parse(Box::new(e.clone())))?;
        let subject = cert.subject();

        let vid = extensions::extract_vid(subject)
            .map_err(|e| AttestationError::Parse(Box::new(e)))?
            .ok_or_else(|| {
                AttestationError::Parse(Box::new(MissingRequired("DAC subject VendorId")))
            })?;

        let pid = extensions::extract_pid(subject)
            .map_err(|e| AttestationError::Parse(Box::new(e)))?
            .ok_or_else(|| {
                AttestationError::Parse(Box::new(MissingRequired("DAC subject ProductId")))
            })?;

        let public_key = cert.public_key().subject_public_key.data.as_ref().to_vec();

        Ok(Self {
            der: bytes.to_vec(),
            subject_vid: vid,
            subject_pid: pid,
            public_key,
        })
    }

    /// Borrow the original DER bytes.
    pub fn der(&self) -> &[u8] {
        &self.der
    }

    /// Subject [`VendorId`] (Matter spec: required on DAC).
    pub fn subject_vid(&self) -> VendorId {
        self.subject_vid
    }

    /// Subject [`ProductId`] (Matter spec: required on DAC).
    pub fn subject_pid(&self) -> ProductId {
        self.subject_pid
    }

    /// Subject Public Key Info — raw P-256 SEC1 uncompressed bytes
    /// (`0x04` || X || Y, 65 bytes total). This is what `ring` will
    /// consume in M6.2.3 for ECDSA signature verification.
    pub fn public_key(&self) -> &[u8] {
        &self.public_key
    }
}

/// Product Attestation Intermediate certificate.
///
/// Matter Core Spec §6.2.3: an intermediate X.509v3 CA cert issued by
/// a PAA, that in turn issues DACs. Subject DN MUST contain a
/// [`VendorId`]; MAY contain a [`ProductId`] (which, when present,
/// scopes the PAI to a single product within the vendor).
#[derive(Debug, Clone)]
pub struct Pai {
    der: Vec<u8>,
    subject_vid: VendorId,
    subject_pid: Option<ProductId>,
    public_key: Vec<u8>,
    issuer_raw: Vec<u8>,
}

impl Pai {
    /// Parse and validate the structural shape of a PAI from its DER
    /// bytes. Does NOT validate the signature chain — that's the job
    /// of `verify_chain` in M6.2.2.
    ///
    /// # Errors
    ///
    /// Returns [`AttestationError::Parse`] if the DER is malformed,
    /// or if the subject DN is missing the required Matter
    /// [`VendorId`] attribute, or if the VID/PID attribute values
    /// are themselves malformed (not 4 UPPERCASE hex chars per
    /// Matter §6.5.6.1).
    pub fn from_der(bytes: &[u8]) -> Result<Self, AttestationError> {
        let (_, cert) = X509Certificate::from_der(bytes)
            .map_err(|e| AttestationError::Parse(Box::new(e.clone())))?;
        let subject = cert.subject();

        let vid = extensions::extract_vid(subject)
            .map_err(|e| AttestationError::Parse(Box::new(e)))?
            .ok_or_else(|| {
                AttestationError::Parse(Box::new(MissingRequired("PAI subject VendorId")))
            })?;

        let pid =
            extensions::extract_pid(subject).map_err(|e| AttestationError::Parse(Box::new(e)))?;

        let public_key = cert.public_key().subject_public_key.data.as_ref().to_vec();

        // Cache the issuer Name's raw DER so M6.2.2's `verify_chain`
        // can match PAI -> PAA on the trust-store side without
        // re-parsing the certificate. Each anchor's `subject` field
        // (a `rustls_pki_types::Der`) compares byte-for-byte against
        // this slice — both come from the same encoding of the same
        // Name in the chain.
        let issuer_raw = cert.tbs_certificate.issuer.as_raw().to_vec();

        Ok(Self {
            der: bytes.to_vec(),
            subject_vid: vid,
            subject_pid: pid,
            public_key,
            issuer_raw,
        })
    }

    /// Borrow the original DER bytes.
    pub fn der(&self) -> &[u8] {
        &self.der
    }

    /// Issuer Name (DER-encoded `Name` SEQUENCE, exactly as it
    /// appears in the certificate's `tbsCertificate.issuer` field).
    ///
    /// Used by `verify_chain` to identify which PAA in the trust
    /// store actually anchored the validated chain — the matching
    /// anchor's `subject` (per RFC 5280 §4.1.2.4 a self-signed
    /// root's `issuer` and `subject` are identical) is the value
    /// returned here.
    pub fn issuer_raw(&self) -> &[u8] {
        &self.issuer_raw
    }

    /// Subject [`VendorId`] (Matter spec: required on PAI).
    pub fn subject_vid(&self) -> VendorId {
        self.subject_vid
    }

    /// Subject [`ProductId`] (Matter spec: optional on PAI; `None`
    /// means the PAI authorises any product within its vendor).
    pub fn subject_pid(&self) -> Option<ProductId> {
        self.subject_pid
    }

    /// Subject Public Key Info — raw P-256 SEC1 uncompressed bytes
    /// (`0x04` || X || Y, 65 bytes total).
    pub fn public_key(&self) -> &[u8] {
        &self.public_key
    }
}

/// Product Attestation Authority — the trust root for a Matter
/// attestation chain.
///
/// Matter Core Spec §6.2.3: a self-signed X.509v3 CA cert serving as
/// a root of trust. Subject DN MAY contain a [`VendorId`]; MUST NOT
/// contain a [`ProductId`].
#[derive(Debug, Clone)]
pub struct Paa {
    der: Vec<u8>,
    subject_vid: Option<VendorId>,
    public_key: Vec<u8>,
}

impl Paa {
    /// Parse and validate the structural shape of a PAA from its DER
    /// bytes. Rejects with [`AttestationError::Parse`] if the subject
    /// contains a [`ProductId`] attribute (forbidden by Matter §6.5).
    /// Does NOT validate the self-signature — that's the job of
    /// `verify_chain` in M6.2.2.
    ///
    /// # Errors
    ///
    /// Returns [`AttestationError::Parse`] if the DER is malformed,
    /// if the subject DN contains a forbidden [`ProductId`]
    /// attribute, or if the VID attribute (when present) is itself
    /// malformed (not 4 UPPERCASE hex chars per Matter §6.5.6.1).
    pub fn from_der(bytes: &[u8]) -> Result<Self, AttestationError> {
        let (_, cert) = X509Certificate::from_der(bytes)
            .map_err(|e| AttestationError::Parse(Box::new(e.clone())))?;
        let subject = cert.subject();

        let vid =
            extensions::extract_vid(subject).map_err(|e| AttestationError::Parse(Box::new(e)))?;

        // PID in PAA subject is forbidden — error out if present.
        if extensions::extract_pid(subject)
            .map_err(|e| AttestationError::Parse(Box::new(e)))?
            .is_some()
        {
            return Err(AttestationError::Parse(Box::new(ForbiddenField(
                "PAA subject must not contain a ProductId",
            ))));
        }

        let public_key = cert.public_key().subject_public_key.data.as_ref().to_vec();

        Ok(Self {
            der: bytes.to_vec(),
            subject_vid: vid,
            public_key,
        })
    }

    /// Borrow the original DER bytes.
    pub fn der(&self) -> &[u8] {
        &self.der
    }

    /// Subject [`VendorId`] (Matter spec: optional on PAA; `None`
    /// means the PAA covers any vendor).
    pub fn subject_vid(&self) -> Option<VendorId> {
        self.subject_vid
    }

    /// Subject Public Key Info — raw P-256 SEC1 uncompressed bytes
    /// (`0x04` || X || Y, 65 bytes total).
    pub fn public_key(&self) -> &[u8] {
        &self.public_key
    }

    /// The PAA's `SubjectKeyIdentifier` extension value, if present.
    ///
    /// Used to match a Certification Declaration's `authorized_paa_list`
    /// (Matter §6.2.3) against the PAA that anchored the DAC chain. Matter
    /// PAAs carry a 20-byte SKID, but this returns whatever is present
    /// (or `None` if the extension is absent or the DER no longer parses).
    #[must_use]
    pub fn subject_key_identifier(&self) -> Option<Vec<u8>> {
        use x509_parser::extensions::ParsedExtension;
        use x509_parser::prelude::{FromDer, X509Certificate};

        let (_, cert) = X509Certificate::from_der(&self.der).ok()?;
        cert.extensions()
            .iter()
            .find_map(|ext| match ext.parsed_extension() {
                ParsedExtension::SubjectKeyIdentifier(kid) => Some(kid.0.to_vec()),
                _ => None,
            })
    }
}

/// Internal error: a required DN attribute was missing. Wrapped into
/// [`AttestationError::Parse`] by callers.
#[derive(Debug, thiserror::Error)]
#[error("required field absent: {0}")]
struct MissingRequired(&'static str);

/// Internal error: a forbidden DN attribute was present. Wrapped into
/// [`AttestationError::Parse`] by callers.
#[derive(Debug, thiserror::Error)]
#[error("forbidden field present: {0}")]
struct ForbiddenField(&'static str);

#[cfg(test)]
mod tests {
    use super::*;

    const DAC_DER: &[u8] = include_bytes!(
        "../../../../test-vectors/certs/attestation/happy-path/Chip-Test-DAC-FFF1-8000-0004-Cert.der"
    );

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn dac_from_der_parses_happy_path() {
        let dac = Dac::from_der(DAC_DER).expect("happy-path DAC parses");
        assert_eq!(dac.subject_vid(), VendorId::new(0xFFF1));
        assert_eq!(dac.subject_pid(), ProductId::new(0x8000));
    }

    #[test]
    #[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn dac_round_trips_der_bytes() {
        let dac = Dac::from_der(DAC_DER).unwrap();
        assert_eq!(dac.der(), DAC_DER);
    }

    #[test]
    #[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn dac_public_key_is_sec1_uncompressed_p256() {
        let dac = Dac::from_der(DAC_DER).unwrap();
        let pk = dac.public_key();
        // P-256 SEC1 uncompressed is 65 bytes: 0x04 || X(32) || Y(32).
        assert_eq!(pk.len(), 65, "P-256 uncompressed SPKI must be 65 bytes");
        assert_eq!(pk[0], 0x04, "leading byte must be 0x04 (uncompressed)");
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn dac_from_der_rejects_empty_input() {
        let err = Dac::from_der(&[]).expect_err("empty bytes must error");
        assert!(matches!(err, AttestationError::Parse(_)));
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn dac_from_der_rejects_truncated_input() {
        let err = Dac::from_der(&DAC_DER[..10]).expect_err("truncated must error");
        assert!(matches!(err, AttestationError::Parse(_)));
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn dac_from_der_rejects_random_bytes() {
        let garbage = vec![0xAA; 256];
        let err = Dac::from_der(&garbage).expect_err("garbage must error");
        assert!(matches!(err, AttestationError::Parse(_)));
    }

    const PAI_DER: &[u8] = include_bytes!(
        "../../../../test-vectors/certs/attestation/happy-path/Chip-Test-PAI-FFF1-8000-Cert.der"
    );

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn pai_from_der_parses_happy_path() {
        let pai = Pai::from_der(PAI_DER).expect("happy-path PAI parses");
        assert_eq!(pai.subject_vid(), VendorId::new(0xFFF1));
        // The vendored fixture is a VID+PID-scoped PAI.
        assert_eq!(pai.subject_pid(), Some(ProductId::new(0x8000)));
    }

    #[test]
    #[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn pai_round_trips_der_bytes() {
        let pai = Pai::from_der(PAI_DER).unwrap();
        assert_eq!(pai.der(), PAI_DER);
    }

    #[test]
    #[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn pai_public_key_is_sec1_uncompressed_p256() {
        let pai = Pai::from_der(PAI_DER).unwrap();
        let pk = pai.public_key();
        assert_eq!(pk.len(), 65);
        assert_eq!(pk[0], 0x04);
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn pai_from_der_rejects_garbage() {
        let err = Pai::from_der(&vec![0xAA; 256]).expect_err("garbage must error");
        assert!(matches!(err, AttestationError::Parse(_)));
    }

    const PAA_FFF1_DER: &[u8] = include_bytes!("csa_test_roots/Chip-Test-PAA-FFF1-Cert.der");
    const PAA_NOVID_DER: &[u8] = include_bytes!("csa_test_roots/Chip-Test-PAA-NoVID-Cert.der");

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn paa_from_der_parses_vid_scoped_root() {
        let paa = Paa::from_der(PAA_FFF1_DER).expect("VID-scoped PAA parses");
        assert_eq!(paa.subject_vid(), Some(VendorId::new(0xFFF1)));
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn paa_from_der_parses_unscoped_root() {
        let paa = Paa::from_der(PAA_NOVID_DER).expect("non-VID-scoped PAA parses");
        assert_eq!(paa.subject_vid(), None);
    }

    #[test]
    #[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn paa_round_trips_der_bytes() {
        let paa = Paa::from_der(PAA_FFF1_DER).unwrap();
        assert_eq!(paa.der(), PAA_FFF1_DER);
    }

    #[test]
    #[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn paa_public_key_is_sec1_uncompressed_p256() {
        let paa = Paa::from_der(PAA_FFF1_DER).unwrap();
        assert_eq!(paa.public_key().len(), 65);
        assert_eq!(paa.public_key()[0], 0x04);
    }

    #[test]
    #[allow(clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn paa_from_der_rejects_garbage() {
        let err = Paa::from_der(&vec![0xAA; 256]).expect_err("garbage must error");
        assert!(matches!(err, AttestationError::Parse(_)));
    }
}
