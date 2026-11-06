//! Matter time representation.
//!
//! Matter certificates use a 32-bit "epoch-s" counter — seconds since
//! 2000-01-01T00:00:00Z (NOT Unix epoch). [`MatterTime`] wraps this
//! representation natively; conversions to/from Unix time are explicit.

use core::cmp::Ordering;

/// The offset (in Unix seconds) of the Matter epoch from the Unix epoch.
/// Matter epoch starts 2000-01-01T00:00:00Z; that's 946,684,800 Unix seconds.
const MATTER_EPOCH_UNIX_OFFSET: u64 = 946_684_800;

/// A Matter time value: seconds since 2000-01-01T00:00:00Z, wrapping
/// as a `u32` (the spec's wire-native representation).
///
/// `MatterTime(0)` carries a special meaning of "no expiry" for
/// certificate `not_after` fields; see [`MatterTime::NO_EXPIRY`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MatterTime(pub u32);

impl MatterTime {
    /// Sentinel meaning "no expiry" for certificate `not_after`.
    pub const NO_EXPIRY: Self = Self(0);

    /// Convert from Unix seconds. Saturates at `u32::MAX` on overflow.
    /// Unix times before the Matter epoch saturate to `MatterTime(0)`.
    #[must_use]
    pub fn from_unix_secs(unix: u64) -> Self {
        let matter = unix.saturating_sub(MATTER_EPOCH_UNIX_OFFSET);
        let clamped = u32::try_from(matter).unwrap_or(u32::MAX);
        Self(clamped)
    }

    /// Convert to Unix seconds.
    #[must_use]
    pub fn to_unix_secs(self) -> u64 {
        u64::from(self.0) + MATTER_EPOCH_UNIX_OFFSET
    }
}

impl PartialOrd for MatterTime {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MatterTime {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;

    #[test]
    fn unix_to_matter_to_unix_round_trip() {
        let unix = 1_763_337_600u64;
        let matter = MatterTime::from_unix_secs(unix);
        assert_eq!(matter.to_unix_secs(), unix);
    }

    #[test]
    fn matter_epoch_zero_maps_to_2000_01_01() {
        assert_eq!(MatterTime(0).to_unix_secs(), MATTER_EPOCH_UNIX_OFFSET);
    }

    #[test]
    fn no_expiry_constant_is_zero() {
        assert_eq!(MatterTime::NO_EXPIRY, MatterTime(0));
    }

    #[test]
    fn pre_matter_epoch_unix_saturates_to_zero() {
        let unix = 946_684_799u64;
        assert_eq!(MatterTime::from_unix_secs(unix), MatterTime(0));
    }

    #[test]
    fn unix_at_matter_u32_max_boundary_is_exact() {
        // The largest Matter-representable Unix second: epoch offset
        // plus u32::MAX. This must NOT saturate — it's the highest
        // exact-representable point.
        let unix = MATTER_EPOCH_UNIX_OFFSET + u64::from(u32::MAX);
        assert_eq!(MatterTime::from_unix_secs(unix), MatterTime(u32::MAX));
    }

    #[test]
    fn unix_just_above_matter_u32_max_saturates() {
        // One second past the boundary must saturate to u32::MAX (not
        // wrap to 0). Exercises the `u32::try_from(...).unwrap_or(u32::MAX)`
        // branch in from_unix_secs.
        let unix = MATTER_EPOCH_UNIX_OFFSET + u64::from(u32::MAX) + 1;
        assert_eq!(MatterTime::from_unix_secs(unix), MatterTime(u32::MAX));
    }

    #[test]
    fn unix_u64_max_saturates_to_u32_max() {
        // The most extreme overflow input.
        assert_eq!(MatterTime::from_unix_secs(u64::MAX), MatterTime(u32::MAX));
    }

    #[test]
    fn ordering_uses_native_u32() {
        assert!(MatterTime(100) < MatterTime(200));
        assert!(MatterTime(u32::MAX) > MatterTime(0));
    }
}
