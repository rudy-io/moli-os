//! The setpoints of the switch-wired week programs, kept by Moli
//! (`tuya-programs.json`, next to the device file).
//!
//! A module keeps its setpoints only while it is on switch wiring: rewired
//! to four or six orders it drops them from its parts, rewired to switch it
//! writes them back, a minute or more later (the same ones, seen on real
//! modules). Moli keeps the last ones it read, so that a module rewired to
//! switch shows its program at once, as on four or six orders, and nothing
//! waits for the module's rewrite. Once the module has rewritten its parts,
//! they are compared with what Moli keeps and, if they differ, Moli's are
//! sent: what Moli keeps is the program. A program saved meanwhile is kept
//! and sent the same way. Otherwise the module is the truth: whatever
//! changes its setpoints (Moli, Smart Life) is read and kept.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::program::{self, DAYS, PARTS, SLOTS};

/// A module's parts by code, as the cloud gives them.
pub type Parts = HashMap<String, Vec<u8>>;

/// Shared by the driver's loop (wirings) and the cloud's tasks (reads, writes).
pub type Shared = Arc<Mutex<Kept>>;

/// Orders at the head of a part (the rest are setpoints).
const ORDERS: usize = SLOTS / 2;
/// A week's setpoints: 28 parts of 24.
const WEEK: usize = DAYS as usize * PARTS as usize * SLOTS;

#[derive(Debug, Default)]
pub struct Kept {
    path: Option<PathBuf>,
    /// By Tuya id: the week's setpoints in half degrees, part after part
    /// (Monday 0 h first).
    weeks: HashMap<String, Vec<u8>>,
    /// The modules on switch wiring, as their wiring was last heard.
    switch: HashSet<String>,
    /// The modules behind what Moli keeps (rewired, or a week saved while
    /// they rewrote): Moli's is shown until they hold it.
    ahead: HashSet<String>,
    /// The parts last read, by module.
    last: HashMap<String, Parts>,
}

/// Whether all 28 parts are there with their setpoints (switch wiring, rewrite done).
#[must_use]
pub fn complete(parts: &Parts) -> bool {
    codes().all(|code| parts.get(&code).is_some_and(|b| b.len() == ORDERS + SLOTS))
}

/// The 28 parts' codes, Monday 0 h first.
fn codes() -> impl Iterator<Item = String> {
    (1..=DAYS).flat_map(|day| (1..=PARTS).map(move |part| program::part_code(day, part)))
}

/// The setpoints of complete parts, part after part.
fn setpoints(parts: &Parts) -> Vec<u8> {
    codes()
        .flat_map(|code| parts[&code][ORDERS..].to_vec())
        .collect()
}

impl Kept {
    /// What `path` keeps; nothing if it is missing or unreadable (said).
    #[must_use]
    pub fn load(path: PathBuf) -> Self {
        let weeks = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|json| from_json(&json))
                .unwrap_or_else(|| {
                    tracing::warn!(file = %path.display(), "tuya: kept setpoints unreadable, read again from the modules");
                    HashMap::new()
                }),
            Err(_) => HashMap::new(),
        };
        Self {
            path: Some(path),
            weeks,
            ..Self::default()
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let text = to_json(&self.weeks).to_string();
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, path)) {
            tracing::warn!(file = %path.display(), error = %e, "tuya: kept setpoints not saved");
        }
    }

    /// A module's wiring heard (`changed`: not the one heard before). Rewired
    /// to switch with setpoints kept: they are shown until the module holds
    /// them (true then: show the week again now).
    pub fn wired(&mut self, id: &str, switch: bool, changed: bool) -> bool {
        if !switch {
            self.switch.remove(id);
            self.ahead.remove(id);
            return false;
        }
        self.switch.insert(id.to_owned());
        if changed && self.weeks.contains_key(id) {
            self.ahead.insert(id.to_owned());
            return true;
        }
        false
    }

    /// Whether a module is on switch wiring.
    #[must_use]
    pub fn is_switch(&self, id: &str) -> bool {
        self.switch.contains(id)
    }

    /// A module's parts read: kept when the module holds the program (its
    /// setpoints are then Moli's), and the week to show.
    pub fn seen(&mut self, id: &str, parts: Parts) -> serde_json::Value {
        if complete(&parts) {
            let module = setpoints(&parts);
            if self.ahead.contains(id) {
                if self.weeks.get(id) == Some(&module) {
                    self.ahead.remove(id);
                }
            } else if self.weeks.get(id) != Some(&module) {
                self.weeks.insert(id.to_owned(), module);
                self.save();
            }
        }
        self.last.insert(id.to_owned(), parts);
        self.week(id)
    }

    /// The week to show: on switch wiring with setpoints kept, the orders
    /// last read and Moli's setpoints; otherwise as last read.
    #[must_use]
    pub fn week(&self, id: &str) -> serde_json::Value {
        let empty = Parts::new();
        let parts = self.last.get(id).unwrap_or(&empty);
        match self.weeks.get(id).filter(|_| self.switch.contains(id)) {
            Some(kept) => program::week(|day, part| {
                let at = usize::from((day - 1) * PARTS + part - 1) * SLOTS;
                let code = program::part_code(day, part);
                let mut bytes = parts
                    .get(&code)
                    .map_or_else(|| vec![0; ORDERS], |b| b[..ORDERS.min(b.len())].to_vec());
                bytes.resize(ORDERS, 0);
                bytes.extend_from_slice(&kept[at..at + SLOTS]);
                Some(bytes)
            }),
            None => program::week(|day, part| parts.get(&program::part_code(day, part)).cloned()),
        }
    }

    /// A week a person saved for a switch-wired module still rewriting its
    /// parts: kept, to send once it holds them. False when Moli keeps no
    /// setpoints for it (never read on switch wiring).
    ///
    /// # Errors
    /// The week is not taken (`program::parse_week`).
    pub fn take(&mut self, id: &str, week: &str) -> Result<bool, program::Refused> {
        let days = program::parse_week(week)?;
        let Some(kept) = self.weeks.get_mut(id) else {
            return Ok(false);
        };
        for (day, slots) in days.iter().enumerate() {
            for (at, slot) in slots.iter().flatten().enumerate() {
                if let Some(half) = slot.celsius.and_then(program::half) {
                    kept[day * program::DAY_SLOTS + at] = half;
                }
            }
        }
        self.ahead.insert(id.to_owned());
        self.save();
        Ok(true)
    }

    /// The parts to send so that a module that holds its setpoints holds
    /// Moli's (its own orders kept); none when they are the same.
    #[must_use]
    pub fn to_send(&self, id: &str, parts: &Parts) -> Vec<(String, Vec<u8>)> {
        let Some(kept) = self.weeks.get(id).filter(|_| complete(parts)) else {
            return Vec::new();
        };
        codes()
            .zip(kept.chunks(SLOTS))
            .filter_map(|(code, setpoints)| {
                let current = &parts[&code];
                let mut new = current[..ORDERS].to_vec();
                new.extend_from_slice(setpoints);
                (new != *current).then_some((code, new))
            })
            .collect()
    }

    /// A module that did not take Moli's setpoints: shown as it is again.
    pub fn settle(&mut self, id: &str) {
        self.ahead.remove(id);
    }

    /// Whether Moli's setpoints are shown for a module until it holds them.
    #[must_use]
    pub fn is_ahead(&self, id: &str) -> bool {
        self.ahead.contains(id)
    }
}

/// On disk, in the point's words, setpoints only: `{"<id>": {"days":
/// [[{"at": 0, "celsius": 17.0}, …], …]}}`.
fn to_json(weeks: &HashMap<String, Vec<u8>>) -> serde_json::Value {
    let mut ids: Vec<&String> = weeks.keys().collect();
    ids.sort();
    let out: serde_json::Map<String, serde_json::Value> = ids
        .into_iter()
        .map(|id| {
            let days: Vec<serde_json::Value> = weeks[id]
                .chunks(program::DAY_SLOTS)
                .map(|day| {
                    let mut runs = Vec::new();
                    for (at, half) in day.iter().enumerate() {
                        if at == 0 || day[at - 1] != *half {
                            runs.push(
                                serde_json::json!({ "at": at, "celsius": f64::from(*half) / 2.0 }),
                            );
                        }
                    }
                    serde_json::Value::Array(runs)
                })
                .collect();
            (id.clone(), serde_json::json!({ "days": days }))
        })
        .collect();
    serde_json::Value::Object(out)
}

fn from_json(json: &serde_json::Value) -> Option<HashMap<String, Vec<u8>>> {
    let mut weeks = HashMap::new();
    for (id, week) in json.as_object()? {
        let days = week["days"]
            .as_array()
            .filter(|d| d.len() == usize::from(DAYS))?;
        let mut halves = Vec::with_capacity(WEEK);
        for runs in days {
            let runs = runs.as_array()?;
            let start = halves.len();
            for (i, run) in runs.iter().enumerate() {
                let at = usize::try_from(run["at"].as_u64()?).ok()?;
                let end = runs.get(i + 1).map_or(Some(program::DAY_SLOTS), |next| {
                    next["at"].as_u64().and_then(|a| usize::try_from(a).ok())
                })?;
                let celsius = run["celsius"].as_f64()?;
                if start + at != halves.len()
                    || end <= at
                    || end > program::DAY_SLOTS
                    || !(0.0..=127.0).contains(&celsius)
                {
                    return None;
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked above
                let half = (celsius * 2.0).round() as u8;
                halves.resize(start + end, half);
            }
            if halves.len() != start + program::DAY_SLOTS {
                return None;
            }
        }
        weeks.insert(id.clone(), halves);
    }
    Some(weeks)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A module's 28 parts: comfort all along, then `setpoints` (half
    /// degrees, the same all week) on switch wiring.
    fn module(setpoints: Option<u8>) -> Parts {
        codes()
            .map(|code| {
                let mut bytes = vec![0x11; ORDERS];
                if let Some(half) = setpoints {
                    bytes.extend(std::iter::repeat_n(half, SLOTS));
                }
                (code, bytes)
            })
            .collect()
    }

    fn celsius(week: &serde_json::Value) -> Vec<Option<f64>> {
        week["days"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|runs| {
                runs.as_array()
                    .unwrap()
                    .iter()
                    .map(|r| r["celsius"].as_f64())
            })
            .collect()
    }

    #[test]
    fn a_module_rewired_to_switch_shows_its_setpoints_at_once() {
        let mut kept = Kept::default();
        kept.wired("fp", true, false);
        kept.seen("fp", module(Some(34)));
        // Rewired to six orders: its parts lose their setpoints.
        kept.wired("fp", false, true);
        let six = kept.seen("fp", module(None));
        assert!(celsius(&six).iter().all(Option::is_none));
        // Back to switch: 17 °C at once, the module still on orders only.
        assert!(kept.wired("fp", true, true), "shown again now");
        assert_eq!(celsius(&kept.week("fp")), vec![Some(17.0); 7]);
        assert_eq!(celsius(&kept.seen("fp", module(None))), vec![Some(17.0); 7]);
        assert!(
            kept.to_send("fp", &module(None)).is_empty(),
            "nothing sent before the rewrite"
        );
        // Rewritten with the same setpoints: nothing to send, the module holds it.
        assert!(kept.to_send("fp", &module(Some(34))).is_empty());
        kept.seen("fp", module(Some(34)));
        assert!(!kept.is_ahead("fp"));
    }

    #[test]
    fn a_module_rewritten_otherwise_is_sent_what_moli_keeps() {
        let mut kept = Kept::default();
        kept.wired("fp", true, false);
        kept.seen("fp", module(Some(34)));
        kept.wired("fp", false, true);
        kept.wired("fp", true, true);
        // The module comes back at 16 °C: Moli's 17 °C is shown and sent.
        let week = kept.seen("fp", module(Some(32)));
        assert_eq!(celsius(&week), vec![Some(17.0); 7]);
        let sent = kept.to_send("fp", &module(Some(32)));
        assert_eq!(sent.len(), 28);
        assert_eq!(sent[0].1[..ORDERS], [0x11; ORDERS], "its orders kept");
        assert!(sent[0].1[ORDERS..].iter().all(|h| *h == 34));
        // Once it holds them, it is the truth again.
        kept.seen("fp", module(Some(34)));
        assert!(!kept.is_ahead("fp"));
        kept.seen("fp", module(Some(40)));
        assert_eq!(
            celsius(&kept.week("fp")),
            vec![Some(20.0); 7],
            "Smart Life's change kept"
        );
    }

    #[test]
    fn a_week_saved_while_the_module_rewrites_is_kept_for_it() {
        let mut kept = Kept::default();
        kept.wired("fp", true, false);
        kept.seen("fp", module(Some(34)));
        kept.wired("fp", false, true);
        kept.wired("fp", true, true);
        kept.seen("fp", module(None));
        // Monday from 6:00 to 8:00 at 19.5 °C.
        let mut days = vec![serde_json::Value::Null; 7];
        days[0] = serde_json::json!([{ "at": 0, "celsius": 17.0 }, { "at": 24, "celsius": 19.5 }, { "at": 32, "celsius": 17.0 }]);
        let week = serde_json::json!({ "days": days }).to_string();
        assert_eq!(kept.take("fp", &week), Ok(true));
        assert_eq!(
            kept.week("fp")["days"][0][1],
            serde_json::json!({ "at": 24, "order": "comfort", "celsius": 19.5 })
        );
        // The module rewritten at 17 °C: Monday's second part leaves.
        let sent = kept.to_send("fp", &module(Some(34)));
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "week_program_18_1_2");
        assert_eq!(
            sent[0].1[ORDERS..ORDERS + 9],
            [39, 39, 39, 39, 39, 39, 39, 39, 34]
        );
    }

    #[test]
    fn a_module_never_read_on_switch_waits_for_its_module() {
        let mut kept = Kept::default();
        assert!(!kept.wired("fp", true, true));
        assert_eq!(
            kept.take(
                "fp",
                r#"{"days": [null, null, null, null, null, null, null]}"#
            ),
            Ok(false)
        );
        assert!(
            celsius(&kept.seen("fp", module(None)))
                .iter()
                .all(Option::is_none)
        );
    }

    #[test]
    fn what_moli_keeps_is_read_back_from_its_file() {
        let dir = std::env::temp_dir().join(format!("moli-kept-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tuya-programs.json");
        let mut kept = Kept::load(path.clone());
        kept.wired("fp", true, false);
        let mut parts = module(Some(34));
        parts.get_mut("week_program_18_3_1").unwrap()[ORDERS + 6] = 31;
        kept.seen("fp", parts.clone());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#"{"at":6,"celsius":15.5}"#), "{text}");
        let mut again = Kept::load(path);
        again.wired("fp", true, false);
        assert!(again.to_send("fp", &parts).is_empty(), "the same week");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
