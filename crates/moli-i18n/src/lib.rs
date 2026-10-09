//! The house's words in its language.
//!
//! A house speaks one language (`[server] language`, or the one chosen at
//! the installation): its errors, notices and assistant use it, and so does
//! the dashboard. The words live in `locales/<language>/<area>.json`, flat
//! keys prefixed by their area (`automatismes.held`), `{name}` for
//! parameters, `_one` / `_other` suffixes for plurals. They are embedded at
//! build time.
//!
//! A missing word falls back to French, then to its key: visible, so found.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::{OnceLock, RwLock};

include!(concat!(env!("OUT_DIR"), "/catalogues.rs"));

/// The language every word falls back to.
pub const FALLBACK: &str = "fr";

type Words = HashMap<String, String>;

fn catalogues() -> &'static HashMap<&'static str, Words> {
    static ALL: OnceLock<HashMap<&'static str, Words>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut all: HashMap<&'static str, Words> = HashMap::new();
        for (lang, json) in CATALOGUES {
            // A catalogue that does not parse is a build mistake: its words
            // fall back, and the check script names the file.
            if let Ok(words) = serde_json::from_str::<Words>(json) {
                all.entry(lang).or_default().extend(words);
            }
        }
        all
    })
}

fn current() -> &'static RwLock<String> {
    static LANGUAGE: OnceLock<RwLock<String>> = OnceLock::new();
    LANGUAGE.get_or_init(|| RwLock::new(FALLBACK.to_owned()))
}

/// The languages Moli speaks (those with a catalogue).
#[must_use]
pub fn languages() -> Vec<&'static str> {
    let mut list: Vec<&'static str> = catalogues().keys().copied().collect();
    list.sort_unstable();
    list
}

/// The house's language, from now on. An unknown one is refused: the
/// house keeps speaking the one it had.
pub fn set_language(language: &str) -> bool {
    let language = language.trim().to_ascii_lowercase();
    if !catalogues().contains_key(language.as_str()) {
        return false;
    }
    *current()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = language;
    true
}

/// The house's language.
#[must_use]
pub fn language() -> String {
    current()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The word for `key` in the house's language.
#[must_use]
pub fn tr(key: &str) -> String {
    trf(key, &[])
}

/// The word for `key`, `{name}` replaced by each argument; `count` chooses
/// between `key_one` and `key_other` when they exist.
#[must_use]
pub fn trf(key: &str, args: &[(&str, &dyn Display)]) -> String {
    let all = catalogues();
    let language = language();
    let in_language = |k: &str| all.get(language.as_str()).and_then(|w| w.get(k));
    let in_fallback = |k: &str| all.get(FALLBACK).and_then(|w| w.get(k));
    let found = pick(&language, key, args, &in_language)
        .or_else(|| pick(FALLBACK, key, args, &in_fallback));
    found.map_or_else(|| key.to_owned(), |template| render(template, args))
}

/// The template for `key` among the words `get` knows: the plural form for
/// `count` first (`_one` or `_other`, by the language's rule), else `key`.
fn pick<'a>(
    language: &str,
    key: &str,
    args: &[(&str, &dyn Display)],
    get: &dyn Fn(&str) -> Option<&'a String>,
) -> Option<&'a String> {
    let count = args
        .iter()
        .find(|(name, _)| *name == "count")
        .and_then(|(_, v)| v.to_string().parse::<f64>().ok());
    count
        .map(|n| {
            if singular(language, n) {
                "_one"
            } else {
                "_other"
            }
        })
        .and_then(|suffix| get(&format!("{key}{suffix}")))
        .or_else(|| get(key))
}

/// The language's own rule: in French 0 and 1 are singular (« 0 appareil »),
/// in English only 1.
fn singular(language: &str, n: f64) -> bool {
    match language {
        "fr" => n.abs() < 2.0,
        _ => (n - 1.0).abs() < f64::EPSILON,
    }
}

/// `{name}` replaced by each argument.
fn render(template: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut text = template.to_owned();
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), &value.to_string());
    }
    text
}

/// `tr!("area.key")` or `tr!("area.key", name = value, …)`.
#[macro_export]
macro_rules! tr {
    ($key:expr) => {
        $crate::tr($key)
    };
    ($key:expr, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::trf($key, &[$((stringify!($name), &$value as &dyn ::std::fmt::Display)),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_word_shows_its_key_and_languages_are_checked() {
        assert!(languages().contains(&"fr"));
        assert_eq!(tr("absent.key"), "absent.key");
        assert_eq!(tr!("absent.key", name = "Sam"), "absent.key");
        assert!(!set_language("xx"), "an unknown language is refused");
        assert_eq!(language(), "fr");
    }

    #[test]
    fn plurals_and_parameters() {
        let words: Words = [
            ("salut", "Bonjour {name}"),
            ("appareils_one", "{count} appareil"),
            ("appareils_other", "{count} appareils"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let get = |k: &str| words.get(k);
        let one: [(&str, &dyn Display); 1] = [("count", &1)];
        let three: [(&str, &dyn Display); 1] = [("count", &3)];
        let sam: [(&str, &dyn Display); 1] = [("name", &"Sam")];
        let zero: [(&str, &dyn Display); 1] = [("count", &0)];
        assert_eq!(
            render(pick("fr", "appareils", &zero, &get).unwrap(), &zero),
            "0 appareil",
            "French: zero is singular"
        );
        assert_eq!(
            pick("en", "appareils", &zero, &get).unwrap(),
            "{count} appareils",
            "English: zero is plural"
        );
        assert_eq!(
            render(pick("fr", "appareils", &one, &get).unwrap(), &one),
            "1 appareil"
        );
        assert_eq!(
            render(pick("fr", "appareils", &three, &get).unwrap(), &three),
            "3 appareils"
        );
        assert_eq!(
            render(pick("fr", "salut", &sam, &get).unwrap(), &sam),
            "Bonjour Sam"
        );
        assert!(pick("fr", "absent", &sam, &get).is_none());
    }
}
