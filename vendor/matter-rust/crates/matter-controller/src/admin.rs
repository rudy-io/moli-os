//! `AdministratorCommissioning` (0x003C) controller support: types and the pure
//! request/response codecs the `Node` verbs compose.

use matter_codec::{Tag, Value};
use matter_interaction::AttributePath;

use crate::error::Error;

/// `AdministratorCommissioning` cluster id.
pub(crate) const ADMIN_COMMISSIONING_CLUSTER: u32 = 0x003C;
/// Command id for `OpenCommissioningWindow`.
pub(crate) const CMD_OPEN_COMMISSIONING_WINDOW: u32 = 0x00;
/// Command id for `OpenBasicCommissioningWindow`.
pub(crate) const CMD_OPEN_BASIC_COMMISSIONING_WINDOW: u32 = 0x01;
/// Command id for `RevokeCommissioning`.
pub(crate) const CMD_REVOKE_COMMISSIONING: u32 = 0x02;
/// Attribute id for `WindowStatus`.
pub(crate) const ATTR_WINDOW_STATUS: u32 = 0x0000;
/// Attribute id for `AdminFabricIndex`.
pub(crate) const ATTR_ADMIN_FABRIC_INDEX: u32 = 0x0001;
/// Attribute id for `AdminVendorId`.
pub(crate) const ATTR_ADMIN_VENDOR_ID: u32 = 0x0002;
/// Spec default/floor PBKDF iterations for an opened window.
pub const DEFAULT_WINDOW_ITERATIONS: u32 = 1000;
/// Spec-recommended commissioning-window timeout (seconds).
pub const DEFAULT_WINDOW_TIMEOUT_S: u16 = 180;

/// Options for `Node::open_commissioning_window`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct OpenWindowOpts {
    /// How long the window stays open, in seconds.
    pub timeout_s: u16,
    /// PBKDF2 iteration count for the generated verifier (≥ 1000).
    pub iterations: u32,
    /// Device Vendor ID — required to emit a QR code (read it from Basic
    /// Information, or leave `None` to get only the manual pairing code).
    pub vendor_id: Option<u16>,
    /// Device Product ID — pair with `vendor_id`.
    pub product_id: Option<u16>,
}

impl Default for OpenWindowOpts {
    fn default() -> Self {
        Self {
            timeout_s: DEFAULT_WINDOW_TIMEOUT_S,
            iterations: DEFAULT_WINDOW_ITERATIONS,
            vendor_id: None,
            product_id: None,
        }
    }
}

/// The result of opening an enhanced commissioning window — everything a second
/// commissioner needs to onboard the device onto its own fabric.
///
/// `Debug` is written by hand so that `passcode`, `manual_code` and `qr_code`
/// print as `<redacted>` (`qr_code` keeps its `Option` shape): each one is
/// enough to commission the device while the window is open, so none may
/// reach a log through a stray `{:?}`. Read the fields directly to show the
/// codes to the person who will use them.
#[derive(Clone)]
#[non_exhaustive]
pub struct CommissioningWindow {
    /// The freshly generated 27-bit setup passcode.
    /// Secret: this struct's `Debug` output shows `<redacted>` instead.
    pub passcode: u32,
    /// The full 12-bit discriminator advertised while the window is open.
    ///
    /// This is the value the device puts in its mDNS `D` TXT record. Note
    /// that [`Self::manual_code`] carries only its upper 4 bits — see there.
    pub discriminator: u16,
    /// PBKDF2 iterations used.
    pub iterations: u32,
    /// PBKDF2 salt used.
    pub salt: Vec<u8>,
    /// 11-digit manual pairing code (always present).
    ///
    /// A manual pairing code has room for only the **short** discriminator —
    /// the upper 4 bits of [`Self::discriminator`] — so it identifies the
    /// device less precisely than [`Self::qr_code`] does. A commissioner
    /// given this code must match on the short discriminator; feeding it
    /// back through `matter_commissioning::setup::parse_manual_code` does
    /// **not** return [`Self::discriminator`]. That is inherent to the
    /// format (Matter Core Spec §5.1.4), not a limitation of this crate.
    ///
    /// Secret (it encodes the passcode): this struct's `Debug` output shows
    /// `<redacted>` instead.
    pub manual_code: String,
    /// `MT:` QR string — `Some` only when `vendor_id`/`product_id` were supplied.
    ///
    /// Secret (it encodes the passcode): this struct's `Debug` output shows
    /// `Some("<redacted>")` or `None` instead.
    pub qr_code: Option<String>,
}

impl std::fmt::Debug for CommissioningWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Same name, fields and order as the derive this replaces. The three
        // values that carry the passcode are withheld; `qr_code` still shows
        // whether a QR code was produced.
        f.debug_struct("CommissioningWindow")
            .field("passcode", &"<redacted>")
            .field("discriminator", &self.discriminator)
            .field("iterations", &self.iterations)
            .field("salt", &self.salt)
            .field("manual_code", &"<redacted>")
            .field("qr_code", &self.qr_code.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Decoded `WindowStatus` enum8.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CommissioningWindowStatus {
    /// 0 — no window open.
    WindowNotOpen,
    /// 1 — enhanced window open.
    EnhancedWindowOpen,
    /// 2 — basic window open.
    BasicWindowOpen,
    /// Any other (future) value.
    Unknown(u8),
}

impl CommissioningWindowStatus {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::WindowNotOpen,
            1 => Self::EnhancedWindowOpen,
            2 => Self::BasicWindowOpen,
            other => Self::Unknown(other),
        }
    }
}

/// Snapshot of the `AdministratorCommissioning` status attributes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct WindowStatus {
    /// Current window status.
    pub status: CommissioningWindowStatus,
    /// Fabric index of the admin that opened the window, if any.
    pub admin_fabric_index: Option<u8>,
    /// Vendor id of the admin that opened the window, if any.
    pub admin_vendor_id: Option<u16>,
}

/// Build the `OpenCommissioningWindow` command fields struct (spec field tags).
///
/// Fields per Matter Core Spec §11.18.8.1:
/// - tag 0: `CommissioningTimeout` (uint16)
/// - tag 1: `PAKEPasscodeVerifier` (bytes, 97 octets)
/// - tag 2: `Discriminator` (uint16, 12-bit)
/// - tag 3: `Iterations` (uint32)
/// - tag 4: `Salt` (bytes, 16–32 octets)
pub(crate) fn open_window_fields(
    timeout_s: u16,
    verifier: &[u8],
    discriminator: u16,
    iterations: u32,
    salt: &[u8],
) -> Value {
    Value::Structure(vec![
        (Tag::Context(0), Value::Uint(u64::from(timeout_s))),
        (Tag::Context(1), Value::Bytes(verifier.to_vec())),
        (Tag::Context(2), Value::Uint(u64::from(discriminator))),
        (Tag::Context(3), Value::Uint(u64::from(iterations))),
        (Tag::Context(4), Value::Bytes(salt.to_vec())),
    ])
}

/// Build the manual code (always) and QR (`Some` iff vid+pid given) for a window.
///
/// The two codes do not carry the same discriminator precision: the QR carries
/// all 12 bits, the manual code only the upper 4 (Matter Core Spec §5.1.4). A
/// commissioner given the manual code therefore matches on the short
/// discriminator, which is expected and is what `driver::commission` does. The
/// window's own `discriminator` field stays the full 12-bit value, because that
/// is what the device advertises.
///
/// # Errors
///
/// Returns [`Error::SetupCode`] if `passcode` or `discriminator` are out of
/// their valid ranges, or if QR encoding fails.
pub(crate) fn onboarding_payload(
    passcode: u32,
    discriminator: u16,
    vendor_id: Option<u16>,
    product_id: Option<u16>,
) -> Result<(String, Option<String>), Error> {
    use matter_commissioning::setup::{
        encode_manual_code, encode_qr, CommissioningFlow, DiscoveryCapabilities, Discriminator,
        Passcode, SetupPayload,
    };
    let map = |e: matter_commissioning::setup::Error| Error::SetupCode(e.to_string());
    let base = SetupPayload {
        version: 0,
        vendor_id: None,
        product_id: None,
        commissioning_flow: CommissioningFlow::Standard,
        discovery_capabilities: DiscoveryCapabilities::ON_NETWORK,
        discriminator: Discriminator::new(discriminator).map_err(map)?,
        passcode: Passcode::new(passcode).map_err(map)?,
    };
    // Build the manual code (borrows base) before consuming base into the QR payload.
    let manual_code = encode_manual_code(&base);
    let qr_code = match (vendor_id, product_id) {
        (Some(v), Some(p)) => {
            let qr = SetupPayload {
                vendor_id: Some(v),
                product_id: Some(p),
                ..base
            };
            Some(encode_qr(&qr).map_err(map)?)
        }
        _ => None,
    };
    Ok((manual_code, qr_code))
}

/// Generate a valid `(passcode, salt, discriminator)` for an enhanced window.
///
/// Passcode is a fresh 27-bit value with the spec's trivial values excluded;
/// salt is 32 random bytes; discriminator is a random 12-bit value.
///
/// # Errors
/// Returns [`Error::Operational`] if the system RNG fails or no valid passcode
/// is found within the retry budget (practically never — ~12 values excluded).
pub(crate) fn random_window_secrets() -> Result<(u32, [u8; 32], u16), Error> {
    use matter_commissioning::setup::Passcode;
    let rng = |buf: &mut [u8]| {
        matter_crypto::random_bytes(buf).map_err(|e| Error::Operational(format!("rng: {e}")))
    };
    let mut salt = [0u8; 32];
    rng(&mut salt)?;
    let mut db = [0u8; 2];
    rng(&mut db)?;
    // All 12 bits, deliberately: this is what the device advertises in its
    // mDNS `D` record and what a QR code carries. Do NOT narrow this to
    // `0x0F00` to make the manual-code roundtrip look lossless (proposed in
    // #120) — a manual code only ever carries the upper 4 bits, so that
    // would collapse the *advertised* discriminator from 4096 values to 16
    // and make collisions between nearby commissionable devices 256x more
    // likely, to fix nothing.
    let discriminator = u16::from_le_bytes(db) & 0x0FFF;
    // Passcode: draw 27-bit values until one is spec-valid (Passcode::new rejects
    // out-of-range and the disallowed-trivial set).
    for _ in 0..64 {
        let mut pb = [0u8; 4];
        rng(&mut pb)?;
        let candidate = u32::from_le_bytes(pb) & 0x07FF_FFFF; // 27-bit
        if Passcode::new(candidate).is_ok() {
            return Ok((candidate, salt, discriminator));
        }
    }
    Err(Error::Operational(
        "could not generate a valid passcode".into(),
    ))
}

/// Parse the three status attributes from a `read` result.
pub(crate) fn parse_window_status(reports: &[(AttributePath, Value)]) -> WindowStatus {
    let mut status = CommissioningWindowStatus::WindowNotOpen;
    let mut admin_fabric_index: Option<u8> = None;
    let mut admin_vendor_id: Option<u16> = None;
    for (path, value) in reports {
        match path.attribute {
            ATTR_WINDOW_STATUS => {
                if let Value::Uint(v) = value {
                    #[allow(clippy::cast_possible_truncation)]
                    // The spec defines WindowStatus as enum8; truncation to u8 is correct.
                    {
                        status = CommissioningWindowStatus::from_u8(*v as u8);
                    }
                }
            }
            ATTR_ADMIN_FABRIC_INDEX => {
                if let Value::Uint(v) = value {
                    #[allow(clippy::cast_possible_truncation)]
                    // The spec defines AdminFabricIndex as fabric-idx (uint8); truncation correct.
                    {
                        admin_fabric_index = Some(*v as u8);
                    }
                }
            }
            ATTR_ADMIN_VENDOR_ID => {
                if let Value::Uint(v) = value {
                    #[allow(clippy::cast_possible_truncation)]
                    // The spec defines AdminVendorId as vendor-id (uint16); truncation correct.
                    {
                        admin_vendor_id = Some(*v as u16);
                    }
                }
            }
            _ => {}
        }
    }
    WindowStatus {
        status,
        admin_fabric_index,
        admin_vendor_id,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    #[test]
    fn open_window_fields_uses_spec_tags() {
        let v = open_window_fields(180, &[0xAA; 97], 0xABC, 1000, &[0x01; 32]);
        let Value::Structure(members) = v else {
            panic!("expected struct")
        };
        assert_eq!(members.len(), 5);
        assert_eq!(members[0].0, Tag::Context(0));
        assert_eq!(members[0].1, Value::Uint(180));
        assert_eq!(members[2].1, Value::Uint(0xABC));
        assert_eq!(members[3].1, Value::Uint(1000));
        assert!(matches!(&members[1].1, Value::Bytes(b) if b.len() == 97));
        assert!(matches!(&members[4].1, Value::Bytes(b) if b.len() == 32));
    }

    #[test]
    fn onboarding_payload_manual_always_qr_only_with_vidpid() {
        let (manual, qr) = onboarding_payload(20_202_021, 3840, None, None).unwrap();
        assert_eq!(manual.len(), 11);
        assert!(qr.is_none());
        let (_m, qr2) = onboarding_payload(20_202_021, 3840, Some(0xFFF1), Some(0x8000)).unwrap();
        assert!(qr2.unwrap().starts_with("MT:"));
    }

    #[test]
    fn random_window_secrets_are_valid_and_vary() {
        use matter_commissioning::setup::{Discriminator, Passcode};
        let (p1, s1, d1) = random_window_secrets().unwrap();
        let (p2, _s2, _d2) = random_window_secrets().unwrap();
        // Passcode is constructible (27-bit, non-trivial) and discriminator ≤ 0x0FFF.
        Passcode::new(p1).unwrap();
        Discriminator::new(d1).unwrap();
        assert_eq!(s1.len(), 32);
        assert_ne!(s1, [0u8; 32]);
        assert_ne!(p1, p2); // overwhelmingly likely to differ
    }

    #[test]
    fn parse_window_status_reads_three_attrs() {
        let ap = |a: u32| AttributePath {
            endpoint: 0,
            cluster: ADMIN_COMMISSIONING_CLUSTER,
            attribute: a,
        };
        let reports = vec![
            (ap(ATTR_WINDOW_STATUS), Value::Uint(1)),
            (ap(ATTR_ADMIN_FABRIC_INDEX), Value::Uint(2)),
            (ap(ATTR_ADMIN_VENDOR_ID), Value::Null),
        ];
        let ws = parse_window_status(&reports);
        assert_eq!(ws.status, CommissioningWindowStatus::EnhancedWindowOpen);
        assert_eq!(ws.admin_fabric_index, Some(2));
        assert_eq!(ws.admin_vendor_id, None);
    }
}
