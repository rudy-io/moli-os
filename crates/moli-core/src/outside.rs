//! Rooms outside the walls (« Extérieur », « Jardin », « Terrasse »…).
//!
//! A room is outside when, in this order, `home.json` says so
//! (`rooms[].outside`), the plan draws it only outside (a garden, a
//! terrace, a driveway…), or its name or one of its aliases holds one of
//! [`WORDS`]. This module is the last step: the words, in one place, so a
//! house in another language is one pull request away.

use unicode_normalization::UnicodeNormalization as _;
use unicode_normalization::char::is_combining_mark;

/// The words that make a room outside, accents and case aside. A whole word
/// of the name must be one of them (or it with an `s`): « Allée du garage »
/// is outside, « Courgettes » is not. « Jardin d'hiver » is a case for
/// `rooms[].outside: false`.
pub const WORDS: &[&str] = &[
    // French
    "exterieur",
    "dehors",
    "jardin",
    "terrasse",
    "cour",
    "allee",
    "balcon",
    "patio",
    "verger",
    "potager",
    "parvis",
    "porche",
    "pergola",
    "piscine",
    "spa",
    // English
    "outside",
    "outdoor",
    "garden",
    "yard",
    "backyard",
    "terrace",
    "deck",
    "balcony",
    "orchard",
    "porch",
    "driveway",
    "pool",
];

/// What the plan calls a room drawn outside the walls.
pub const PLAN_KINDS: &[&str] = &["garden", "terrace", "deck", "pool", "driveway"];

/// Whether `name` says the room is outside (« Extérieur », « Jardin
/// d'hiver », « Backyard »).
#[must_use]
pub fn named_outside(name: &str) -> bool {
    fold(name)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .any(|w| WORDS.contains(&w) || w.strip_suffix('s').is_some_and(|w| WORDS.contains(&w)))
}

/// A room's name as compared: without accents, capitals nor extra spaces
/// (« Allée  » and « allee » are one room).
#[must_use]
pub fn fold(s: &str) -> String {
    s.nfd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_usual_names_are_outside() {
        for name in [
            "Extérieur",
            "EXTERIEUR",
            "Dehors",
            "Jardin",
            "Terrasse",
            "Allée du garage",
            "Cour",
            "Porche",
            "Pergola",
            "Piscine",
            "Spa",
            "Garden",
            "Backyard",
            "Front porch",
            "Pool",
        ] {
            assert!(named_outside(name), "{name}");
        }
    }

    #[test]
    fn a_plural_is_the_same_place() {
        assert!(named_outside("Jardins"));
        assert!(named_outside("Terrasses"));
    }

    #[test]
    fn a_word_inside_another_is_not_enough() {
        for name in [
            "Salon",
            "Garage",
            "Courgettes",
            "Spatule",
            "Poolhouse",
            "Chambre parentale",
            "Bureau",
            "",
        ] {
            assert!(!named_outside(name), "{name}");
        }
    }

    #[test]
    fn an_apostrophe_separates_words() {
        assert!(named_outside("L'allée"));
        // The name alone cannot tell: `rooms[].outside: false` does.
        assert!(named_outside("Jardin d'hiver"));
    }
}
