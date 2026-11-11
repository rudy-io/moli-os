//! The store chains the Cellier compares. A new chain is one line here, plus an
//! adapter in the skill that reads its prices.

use crate::model::{ProviderInfo, ProviderKind};

pub const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        id: "intermarche",
        name: "Intermarché",
        kind: ProviderKind::Drive,
        site: "https://www.intermarche.com",
    },
    ProviderInfo {
        id: "carrefour",
        name: "Carrefour",
        kind: ProviderKind::Drive,
        site: "https://www.carrefour.fr",
    },
    ProviderInfo {
        id: "amazon",
        name: "Amazon",
        kind: ProviderKind::Online,
        site: "https://www.amazon.fr",
    },
    ProviderInfo {
        id: "nespresso",
        name: "Nespresso",
        kind: ProviderKind::Online,
        site: "https://www.nespresso.com/fr",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chains_are_unique_and_well_formed() {
        for (i, p) in PROVIDERS.iter().enumerate() {
            assert!(p.site.starts_with("https://"), "{}", p.id);
            assert!(!PROVIDERS[..i].iter().any(|q| q.id == p.id), "{}", p.id);
        }
        let drives = PROVIDERS
            .iter()
            .filter(|p| p.kind == ProviderKind::Drive)
            .count();
        assert_eq!(drives, 2);
        assert_eq!(PROVIDERS.len(), 4);
    }
}
