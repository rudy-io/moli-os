//! How sensitive a piece of data is, and which provider may receive it.

/// The smaller the number, the more sensitive the data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tier {
    /// Never leaves the house, whatever the provider (secrets, camera images…).
    Never = 0,
    /// Only a model running at home may see it.
    LocalOnly = 1,
    /// A cloud provider may see it once pseudonymised (the house's structure).
    Pseudonymised = 2,
    /// Free: code, documentation, anything without a trace of the house.
    Free = 3,
}

impl Tier {
    #[must_use]
    pub const fn level(self) -> u8 {
        self as u8
    }

    #[must_use]
    pub const fn from_level(level: u8) -> Option<Self> {
        match level {
            0 => Some(Self::Never),
            1 => Some(Self::LocalOnly),
            2 => Some(Self::Pseudonymised),
            3 => Some(Self::Free),
            _ => None,
        }
    }
}

/// Whether a provider whose own tier is `provider` may receive data of tier
/// `data`: its own tier and the less sensitive ones. Tier 0 goes nowhere, and
/// tier 0 is no provider's level.
#[must_use]
pub fn may_receive(provider: Tier, data: Tier) -> bool {
    provider != Tier::Never && data != Tier::Never && data >= provider
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_round_trip() {
        for level in 0..=3 {
            assert_eq!(Tier::from_level(level).unwrap().level(), level);
        }
        assert_eq!(Tier::from_level(4), None);
    }

    #[test]
    fn the_smaller_the_number_the_more_sensitive() {
        assert!(Tier::Never < Tier::LocalOnly);
        assert!(Tier::LocalOnly < Tier::Pseudonymised);
        assert!(Tier::Pseudonymised < Tier::Free);
    }

    #[test]
    fn a_provider_receives_its_own_tier_and_less_sensitive_ones() {
        use Tier::{Free, LocalOnly, Pseudonymised};
        // The home's own model (tier 1): everything but tier 0.
        assert!(may_receive(LocalOnly, LocalOnly));
        assert!(may_receive(LocalOnly, Pseudonymised));
        assert!(may_receive(LocalOnly, Free));
        // A pseudonymised cloud (tier 2) never sees tier 1.
        assert!(!may_receive(Pseudonymised, LocalOnly));
        assert!(may_receive(Pseudonymised, Pseudonymised));
        assert!(may_receive(Pseudonymised, Free));
        // A free cloud (tier 3) only sees tier 3.
        assert!(!may_receive(Free, Pseudonymised));
        assert!(may_receive(Free, Free));
    }

    #[test]
    fn nothing_of_tier_zero_goes_anywhere_and_tier_zero_is_no_provider() {
        use Tier::{Free, LocalOnly, Never};
        for provider in [LocalOnly, Free] {
            assert!(!may_receive(provider, Never));
        }
        assert!(!may_receive(Never, Free));
    }
}
