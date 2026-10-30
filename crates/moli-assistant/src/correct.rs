//! « Pas ça »: a person corrects a turn of the house model: puts back what its orders replaced,
//! picks what was meant among what the house can do, and may have Moli keep it as a rule.
//! Everything here is a person's: a script or an agent is refused whatever it claims.

use std::collections::BTreeMap;

use moli_core::{DeviceId, Kind, Origin, Value};
use serde::Serialize;
use serde_json::{Value as Json, json};

use crate::corrections::{
    Rule, RuleAction, RuleCall, Turned, contextual, log_correction, normalize,
};
use crate::house::{self, Layout, name_of, norm};
use crate::{Assistant, Message, Reply, Role, Run, Turn};

/// Why a correction did not go through.
#[derive(Debug)]
pub enum CorrectionError {
    /// A script or an agent tried: only a person corrects, from the dashboard.
    HumanOnly,
    /// No such turn: it is too old, or never was.
    Unknown,
    /// The choice is not one the house offered for this turn.
    NotOffered,
    Failed(String),
}

impl std::fmt::Display for CorrectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HumanOnly => f.write_str("only a person corrects, from the dashboard"),
            Self::Unknown => f.write_str("this turn is not there any more"),
            Self::NotOffered => f.write_str("that choice was not offered"),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for CorrectionError {}

/// What a correction came to.
#[derive(Debug, Serialize)]
pub struct Corrected {
    pub reply: Reply,
    /// The sentence is now a rule of the house.
    pub remembered: bool,
    /// Why it was not kept when the person asked, in words.
    pub note: Option<String>,
}

/// Devices corrected together, at most.
const MAX_TOGETHER: usize = 12;
/// Actions offered for one device at most; steps of a level.
const MAX_ACTIONS: usize = 24;
const MAX_CHOICES: usize = 8;
const STEPS: [f64; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

/// Whether the house offered this very call for the turn (a device's action, a whole room, or another answer).
fn offered_has(offered: &Json, c: &RuleCall) -> bool {
    let all = |key: &str| {
        offered[key]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|d| d["actions"].as_array().into_iter().flatten())
            .cloned()
            .collect::<Vec<_>>()
    };
    let other = offered["other"].as_array().cloned().unwrap_or_default();
    all("devices")
        .into_iter()
        .chain(all("groups"))
        .chain(other)
        .any(|a| a["tool"] == c.tool.as_str() && a["arguments"] == c.arguments)
}

fn human(origin: Origin) -> Result<(), CorrectionError> {
    if origin == Origin::Ui {
        Ok(())
    } else {
        Err(CorrectionError::HumanOnly)
    }
}

/// A number as JSON, whole when it is whole (50, not 50.0): what a client sends back compares equal.
fn number(v: f64) -> Json {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        #[allow(clippy::cast_possible_truncation)]
        let whole = v as i64;
        json!(whole)
    } else {
        json!(v)
    }
}

#[allow(clippy::needless_pass_by_value)] // built inline at each call site, from temporaries
fn action(label: String, tool: &str, arguments: Json) -> Json {
    json!({ "label": label, "tool": tool, "arguments": arguments })
}

/// What a device can be made to do, from its writable points (settings, noise and private values aside).
fn actions_of(view: &moli_runtime::DeviceView) -> Vec<Json> {
    let id = view.device.id.as_str();
    let mut out = Vec::new();
    for p in view.device.points.iter().filter(|p| {
        p.access.write
            && !house::setting(&p.key, p.semantic)
            && !house::noise(&p.key)
            && !house::private(&p.key)
    }) {
        let at = format!("{id}/{}", p.key);
        match &p.kind {
            Kind::Binary => {
                out.push(action(
                    moli_i18n::tr!("assistant.correct.on"),
                    "set",
                    json!({ "point": at, "value": true }),
                ));
                out.push(action(
                    moli_i18n::tr!("assistant.correct.off"),
                    "set",
                    json!({ "point": at, "value": false }),
                ));
            }
            Kind::Enum { values } => {
                for v in values.iter().take(MAX_CHOICES) {
                    out.push(action(
                        moli_i18n::tr!(
                            "assistant.correct.set_value",
                            key = p.label.to_string(),
                            value = v.to_string()
                        ),
                        "set",
                        json!({ "point": at, "value": &**v }),
                    ));
                }
            }
            Kind::Numeric {
                min: Some(min),
                max: Some(max),
                ..
            } => {
                let unit = p
                    .unit
                    .as_ref()
                    .map(|u| format!(" {}", u.symbol()))
                    .unwrap_or_default();
                for f in STEPS {
                    let raw = min + (max - min) * f;
                    let v = if max - min >= 10.0 {
                        raw.round()
                    } else {
                        (raw * 10.0).round() / 10.0
                    };
                    out.push(action(
                        moli_i18n::tr!(
                            "assistant.correct.set_value",
                            key = p.label.to_string(),
                            value = format!("{}{unit}", number(v))
                        ),
                        "set",
                        json!({ "point": at, "value": number(v) }),
                    ));
                }
            }
            _ => {}
        }
    }
    out.truncate(MAX_ACTIONS - 1);
    out.push(action(
        moli_i18n::tr!("assistant.correct.read"),
        "get_device",
        json!({ "device": id }),
    ));
    out
}

/// The call the house model would have made for this rule or choice: real identifiers become the
/// numbers of the current prompt. `None`: it names a device the house no longer has.
pub(crate) fn numbered_call(
    tool: &str,
    arguments: &Json,
    aliases: &BTreeMap<String, DeviceId>,
) -> Option<Json> {
    let number = |real: &str| -> Option<String> {
        let (device, key) = real
            .split_once('/')
            .map_or((real, None), |(d, k)| (d, Some(k)));
        let alias = aliases.iter().find(|(_, id)| id.as_str() == device)?.0;
        Some(key.map_or_else(|| alias.clone(), |k| format!("{alias}/{k}")))
    };
    let mut arguments = arguments.clone();
    for field in ["point", "device"] {
        if let Some(real) = arguments[field].as_str() {
            arguments[field] = Json::String(number(real)?);
        }
    }
    Some(json!({ "function": { "name": tool, "arguments": arguments } }))
}

impl Assistant {
    /// Puts back what the orders of a turn replaced (once). `asker`: who
    /// asks, as the house knows them: what is held is theirs
    /// (`Hub::command_from`).
    pub async fn undo_turn(
        &self,
        id: u64,
        origin: Origin,
        actor: Option<String>,
        asker: Option<&str>,
    ) -> Result<String, CorrectionError> {
        human(origin)?;
        let (_question, undo) = {
            let mut turns = self
                .0
                .turns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let (_, turned) = turns
                .iter_mut()
                .find(|(i, t)| *i == id && t.at.elapsed() < crate::corrections::TURN_LIFETIME)
                .ok_or(CorrectionError::Unknown)?;
            (turned.question.clone(), std::mem::take(&mut turned.undo))
        };
        if undo.is_empty() {
            return Ok(moli_i18n::tr!("assistant.correct.nothing_to_undo"));
        }
        let (mut done, mut held, mut changed) = (0_usize, 0_usize, 0_usize);
        let mut failed = Vec::new();
        // One order per point: the value it had before the first, the value Moli wrote last;
        // the points the turn touched last are put back first.
        let mut points: Vec<crate::corrections::Undo> = Vec::new();
        for order in &undo {
            match points.iter().position(|u| u.point == order.point) {
                Some(i) => {
                    let mut kept = points.remove(i);
                    kept.written.clone_from(&order.written);
                    points.push(kept);
                }
                None => points.push(order.clone()),
            }
        }
        for order in points.iter().rev() {
            // Changed since (by hand, by someone, by an automation): theirs, left as it is.
            let now = order.point.split().and_then(|(d, key)| {
                self.0
                    .hub
                    .device(&d)?
                    .state
                    .get(key)
                    .map(|s| s.value.clone())
            });
            if let Some(now) = now
                && !crate::house_model::same(&now, &order.written)
            {
                changed += 1;
                continue;
            }
            let by = actor.clone().or_else(|| Some("Moli".into()));
            let before = Value::from_json(&order.before);
            let hub = &self.0.hub;
            match hub
                .command_from(&order.point, before, Origin::Assistant, by, asker)
                .await
            {
                Ok(()) => done += 1,
                Err(moli_runtime::CommandError::NeedsApproval { .. }) => held += 1,
                Err(e) => {
                    tracing::warn!(point = %order.point, error = %e, "an order could not be put back");
                    failed.push(order.clone());
                }
            }
        }
        // What could not be put back stays, for another try.
        if !failed.is_empty() {
            failed.reverse();
            let mut turns = self
                .0
                .turns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some((_, turned)) = turns.iter_mut().find(|(i, _)| *i == id) {
                turned.undo.clone_from(&failed);
            }
        }
        self.journal(&json!({
            "kind": "undo", "put_back": done, "held": held, "changed": changed, "failed": failed.len(),
        }));
        let mut said = moli_i18n::tr!("assistant.correct.undone", count = done);
        if changed > 0 {
            said.push(' ');
            said.push_str(&moli_i18n::tr!(
                "assistant.correct.undo_changed",
                count = changed
            ));
        }
        if held > 0 {
            said.push(' ');
            said.push_str(&moli_i18n::tr!("assistant.house.all_held", count = held));
        }
        if !failed.is_empty() {
            said.push(' ');
            said.push_str(&moli_i18n::tr!(
                "assistant.correct.undo_failed",
                count = failed.len()
            ));
        }
        Ok(said)
    }

    /// What the person may have meant, for a turn: the rooms, the devices with what each can do, and a few
    /// other answers. Every correction is one of these (nothing else is carried out).
    pub async fn choices(&self, id: u64) -> Result<Json, CorrectionError> {
        let turned = self.turn_of(id).ok_or(CorrectionError::Unknown)?;
        let layout = Layout::load(self.0.layout_path.as_deref()).await;
        Ok(self.choices_for(&turned, &layout))
    }

    fn choices_for(&self, turned: &Turned, layout: &Layout) -> Json {
        let snapshot = self.0.hub.snapshot();
        let said = self.rooms_said(layout, &turned.question);
        let mut rooms: Vec<String> = Vec::new();
        let mut named = None;
        let mut devices = Vec::new();
        for view in snapshot
            .devices
            .iter()
            .filter(|v| !layout.hides(v.device.id.as_str()))
        {
            let room = layout.room_of(view);
            if !rooms.contains(&room) {
                rooms.push(room.clone());
            }
            if named.is_none() && said.contains(&norm(&room)) {
                named = Some(room.clone());
            }
            devices.push(json!({ "id": view.device.id.as_str(), "name": name_of(view), "room": room, "actions": actions_of(view) }));
        }
        rooms.sort();
        let groups = self.room_groups(&rooms, layout);
        json!({
            "remember_allowed": !contextual(&turned.question),
            "room": named,
            "rooms": rooms,
            "devices": devices,
            "groups": groups,
            "other": [
                action(moli_i18n::tr!("assistant.correct.now"), "now", json!({})),
                action(moli_i18n::tr!("assistant.correct.weather"), "weather", json!({ "day": 1 })),
                action(moli_i18n::tr!("assistant.correct.nothing"), "nothing", json!({})),
            ],
        })
    }

    /// For each room, what can be done to all of a kind at once: the lights (on, off, a colour), the shutters.
    /// A rule kept this way follows the room (a new light is part of it).
    fn room_groups(&self, rooms: &[String], layout: &Layout) -> Vec<Json> {
        use crate::house_tools::{Family, family_of};
        const COLOURS: [&str; 8] = [
            "rouge",
            "bleu",
            "vert",
            "jaune",
            "orange",
            "violet",
            "rose",
            "blanc chaud",
        ];
        let snapshot = self.0.hub.snapshot();
        rooms
            .iter()
            .map(|room| {
                let here: Vec<_> = snapshot
                    .devices
                    .iter()
                    .filter(|v| !layout.hides(v.device.id.as_str()) && layout.room_of(v) == *room)
                    .collect();
                let kind = |f: Family| here.iter().any(|v| family_of(v) == f);
                let colour = here.iter().any(|v| {
                    family_of(v) == Family::Light
                        && v.device
                            .points
                            .iter()
                            .any(|p| p.access.write && p.semantic == moli_core::Semantic::Color)
                });
                let all =
                    |kind: &str, value: Json| json!({ "kind": kind, "room": room, "value": value });
                let mut actions = Vec::new();
                if kind(Family::Light) {
                    for (on, key) in [
                        (true, "assistant.correct.room_lights_on"),
                        (false, "assistant.correct.room_lights_off"),
                    ] {
                        actions.push(action(
                            moli_i18n::tr(key),
                            "set_all",
                            all("lumières", json!(on)),
                        ));
                    }
                }
                if colour {
                    for c in COLOURS {
                        actions.push(action(
                            moli_i18n::tr!("assistant.correct.room_colour", colour = c.to_owned()),
                            "ambiance",
                            json!({ "room": room, "color": c }),
                        ));
                    }
                }
                if kind(Family::Cover) {
                    for (v, key) in [
                        ("open", "assistant.correct.room_covers_open"),
                        ("close", "assistant.correct.room_covers_close"),
                    ] {
                        actions.push(action(
                            moli_i18n::tr(key),
                            "set_all",
                            all("volets", json!(v)),
                        ));
                    }
                }
                json!({ "room": room, "actions": actions })
            })
            .filter(|g| !g["actions"].as_array().is_some_and(Vec::is_empty))
            .collect()
    }

    /// Carries out what was meant instead, and keeps it (journal; a rule too when asked, unless the
    /// sentence only makes sense with what came before). `asker`: who corrects, as the house knows
    /// them: what is held is theirs (`Hub::command_from`).
    pub async fn correct(
        &self,
        id: u64,
        choice: Json,
        remember: bool,
        origin: Origin,
        actor: Option<String>,
        asker: Option<&str>,
    ) -> Result<Corrected, CorrectionError> {
        self.correct_many(id, vec![choice], remember, origin, actor, asker)
            .await
    }

    /// Several devices that share a command, corrected together: every choice must be one the house
    /// offered, and none is sent unless all are.
    pub async fn correct_many(
        &self,
        id: u64,
        choices: Vec<Json>,
        remember: bool,
        origin: Origin,
        actor: Option<String>,
        asker: Option<&str>,
    ) -> Result<Corrected, CorrectionError> {
        human(origin)?;
        let turned = self.turn_of(id).ok_or(CorrectionError::Unknown)?;
        let layout = Layout::load(self.0.layout_path.as_deref()).await;
        let offered = self.choices_for(&turned, &layout);
        let calls: Vec<RuleCall> = choices
            .iter()
            .map(|c| RuleCall {
                tool: c["tool"].as_str().unwrap_or_default().to_owned(),
                arguments: c["arguments"].clone(),
            })
            .collect();
        if calls.is_empty()
            || calls.len() > MAX_TOGETHER
            || calls
                .iter()
                .any(|c| c.tool.is_empty() || !offered_has(&offered, c))
            || (calls.len() > 1 && calls.iter().any(|c| c.tool == "nothing"))
        {
            return Err(CorrectionError::NotOffered);
        }
        let (_, aliases) = self
            .house_prompt(&layout)
            .map_err(|e| CorrectionError::Failed(e.to_string()))?;
        let numbered: Vec<Option<Json>> = calls
            .iter()
            .map(|c| {
                if c.tool == "nothing" {
                    Some(Json::Null)
                } else {
                    numbered_call(&c.tool, &c.arguments, &aliases)
                }
            })
            .collect();
        if numbered.iter().any(Option::is_none) {
            return Err(CorrectionError::Failed(
                "this device is not there any more".into(),
            ));
        }
        let mut run = Run {
            asker: asker.unwrap_or_default().to_owned(),
            ..Run::default()
        };
        let mut said = Vec::new();
        for (c, call) in calls.iter().zip(numbered.into_iter().flatten()) {
            let words = if c.tool == "nothing" {
                moli_i18n::tr!("assistant.house.rule_nothing")
            } else {
                // The person picked it: what the sentence said (a delay, another room) no longer decides.
                let question = if c.tool == "get_device" {
                    turned.question.as_str()
                } else {
                    ""
                };
                self.house_call(&call, &layout, false, &aliases, question, &mut run)
                    .await
            };
            if !words.is_empty() {
                said.push(words);
            }
        }
        let reply = self.finish(&said.join(" "), run, &layout, "");
        let turn = Turn {
            messages: vec![Message {
                role: Role::User,
                content: turned.question.clone(),
            }],
            surface: None,
            spoken: false,
            id: None,
            asker: None,
        };
        let reply = self.seal(reply, &turn, &turned.question);
        let note = if remember {
            self.not_kept(&layout, &turned.question, &calls)
        } else {
            None
        };
        let action = match calls.as_slice() {
            [one] if one.tool == "nothing" => RuleAction::Nothing,
            [one] => RuleAction::Call {
                tool: one.tool.clone(),
                arguments: one.arguments.clone(),
            },
            _ => RuleAction::Many {
                calls: calls.clone(),
            },
        };
        let remembered = remember
            && !contextual(&turned.question)
            && note.is_none()
            && self.keep_rule(&turned.question, action, actor.clone());
        self.journal_correction(&turned, &calls, remembered, actor.as_deref());
        Ok(Corrected {
            reply,
            remembered,
            note,
        })
    }

    /// A correction in the journal: what the next training learns from.
    fn journal_correction(
        &self,
        turned: &Turned,
        calls: &[RuleCall],
        remembered: bool,
        by: Option<&str>,
    ) {
        self.journal(&json!({
            "kind": "correct",
            "at": moli_core::now_ms(),
            "question": turned.question,
            "context": turned.context,
            "choice": { "tool": calls[0].tool, "arguments": calls[0].arguments },
            "choices": calls,
            "remembered": remembered,
            "by": by,
        }));
    }

    /// Why a correction is not kept as a rule, in words (`None`: it can be, or the sentence leans on what came
    /// before and the sheet already says so). Kept only when it will not fire for something else: not a
    /// sentence that names no device yet picks a single device (« éteins la cuisine » for one ribbon of two:
    /// the whole room, or several devices, is the way); not a level picked from steps for a « baisse ».
    fn not_kept(&self, layout: &Layout, question: &str, calls: &[RuleCall]) -> Option<String> {
        if contextual(question) {
            return None;
        }
        let one_device = calls.len() == 1
            && (calls[0].arguments["point"].is_string()
                || calls[0].arguments["device"].is_string());
        let level = calls
            .iter()
            .any(|c| c.tool == "set" && c.arguments["value"].is_number())
            && !question.chars().any(|c| c.is_ascii_digit());
        if one_device && !self.names_device(layout, question) {
            Some(moli_i18n::tr!("assistant.correct.not_kept_unnamed"))
        } else if level {
            Some(moli_i18n::tr!("assistant.correct.not_kept_level"))
        } else {
            None
        }
    }

    /// Teaches Moli a sentence; `false` when it could not be written down.
    fn keep_rule(&self, said: &str, action: RuleAction, by: Option<String>) -> bool {
        let mut rules = self
            .0
            .rules
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let backup = rules.snapshot();
        rules.upsert(Rule {
            id: 0,
            said: said.to_owned(),
            key: normalize(said),
            action,
            created: moli_core::now_ms(),
            by,
            enabled: true,
        });
        match rules.save() {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "the rules could not be written down");
                rules.restore(backup);
                false
            }
        }
    }

    /// One line in the corrections journal (next to `home.json`); a full or missing folder is only a warning.
    fn journal(&self, line: &Json) {
        let Some(dir) = self
            .0
            .layout_path
            .as_deref()
            .and_then(std::path::Path::parent)
        else {
            return;
        };
        if let Err(e) = log_correction(dir, line) {
            tracing::warn!(error = %e, "a correction could not be journaled");
        }
    }

    /// The rules for the screen: what was said, what it does in words, and whether its device is still there.
    #[must_use]
    pub fn rules_view(&self) -> Json {
        let rules = self
            .0
            .rules
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Json::Array(
            rules
                .list()
                .iter()
                .map(|r| {
                    let (words, orphan) = self.describe(&r.action);
                    json!({ "id": r.id, "said": r.said, "enabled": r.enabled, "created": r.created, "by": r.by, "action": r.action, "words": words, "orphan": orphan })
                })
                .collect(),
        )
    }

    fn describe(&self, action: &RuleAction) -> (String, bool) {
        let (tool, arguments) = match action {
            RuleAction::Call { tool, arguments } => (tool, arguments),
            RuleAction::Many { calls } => {
                let parts: Vec<(String, bool)> = calls
                    .iter()
                    .map(|c| {
                        self.describe(&RuleAction::Call {
                            tool: c.tool.clone(),
                            arguments: c.arguments.clone(),
                        })
                    })
                    .collect();
                let orphan = parts.iter().any(|(_, gone)| *gone);
                let words = parts
                    .into_iter()
                    .map(|(w, _)| w)
                    .collect::<Vec<_>>()
                    .join(" ; ");
                return (words, orphan);
            }
            RuleAction::Nothing => return (moli_i18n::tr!("assistant.correct.nothing"), false),
        };
        let device = |real: &str| {
            self.0
                .hub
                .device(&DeviceId::from(real.split('/').next().unwrap_or(real)))
        };
        let target = arguments["point"]
            .as_str()
            .or_else(|| arguments["device"].as_str());
        let Some(real) = target else {
            return (tool.clone(), false);
        };
        let Some(view) = device(real) else {
            return (tool.clone(), true);
        };
        let key = real.split_once('/').map(|(_, k)| k);
        let gone = key.is_some_and(|k| !view.device.points.iter().any(|p| &*p.key == k));
        let words = match (tool.as_str(), key) {
            ("set", Some(k)) => moli_i18n::tr!(
                "assistant.correct.rule_set",
                device = name_of(&view).to_owned(),
                key = k.to_owned(),
                value = arguments["value"].to_string().trim_matches('"').to_owned()
            ),
            ("get_device", _) => moli_i18n::tr!(
                "assistant.correct.rule_read",
                device = name_of(&view).to_owned()
            ),
            _ => tool.clone(),
        };
        (words, gone)
    }

    pub fn set_rule_enabled(
        &self,
        id: u64,
        enabled: bool,
        origin: Origin,
    ) -> Result<(), CorrectionError> {
        human(origin)?;
        let mut rules = self
            .0
            .rules
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let backup = rules.snapshot();
        if !rules.set_enabled(id, enabled) {
            return Err(CorrectionError::Unknown);
        }
        rules.save().map_err(|e| {
            rules.restore(backup);
            CorrectionError::Failed(e.to_string())
        })
    }

    pub fn delete_rule(&self, id: u64, origin: Origin) -> Result<(), CorrectionError> {
        human(origin)?;
        let mut rules = self
            .0
            .rules
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let backup = rules.snapshot();
        if !rules.remove(id) {
            return Err(CorrectionError::Unknown);
        }
        rules.save().map_err(|e| {
            rules.restore(backup);
            CorrectionError::Failed(e.to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use moli_core::{Kind, Origin, Semantic, Value};
    use serde_json::{Value as Json, json};

    use crate::CorrectionError;
    use crate::corrections::RuleAction;
    use crate::house_model::tests::{ask, call, house_model_over, with_layout, words};
    use crate::tests::{Fake, hub_with, point};

    async fn lights() -> moli_runtime::Hub {
        let state = point("on", Kind::Binary, true);
        let mut level = point(
            "brightness",
            Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: None,
            },
            true,
        );
        level.semantic = Semantic::Brightness;
        let mut delay = point(
            "off_on_duration",
            Kind::Numeric {
                min: Some(0.0),
                max: Some(10.0),
                step: None,
            },
            true,
        );
        delay.semantic = Semantic::Config;
        hub_with(Fake(vec![(
            "lamp",
            vec![state, level, delay],
            vec![
                ("on", Value::Bool(false)),
                ("brightness", Value::Float(40.0)),
                ("off_on_duration", Value::Float(0.0)),
            ],
        )]))
        .await
    }

    fn offered<'a>(choices: &'a Json, tool: &str, arguments: &Json) -> Option<&'a Json> {
        let all = choices["devices"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|d| d["actions"].as_array().into_iter().flatten())
            .chain(choices["other"].as_array().into_iter().flatten());
        all.into_iter()
            .find(|a| a["tool"] == tool && &a["arguments"] == arguments)
    }

    #[tokio::test]
    async fn undoing_a_turn_puts_back_what_its_orders_replaced() {
        let answer = call("set", r#"{"point":"d1/on","value":true}"#);
        let (moli, _) = house_model_over(crate::tests::lamp_hub().await, vec![&answer]).await;
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        let on = || {
            moli.0
                .hub
                .device(&"fake:lamp".into())
                .unwrap()
                .state
                .get("on")
                .map(|s| s.value.clone())
        };
        assert_eq!(on(), Some(Value::Bool(true)));
        let said = moli
            .undo_turn(reply.turn_id, Origin::Ui, None, None)
            .await
            .unwrap();
        assert_eq!(on(), Some(Value::Bool(false)));
        assert_eq!(said, moli_i18n::tr!("assistant.correct.undone", count = 1));
        // once: the turn has nothing left to put back
        let again = moli
            .undo_turn(reply.turn_id, Origin::Ui, None, None)
            .await
            .unwrap();
        assert_eq!(again, moli_i18n::tr!("assistant.correct.nothing_to_undo"));
    }

    #[tokio::test]
    async fn only_a_person_corrects_and_only_a_known_turn() {
        let answer = call("set", r#"{"point":"d1/on","value":true}"#);
        let (moli, _) = house_model_over(crate::tests::lamp_hub().await, vec![&answer]).await;
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        for origin in [
            Origin::Household,
            Origin::Api,
            Origin::Mcp,
            Origin::Assistant,
        ] {
            assert!(
                matches!(
                    moli.undo_turn(reply.turn_id, origin, None, None).await,
                    Err(CorrectionError::HumanOnly)
                ),
                "{origin:?}"
            );
            assert!(matches!(
                moli.correct(reply.turn_id, json!({}), true, origin, None, None)
                    .await,
                Err(CorrectionError::HumanOnly)
            ));
            assert!(matches!(
                moli.set_rule_enabled(1, false, origin),
                Err(CorrectionError::HumanOnly)
            ));
            assert!(matches!(
                moli.delete_rule(1, origin),
                Err(CorrectionError::HumanOnly)
            ));
        }
        assert!(matches!(
            moli.undo_turn(12345, Origin::Ui, None, None).await,
            Err(CorrectionError::Unknown)
        ));
        assert!(matches!(
            moli.choices(12345).await,
            Err(CorrectionError::Unknown)
        ));
    }

    #[tokio::test]
    async fn the_choices_are_the_things_a_device_can_do_and_never_its_settings() {
        let answer = words("Je ne sais pas.");
        let (moli, _) = house_model_over(lights().await, vec![&answer]).await;
        let reply = moli.turn(ask("Fais un truc")).await.unwrap();
        let choices = moli.choices(reply.turn_id).await.unwrap();
        assert_eq!(choices["devices"].as_array().unwrap().len(), 1);
        assert!(
            offered(
                &choices,
                "set",
                &json!({ "point": "fake:lamp/on", "value": true })
            )
            .is_some()
        );
        assert!(
            offered(
                &choices,
                "set",
                &json!({ "point": "fake:lamp/on", "value": false })
            )
            .is_some()
        );
        assert!(
            offered(
                &choices,
                "set",
                &json!({ "point": "fake:lamp/brightness", "value": 50 })
            )
            .is_some()
        );
        assert!(offered(&choices, "get_device", &json!({ "device": "fake:lamp" })).is_some());
        assert!(offered(&choices, "now", &json!({})).is_some());
        assert!(offered(&choices, "nothing", &json!({})).is_some());
        assert!(
            !choices.to_string().contains("off_on_duration"),
            "{choices}"
        );
        assert_eq!(choices["remember_allowed"], true);
    }

    #[tokio::test]
    async fn a_choice_the_house_did_not_offer_is_refused() {
        let answer = words("Je ne sais pas.");
        let (moli, _) = house_model_over(lights().await, vec![&answer]).await;
        let reply = moli.turn(ask("Fais un truc")).await.unwrap();
        for choice in [
            json!({ "tool": "set", "arguments": { "point": "fake:lamp/off_on_duration", "value": 10 } }),
            json!({ "tool": "set", "arguments": { "point": "fake:lamp/on", "value": "boom" } }),
            json!({ "tool": "add_person", "arguments": { "name": "Intrus" } }),
            json!({}),
        ] {
            let done = moli
                .correct(reply.turn_id, choice.clone(), true, Origin::Ui, None, None)
                .await;
            assert!(matches!(done, Err(CorrectionError::NotOffered)), "{choice}");
        }
        assert!(moli.0.rules.read().unwrap().list().is_empty());
    }

    #[tokio::test]
    async fn a_correction_acts_is_journaled_and_kept_as_a_rule_the_model_is_not_asked_again() {
        let answer = words("Je ne sais pas.");
        let (moli, path) = with_layout(lights().await, "{}", vec![&answer]).await;
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        let choice =
            json!({ "tool": "set", "arguments": { "point": "fake:lamp/on", "value": true } });
        let done = moli
            .correct(
                reply.turn_id,
                choice,
                true,
                Origin::Ui,
                Some("test".into()),
                None,
            )
            .await
            .unwrap();
        assert!(done.remembered);
        assert_eq!(done.reply.actions[0].point.as_str(), "fake:lamp/on");
        assert_eq!(done.reply.actions[0].status, "done");
        let journal =
            std::fs::read_to_string(path.with_file_name("assistant-corrections.jsonl")).unwrap();
        let line: Json = serde_json::from_str(journal.lines().next().unwrap()).unwrap();
        assert_eq!(line["question"], "Allume la lampe");
        assert_eq!(line["remembered"], true);
        assert_eq!(line["choice"]["arguments"]["point"], "fake:lamp/on");
        // the rule is on disk, and answers next time without asking the model (no answer is left to give)
        assert!(path.with_file_name("assistant-rules.json").exists());
        let second = moli.turn(ask("allume la lampe")).await.unwrap();
        assert!(second.rule.is_some());
        assert_eq!(
            moli.0.rules.read().unwrap().list()[0].action,
            RuleAction::Call {
                tool: "set".into(),
                arguments: json!({ "point": "fake:lamp/on", "value": true }),
            }
        );
    }

    #[tokio::test]
    async fn a_sentence_that_leans_on_the_context_is_corrected_but_never_kept() {
        let answer = words("Je ne sais pas.");
        let (moli, path) = with_layout(lights().await, "{}", vec![&answer]).await;
        let reply = moli.turn(ask("Éteins-la")).await.unwrap();
        let choices = moli.choices(reply.turn_id).await.unwrap();
        assert_eq!(choices["remember_allowed"], false);
        let choice =
            json!({ "tool": "set", "arguments": { "point": "fake:lamp/on", "value": false } });
        let done = moli
            .correct(reply.turn_id, choice, true, Origin::Ui, None, None)
            .await
            .unwrap();
        assert!(!done.remembered);
        assert!(moli.0.rules.read().unwrap().list().is_empty());
        let line =
            std::fs::read_to_string(path.with_file_name("assistant-corrections.jsonl")).unwrap();
        assert!(
            line.contains("Éteins-la") && line.contains("\"remembered\":false"),
            "{line}"
        );
    }

    #[tokio::test]
    async fn the_rules_can_be_listed_switched_off_and_deleted_by_a_person() {
        let answer = words("Je ne sais pas.");
        let (moli, _) = with_layout(lights().await, "{}", vec![&answer]).await;
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        let choice =
            json!({ "tool": "set", "arguments": { "point": "fake:lamp/on", "value": true } });
        moli.correct(reply.turn_id, choice, true, Origin::Ui, None, None)
            .await
            .unwrap();
        let listed = moli.rules_view();
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["said"], "Allume la lampe");
        assert_eq!(listed[0]["orphan"], false);
        let id = listed[0]["id"].as_u64().unwrap();
        moli.set_rule_enabled(id, false, Origin::Ui).unwrap();
        assert_eq!(moli.rules_view()[0]["enabled"], false);
        moli.delete_rule(id, Origin::Ui).unwrap();
        assert!(moli.rules_view().as_array().unwrap().is_empty());
        assert!(matches!(
            moli.delete_rule(id, Origin::Ui),
            Err(CorrectionError::Unknown)
        ));
    }

    #[tokio::test]
    async fn orders_are_put_back_last_first_and_a_failure_is_said_and_kept() {
        let two = crate::house_model::tests::calls(&[
            ("set", r#"{"point":"d1/on","value":true}"#),
            ("set", r#"{"point":"d1/on","value":false}"#),
        ]);
        let once = call("set", r#"{"point":"d1/on","value":true}"#);
        let (moli, _) =
            house_model_over(crate::tests::lamp_hub().await, vec![&two, &once, &once]).await;
        let on = || {
            moli.0
                .hub
                .device(&"fake:lamp".into())
                .unwrap()
                .state
                .get("on")
                .map(|s| s.value.clone())
        };
        let set = |v: bool| {
            let hub = moli.0.hub.clone();
            async move {
                hub.command(&"fake:lamp/on".into(), Value::Bool(v), Origin::Ui, None)
                    .await
                    .unwrap();
            }
        };
        // The lamp starts off. Two orders on one point (on, then off): one order put back, to
        // the value before the first.
        let reply = moli.turn(ask("Allume puis éteins")).await.unwrap();
        let said = moli
            .undo_turn(reply.turn_id, Origin::Ui, None, None)
            .await
            .unwrap();
        assert_eq!(said, moli_i18n::tr!("assistant.correct.undone", count = 1));
        assert_eq!(on(), Some(Value::Bool(false)));
        // Untouched since Moli lit it: put back off.
        let reply = moli.turn(ask("Allume")).await.unwrap();
        assert_eq!(on(), Some(Value::Bool(true)));
        let said = moli
            .undo_turn(reply.turn_id, Origin::Ui, None, None)
            .await
            .unwrap();
        assert_eq!(said, moli_i18n::tr!("assistant.correct.undone", count = 1));
        assert_eq!(on(), Some(Value::Bool(false)));
        // Moli lights it, then a person switches it off by hand: no longer what Moli wrote,
        // so « Défaire » leaves it (nothing sent) and says so.
        let reply = moli.turn(ask("Allume")).await.unwrap();
        set(false).await;
        let sent_before = moli.0.hub.journal(100).len();
        let said = moli
            .undo_turn(reply.turn_id, Origin::Ui, None, None)
            .await
            .unwrap();
        assert_eq!(
            moli.0.hub.journal(100).len(),
            sent_before,
            "nothing was sent"
        );
        assert_eq!(
            said,
            moli_i18n::tr!("assistant.correct.undone", count = 0)
                + " "
                + &moli_i18n::tr!("assistant.correct.undo_changed", count = 1)
        );

        let stuck = moli.remember_turn(crate::corrections::Turned {
            question: "x".into(),
            context: Vec::new(),
            undo: vec![crate::corrections::Undo {
                point: "fake:ghost/on".into(),
                before: json!(true),
                written: json!(false),
            }],
            at: std::time::Instant::now(),
        });
        let said = moli.undo_turn(stuck, Origin::Ui, None, None).await.unwrap();
        assert_eq!(
            said,
            moli_i18n::tr!("assistant.correct.undone", count = 0)
                + " "
                + &moli_i18n::tr!("assistant.correct.undo_failed", count = 1)
        );
        let again = moli.undo_turn(stuck, Origin::Ui, None, None).await.unwrap();
        assert_ne!(
            again,
            moli_i18n::tr!("assistant.correct.nothing_to_undo"),
            "the failed order is kept for another try"
        );
    }

    #[tokio::test]
    async fn a_sentence_naming_nothing_is_not_kept_for_a_chosen_device() {
        let answer = words("Je ne sais pas.");
        let (moli, _) = with_layout(lights().await, "{}", vec![&answer, &answer, &answer]).await;
        let reply = moli.turn(ask("Éteins la lumière")).await.unwrap();
        let off =
            json!({ "tool": "set", "arguments": { "point": "fake:lamp/on", "value": false } });
        let done = moli
            .correct(reply.turn_id, off.clone(), true, Origin::Ui, None, None)
            .await
            .unwrap();
        assert!(!done.remembered);
        assert_eq!(
            done.note.as_deref(),
            Some(moli_i18n::tr!("assistant.correct.not_kept_unnamed").as_str())
        );
        assert!(moli.0.rules.read().unwrap().list().is_empty());
        // naming the device (« lampe » is « lamp » by its stem), or asking for the time, is kept
        let reply = moli.turn(ask("Éteins la lampe")).await.unwrap();
        assert!(
            moli.correct(reply.turn_id, off, true, Origin::Ui, None, None)
                .await
                .unwrap()
                .remembered
        );
        let reply = moli.turn(ask("Il est quelle heure")).await.unwrap();
        let now = json!({ "tool": "now", "arguments": {} });
        assert!(
            moli.correct(reply.turn_id, now, true, Origin::Ui, None, None)
                .await
                .unwrap()
                .remembered
        );
    }

    #[tokio::test]
    async fn a_level_picked_from_steps_for_a_sentence_without_a_number_is_not_kept() {
        let answer = words("Je ne sais pas.");
        let (moli, _) = with_layout(lights().await, "{}", vec![&answer]).await;
        let reply = moli.turn(ask("Baisse la lampe")).await.unwrap();
        let level =
            json!({ "tool": "set", "arguments": { "point": "fake:lamp/brightness", "value": 25 } });
        let done = moli
            .correct(reply.turn_id, level, true, Origin::Ui, None, None)
            .await
            .unwrap();
        assert!(
            !done.remembered,
            "« baisse » is relative: 25 % forever would be wrong"
        );
        assert!(moli.0.rules.read().unwrap().list().is_empty());
    }

    #[tokio::test]
    async fn a_failed_write_of_the_rules_keeps_nothing_in_memory() {
        let answer = words("Je ne sais pas.");
        let (moli, path) = with_layout(lights().await, "{}", vec![&answer]).await;
        std::fs::create_dir(path.with_file_name("assistant-rules.json")).unwrap();
        *moli.0.rules.write().unwrap() =
            crate::corrections::Rules::load(&path.with_file_name("assistant-rules.json"));
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        let on = json!({ "tool": "set", "arguments": { "point": "fake:lamp/on", "value": true } });
        assert!(
            !moli
                .correct(reply.turn_id, on, true, Origin::Ui, None, None)
                .await
                .unwrap()
                .remembered
        );
        assert!(moli.0.rules.read().unwrap().list().is_empty());
    }

    async fn kitchen() -> moli_runtime::Hub {
        let ribbon = || {
            let mut state = point("state", Kind::Binary, true);
            state.semantic = Semantic::OnOff;
            let mut color = point("color", Kind::Text, true);
            color.semantic = Semantic::Color;
            vec![state, color]
        };
        let values = || {
            vec![
                ("state", Value::Bool(false)),
                ("color", Value::Text("#ffffff".into())),
            ]
        };
        let hub = hub_with(Fake(vec![
            ("Ruban1", ribbon(), values()),
            ("Ruban2", ribbon(), values()),
        ]))
        .await;
        for name in ["Ruban1", "Ruban2"] {
            let room = moli_runtime::LabelPatch {
                name: Some(format!("Ruban led {}", &name[5..])),
                room: Some("Cuisine".into()),
            };
            hub.set_label(
                &format!("fake:{name}").as_str().into(),
                room,
                Origin::Ui,
                None,
            )
            .await
            .unwrap();
        }
        hub
    }

    fn state_of(moli: &crate::Assistant, name: &str) -> Option<Value> {
        moli.0
            .hub
            .device(&format!("fake:{name}").as_str().into())
            .unwrap()
            .state
            .get("state")
            .map(|s| s.value.clone())
    }

    #[tokio::test]
    async fn a_whole_room_is_offered_at_once_for_lights_colours_and_covers() {
        let answer = words("Je ne sais pas.");
        let (moli, _) = house_model_over(kitchen().await, vec![&answer]).await;
        let reply = moli.turn(ask("Allume la cuisine en rouge")).await.unwrap();
        let choices = moli.choices(reply.turn_id).await.unwrap();
        let group = choices["groups"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["room"] == "Cuisine")
            .expect("the kitchen");
        let has = |tool: &str, arguments: Json| {
            group["actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["tool"] == tool && a["arguments"] == arguments)
        };
        assert!(has(
            "set_all",
            json!({ "kind": "lumières", "room": "Cuisine", "value": false })
        ));
        assert!(has(
            "set_all",
            json!({ "kind": "lumières", "room": "Cuisine", "value": true })
        ));
        assert!(has(
            "ambiance",
            json!({ "room": "Cuisine", "color": "rouge" })
        ));
        assert!(
            !has(
                "set_all",
                json!({ "kind": "volets", "room": "Cuisine", "value": "open" })
            ),
            "no shutter here"
        );
    }

    #[tokio::test]
    async fn several_devices_are_corrected_in_one_go_all_or_nothing() {
        let answer = words("Je ne sais pas.");
        let (moli, _) = house_model_over(kitchen().await, vec![&answer]).await;
        let reply = moli.turn(ask("Allume les rubans")).await.unwrap();
        let on = |name: &str| json!({ "tool": "set", "arguments": { "point": format!("fake:{name}/state"), "value": true } });
        let bad = json!({ "tool": "set", "arguments": { "point": "fake:Ruban2/state", "value": "boom" } });
        let refused = moli
            .correct_many(
                reply.turn_id,
                vec![on("Ruban1"), bad],
                false,
                Origin::Ui,
                None,
                None,
            )
            .await;
        assert!(matches!(refused, Err(CorrectionError::NotOffered)));
        assert_eq!(
            state_of(&moli, "Ruban1"),
            Some(Value::Bool(false)),
            "nothing was sent"
        );
        let done = moli
            .correct_many(
                reply.turn_id,
                vec![on("Ruban1"), on("Ruban2")],
                false,
                Origin::Ui,
                None,
                None,
            )
            .await
            .unwrap();
        assert_eq!(done.reply.actions.len(), 2);
        assert_eq!(state_of(&moli, "Ruban1"), Some(Value::Bool(true)));
        assert_eq!(state_of(&moli, "Ruban2"), Some(Value::Bool(true)));
    }

    #[tokio::test]
    async fn several_devices_kept_as_one_rule_are_all_done_the_next_time() {
        let answer = words("Je ne sais pas.");
        let (moli, _) = with_layout(kitchen().await, "{}", vec![&answer]).await;
        let reply = moli
            .turn(ask("Allume les rubans de la cuisine"))
            .await
            .unwrap();
        let on = |name: &str| json!({ "tool": "set", "arguments": { "point": format!("fake:{name}/state"), "value": true } });
        let done = moli
            .correct_many(
                reply.turn_id,
                vec![on("Ruban1"), on("Ruban2")],
                true,
                Origin::Ui,
                None,
                None,
            )
            .await
            .unwrap();
        assert!(done.remembered, "{:?}", done.note);
        moli.0
            .hub
            .command(
                &"fake:Ruban1/state".into(),
                Value::Bool(false),
                Origin::Ui,
                None,
            )
            .await
            .unwrap();
        moli.0
            .hub
            .command(
                &"fake:Ruban2/state".into(),
                Value::Bool(false),
                Origin::Ui,
                None,
            )
            .await
            .unwrap();
        let again = moli
            .turn(ask("allume les rubans de la cuisine"))
            .await
            .unwrap();
        assert!(again.rule.is_some());
        assert_eq!(again.actions.len(), 2);
        assert_eq!(state_of(&moli, "Ruban2"), Some(Value::Bool(true)));
    }

    #[tokio::test]
    async fn a_group_choice_is_done_and_kept_for_a_sentence_that_only_names_the_room() {
        let answer = words("Je ne sais pas.");
        let (moli, _) = with_layout(kitchen().await, "{}", vec![&answer, &answer]).await;
        let reply = moli.turn(ask("Éteins la cuisine")).await.unwrap();
        // one ribbon of two for a sentence that names the room only: not kept (it would turn off one of two)
        let one =
            json!({ "tool": "set", "arguments": { "point": "fake:Ruban1/state", "value": false } });
        let done = moli
            .correct(reply.turn_id, one, true, Origin::Ui, None, None)
            .await
            .unwrap();
        assert!(!done.remembered);
        assert_eq!(
            done.note.as_deref(),
            Some(moli_i18n::tr!("assistant.correct.not_kept_unnamed").as_str())
        );
        // the whole room: kept, and it adapts to the lights the room has
        let reply = moli.turn(ask("Éteins la cuisine")).await.unwrap();
        let all = json!({ "tool": "set_all", "arguments": { "kind": "lumières", "room": "Cuisine", "value": false } });
        assert!(
            moli.correct(reply.turn_id, all, true, Origin::Ui, None, None)
                .await
                .unwrap()
                .remembered
        );
    }
}
