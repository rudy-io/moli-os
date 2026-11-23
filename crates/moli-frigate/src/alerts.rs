//! Frigate's reviews (`frigate/reviews`): which alerts are going on, on which
//! camera and in which zones. Pure: no network, no clock.
//!
//! A review is one event as Frigate groups it (`new`, then `update`s, then
//! `end`); its severity is `alert` or `detection`. An alert going on makes
//! the `alert` point of its camera, and of each zone it reached, true once,
//! whatever the object counts do meanwhile (they blink while a person moves).

use std::collections::BTreeMap;

use serde::Deserialize;

/// Alerts followed at once: beyond, the oldest is forgotten (an `end` lost
/// with a broker restart must not keep memory growing).
const MOST: usize = 64;

#[derive(Deserialize)]
struct Message {
    #[serde(rename = "type")]
    kind: String,
    after: Side,
}

#[derive(Deserialize)]
struct Side {
    id: String,
    camera: String,
    severity: String,
    #[serde(default)]
    data: Data,
}

#[derive(Default, Deserialize)]
struct Data {
    #[serde(default)]
    zones: Vec<String>,
}

/// One review message, as the driver needs it.
#[derive(Debug, PartialEq)]
pub struct Review {
    pub id: String,
    pub camera: String,
    /// An alert going on (`new` or `update` of severity `alert`); false for
    /// its `end`, and for a mere detection.
    pub alert: bool,
    pub zones: Vec<String>,
}

/// Reads one message of `frigate/reviews`. Names Frigate would refuse (a
/// camera, a zone) are left out; an id longer than Frigate's is refused.
#[must_use]
pub fn read(payload: &str) -> Option<Review> {
    let m: Message = serde_json::from_str(payload).ok()?;
    let a = m.after;
    if a.id.is_empty() || a.id.len() > 64 || !crate::review::is_camera(&a.camera) {
        return None;
    }
    let going_on = matches!(m.kind.as_str(), "new" | "update");
    Some(Review {
        alert: going_on && a.severity == "alert",
        zones: a
            .data
            .zones
            .into_iter()
            .filter(|z| crate::review::is_camera(z))
            .collect(),
        id: a.id,
        camera: a.camera,
    })
}

/// The alerts going on.
#[derive(Debug, Default)]
pub struct Alerts {
    /// id → (camera, zones), oldest first by arrival.
    going_on: BTreeMap<String, (u64, String, Vec<String>)>,
    next: u64,
}

impl Alerts {
    /// Takes one review into account.
    pub fn apply(&mut self, review: Review) {
        if review.alert {
            let order = self.going_on.get(&review.id).map_or_else(
                || {
                    self.next += 1;
                    self.next
                },
                |(order, _, _)| *order,
            );
            self.going_on
                .insert(review.id, (order, review.camera, review.zones));
            while self.going_on.len() > MOST {
                let oldest = self
                    .going_on
                    .iter()
                    .min_by_key(|(_, (order, _, _))| *order)
                    .map(|(id, _)| id.clone());
                if let Some(id) = oldest {
                    self.going_on.remove(&id);
                }
            }
        } else {
            // An `end`, or a review that is (or became) a mere detection.
            self.going_on.remove(&review.id);
        }
    }

    /// Nothing is going on any more (Frigate or the broker went away: the
    /// `end`s will not come).
    pub fn clear(&mut self) {
        self.going_on.clear();
    }

    /// Whether an alert is going on on this camera.
    #[must_use]
    pub fn on_camera(&self, camera: &str) -> bool {
        self.going_on.values().any(|(_, c, _)| c == camera)
    }

    /// Whether an alert going on reached this zone.
    #[must_use]
    pub fn in_zone(&self, zone: &str) -> bool {
        self.going_on
            .values()
            .any(|(_, _, zones)| zones.iter().any(|z| z == zone))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(kind: &str, id: &str, severity: &str, zones: &[&str]) -> String {
        serde_json::json!({
            "type": kind,
            "before": { "id": id, "camera": "garage", "severity": "detection",
                        "data": { "zones": [] } },
            "after": { "id": id, "camera": "garage", "severity": severity,
                       "data": { "objects": ["person"], "zones": zones } },
        })
        .to_string()
    }

    #[test]
    fn a_review_is_read_or_refused() {
        let r = read(&message("new", "1763", "alert", &["propriete"])).unwrap();
        assert_eq!(
            r,
            Review {
                id: "1763".into(),
                camera: "garage".into(),
                alert: true,
                zones: vec!["propriete".into()]
            }
        );
        assert!(
            !read(&message("end", "1763", "alert", &["propriete"]))
                .unwrap()
                .alert
        );
        assert!(
            !read(&message("new", "1764", "detection", &[]))
                .unwrap()
                .alert
        );
        assert_eq!(read("nope"), None);
        assert_eq!(read(&message("new", "", "alert", &[])), None);
        assert_eq!(read(&message("new", &"9".repeat(65), "alert", &[])), None);
        let odd = message("new", "1765", "alert", &["propriete", "../etc", "allée"]);
        assert_eq!(read(&odd).unwrap().zones, ["propriete"]);
        let stranger = message("new", "1766", "alert", &[]).replace("\"garage\"", "\"../x\"");
        assert_eq!(read(&stranger), None);
    }

    /// The counts blink while a person moves; the alert stays on from the
    /// review's start to its end: one rise per intrusion.
    #[test]
    fn an_alert_goes_on_from_its_start_to_its_end() {
        let mut alerts = Alerts::default();
        let step = |alerts: &mut Alerts, text: String| alerts.apply(read(&text).unwrap());
        step(&mut alerts, message("new", "a", "detection", &[]));
        assert!(!alerts.on_camera("garage"));
        // Becomes an alert as the person enters the zone.
        step(&mut alerts, message("update", "a", "alert", &["propriete"]));
        assert!(alerts.on_camera("garage") && alerts.in_zone("propriete"));
        step(&mut alerts, message("update", "a", "alert", &["propriete"]));
        assert!(alerts.in_zone("propriete"));
        assert!(!alerts.in_zone("allee") && !alerts.on_camera("jardin"));
        step(&mut alerts, message("end", "a", "alert", &["propriete"]));
        assert!(!alerts.on_camera("garage") && !alerts.in_zone("propriete"));
    }

    #[test]
    fn two_alerts_overlap_and_lost_ends_are_bounded() {
        let mut alerts = Alerts::default();
        alerts.apply(read(&message("new", "a", "alert", &["propriete"])).unwrap());
        alerts.apply(read(&message("new", "b", "alert", &[])).unwrap());
        alerts.apply(read(&message("end", "a", "alert", &["propriete"])).unwrap());
        assert!(alerts.on_camera("garage") && !alerts.in_zone("propriete"));
        alerts.clear();
        assert!(!alerts.on_camera("garage"));
        for i in 0..200 {
            alerts.apply(read(&message("new", &i.to_string(), "alert", &[])).unwrap());
        }
        assert_eq!(alerts.going_on.len(), MOST);
        // The newest are kept.
        assert!(alerts.going_on.contains_key("199"));
        assert!(!alerts.going_on.contains_key("0"));
    }
}
