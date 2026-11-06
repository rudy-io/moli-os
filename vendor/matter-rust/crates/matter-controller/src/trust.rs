//! Device-attestation trust material: the PAA roots that anchor DAC/PAI chain
//! validation and the CD signing roots that anchor Certification-Declaration
//! signatures. Configured once on the controller (attestation is a fabric-wide
//! security policy — chip holds it on the commissioner the same way).
//!
//! This is a concrete value type for v1.0. When ledger-backed sourcing (DCL)
//! lands post-1.0, a `trait AttestationVerifier` can emerge here without an
//! API break to the commissioning entry point.

use std::path::Path;

use matter_commissioning::{CdSigningRoots, PaaTrustStore};

use crate::error::Error;

/// The trust anchors used to verify a device during commissioning.
#[derive(Debug)]
pub struct AttestationTrust {
    pub(crate) paa: PaaTrustStore,
    pub(crate) cd: CdSigningRoots,
}

impl AttestationTrust {
    /// Construct from the bundled CSA **test / example** roots: the PAA
    /// test roots plus the CD signing roots that verify chip's example
    /// devices — the synthetic loopback root, chip's test CD signing
    /// authority, and CSA production "CD Signing Key 001" (which signs the
    /// VID=0xFFF1 CD served by every example-DAC device, including the
    /// esp-matter ESP32-C6). Suitable for CSA-test / dev / example devices
    /// and the hermetic loopback.
    ///
    /// This is **not** the full CSA production trust set: an arbitrary
    /// certified product may present a DAC chained to a PAA, or a CD signed
    /// by a CSA production key, that is not bundled here. Commissioners for
    /// arbitrary certified devices use [`Self::from_dirs`] pointed at the
    /// production roots.
    #[must_use]
    pub fn example_device_roots() -> Self {
        Self {
            paa: PaaTrustStore::with_example_device_roots(),
            cd: CdSigningRoots::with_example_device_roots(),
        }
    }

    /// Load PAA roots from a directory of `.der` certificates and CD signing
    /// roots from a directory (or single file) of `.der` certificates — the
    /// production path (e.g. connectedhomeip's `credentials/production/...`).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Trust`] if a directory cannot be read or a certificate
    /// fails to parse.
    pub fn from_dirs(paa_dir: &Path, cd_dir: &Path) -> Result<Self, Error> {
        let mut paa = PaaTrustStore::empty();
        for entry in
            std::fs::read_dir(paa_dir).map_err(|e| Error::Trust(format!("paa dir: {e}")))?
        {
            let path = entry
                .map_err(|e| Error::Trust(format!("paa entry: {e}")))?
                .path();
            if path.extension().and_then(|x| x.to_str()) != Some("der") {
                continue;
            }
            let der = std::fs::read(&path).map_err(|e| Error::Trust(format!("paa read: {e}")))?;
            let cert = matter_commissioning::Paa::from_der(&der)
                .map_err(|e| Error::Trust(format!("paa parse {}: {e:?}", path.display())))?;
            paa.add(cert);
        }

        let mut cd_ders: Vec<Vec<u8>> = Vec::new();
        if cd_dir.is_dir() {
            for entry in
                std::fs::read_dir(cd_dir).map_err(|e| Error::Trust(format!("cd dir: {e}")))?
            {
                let path = entry
                    .map_err(|e| Error::Trust(format!("cd entry: {e}")))?
                    .path();
                if path.extension().and_then(|x| x.to_str()) != Some("der") {
                    continue;
                }
                cd_ders
                    .push(std::fs::read(&path).map_err(|e| Error::Trust(format!("cd read: {e}")))?);
            }
        } else {
            cd_ders.push(std::fs::read(cd_dir).map_err(|e| Error::Trust(format!("cd read: {e}")))?);
        }
        let refs: Vec<&[u8]> = cd_ders.iter().map(Vec::as_slice).collect();
        let cd = CdSigningRoots::from_cert_der(&refs)
            .map_err(|e| Error::Trust(format!("cd parse: {e:?}")))?;

        Ok(Self { paa, cd })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests with justification (CONTRIBUTING.md).
mod tests {
    use super::*;

    #[test]
    fn example_device_roots_constructs() {
        let _trust = AttestationTrust::example_device_roots();
        // Construction succeeds and yields usable PAA + CD stores; deeper
        // verification is covered by matter-commissioning's attestation tests.
    }

    #[test]
    fn from_dirs_errors_on_missing_dir() {
        let err = AttestationTrust::from_dirs(
            Path::new("/nonexistent/paa"),
            Path::new("/nonexistent/cd"),
        )
        .expect_err("missing dir must error");
        assert!(matches!(err, Error::Trust(_)));
    }

    /// `from_dirs` must skip non-`.der` files (`.pem`, `.txt`, etc.) in
    /// both the PAA and CD directories.  The connectedhomeip
    /// `credentials/development/{paa-root-certs,cd-certs}` directories
    /// contain `.pem` files alongside each `.der`; without the extension
    /// filter, `from_dirs` errors trying to parse PEM as DER.
    ///
    /// Test strategy:
    /// - Write a real PAA root DER into a temp PAA dir alongside a junk
    ///   `.pem` and a `.txt`.
    /// - Write a real X.509 P-256 cert DER into a temp CD dir alongside
    ///   the same junk files.
    /// - Assert that `from_dirs` succeeds (junk files were skipped) and
    ///   that each store contains exactly 1 entry.
    ///
    /// Temp dirs are created under `target/` so they stay out of the
    /// source tree and survive interrupted runs gracefully (the directory
    /// is cleaned up at the end of the test).
    ///
    /// Cert fixtures are read from the in-repo `test-vectors/` tree via
    /// `CARGO_MANIFEST_DIR` so no extra crate dependency is required.
    #[test]
    fn from_dirs_skips_non_der_files() {
        use std::fs;

        // ── locate in-repo fixtures ────────────────────────────────────────
        // CARGO_MANIFEST_DIR points to `crates/matter-controller/`.
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest_dir
            .parent() // crates/
            .unwrap()
            .parent() // matter-rust/
            .unwrap();

        // PAA cert: a real Matter PAA (no-VID variant) bundled in the
        // commissioning crate's CSA test-root collection.
        let paa_der_src = repo_root
            .join("crates/matter-commissioning/src/attestation/csa_test_roots")
            .join("Chip-Test-PAA-NoVID-Cert.der");
        let paa_bytes = fs::read(&paa_der_src).expect("bundled PAA NoVID DER must be readable");

        // CD signing cert: reuse the same PAA cert (any X.509 P-256 cert
        // satisfies `CdSigningRoots::from_cert_der`; we only need the
        // extension filter to run, not a real attestation verification).
        let cd_bytes = paa_bytes.clone();

        // ── build temp directories under target/ ──────────────────────────
        let target_dir = repo_root.join("target").join("from-dirs-test");
        let paa_dir = target_dir.join("paa");
        let cd_dir = target_dir.join("cd");
        fs::create_dir_all(&paa_dir).expect("create temp PAA dir");
        fs::create_dir_all(&cd_dir).expect("create temp CD dir");

        // Write the real DER cert into each dir.
        fs::write(paa_dir.join("test-paa.der"), &paa_bytes).expect("write PAA DER");
        fs::write(cd_dir.join("test-cd.der"), &cd_bytes).expect("write CD DER");

        // Write junk files alongside — these must be silently skipped.
        fs::write(paa_dir.join("test-paa.pem"), b"not der at all")
            .expect("write junk pem in PAA dir");
        fs::write(paa_dir.join("README.txt"), b"also junk").expect("write junk txt in PAA dir");
        fs::write(cd_dir.join("test-cd.pem"), b"not der at all").expect("write junk pem in CD dir");
        fs::write(cd_dir.join("notes.txt"), b"also junk").expect("write junk txt in CD dir");

        // ── exercise `from_dirs` ──────────────────────────────────────────
        let trust = AttestationTrust::from_dirs(&paa_dir, &cd_dir)
            .expect("from_dirs must succeed when non-.der files are present");

        // Each dir contained exactly one .der file.
        assert_eq!(trust.paa.len(), 1, "exactly one PAA loaded");
        assert_eq!(trust.cd.len(), 1, "exactly one CD signing root loaded");

        // ── clean up ──────────────────────────────────────────────────────
        // Best-effort: a failure here does not invalidate the test result.
        let _ = fs::remove_dir_all(&target_dir);
    }
}
