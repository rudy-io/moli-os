//! The setpoints of the switch-wired week programs, kept by Moli
//! (`tuya-programs.json`, next to the device file).
//!
//! On switch wiring a part holds its orders then 24 setpoints (36 bytes), on
//! four or six orders its orders only (12). The cloud's copy of the parts
//! cannot be trusted on that: rewired, a module rewrites its 28 parts, but
//! some never reach the cloud, for days (seen on real modules: a part of 12
//! bytes on switch wiring, one of 36 on six orders). So a switch program is
//! read and written part by part, as a six-order one: each part's orders as
//! the cloud gives them, its setpoints as the cloud last gave them with
//! setpoints, kept by Moli across rewirings and restarts. A part the cloud
//! never gave with setpoints takes the setpoint before it in the week.
//! Saving a program also sends the parts the cloud holds without setpoints,
//! so that the module holds what is shown.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::program::{self, DAYS, PARTS, SLOTS};

/// A module's parts by code, as the cloud gives them.
pub type Parts = HashMap<String, Vec<u8>>;

/// Shared by the driver's loop (wirings) and the cloud's tasks (reads, writes).
pub type Shared = Arc<Mutex<Kept>>;

/// Orders at the head of a part (the rest are setpoints).
const ORDERS: usize = SLOTS / 2;
/// A week's slots: 28 parts of 24.
const WEEK: usize = DAYS as usize * PARTS as usize * SLOTS;

#[derive(Debug, Default)]
pub struct Kept {
    path: Option<PathBuf>,
    /// By Tuya id: the week's setpoints in half degrees, slot after slot
    /// (Monday 0 h first); `None` in a part never read with setpoints.
    weeks: HashMap<String, Vec<Option<u8>>>,
    /// The modules' wiring, as last heard (true: switch).
    switch: HashMap<String, bool>,
    /// The parts last read, by module.
    last: HashMap<String, Parts>,
}

/// The 28 parts' codes, Monday 0 h first.
fn codes() -> impl Iterator<Item = String> {
    (1..=DAYS).flat_map(|day| (1..=PARTS).map(move |part| program::part_code(day, part)))
}

/// Where a part's slots start in the week.
fn start(day: u8, part: u8) -> usize {
    usize::from((day - 1) * PARTS + part - 1) * SLOTS
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

    /// A module's wiring heard; whether it changes the one Moli knew (the
    /// week is then shown again).
    pub fn wired(&mut self, id: &str, switch: bool) -> bool {
        self.switch.insert(id.to_owned(), switch) != Some(switch)
    }

    fn is_switch(&self, id: &str) -> bool {
        self.switch.get(id).copied().unwrap_or(false)
    }

    /// A module's parts read (`wiring`: as the cloud says it, taken until
    /// the driver says it): the setpoints of those that hold some are kept,
    /// and the week to show.
    pub fn seen(&mut self, id: &str, parts: Parts, wiring: Option<bool>) -> serde_json::Value {
        if let Some(switch) = wiring {
            self.switch.entry(id.to_owned()).or_insert(switch);
        }
        let week = self
            .weeks
            .entry(id.to_owned())
            .or_insert_with(|| vec![None; WEEK]);
        let mut changed = false;
        for (code, at) in codes().zip((0..WEEK).step_by(SLOTS)) {
            let Some(bytes) = parts.get(&code).filter(|b| b.len() == ORDERS + SLOTS) else {
                continue;
            };
            for (slot, half) in week[at..at + SLOTS].iter_mut().zip(&bytes[ORDERS..]) {
                changed |= *slot != Some(*half);
                *slot = Some(*half);
            }
        }
        if week.iter().all(Option::is_none) {
            self.weeks.remove(id);
        } else if changed {
            self.save();
        }
        self.last.insert(id.to_owned(), parts);
        self.week(id)
    }

    /// The week to show, once the module's parts have been read.
    #[must_use]
    pub fn shown(&self, id: &str) -> Option<serde_json::Value> {
        self.last.contains_key(id).then(|| self.week(id))
    }

    fn week(&self, id: &str) -> serde_json::Value {
        let empty = Parts::new();
        let parts = self.last.get(id).unwrap_or(&empty);
        program::week(|day, part| self.part(id, day, part, parts))
    }

    /// A part as its wiring holds it: on switch wiring its orders, then the
    /// setpoints it has or else the ones Moli keeps; as `parts` has it
    /// otherwise, or when Moli keeps no setpoint for the module.
    fn part(&self, id: &str, day: u8, part: u8, parts: &Parts) -> Option<Vec<u8>> {
        let bytes = parts.get(&program::part_code(day, part))?;
        if !self.is_switch(id) || bytes.len() != ORDERS {
            return Some(bytes.clone());
        }
        let Some(setpoints) = self.setpoints(id) else {
            return Some(bytes.clone());
        };
        let at = start(day, part);
        let mut out = bytes.clone();
        out.extend_from_slice(&setpoints[at..at + SLOTS]);
        Some(out)
    }

    /// The week's setpoints, a slot never read taking the one before it in
    /// the week; none when no part was ever read with setpoints.
    fn setpoints(&self, id: &str) -> Option<Vec<u8>> {
        let week = self.weeks.get(id)?;
        let mut last = week.iter().rev().flatten().next().copied()?;
        Some(
            week.iter()
                .map(|half| {
                    last = half.unwrap_or(last);
                    last
                })
                .collect(),
        )
    }

    /// The parts to send for a week a person saved (`program::changes`),
    /// against the parts just read; on switch wiring, with every part the
    /// cloud holds without setpoints, as shown.
    ///
    /// # Errors
    /// As `program::changes`.
    pub fn changes(
        &mut self,
        id: &str,
        week: &str,
        parts: &Parts,
        wiring: Option<bool>,
    ) -> Result<Vec<(String, Vec<u8>)>, program::Refused> {
        self.seen(id, parts.clone(), wiring);
        let mut out = program::changes(week, |day, part| self.part(id, day, part, parts))?;
        if self.is_switch(id) {
            for (day, part) in (1..=DAYS).flat_map(|d| (1..=PARTS).map(move |p| (d, p))) {
                let code = program::part_code(day, part);
                let held = self.part(id, day, part, parts);
                if parts.get(&code).map(Vec::len) == Some(ORDERS)
                    && held.as_ref().is_some_and(|b| b.len() == ORDERS + SLOTS)
                    && !out.iter().any(|(c, _)| *c == code)
                {
                    out.extend(held.map(|b| (code, b)));
                }
            }
        }
        Ok(out)
    }
}

/// On disk, in the point's words, setpoints only (`null` in a part never
/// read with them): `{"<id>": {"days": [[{"at": 0, "celsius": 17.0}, …], …]}}`.
fn to_json(weeks: &HashMap<String, Vec<Option<u8>>>) -> serde_json::Value {
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
                            let celsius = half.map(|h| f64::from(h) / 2.0);
                            runs.push(serde_json::json!({ "at": at, "celsius": celsius }));
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

fn from_json(json: &serde_json::Value) -> Option<HashMap<String, Vec<Option<u8>>>> {
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
                let celsius = match &run["celsius"] {
                    serde_json::Value::Null => None,
                    c => Some(c.as_f64().filter(|c| (0.0..=127.0).contains(c))?),
                };
                if start + at != halves.len() || end <= at || end > program::DAY_SLOTS {
                    return None;
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // checked above
                let half = celsius.map(|c| (c * 2.0).round() as u8);
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

    /// The same, with one part as the cloud still holds it on four or six orders.
    fn but_one(setpoints: u8, code: &str) -> Parts {
        let mut parts = module(Some(setpoints));
        parts.insert(code.into(), vec![0x11; ORDERS]);
        parts
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

    /// A week of seven days, Monday given, the others left as they are.
    fn monday(runs: &serde_json::Value) -> String {
        let mut days = vec![serde_json::Value::Null; 7];
        days[0] = runs.clone();
        serde_json::json!({ "days": days }).to_string()
    }

    #[test]
    fn a_module_rewired_to_switch_shows_its_setpoints_at_once() {
        let mut kept = Kept::default();
        kept.wired("fp", true);
        kept.seen("fp", module(Some(34)), None);
        // Rewired to six orders: its parts lose their setpoints.
        assert!(kept.wired("fp", false));
        let six = kept.seen("fp", module(None), None);
        assert!(celsius(&six).iter().all(Option::is_none));
        // Back to switch: 17 °C at once, the module still on orders only.
        assert!(kept.wired("fp", true), "shown again now");
        assert_eq!(celsius(&kept.shown("fp").unwrap()), vec![Some(17.0); 7]);
        assert_eq!(
            celsius(&kept.seen("fp", module(None), None)),
            vec![Some(17.0); 7]
        );
        assert!(!kept.wired("fp", true), "the same wiring heard again");
    }

    #[test]
    fn a_part_the_cloud_never_rewrote_is_shown_and_saved_as_the_others() {
        // As the living room's module: on switch wiring for days, Thursday
        // 12 h – 18 h still without setpoints in the cloud's copy. Saving
        // Monday was refused (« day 4 part 3: this wiring keeps no setpoints »).
        let mut kept = Kept::default();
        let parts = but_one(32, "week_program_18_4_3");
        let week = kept.seen("fp", parts.clone(), Some(true));
        assert_eq!(celsius(&week), vec![Some(16.0); 7], "16 °C all along");
        let sent = kept
            .changes(
                "fp",
                &monday(&serde_json::json!([{ "at": 0, "celsius": 19.0 }])),
                &parts,
                Some(true),
            )
            .unwrap();
        let codes: Vec<&str> = sent.iter().map(|(c, _)| c.as_str()).collect();
        assert_eq!(
            codes,
            [
                "week_program_18_1_1",
                "week_program_18_1_2",
                "week_program_18_1_3",
                "week_program_18_1_4",
                "week_program_18_4_3",
            ],
            "Monday, and Thursday's part the module may not hold"
        );
        assert!(sent[0].1[ORDERS..].iter().all(|h| *h == 38));
        assert_eq!(sent[4].1[..ORDERS], [0x11; ORDERS], "its orders kept");
        assert!(sent[4].1[ORDERS..].iter().all(|h| *h == 32), "as shown");
    }

    #[test]
    fn a_part_never_read_with_setpoints_takes_the_one_before_it() {
        // Monday 0 h – 6 h without setpoints: Sunday evening's 16 °C.
        let mut kept = Kept::default();
        let mut parts = but_one(34, "week_program_18_1_1");
        parts.get_mut("week_program_18_7_4").unwrap()[ORDERS..].fill(32);
        let week = kept.seen("fp", parts, Some(true));
        assert_eq!(
            week["days"][0][0],
            serde_json::json!({ "at": 0, "order": "comfort", "celsius": 16.0 })
        );
        assert_eq!(
            week["days"][0][1],
            serde_json::json!({ "at": 24, "order": "comfort", "celsius": 17.0 })
        );
    }

    #[test]
    fn six_orders_are_shown_and_saved_as_read() {
        // A part still holding setpoints from switch wiring is left as it is.
        let mut kept = Kept::default();
        let mut parts = module(None);
        parts.insert("week_program_18_5_3".into(), vec![0x11; ORDERS + SLOTS]);
        let week = kept.seen("fp", parts.clone(), Some(false));
        assert_eq!(
            week,
            program::week(|d, p| parts.get(&program::part_code(d, p)).cloned())
        );
        let eco = monday(
            &serde_json::json!([{ "at": 0, "order": "eco" }, { "at": 12, "order": "comfort" }]),
        );
        let sent = kept.changes("fp", &eco, &parts, Some(false)).unwrap();
        assert_eq!(sent.len(), 1, "only the part that changes");
        assert_eq!(sent[0].0, "week_program_18_1_1");
    }

    #[test]
    fn a_module_never_read_with_setpoints_waits_for_its_module() {
        let mut kept = Kept::default();
        assert_eq!(kept.shown("fp"), None, "nothing read, nothing shown");
        let week = kept.seen("fp", module(None), Some(true));
        assert!(celsius(&week).iter().all(Option::is_none));
        assert!(kept.weeks.is_empty());
    }

    #[test]
    fn what_the_cloud_says_is_kept() {
        // Smart Life sets 20 °C: read, kept, shown.
        let mut kept = Kept::default();
        kept.seen("fp", module(Some(34)), Some(true));
        kept.seen("fp", module(Some(40)), None);
        kept.wired("fp", false);
        kept.seen("fp", module(None), None);
        kept.wired("fp", true);
        assert_eq!(celsius(&kept.shown("fp").unwrap()), vec![Some(20.0); 7]);
    }

    #[test]
    fn the_driver_says_the_wiring_before_the_cloud() {
        let mut kept = Kept::default();
        kept.wired("fp", true);
        kept.seen("fp", module(Some(34)), None);
        // The cloud's copy still says six orders: the driver's word stands.
        let week = kept.seen("fp", module(None), Some(false));
        assert_eq!(celsius(&week), vec![Some(17.0); 7]);
    }

    #[test]
    fn what_moli_keeps_is_read_back_from_its_file() {
        let dir = std::env::temp_dir().join(format!("moli-kept-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tuya-programs.json");
        let mut kept = Kept::load(path.clone());
        let mut parts = but_one(34, "week_program_18_2_1");
        parts.get_mut("week_program_18_3_1").unwrap()[ORDERS + 6] = 31;
        kept.seen("fp", parts, Some(true));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#"{"at":6,"celsius":15.5}"#), "{text}");
        assert!(text.contains(r#"{"at":0,"celsius":null}"#), "{text}");
        let again = Kept::load(path);
        assert_eq!(again.weeks, kept.weeks);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
