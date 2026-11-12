//! The RecycleApp API's answers decoded, pure (no I/O): the address's ids,
//! then each waste type's next collection.

use std::collections::BTreeMap;

use jiff::civil::Date;
use serde_json::Value as Json;

/// A waste type by the logo the API gives it (the ids Home Assistant's
/// RecycleApp component knows), as a stable point key. Others are left out.
#[must_use]
pub fn kind(logo: &str) -> Option<&'static str> {
    Some(match logo {
        "5d610b86162c063cc0400108" | "5d610b86162c063cc0400111" => "organic",
        "5d610b86162c063cc0400107" => "garden",
        "5d610b86162c063cc0400125" => "pmd",
        "5d610b86162c063cc0400123" => "paper",
        "5d610b86162c063cc0400112" => "residual",
        "5d610b86162c063cc0400133" => "non_recyclable",
        "5d610b86162c063cc0400127" => "pruning",
        "5d610b86162c063cc0400110" => "glass",
        "5d610b86162c063cc0400102" => "christmas_trees",
        "5d610b86162c063cc0400131" => "textiles",
        "5d610b86162c063cc0400115" => "hazardous",
        "609a4b94e85d9b1e58b530e8" => "container",
        _ => return None,
    })
}

fn name(names: &Json, language: &str) -> Option<String> {
    [language, "nl", "fr", "en", "de"]
        .iter()
        .find_map(|l| names[*l].as_str().filter(|s| !s.is_empty()))
        .map(str::to_owned)
}

/// `GET zipcodes?q=`: the ids of the zip code `code` (the API answers every
/// code that starts with it; several municipalities can share one code).
#[must_use]
pub fn zipcodes(json: &Json, code: &str) -> Vec<String> {
    json["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["code"].as_str() == Some(code))
        .filter_map(|i| i["id"].as_str())
        .map(str::to_owned)
        .collect()
}

/// `POST streets?q=&zipcodes=`: the id of the street, by its name in any
/// language (case and spaces aside); a street of another name, even alone,
/// is not taken.
#[must_use]
pub fn street(json: &Json, wanted: &str) -> Option<String> {
    let items = json["items"].as_array()?;
    let wanted = wanted.trim().to_lowercase();
    let same = |i: &&Json| {
        i["names"].as_object().is_some_and(|n| {
            n.values().any(|v| {
                v.as_str()
                    .is_some_and(|s| s.trim().to_lowercase() == wanted)
            })
        })
    };
    items
        .iter()
        .find(same)
        .and_then(|i| i["id"].as_str())
        .map(str::to_owned)
}

/// A waste type and its collections, soonest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kind {
    pub name: String,
    pub dates: Vec<Date>,
}

/// One page of `GET collections?…`: each known waste type's dates added
/// (cancelled ones, replaced by another day, left out). Returns the number
/// of pages.
pub fn add_collections(
    json: &Json,
    language: &str,
    kinds: &mut BTreeMap<&'static str, Kind>,
) -> u64 {
    for item in json["items"].as_array().into_iter().flatten() {
        if item["exception"]["replacedBy"].is_object()
            || item["exception"]["replacedBy"].is_string()
        {
            continue;
        }
        let fraction = &item["fraction"];
        let Some(key) = fraction["logo"]["id"].as_str().and_then(kind) else {
            continue;
        };
        // `2026-11-13T00:00:00.000Z`: the day as the municipality gives it.
        let Some(date) = item["timestamp"]
            .as_str()
            .and_then(|t| t.get(..10))
            .and_then(|d| d.parse::<Date>().ok())
        else {
            continue;
        };
        let entry = kinds.entry(key).or_insert_with(|| Kind {
            name: name(&fraction["name"], language).unwrap_or_else(|| key.to_owned()),
            dates: Vec::new(),
        });
        if !entry.dates.contains(&date) {
            entry.dates.push(date);
            entry.dates.sort();
        }
    }
    json["pages"].as_u64().unwrap_or(1)
}

/// The next collection of a waste type from `today` (today's included).
#[must_use]
pub fn next(kind: &Kind, today: Date) -> Option<Date> {
    kind.dates.iter().copied().find(|d| *d >= today)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Json {
        let path = format!(
            "{}/../../integrations/recycleapp/fixtures/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn the_address_is_found() {
        assert_eq!(zipcodes(&fixture("zipcodes"), "1000"), ["1000-00001"]);
        assert!(zipcodes(&fixture("zipcodes"), "1001").is_empty());
        // One code, two municipalities: both are tried.
        let shared = serde_json::json!({"items": [
            {"id": "a", "code": "1000"}, {"id": "b", "code": "1000"}
        ]});
        assert_eq!(zipcodes(&shared, "1000"), ["a", "b"]);
        let streets = fixture("streets");
        assert_eq!(
            street(&streets, " rue exemple ").as_deref(),
            Some("https://data.vlaanderen.be/id/straatnaam-1")
        );
        assert_eq!(
            street(&streets, "Voorbeeldstraat").as_deref(),
            Some("https://data.vlaanderen.be/id/straatnaam-1")
        );
        assert_eq!(
            street(&streets, "Ailleurs"),
            None,
            "two answers, none named so"
        );
        let alone = serde_json::json!({"items": [
            {"id": "x", "names": {"fr": "Autre rue"}}
        ]});
        assert_eq!(street(&alone, "Rue Exemple"), None, "alone, but not it");
    }

    #[test]
    fn each_kind_has_its_next_collection() {
        let mut kinds = BTreeMap::new();
        assert_eq!(
            add_collections(&fixture("collections"), "fr", &mut kinds),
            1
        );
        let today: Date = "2026-11-13".parse().unwrap();
        let residual = &kinds["residual"];
        assert_eq!(residual.name, "Déchets résiduels");
        assert_eq!(
            next(residual, today).unwrap().to_string(),
            "2026-11-13",
            "today counts"
        );
        assert_eq!(
            next(&kinds["pmd"], today).unwrap().to_string(),
            "2026-11-17"
        );
        // The cancelled paper collection is not a date; the one replacing it is.
        assert_eq!(
            next(&kinds["paper"], today).unwrap().to_string(),
            "2026-11-20"
        );
        assert!(
            !kinds.contains_key("unknown"),
            "an unknown logo is left out"
        );
        assert_eq!(
            kinds["organic"].name, "GFT",
            "a name missing in French: Dutch"
        );
    }
}
