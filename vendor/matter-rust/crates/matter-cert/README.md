# matter-cert

Matter protocol certificate format — parsing, serialisation, issuance,
X.509 DER conversion, signature verification, and chain validation. Part
of the [matter-rust](https://github.com/phunapps/matter-rust) workspace.

## Scope

Implements Matter Core Specification §6.5: a TLV-encoded variant of
X.509 used for both attestation chains (DAC → PAI → PAA) and
operational chains (NOC → ICAC → RCAC). Reading, validating, and
issuing all live here.

- TLV parser + serialiser (round-trip byte-exact)
- Distinguished-name attributes including Matter-specific OIDs
- Extension parsing (BasicConstraints, KeyUsage, ExtKeyUsage, SKI, AKI)
- ECDSA-P256-SHA256 public-key extraction
- X.509 DER TBSCertificate conversion (byte-identical to matter.js and
  to `chip-cert`)
- Signature verification via `ring`
- Chain validation against configurable trust anchors
- **Issuance** — `Builder` produces an `UnsignedCertificate`, and the
  `operational` module adds role-aware constructors that bake in the
  extension and DN profile the spec mandates for RCAC, ICAC, and NOC.
  Signing is a separate step, so the key can stay in an HSM, an OS
  keychain, or an offline ceremony rather than in this process.

## Status

**0.3.1**, published on crates.io. Feature-complete, and cross-verified
byte-for-byte against both matter.js and `connectedhomeip`'s `chip-cert`
on 3-tier RCAC/ICAC/NOC chains. Stability: a `0.x` crate, so a **minor**
bump may break API.

```toml
[dependencies]
matter-cert = "0.3"
```

One gap is tracked in [`TODO-1.0.md`](../../TODO-1.0.md): the CSA test
PAA roots are bundled in `matter-commissioning`, not here, so validating
an attestation chain against them means reaching for that crate.

## Minimal example

```rust,no_run
use matter_cert::{
    CertificateChain, MatterCertificate, MatterTime, TrustAnchor, TrustedRoots,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let rcac = MatterCertificate::from_tlv(&std::fs::read("rcac.bin")?)?;
    let icac = MatterCertificate::from_tlv(&std::fs::read("icac.bin")?)?;
    let noc = MatterCertificate::from_tlv(&std::fs::read("noc.bin")?)?;

    let mut roots = TrustedRoots::new();
    roots.add(TrustAnchor::from_root_cert(&rcac));

    // `CertificateChain` borrows the leaf-to-root slice, so bind it first.
    let certs = [noc, icac];
    let chain = CertificateChain::new(&certs);
    let now = MatterTime::from_unix_secs(1_750_000_000);
    chain.validate(&roots, now)?;
    // Ok(()) means: every cert is time-valid, the issuer/subject chain
    // is structurally sound, every signature verifies, and the top cert
    // anchors against rcac.
    Ok(())
}
```

## Cryptographic primitives

This crate **does not implement crypto primitives.** It delegates to
[`ring`](https://github.com/briansmith/ring) for ECDSA-P256-SHA256
signature verification. ASN.1 DER encoding uses
[`der`](https://crates.io/crates/der).

## Cross-verification

`MatterCertificate::to_x509_tbs_der()` produces bytes byte-identical
to matter.js's `Certificate.asUnsignedDer()`. This parity is enforced
in CI against **two** independently captured 3-tier RCAC/ICAC/NOC
chains: one from matter.js (`test-vectors/certs/`) and one from
`project-chip/connectedhomeip`'s `chip-cert`
(`test-vectors/certs/connectedhomeip/`, produced by
`cargo xtask capture-cert-chip`). Parse, TLV round-trip, TBS byte
parity, and signature-chain validation all run against both sets.

## License

Apache 2.0. See [LICENSE](../../LICENSE).
