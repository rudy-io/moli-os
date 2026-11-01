//! Where a place is: a town's name to a point (Open-Meteo's geocoding, free and
//! keyless, the service that already gives the forecast). Only the name of the
//! town goes out, never the house; the point kept is the town's, rounded to a
//! street-less kilometre.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use http::{Method, Request};
use http_body_util::Full;
use moli_net::Body as Bytes;
use serde_json::Value as Json;

const HOST: &str = "geocoding-api.open-meteo.com";
/// One lookup's own limit, and all the lookups of a request together: the turn
/// must not hang on a slow service (five lookups of ten seconds were nearly a minute).
const LIMIT: Duration = Duration::from_secs(4);
pub(crate) const BUDGET: Duration = Duration::from_secs(12);
const MAX_BODY: usize = 64 * 1024;
const MAX_LABEL: usize = 60;

/// A town: how to say it, and where it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub label: String,
    pub latitude: f64,
    pub longitude: f64,
}

pub type Found<'a> = Pin<Box<dyn Future<Output = anyhow::Result<Option<Place>>> + Send + 'a>>;

/// Finds a town by its name.
pub trait Places: Send + Sync + std::fmt::Debug {
    fn find<'a>(&'a self, name: &'a str) -> Found<'a>;
}

#[derive(Debug)]
pub struct OpenMeteoPlaces;

impl Places for OpenMeteoPlaces {
    fn find<'a>(&'a self, name: &'a str) -> Found<'a> {
        Box::pin(async move {
            let request = Request::builder()
                .method(Method::GET)
                .uri(format!(
                    "/v1/search?name={}&count=1&language=fr&format=json",
                    encode(name)
                ))
                .header("accept", "application/json")
                .body(Full::new(Bytes::new()))?;
            let (status, body) = moli_net::web(HOST, 443, true, request, LIMIT, MAX_BODY).await?;
            anyhow::ensure!(status.is_success(), "geocoding: HTTP {}", status.as_u16());
            Ok(parse(&serde_json::from_slice(&body)?))
        })
    }
}

/// The first town of an answer, at the kilometre.
pub(crate) fn parse(answer: &Json) -> Option<Place> {
    let first = answer["results"].as_array()?.first()?;
    let name = clean(first["name"].as_str()?);
    let (latitude, longitude) = (first["latitude"].as_f64()?, first["longitude"].as_f64()?);
    if name.is_empty()
        || !(-90.0..=90.0).contains(&latitude)
        || !(-180.0..=180.0).contains(&longitude)
    {
        return None;
    }
    let around: Vec<String> = ["admin1", "country"]
        .iter()
        .filter_map(|k| first[k].as_str().map(clean))
        .filter(|s| !s.is_empty() && *s != name)
        .collect();
    let label = if around.is_empty() {
        name
    } else {
        format!("{name} ({})", around.join(", "))
    };
    Some(Place {
        label,
        latitude: (latitude * 100.0).round() / 100.0,
        longitude: (longitude * 100.0).round() / 100.0,
    })
}

/// Words before a town (« j'habite à Lyon ») and words that end it (« … pour la météo »).
/// « a » is only « à » without its accent right after living somewhere (« j'habite a
/// Lyon »): elsewhere it is « avoir » (« il y a », « elle a »), never a town's door.
const MARKERS: [&str; 6] = ["à", "au", "en", "sur", "dans", "vers"];
const LIVING: [&str; 11] = [
    "habite", "habitons", "habitez", "vis", "vit", "vivons", "suis", "sommes", "situe", "situé",
    "située",
];
const STOPS: [&str; 5] = ["pour", "parce", "car", "et", "mais"];
/// Lookups made for one request at most.
const MAX_CANDIDATES: usize = 5;

/// The names to try for one request, best first. The small model garbles a rare town
/// (« Ormevaal ») or adds the region to it: so after what it wrote come the person's
/// own words after « à / sur / en … », and each name loses its trailing words one by one
/// (down to two words) until the service knows it.
pub(crate) fn candidates(model: &str, question: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for source in [model.to_owned(), tail_of(question)] {
        let words: Vec<&str> = source.split_whitespace().collect();
        let floor = if words.len() >= 3 { 2 } else { words.len() };
        for n in (floor.max(1)..=words.len()).rev() {
            let candidate = words[..n].join(" ");
            if !out
                .iter()
                .any(|o| o.to_lowercase() == candidate.to_lowercase())
            {
                out.push(candidate);
            }
        }
    }
    out.truncate(MAX_CANDIDATES);
    out
}

/// The first of `names` the service knows, all the lookups within [`BUDGET`].
pub(crate) async fn first_known(
    places: &dyn Places,
    names: &[String],
) -> anyhow::Result<Option<Place>> {
    let lookups = async {
        for name in names {
            if let Some(place) = places.find(name).await? {
                return Ok(Some(place));
            }
        }
        Ok(None)
    };
    tokio::time::timeout(BUDGET, lookups)
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("the town lookups ran out of time")))
}

/// What the person wrote after the first « à / sur / en … », up to a word that ends a town.
fn tail_of(question: &str) -> String {
    let punctuation = |c: char| ",.?!;:".contains(c);
    let words: Vec<&str> = question.split_whitespace().collect();
    let word = |i: usize| words[i].trim_matches(punctuation).to_lowercase();
    let Some(start) = (0..words.len()).position(|i| {
        let w = word(i);
        MARKERS.contains(&w.as_str())
            || (w == "a" && i > 0 && {
                let before = word(i - 1);
                let verb = before
                    .strip_prefix("j'")
                    .or_else(|| before.strip_prefix("j’"))
                    .unwrap_or(&before);
                LIVING.contains(&verb)
            })
    }) else {
        return String::new();
    };
    words[start + 1..]
        .iter()
        .take_while(|w| !STOPS.contains(&w.to_lowercase().as_str()))
        .map(|w| w.trim_matches(punctuation))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A label from the service: no control character, no more than a name's length.
fn clean(written: &str) -> String {
    written
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// Percent-encoding of a name for a query string.
fn encode(name: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_town_is_named_with_its_region_and_placed_at_the_kilometre() {
        let answer = json!({ "results": [{
            "name": "Lyon", "latitude": 45.76404, "longitude": 4.83566,
            "admin1": "Auvergne-Rhône-Alpes", "country": "France" }] });
        assert_eq!(
            parse(&answer),
            Some(Place {
                label: "Lyon (Auvergne-Rhône-Alpes, France)".into(),
                latitude: 45.76,
                longitude: 4.84
            })
        );
    }

    #[test]
    fn an_answer_without_a_town_finds_nothing() {
        assert_eq!(parse(&json!({})), None);
        assert_eq!(parse(&json!({ "results": [] })), None);
        let off = json!({ "results": [{ "name": "X", "latitude": 143.0, "longitude": 1.0 }] });
        assert_eq!(parse(&off), None);
    }

    #[test]
    fn a_label_from_the_service_is_cleaned_and_cut() {
        let answer = json!({ "results": [{
            "name": format!("Ly\non{}", "x".repeat(200)), "latitude": 45.0, "longitude": 4.0,
            "admin1": "\u{7}", "country": "France" }] });
        let place = parse(&answer).unwrap();
        assert!(place.label.chars().all(|c| !c.is_control()));
        assert!(place.label.chars().count() < 100, "{}", place.label.len());
        assert_eq!(
            parse(&json!({ "results": [{ "name": "\n", "latitude": 1.0, "longitude": 1.0 }] })),
            None
        );
    }

    #[test]
    fn the_candidates_start_with_the_models_town_then_the_persons_own_words() {
        let c = candidates("Villeneuve d'Ormevaal", "J'habite a villeneuve d'ormeval");
        assert_eq!(c[0], "Villeneuve d'Ormevaal");
        assert!(c.contains(&"villeneuve d'ormeval".to_owned()), "{c:?}");
    }

    #[test]
    fn a_region_after_the_town_is_dropped_one_word_at_a_time_down_to_two_words() {
        let c = candidates(
            "Villeneuve d'ormeval haute Brume",
            "Villeneuve d'ormeval haute Brume",
        );
        assert!(
            c.contains(&"Villeneuve d'ormeval haute".to_owned()),
            "{c:?}"
        );
        assert!(c.contains(&"Villeneuve d'ormeval".to_owned()), "{c:?}");
        assert!(!c.contains(&"Villeneuve".to_owned()), "{c:?}");
    }

    /// A service that knows nothing, slowly.
    #[derive(Debug)]
    struct Slow;

    impl Places for Slow {
        fn find<'a>(&'a self, _name: &'a str) -> Found<'a> {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(None)
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_lookups_of_a_request_stop_at_their_budget() {
        let names: Vec<String> = (1..=5).map(|n| format!("Ville {n}")).collect();
        let started = tokio::time::Instant::now();
        let found = first_known(&Slow, &names).await;
        assert!(
            found.is_err(),
            "out of time is said, not taken for « unknown »"
        );
        // Five lookups of ten seconds would be fifty.
        assert_eq!(started.elapsed(), BUDGET);
    }

    #[test]
    fn a_is_a_towns_door_only_after_living_somewhere() {
        // « il y a », « elle a » : the verb, not « à ».
        let c = candidates("Lyon", "Il y a du vent, mets la maison à Lyon");
        assert_eq!(c, vec!["Lyon".to_owned()]);
        assert!(tail_of("Il y a de la pluie").is_empty());
        // Without its accent after living somewhere, it is « à ».
        assert_eq!(tail_of("J’habite a Lyon"), "Lyon");
        assert_eq!(tail_of("La maison est située a Annecy"), "Annecy");
    }

    #[test]
    fn a_one_word_town_is_kept_whole_and_a_reason_after_it_is_cut() {
        let c = candidates("Lyon", "Règle la maison sur Lyon pour la météo.");
        assert_eq!(c, vec!["Lyon".to_owned()]);
    }

    #[test]
    fn there_are_never_more_than_five_candidates() {
        let long = "un deux trois quatre cinq six sept huit";
        assert!(candidates(long, &format!("j'habite à {long}")).len() <= 5);
    }

    #[test]
    fn a_name_is_encoded_for_the_query() {
        assert_eq!(encode("Saint-Étienne"), "Saint-%C3%89tienne");
        assert_eq!(encode("a b&c"), "a%20b%26c");
    }
}
