//! Fabric creation. Mints the fabric trust root (RCAC + IPK) and the
//! controller's **stable** commissioner operational identity in one shot.
//! The commissioner NOC is minted here exactly once and persisted; every
//! later CASE handshake reuses it (retiring the earlier per-call minting).

use std::sync::Arc;

use matter_cert::MatterTime;
use matter_commissioning::{issue_icac, issue_noc, FabricRecord, NocRng, VerifiedCsr};
use matter_crypto::{RingSigner, Signer};

use crate::error::Error;
use crate::state::{CommissionerIdentity, FabricEntry, IcacIdentity};

/// Inputs for creating a new fabric.
///
/// `#[non_exhaustive]`: future fabric-creation knobs (e.g. an explicit IPK or
/// an ICAC tier) can be added without a semver break. Construct via
/// [`FabricConfig::new`] from outside this crate.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FabricConfig {
    /// Matter fabric identifier (spec §6.2.1).
    pub fabric_id: u64,
    /// RCAC subject DN's `rcac-id` value.
    pub rcac_id: u64,
    /// The stable node ID the controller takes on this fabric.
    pub commissioner_node_id: u64,
    /// `(not_before, not_after)` validity for the RCAC and commissioner NOC.
    ///
    /// Pass a real wall-clock `not_before` — e.g.
    /// `MatterTime::from_unix_secs(current_unix_time)`, typically backdated a
    /// little (an hour is plenty) to tolerate device clock skew. Use
    /// `MatterTime::NO_EXPIRY` for `not_after` if the fabric should not
    /// expire.
    ///
    /// [`MatterController::create_fabric`](crate::MatterController::create_fabric)
    /// validates this window and returns
    /// [`crate::Error::InvalidFabricValidity`] rather than letting a bad one
    /// reach a device. Four ways to get it wrong:
    ///
    /// - **`not_before` at the Matter epoch** (`MatterTime(0)`, equivalently
    ///   `MatterTime::from_unix_secs(0)`) — the reporter's failure in issue
    ///   #111. The cause is a signature mismatch, not a validity policy:
    ///   chip's `ChipEpochToASN1Time`
    ///   (`connectedhomeip/src/credentials/CHIPCert.cpp`) maps epoch 0 to the
    ///   X.509 sentinel `99991231235959Z` for **both** `notBefore` and
    ///   `notAfter`, so a device rebuilding the X.509 TBS from our TLV
    ///   certificate hashes `99991231235959Z` where we signed
    ///   `20000101000000Z` and the **signature check fails**. chip's own
    ///   comment: such certificates "are not usable with this code" and
    ///   "attempted installation of such certficates will fail during
    ///   commissioning" — surfacing as an opaque `IM status 0x85` rejection of
    ///   `AddTrustedRootCertificate` deep in commissioning.
    /// - **`not_before` far in the future** — most often a *millisecond*
    ///   timestamp handed to `MatterTime::from_unix_secs`, which saturates to
    ///   `MatterTime(u32::MAX)` (≈ year 2136). This one is worse than a
    ///   rejection: `ValidateChipRCAC` deliberately does not check RCAC
    ///   validity times (`CHIPCert.cpp`), so `AddTrustedRootCertificate`
    ///   *succeeds* and the fabric half-commissions, then every CASE session
    ///   fails with `kNotYetValid`.
    /// - **An already-expired window** — the symmetric twin of the one above,
    ///   most often a `(not_before, not_after)` pair copied from an older
    ///   document. The ordering check passes and `not_before` is in the past, so
    ///   only a comparison against the clock catches it; the same
    ///   `ValidateChipRCAC` exemption means the expired root *installs* and CASE
    ///   then fails with `kExpired`.
    /// - **A units mistake in `not_after`** — `from_unix_secs` clamps any
    ///   pre-2000 Unix time to `MatterTime(0)`, and `MatterTime(0)` **is**
    ///   `MatterTime::NO_EXPIRY`, so such a mistake silently yields "never
    ///   expires" — the opposite of the intent — and cannot be rejected here.
    pub validity: (MatterTime, MatterTime),
    /// When `true`, `create_fabric` mints an intermediate CA (ICAC) under
    /// the RCAC and signs the commissioner NOC (and, later, all NOCs
    /// issued on this fabric) under the ICAC instead of directly under the
    /// RCAC. Defaults to `false` (the flat RCAC->NOC path) via
    /// [`FabricConfig::new`].
    pub issue_icac: bool,
}

impl FabricConfig {
    /// Construct a fabric configuration.
    ///
    /// This is the supported construction path now that [`FabricConfig`] is
    /// `#[non_exhaustive]`; the public fields remain readable/writable in
    /// place. See [`FabricConfig::validity`] for what to pass as `validity` —
    /// in particular, a real wall-clock `not_before`, not the Matter epoch.
    #[must_use]
    pub fn new(
        fabric_id: u64,
        rcac_id: u64,
        commissioner_node_id: u64,
        validity: (MatterTime, MatterTime),
    ) -> Self {
        Self {
            fabric_id,
            rcac_id,
            commissioner_node_id,
            validity,
            issue_icac: false,
        }
    }
}

/// Reject a `(not_before, not_after)` validity window devices would reject
/// (issue #111), before any key generation runs.
///
/// - `not_before` must not be the Matter epoch (`MatterTime(0)`, i.e.
///   2000-01-01T00:00:00Z) — the reporter's evidenced failure: a certificate
///   with that `notBefore` round-trips through chip's `ChipEpochToASN1Time` as
///   `99991231235959Z`, so the rebuilt X.509 TBS no longer matches what we
///   signed and the device's **signature** check fails, surfacing as an opaque
///   `IM status 0x85` on `AddTrustedRootCertificate` deep in commissioning.
///   See [`FabricConfig::validity`] for the full citation.
/// - `not_after` must be strictly after `not_before`, UNLESS `not_after` is
///   `MatterTime::NO_EXPIRY` — that sentinel is a legitimate "does not
///   expire" and is exempt from the ordering check.
fn validate_validity(window: (MatterTime, MatterTime)) -> Result<(), Error> {
    let (not_before, not_after) = window;
    if not_before.0 == 0 {
        return Err(Error::InvalidFabricValidity(format!(
            "not_before is {not_before:?} (the Matter epoch, 2000-01-01T00:00:00Z) — pass a \
             real wall-clock time, e.g. MatterTime::from_unix_secs(current_unix_time)"
        )));
    }
    // Deliberate divergence from the C++ reference: chip's
    // `GenerateChipX509Cert.cpp` accepts a zero-width window
    // (`ValidityEnd >= ValidityStart`); we reject `not_after == not_before`
    // because a certificate that is valid for zero seconds is never what a
    // caller meant, and rejecting it here is cheaper than debugging a fabric
    // that expires the instant it is created.
    if not_after != MatterTime::NO_EXPIRY && not_after <= not_before {
        return Err(Error::InvalidFabricValidity(format!(
            "not_after ({not_after:?}) must be after not_before ({not_before:?}), or \
             MatterTime::NO_EXPIRY for no expiry"
        )));
    }
    Ok(())
}

/// How far ahead of the controller's own clock a `not_before` may sit before
/// [`validate_validity_against_now`] refuses it.
///
/// Rationale: a legitimate `not_before` is "about now" — callers are told to
/// *backdate* it for device clock skew, never to postdate it. Anything ahead of
/// now is therefore only ever disagreement between the caller's time source and
/// this host's clock, and a full day is far more slack than any real deployment
/// needs (chip's own commissioning flows assume the two agree to within
/// minutes). It is still tight enough to catch every plausible units mistake:
/// a millisecond timestamp saturates `MatterTime::from_unix_secs` to
/// `u32::MAX`, ≈ 110 years ahead.
const MAX_NOT_BEFORE_AHEAD_SECS: u32 = 24 * 60 * 60;

/// Render a [`MatterTime`] for an error message in both scales: its raw
/// Matter-epoch seconds (what the type holds, so the reader can match it against
/// the value they passed) and the Unix seconds it means (what they can compare
/// against `date +%s`, which is the only one of the two anyone can read).
fn describe(t: MatterTime) -> String {
    format!("MatterTime({}) = unix {}", t.0, t.to_unix_secs())
}

/// Reject the halves of the validity window that can only be judged against a
/// clock: a `not_before` implausibly far ahead of `now`, and a `not_after`
/// already in the past.
///
/// Separate from [`validate_validity`] because it needs a clock reading, which
/// keeps [`create_fabric`] itself pure — the caller (the actor, which already
/// holds `current_matter_time()`) supplies `now`.
///
/// Both are the issue-#111 failure mode — a certificate a device installs but
/// cannot use — in its *worse* form, where the device does not reject the
/// certificate at install time and so nothing names the cause:
///
/// - **`not_before` too far ahead.** `ValidateChipRCAC`
///   (`connectedhomeip/src/credentials/CHIPCert.cpp`) explicitly does not check
///   RCAC `notBefore`/`notAfter`, so `AddTrustedRootCertificate` succeeds, the
///   fabric half-commissions, and then every CASE session fails with
///   `kNotYetValid`.
/// - **`not_after` already past.** The exact symmetric twin — the same
///   `ValidateChipRCAC` exemption lets an *expired* root install just as
///   happily, and every CASE session afterwards fails with `kExpired` on the
///   commissioner NOC. The plausible route here is a window copied from an older
///   document: ordering passes, `not_before` is in the past so the upper bound
///   passes, and we would otherwise mint and persist a fabric that expired
///   months ago.
pub(crate) fn validate_validity_against_now(
    window: (MatterTime, MatterTime),
    now: MatterTime,
) -> Result<(), Error> {
    let (not_before, not_after) = window;
    let limit = now.0.saturating_add(MAX_NOT_BEFORE_AHEAD_SECS);
    if not_before.0 > limit {
        return Err(Error::InvalidFabricValidity(format!(
            "not_before ({}) is more than {MAX_NOT_BEFORE_AHEAD_SECS}s ahead of this host's clock \
             ({}) — the certificate would install but be not-yet-valid, failing every CASE session \
             afterwards. A common cause is passing a MILLISECOND timestamp to \
             MatterTime::from_unix_secs, which saturates to MatterTime(u32::MAX)",
            describe(not_before),
            describe(now),
        )));
    }
    // `NO_EXPIRY` is exempt: it is numerically `MatterTime(0)` and so would
    // otherwise read as "expired at the Matter epoch".
    if not_after != MatterTime::NO_EXPIRY && not_after <= now {
        return Err(Error::InvalidFabricValidity(format!(
            "not_after ({}) is already in the past — this host's clock reads {}. The certificate \
             would install (ValidateChipRCAC skips RCAC validity times) but every CASE session \
             afterwards would fail with kExpired. Pass a future not_after, or \
             MatterTime::NO_EXPIRY for no expiry",
            describe(not_after),
            describe(now),
        )));
    }
    Ok(())
}

/// Create a fabric: generate the RCAC root key + self-signed RCAC, a fresh
/// IPK, the commissioner operational keypair, and the commissioner NOC.
///
/// The returned [`FabricEntry`] is fully persistable (private keys captured
/// as PKCS#8 DER) and has no devices yet.
///
/// # Errors
///
/// Returns [`Error::InvalidFabricValidity`] if `cfg.validity` names a window
/// devices will reject (see [`FabricConfig::validity`], issue #111);
/// [`Error::Signer`] if key generation fails; or [`Error::Noc`] if RCAC
/// construction or NOC issuance fails.
pub(crate) fn create_fabric(cfg: &FabricConfig, rng: &dyn NocRng) -> Result<FabricEntry, Error> {
    // Validate the validity window FIRST — before any key generation — so a
    // bad window is rejected for free instead of surfacing later as an
    // opaque device-side rejection mid-commissioning (issue #111).
    validate_validity(cfg.validity)?;

    // 1. RCAC root key + self-signed root certificate.
    let (root_signer, rcac_pkcs8) =
        RingSigner::generate().map_err(|e| Error::Signer(e.to_string()))?;
    let root_arc: Arc<dyn Signer> = Arc::new(root_signer);
    let mut fabric_record = FabricRecord::new_root_only(
        cfg.fabric_id,
        root_arc,
        cfg.validity.0,
        cfg.validity.1,
        cfg.rcac_id,
        rng,
    )?;

    // 1b. Optionally mint an ICAC tier under the RCAC. This must happen
    //     BEFORE the commissioner NOC is issued below, so the commissioner
    //     NOC itself is signed under the ICAC (matching what a real
    //     ICAC-tier fabric does for every NOC it issues).
    let icac_identity = if cfg.issue_icac {
        let (icac_signer_raw, icac_pkcs8) =
            RingSigner::generate().map_err(|e| Error::Signer(e.to_string()))?;
        let icac_public_key = icac_signer_raw.public_key().clone();
        // Single-ICAC fabric: reuse `cfg.rcac_id` as the ICAC's `IcacId`
        // DN value too. `RcacId` and `IcacId` are distinct DN attribute
        // types (spec §6.5.5), so the shared numeric id is unambiguous —
        // there is no collision between "RCAC id 7" and "ICAC id 7".
        let icac_cert = issue_icac(
            &fabric_record,
            cfg.rcac_id,
            &icac_public_key,
            cfg.validity,
            rng,
        )
        .map_err(Error::Noc)?;
        fabric_record.icac_signer = Some(Arc::new(icac_signer_raw));
        fabric_record.icac_cert = Some(icac_cert.clone());
        Some(IcacIdentity {
            cert: icac_cert,
            pkcs8: icac_pkcs8,
        })
    } else {
        None
    };

    // 2. Commissioner operational keypair.
    let (comm_signer, comm_pkcs8) =
        RingSigner::generate().map_err(|e| Error::Signer(e.to_string()))?;
    let comm_public_key = comm_signer.public_key().clone();

    // 3. Mint the commissioner NOC over our own key. We generated the key
    //    ourselves, so there is no device CSR to verify — `VerifiedCsr`
    //    here asserts "this public key is trusted for issuance", which is
    //    sound for our own identity. When `fabric_record.icac_signer`/
    //    `icac_cert` are `Some` (set just above), `issue_noc` signs this
    //    under the ICAC instead of the RCAC.
    let verified = VerifiedCsr {
        public_key: comm_public_key,
    };
    let noc = issue_noc(
        &fabric_record,
        &verified,
        cfg.commissioner_node_id,
        &[], // no CASE Authenticated Tags for the controller identity
        cfg.validity,
        rng,
    )?;

    Ok(FabricEntry {
        fabric_id: cfg.fabric_id,
        ipk: fabric_record.identity_protection_key,
        rcac_cert: fabric_record.root_cert.clone(),
        rcac_pkcs8,
        commissioner: CommissionerIdentity {
            node_id: cfg.commissioner_node_id,
            operational_pkcs8: comm_pkcs8,
            noc,
        },
        devices: Vec::new(),
        group_keys: Vec::new(),
        outbound_group_counter: 0,
        icd_clients: Vec::new(),
        icac: icac_identity,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests with justification (CONTRIBUTING.md).
mod tests {
    use super::*;
    use matter_commissioning::SystemNocRng;

    fn sample_cfg() -> FabricConfig {
        FabricConfig::new(
            0xDEAD_BEEF_0000_0001,
            1,
            0x0000_0000_0000_0001,
            (
                MatterTime::from_unix_secs(1_700_000_000),
                MatterTime::NO_EXPIRY,
            ),
        )
    }

    #[test]
    fn new_constructor_sets_all_fields() {
        // `FabricConfig` is `#[non_exhaustive]`; `new` is the supported
        // construction path. Verify it populates every field.
        let cfg = FabricConfig::new(
            7,
            9,
            3,
            (MatterTime::from_unix_secs(1), MatterTime::NO_EXPIRY),
        );
        assert_eq!(cfg.fabric_id, 7);
        assert_eq!(cfg.rcac_id, 9);
        assert_eq!(cfg.commissioner_node_id, 3);
        assert_eq!(cfg.validity.0, MatterTime::from_unix_secs(1));
    }

    #[test]
    fn creates_fabric_with_no_devices() {
        let fabric = create_fabric(&sample_cfg(), &SystemNocRng).expect("create");
        assert_eq!(fabric.fabric_id, 0xDEAD_BEEF_0000_0001);
        assert_eq!(fabric.commissioner.node_id, 1);
        assert!(fabric.devices.is_empty());
        assert_ne!(fabric.rcac_pkcs8, b"");
        assert_ne!(fabric.commissioner.operational_pkcs8, b"");
    }

    #[test]
    fn commissioner_noc_is_signed_by_the_rcac() {
        let fabric = create_fabric(&sample_cfg(), &SystemNocRng).expect("create");
        let rcac_key = fabric.rcac_cert.public_key();
        fabric
            .commissioner
            .noc
            .verify_signed_by(rcac_key)
            .expect("commissioner NOC must verify under the RCAC");
    }

    #[test]
    fn default_path_has_no_icac_and_noc_issuer_is_rcac() {
        // `issue_icac = false` (the `FabricConfig::new` default) must not
        // mint an ICAC, and the commissioner NOC's issuer must be the RCAC
        // subject (the flat RCAC->NOC path, byte-unchanged by the optional ICAC).
        let fabric = create_fabric(&sample_cfg(), &SystemNocRng).expect("create");
        assert!(fabric.icac.is_none());
        assert_eq!(fabric.commissioner.noc.issuer(), fabric.rcac_cert.subject());
    }

    #[test]
    fn issue_icac_true_mints_chain_and_signs_commissioner_noc_under_icac() {
        let mut cfg = sample_cfg();
        cfg.issue_icac = true;
        let entry = create_fabric(&cfg, &SystemNocRng).expect("create");

        // The fabric entry carries a minted ICAC.
        let icac = entry.icac.clone().expect("icac must be Some");

        // Reconstructing the runtime FabricRecord restores both the ICAC
        // signer and cert.
        let rec = entry.to_fabric_record().expect("to_fabric_record");
        assert!(rec.icac_signer.is_some());
        assert!(rec.icac_cert.is_some());

        // The commissioner NOC's issuer DN is the ICAC's subject, not the
        // RCAC's.
        assert_eq!(entry.commissioner.noc.issuer(), icac.cert.subject());
        assert_ne!(entry.commissioner.noc.issuer(), entry.rcac_cert.subject());

        // 3-tier chain linkage + signature verification:
        // RCAC issued/signed the ICAC...
        assert_eq!(icac.cert.issuer(), entry.rcac_cert.subject());
        icac.cert
            .verify_signed_by(entry.rcac_cert.public_key())
            .expect("icac must verify under the rcac's public key");
        // ...and the ICAC issued/signed the commissioner NOC.
        assert_eq!(entry.commissioner.noc.issuer(), icac.cert.subject());
        entry
            .commissioner
            .noc
            .verify_signed_by(icac.cert.public_key())
            .expect("commissioner noc must verify under the icac's public key");

        // Snapshot round-trip preserves `icac` as `Some` with a matching
        // cert.
        let state = crate::state::ControllerState::new(vec![entry.clone()]);
        let bytes = crate::snapshot::serialize(&state).expect("serialize");
        let restored = crate::snapshot::deserialize(&bytes).expect("deserialize");
        let restored_icac = restored.fabrics[0]
            .icac
            .clone()
            .expect("icac must round-trip as Some");
        assert_eq!(
            restored_icac.cert.to_tlv().expect("tlv"),
            icac.cert.to_tlv().expect("tlv"),
            "restored icac cert must byte-match the original"
        );
    }

    #[test]
    fn rejects_not_before_at_matter_epoch_zero() {
        // Issue #111's evidenced failure: `MatterTime(0)` as `not_before`
        // (whether via the raw tuple or `from_unix_secs(0)`, which saturates
        // to the same value) must be rejected here, not surface later as an
        // opaque device-side `IM status 0x85`.
        let mut cfg = sample_cfg();
        cfg.validity = (MatterTime::from_unix_secs(0), MatterTime::NO_EXPIRY);
        let err = create_fabric(&cfg, &SystemNocRng).expect_err("epoch-zero not_before");
        assert!(
            matches!(err, Error::InvalidFabricValidity(_)),
            "expected InvalidFabricValidity, got {err:?}"
        );
        assert!(
            err.to_string().contains("not_before"),
            "error must name not_before: {err}"
        );
    }

    #[test]
    fn rejects_not_before_at_matter_epoch_zero_via_raw_constructor() {
        let mut cfg = sample_cfg();
        cfg.validity = (MatterTime(0), MatterTime::NO_EXPIRY);
        let err = create_fabric(&cfg, &SystemNocRng).expect_err("epoch-zero not_before");
        assert!(matches!(err, Error::InvalidFabricValidity(_)));
    }

    #[test]
    fn rejects_inverted_validity_window() {
        let mut cfg = sample_cfg();
        cfg.validity = (
            MatterTime::from_unix_secs(1_700_000_100),
            MatterTime::from_unix_secs(1_700_000_000),
        );
        let err = create_fabric(&cfg, &SystemNocRng).expect_err("inverted window");
        assert!(
            matches!(err, Error::InvalidFabricValidity(_)),
            "expected InvalidFabricValidity, got {err:?}"
        );
        assert!(
            err.to_string().contains("not_after"),
            "error must name not_after: {err}"
        );
    }

    #[test]
    fn rejects_empty_validity_window() {
        // not_after == not_before (neither is NO_EXPIRY): a zero-width
        // window can never be valid.
        let mut cfg = sample_cfg();
        let t = MatterTime::from_unix_secs(1_700_000_000);
        cfg.validity = (t, t);
        let err = create_fabric(&cfg, &SystemNocRng).expect_err("empty window");
        assert!(matches!(err, Error::InvalidFabricValidity(_)));
    }

    #[test]
    fn accepts_no_expiry_sentinel() {
        // `MatterTime::NO_EXPIRY` for not_after is legitimate and exempt from
        // the not_after > not_before ordering check, even though its
        // underlying value is numerically <= a real not_before.
        let cfg = sample_cfg(); // already (from_unix_secs(1_700_000_000), NO_EXPIRY)
        create_fabric(&cfg, &SystemNocRng).expect("NO_EXPIRY must be accepted");
    }

    #[test]
    fn rejects_not_before_far_in_the_future() {
        // The seconds-vs-milliseconds mistake: a millisecond timestamp handed
        // to `from_unix_secs` saturates to `MatterTime(u32::MAX)` (≈ 2136).
        // With `not_after = NO_EXPIRY` the ordering check is exempted, so only
        // this upper bound catches it — and the device would *accept* the RCAC
        // (`ValidateChipRCAC` skips validity times) then fail every CASE
        // session with `kNotYetValid`.
        let now = MatterTime::from_unix_secs(1_700_000_000);
        let window = (
            MatterTime::from_unix_secs(1_700_000_000_000),
            MatterTime::NO_EXPIRY,
        );
        assert_eq!(window.0, MatterTime(u32::MAX), "precondition: saturated");
        let err = validate_validity_against_now(window, now)
            .expect_err("millisecond not_before must be rejected");
        assert!(
            matches!(err, Error::InvalidFabricValidity(_)),
            "expected InvalidFabricValidity, got {err:?}"
        );
        assert!(
            err.to_string().contains("MILLISECOND"),
            "error must name the likely cause: {err}"
        );
    }

    #[test]
    fn not_before_within_the_skew_window_is_accepted() {
        // Just inside the allowance: exactly `now + MAX_NOT_BEFORE_AHEAD_SECS`
        // is still fine (the check is strictly-greater-than).
        let now = MatterTime::from_unix_secs(1_700_000_000);
        let edge = MatterTime(now.0 + MAX_NOT_BEFORE_AHEAD_SECS);
        validate_validity_against_now((edge, MatterTime::NO_EXPIRY), now)
            .expect("not_before exactly at the skew limit must be accepted");
    }

    #[test]
    fn not_before_one_second_past_the_skew_window_is_rejected() {
        // The other side of the same boundary.
        let now = MatterTime::from_unix_secs(1_700_000_000);
        let over = MatterTime(now.0 + MAX_NOT_BEFORE_AHEAD_SECS + 1);
        let err = validate_validity_against_now((over, MatterTime::NO_EXPIRY), now)
            .expect_err("one second past the skew limit must be rejected");
        assert!(matches!(err, Error::InvalidFabricValidity(_)));
    }

    #[test]
    fn rejects_an_already_expired_window() {
        // A window copied from an older document: Nov 2023 -> Nov 2024, with
        // "now" well past both. Ordering passes and `not_before` is in the past
        // so the upper bound passes — only the comparison against the clock
        // catches it. Without it we would mint and persist a root the device
        // installs happily (ValidateChipRCAC skips RCAC validity times) and a
        // commissioner NOC every CASE session then rejects as kExpired.
        let now = MatterTime::from_unix_secs(1_795_000_000);
        let window = (
            MatterTime::from_unix_secs(1_700_000_000),
            MatterTime::from_unix_secs(1_731_536_000),
        );
        let err = validate_validity_against_now(window, now)
            .expect_err("an expired not_after must be rejected");
        assert!(
            matches!(err, Error::InvalidFabricValidity(_)),
            "expected InvalidFabricValidity, got {err:?}"
        );
        assert!(
            err.to_string().contains("not_after"),
            "error must name not_after: {err}"
        );
        assert!(
            err.to_string().contains("unix 1731536000"),
            "error must render not_after in readable unix seconds: {err}"
        );
    }

    #[test]
    fn not_after_exactly_at_now_is_rejected() {
        // The boundary: expiring at this instant leaves zero usable lifetime.
        let now = MatterTime::from_unix_secs(1_700_000_000);
        let window = (MatterTime::from_unix_secs(1_699_000_000), now);
        let err = validate_validity_against_now(window, now)
            .expect_err("not_after == now must be rejected");
        assert!(matches!(err, Error::InvalidFabricValidity(_)));
    }

    #[test]
    fn not_after_one_second_past_now_is_accepted() {
        // The other side of the same boundary. A one-second-long fabric is
        // useless in practice, but it is the caller's call to make: this check
        // only refuses windows that are *already* over.
        let now = MatterTime::from_unix_secs(1_700_000_000);
        let window = (
            MatterTime::from_unix_secs(1_699_000_000),
            MatterTime(now.0 + 1),
        );
        validate_validity_against_now(window, now)
            .expect("a not_after one second ahead must be accepted");
    }

    #[test]
    fn no_expiry_not_after_is_exempt_from_the_expiry_check() {
        // `NO_EXPIRY` is numerically MatterTime(0), i.e. <= every real `now`,
        // so it would read as "expired at the Matter epoch" without the
        // sentinel exemption.
        let now = MatterTime::from_unix_secs(1_700_000_000);
        let window = (
            MatterTime::from_unix_secs(1_699_000_000),
            MatterTime::NO_EXPIRY,
        );
        validate_validity_against_now(window, now)
            .expect("NO_EXPIRY must be exempt from the expiry check");
    }

    #[test]
    fn backdated_not_before_is_always_accepted() {
        // The documented recommendation (backdate an hour for device clock
        // skew) must never trip the upper bound.
        let now = MatterTime::from_unix_secs(1_700_000_000);
        let backdated = MatterTime::from_unix_secs(1_700_000_000 - 3600);
        validate_validity_against_now((backdated, MatterTime::NO_EXPIRY), now)
            .expect("a backdated not_before must be accepted");
    }

    #[test]
    fn commissioner_signer_matches_persisted_noc_key() {
        // The persisted operational key must correspond to the NOC's
        // public key — i.e. we can actually use the identity we minted.
        let fabric = create_fabric(&sample_cfg(), &SystemNocRng).expect("create");
        let signer = fabric.commissioner_signer().expect("reload signer");
        assert_eq!(
            signer.public_key().as_bytes(),
            fabric.commissioner.noc.public_key().as_bytes(),
            "persisted op key must match the NOC subject public key"
        );
    }
}
