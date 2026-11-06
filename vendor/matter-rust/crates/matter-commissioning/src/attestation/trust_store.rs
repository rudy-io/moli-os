//! [`PaaTrustStore`] — the set of PAA certificates the commissioner
//! considers trusted roots for device-attestation chain validation.
//!
//! Constructed via [`PaaTrustStore::empty`] (for callers building
//! their own trust policy) or [`PaaTrustStore::with_example_device_roots`]
//! (for examples and integration tests; never use in production).
//! There is intentionally no `Default` impl — picking the wrong
//! default for a trust anchor list is a security footgun.

use crate::attestation::x509::Paa;

/// A collection of PAA certs that the commissioner trusts as roots of
/// attestation chains.
#[derive(Debug, Clone)]
pub struct PaaTrustStore {
    roots: Vec<Paa>,
}

impl PaaTrustStore {
    /// Construct a trust store with no PAAs. Add roots via
    /// [`PaaTrustStore::add`].
    pub fn empty() -> Self {
        Self { roots: Vec::new() }
    }

    /// Construct a trust store seeded with bundled CSA **test** PAA
    /// roots. **Do not use in production.** See the module-level
    /// docs.
    ///
    /// # Panics
    ///
    /// Panics only if the in-crate bundled DER files
    /// (`src/attestation/csa_test_roots/*.der`) fail to parse — a
    /// build-tree integrity problem, not a runtime input. The
    /// `with_example_device_roots_loads_both` unit test catches this in
    /// CI, so a panic here would mean the crate sources themselves
    /// have been corrupted.
    pub fn with_example_device_roots() -> Self {
        // `include_bytes!` is crate-relative; the DER files live
        // inside the crate (see `csa_test_roots/README.md`) so this
        // path is stable regardless of where the crate is consumed
        // from (workspace path, git dep, or — eventually — crates.io).
        const PAA_FFF1: &[u8] = include_bytes!("csa_test_roots/Chip-Test-PAA-FFF1-Cert.der");
        const PAA_NOVID: &[u8] = include_bytes!("csa_test_roots/Chip-Test-PAA-NoVID-Cert.der");

        // The bundled DER files are vendored from a known commit of
        // connectedhomeip and verified-parsing in the unit tests
        // below. If `Paa::from_der` ever rejects one, we want a hard
        // compile-time-ish failure (the unit test
        // `with_example_device_roots_loads_both` catches it), not a silent
        // empty store. We propagate any parse error by panicking
        // here — only reachable if the bundled files become corrupt,
        // which is a build-tree integrity problem, not a runtime
        // input problem.
        //
        // This is one of the very few places `expect` is acceptable
        // in library code: the input is a compile-time-known
        // constant shipped inside the crate, not untrusted runtime
        // data.
        #[allow(clippy::expect_used)]
        let roots = vec![
            Paa::from_der(PAA_FFF1)
                .expect("bundled CSA test PAA FFF1 must parse — build-tree integrity issue"),
            Paa::from_der(PAA_NOVID)
                .expect("bundled CSA test PAA NoVID must parse — build-tree integrity issue"),
        ];

        Self { roots }
    }

    /// Push a trusted PAA into the store.
    pub fn add(&mut self, paa: Paa) {
        self.roots.push(paa);
    }

    /// Return the number of PAAs in the store.
    pub fn len(&self) -> usize {
        self.roots.len()
    }

    /// Return whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    /// Iterate over the PAAs in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &Paa> {
        self.roots.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attestation::extensions::VendorId;
    use crate::attestation::x509::Paa;

    const PAA_FFF1_DER: &[u8] = include_bytes!("csa_test_roots/Chip-Test-PAA-FFF1-Cert.der");

    #[test]
    fn empty_store_is_empty_and_zero_length() {
        let s = PaaTrustStore::empty();
        assert!(s.is_empty());
        assert_eq!(s.len(), 0);
        assert!(s.iter().next().is_none());
    }

    #[test]
    #[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn add_grows_the_store() {
        let mut s = PaaTrustStore::empty();
        s.add(Paa::from_der(PAA_FFF1_DER).unwrap());
        assert_eq!(s.len(), 1);
        assert!(!s.is_empty());
    }

    #[test]
    #[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    fn iter_returns_paas_in_insertion_order() {
        let mut s = PaaTrustStore::empty();
        let a = Paa::from_der(PAA_FFF1_DER).unwrap();
        let b = Paa::from_der(PAA_FFF1_DER).unwrap();
        s.add(a);
        s.add(b);
        let collected: Vec<&[u8]> = s.iter().map(Paa::der).collect();
        assert_eq!(collected.len(), 2);
        assert_eq!(collected[0], PAA_FFF1_DER);
        assert_eq!(collected[1], PAA_FFF1_DER);
    }

    #[test]
    fn with_example_device_roots_loads_both() {
        let s = PaaTrustStore::with_example_device_roots();
        assert_eq!(s.len(), 2, "exactly two bundled CSA test roots");
    }

    #[test]
    fn with_example_device_roots_contains_vid_scoped_fff1() {
        let s = PaaTrustStore::with_example_device_roots();
        let has_fff1 = s
            .iter()
            .any(|paa| paa.subject_vid() == Some(VendorId::new(0xFFF1)));
        assert!(has_fff1, "bundled roots include the VID 0xFFF1 PAA");
    }

    #[test]
    fn with_example_device_roots_contains_unscoped() {
        let s = PaaTrustStore::with_example_device_roots();
        let has_unscoped = s.iter().any(|paa| paa.subject_vid().is_none());
        assert!(has_unscoped, "bundled roots include a non-VID-scoped PAA");
    }
}
