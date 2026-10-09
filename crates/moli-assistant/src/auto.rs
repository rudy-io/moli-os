//! Moli and the automations: drafting a graph from words (or reworking
//! one), and writing the texts « Moli écrit » nodes ask for.
//!
//! Drafts are only drafts: a person approves them before anything runs.
//! Writing is words only: the engine never lets a model decide.

use moli_automation::{Automation, Automations, Graph, Mode, Node};
use moli_runtime::BoxFuture;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::house::{self, Layout};
use crate::{Assistant, AssistantError, is_reasoning};

/// What Moli proposes.
#[derive(Clone, Debug)]
pub struct Draft {
    pub name: String,
    pub mode: Mode,
    pub graph: Graph,
    /// What Moli could not do as asked, in words.
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Flat {
    name: String,
    mode: Mode,
    note: Option<String>,
    nodes: Vec<Json>,
    edges: Vec<moli_automation::Edge>,
}

fn nullable(t: &str) -> Json {
    json!({ "type": [t, "null"] })
}

fn any_value() -> Json {
    json!({ "type": ["string", "number", "boolean", "null"] })
}

fn rule_schema() -> Json {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["kind", "point", "op", "value", "after", "before", "days", "is"],
        "properties": {
            "kind": { "type": "string", "enum": ["state", "time", "sun"] },
            "point": nullable("string"),
            "op": { "type": ["string", "null"], "enum": ["eq", "ne", "gt", "ge", "lt", "le", null] },
            "value": any_value(),
            "after": nullable("string"),
            "before": nullable("string"),
            "days": { "type": ["array", "null"], "items": { "type": "integer" } },
            "is": { "type": ["string", "null"], "enum": ["day", "night", null] }
        }
    })
}

fn schema() -> Json {
    let fields = [
        "id",
        "type",
        "point",
        "value",
        "to",
        "from",
        "for_s",
        "above",
        "below",
        "at",
        "days",
        "event",
        "offset_min",
        "minutes",
        "seconds",
        "timeout_s",
        "all",
        "rules",
        "rule",
        "title",
        "message",
        "channels",
        "prompt",
    ];
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["name", "mode", "note", "nodes", "edges"],
        "properties": {
            "name": { "type": "string" },
            "mode": { "type": "string", "enum": ["restart", "single", "queued"] },
            "note": nullable("string"),
            "nodes": { "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "required": fields,
                "properties": {
                    "id": { "type": "string" },
                    "type": { "type": "string", "enum": [
                        "when_state", "when_threshold", "at_time", "at_sun", "every", "on_start", "manual",
                        "if", "set", "toggle", "wait", "wait_for", "notify", "write"
                    ] },
                    "point": nullable("string"),
                    "value": any_value(),
                    "to": any_value(),
                    "from": any_value(),
                    "for_s": nullable("integer"),
                    "above": nullable("number"),
                    "below": nullable("number"),
                    "at": nullable("string"),
                    "days": { "type": ["array", "null"], "items": { "type": "integer" } },
                    "event": { "type": ["string", "null"], "enum": ["rise", "set", null] },
                    "offset_min": nullable("integer"),
                    "minutes": nullable("integer"),
                    "seconds": nullable("integer"),
                    "timeout_s": nullable("integer"),
                    "all": nullable("boolean"),
                    "rules": { "type": ["array", "null"], "items": rule_schema() },
                    "rule": { "anyOf": [rule_schema(), { "type": "null" }] },
                    "title": nullable("string"),
                    "message": nullable("string"),
                    "channels": { "type": ["array", "null"], "items": { "type": "string", "enum": ["maison", "voix", "telephone", "telegram"] } },
                    "prompt": nullable("string")
                }
            } },
            "edges": { "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "required": ["from", "port", "to"],
                "properties": {
                    "from": { "type": "string" },
                    "port": { "type": "string", "enum": ["out", "yes", "no", "ok", "timeout"] },
                    "to": { "type": "string" }
                }
            } }
        }
    })
}

/// Nulls out: the engine's format has optional fields, not null ones.
fn strip_nulls(v: &Json) -> Json {
    match v {
        Json::Object(map) => Json::Object(
            map.iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k.clone(), strip_nulls(v)))
                .collect(),
        ),
        Json::Array(items) => Json::Array(items.iter().map(strip_nulls).collect()),
        other => other.clone(),
    }
}

impl Assistant {
    /// Drafts an automation from words, or reworks `current`. Validated
    /// against the house; one correction round if the checker objects.
    pub async fn draft_automation(
        &self,
        autos: &Automations,
        request: &str,
        current: Option<&Automation>,
    ) -> Result<Draft, AssistantError> {
        // What is reworked comes from the client: bounded like a saved
        // automation before it reaches the prompt.
        let current = current
            .cloned()
            .map(|mut a| {
                a.name = moli_automation::tidy_name(&a.name);
                moli_automation::tidy(&mut a.graph).map(|()| a)
            })
            .transpose()
            .map_err(|e| AssistantError::Invalid(e.to_string()))?;
        let key = self.key()?;
        let _permit = self.admit()?;
        let request: String = request.chars().take(crate::MAX_MESSAGE_CHARS).collect();
        self.draft_with(key.as_deref(), autos, &request, current.as_ref())
            .await
    }

    /// Drafting, inside an admitted turn (the conversation's tool).
    pub(crate) async fn draft_with(
        &self,
        key: Option<&str>,
        autos: &Automations,
        request: &str,
        current: Option<&Automation>,
    ) -> Result<Draft, AssistantError> {
        // Imports carry a whole YAML; words typed by a person are capped
        // earlier (2 000 characters).
        let request: String = request.trim().chars().take(12_000).collect();
        if request.is_empty() {
            return Err(AssistantError::Invalid(
                "say what the automation should do".into(),
            ));
        }
        let layout = Layout::load(self.0.layout_path.as_deref()).await;
        let snapshot = self.0.hub.snapshot();
        let now = jiff::Timestamp::now().to_zoned(self.0.tz.clone());
        let system = builder_prompt(
            &crate::house_date(&now),
            &snapshot.guard.protected_rooms.join(", "),
            &house::inventory(
                &snapshot.devices,
                &layout,
                &snapshot.guard.protected_rooms,
                house::Reader::Builder,
            ),
            &self.meters().await,
        );
        let user = request_message(&request, current);
        let mut messages = vec![
            json!({ "role": "system", "content": system }),
            json!({ "role": "user", "content": user }),
        ];
        let mut last_problems = String::new();
        for attempt in 0..2 {
            let model = &self.0.config.builder_model;
            let mut body = json!({
                "model": model,
                "messages": messages,
                "response_format": { "type": "json_schema", "json_schema": { "name": "automation", "strict": true, "schema": schema() } },
            });
            if is_reasoning(model) {
                body["max_completion_tokens"] = json!(8_000);
                body["reasoning_effort"] = json!("low");
            } else {
                body["temperature"] = json!(0.2);
                body["max_tokens"] = json!(3_000);
            }
            let answer = self
                .0
                .endpoint
                .chat(key, &body)
                .await
                .map_err(|e| AssistantError::Upstream(e.to_string()))?;
            let content = answer["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            let draft = match parse(&content, current) {
                Ok(d) => d,
                Err(e) => {
                    last_problems = e;
                    messages.push(json!({ "role": "assistant", "content": content }));
                    messages.push(json!({ "role": "user", "content": moli_i18n::tr!("assistant.auto.unreadable", problems = last_problems) }));
                    continue;
                }
            };
            let check = autos.check(&draft.graph);
            let errors: Vec<String> = check
                .problems
                .iter()
                // Errors, and loose ends: a step nothing leads to is a
                // forgotten link.
                .filter(|p| p.level == moli_automation::Level::Error || p.is_unreached())
                .map(|p| {
                    format!(
                        "{}{}",
                        p.node
                            .as_deref()
                            .map(|n| format!("[{n}] "))
                            .unwrap_or_default(),
                        p.message
                    )
                })
                .collect();
            if errors.is_empty() || attempt == 1 {
                tracing::info!(
                    nodes = draft.graph.nodes.len(),
                    errors = errors.len(),
                    "automation drafted"
                );
                return Ok(draft);
            }
            last_problems = errors.join(" ; ");
            messages.push(json!({ "role": "assistant", "content": content }));
            messages.push(json!({ "role": "user", "content": moli_i18n::tr!("assistant.auto.refused", problems = last_problems) }));
        }
        Err(AssistantError::Upstream(moli_i18n::tr!(
            "assistant.auto.unusable",
            problems = last_problems
        )))
    }

    /// A text for a « Moli écrit » node: words for people, from the house's
    /// current state. Never a decision.
    pub async fn compose(&self, prompt: &str) -> Result<String, AssistantError> {
        let key = self.key()?;
        // Not counted against the conversation: automations are limited on
        // their own (20 runs a minute), and a busy chat must not silence them.

        let layout = Layout::load(self.0.layout_path.as_deref()).await;
        let snapshot = self.0.hub.snapshot();
        let now = jiff::Timestamp::now().to_zoned(self.0.tz.clone());
        let power = self.power_now().await;
        let system = compose_prompt(
            &crate::house_date(&now),
            &power,
            &house::inventory(
                &snapshot.devices,
                &layout,
                &snapshot.guard.protected_rooms,
                house::Reader::Chat,
            ),
        );
        let mut body = json!({
            "model": self.0.config.model,
            "messages": [{ "role": "system", "content": system }, { "role": "user", "content": prompt }],
        });
        if is_reasoning(&self.0.config.model) {
            body["max_completion_tokens"] = json!(3_000);
            body["reasoning_effort"] = json!("low");
        } else {
            body["temperature"] = json!(0.6);
            body["max_tokens"] = json!(400);
        }
        let answer = self
            .0
            .endpoint
            .chat(key.as_deref(), &body)
            .await
            .map_err(|e| AssistantError::Upstream(e.to_string()))?;
        Ok(answer["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_owned())
    }
}

/// What the drafting model is told: the guide, then the house as it is now.
/// The inventory comes last among the words filled in (see `moli_i18n`).
fn builder_prompt(date: &str, protected: &str, inventory: &str, meters: &str) -> String {
    format!(
        "{}\n\n{}",
        moli_i18n::tr!("assistant.auto.guide"),
        moli_i18n::tr!(
            "assistant.auto.context",
            date = date,
            protected = protected,
            meters = meters,
            inventory = inventory
        )
    )
}

/// What writes the message of a « Moli écrit » node: who Moli is talking to,
/// the time, the live power, the house.
fn compose_prompt(date: &str, power: &str, inventory: &str) -> String {
    moli_i18n::tr!(
        "assistant.auto.compose",
        date = date,
        power = power,
        inventory = inventory
    )
}

/// What Moli is asked: the words, and the automation being reworked.
fn request_message(request: &str, current: Option<&Automation>) -> String {
    let Some(a) = current else {
        return moli_i18n::tr!("assistant.auto.request", request = request);
    };
    let flat: Vec<Json> = a
        .graph
        .nodes
        .iter()
        .map(|n| json!({ "id": n.id, "step": n.step }))
        .collect();
    moli_i18n::tr!(
        "assistant.auto.rework",
        mode = format!("{:?}", a.mode),
        nodes = Json::Array(flat),
        edges = serde_json::to_string(&a.graph.edges).unwrap_or_default(),
        name = a.name,
        request = request
    )
}

/// The model's flat JSON → the engine's graph (canvas positions kept from
/// the current version for nodes that stay).
fn parse(content: &str, current: Option<&Automation>) -> Result<Draft, String> {
    let flat: Flat = serde_json::from_str(content).map_err(|e| e.to_string())?;
    let mut nodes = Vec::new();
    for raw in &flat.nodes {
        let mut raw = strip_nulls(raw);
        // Words in the other field: a writer's prompt in `message`, a
        // message in `prompt`.
        match raw["type"].as_str() {
            Some("write") if raw.get("prompt").is_none() => {
                if let Some(m) = raw.get("message").cloned() {
                    raw["prompt"] = m;
                }
            }
            Some("notify") if raw.get("message").is_none() => {
                if let Some(p) = raw.get("prompt").cloned() {
                    raw["message"] = p;
                }
            }
            _ => {}
        }
        let mut node: Node = serde_json::from_value(raw.clone()).map_err(|e| {
            moli_i18n::tr!(
                "assistant.auto.node_error",
                error = e,
                id = raw["id"].as_str().unwrap_or("?")
            )
        })?;
        if let Some(old) = current.and_then(|a| a.graph.node(&node.id)) {
            node.x = old.x;
            node.y = old.y;
        }
        nodes.push(node);
    }
    Ok(Draft {
        name: flat.name.trim().chars().take(80).collect(),
        mode: flat.mode,
        graph: Graph {
            nodes,
            edges: flat.edges,
        },
        note: flat.note.filter(|n| !n.trim().is_empty()),
    })
}

/// The electricity meters by their meaning, one line each (the names the
/// house gave them come last among the words filled in).
fn meters_text(s: &moli_energy::Summary) -> String {
    let mut out = moli_i18n::tr!("assistant.auto.meters_head");
    for m in &s.meters {
        let role = match m.role {
            moli_energy::Role::Total => moli_i18n::tr!("assistant.auto.role_total"),
            moli_energy::Role::Grid => moli_i18n::tr!("assistant.auto.role_grid"),
            moli_energy::Role::Circuit => moli_i18n::tr!("assistant.auto.role_circuit"),
            moli_energy::Role::Appliance => moli_i18n::tr!("assistant.auto.role_appliance"),
        };
        let line = match &m.power {
            Some(power) => moli_i18n::tr!(
                "assistant.auto.meter_power",
                role = role,
                power = power,
                name = m.name
            ),
            None => moli_i18n::tr!(
                "assistant.auto.meter_energy",
                role = role,
                point = m.point,
                name = m.name
            ),
        };
        out.push_str(&line);
        out.push('\n');
    }
    out
}

impl Assistant {
    /// The electricity meters by their meaning: what « the consumption » is,
    /// for the builder.
    async fn meters(&self) -> String {
        let Some(energy) = &self.0.energy else {
            return String::new();
        };
        let Ok(s) = energy.summary().await else {
            return String::new();
        };
        meters_text(&s)
    }

    pub fn set_automations(&self, automations: Automations) {
        *self
            .0
            .automations
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(automations);
    }

    fn automations(&self) -> Option<Automations> {
        self.0
            .automations
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// The conversation's `automation` tool: a draft saved for a person to
    /// approve, shown as a card.
    pub(crate) async fn automation_tool(&self, args: &Json, run: &mut crate::Run) -> Json {
        let Some(autos) = self.automations() else {
            return json!({ "error": moli_i18n::tr!("assistant.auto.not_running") });
        };
        let Some(request) = args["request"].as_str() else {
            return json!({ "error": "request is required" });
        };
        let key = match self.key() {
            Ok(k) => k,
            Err(e) => return json!({ "error": e.to_string() }),
        };
        let request: String = request.chars().take(crate::MAX_MESSAGE_CHARS).collect();
        let draft = match self
            .draft_with(key.as_deref(), &autos, &request, None)
            .await
        {
            Ok(d) => d,
            Err(e) => return json!({ "error": e.to_string() }),
        };
        let a = Automation {
            id: String::new(),
            name: draft.name,
            enabled: false,
            mode: draft.mode,
            graph: draft.graph,
            author: moli_automation::Author::Assistant,
            approved: None,
            approved_version: None,
            note: draft.note.clone(),
            created: 0,
            updated: 0,
        };
        let by = moli_automation::Actor {
            human: false,
            author: moli_automation::Author::Assistant,
        };
        match autos.save(a, by).await {
            Ok(saved) => {
                let check = autos.check(&saved.graph);
                run.cards
                    .push(json!({ "kind": "automation", "id": saved.id }));
                let problems: Vec<&str> =
                    check.problems.iter().map(|p| p.message.as_str()).collect();
                json!({
                    "status": moli_i18n::tr!("assistant.auto.saved"),
                    "summary": check.summary,
                    "problems": problems,
                    "note": draft.note,
                })
            }
            Err(e) => json!({ "error": e.to_string() }),
        }
    }
}

impl moli_automation::Writer for Assistant {
    fn write(&self, prompt: String) -> BoxFuture<'_, anyhow::Result<String>> {
        Box::pin(async move {
            self.compose(&prompt)
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_drafts_become_graphs() {
        let content = json!({
            "name": "Entrée la nuit", "mode": "restart", "note": null,
            "nodes": [
                { "id": "t1", "type": "when_state", "point": "z:1/contact", "value": null, "to": false, "from": null, "for_s": 0,
                  "above": null, "below": null, "at": null, "days": null, "event": null, "offset_min": null, "minutes": null,
                  "seconds": null, "timeout_s": null, "all": null, "rules": null, "rule": null, "title": null, "message": null,
                  "channels": null, "prompt": null },
                { "id": "c1", "type": "if", "rules": [{ "kind": "sun", "is": "night", "point": null, "op": null, "value": null,
                  "after": null, "before": null, "days": null }], "all": true, "point": null },
                { "id": "a1", "type": "set", "point": "hue:2/on", "value": true }
            ],
            "edges": [{ "from": "t1", "port": "out", "to": "c1" }, { "from": "c1", "port": "yes", "to": "a1" }]
        })
        .to_string();
        let d = parse(&content, None).unwrap();
        assert_eq!(d.graph.nodes.len(), 3);
        assert_eq!(d.graph.nodes[0].step.kind(), "when_state");
        assert_eq!(d.graph.next("c1", "yes")[0].id, "a1");
        assert!(d.note.is_none());
    }

    #[test]
    fn the_schema_lists_every_field_as_required() {
        let s = schema();
        let node = &s["properties"]["nodes"]["items"];
        let required = node["required"].as_array().unwrap().len();
        let properties = node["properties"].as_object().unwrap().len();
        assert_eq!(
            required, properties,
            "strict mode needs every property required"
        );
    }

    use crate::before;

    #[test]
    fn the_french_drafting_prompts_are_what_they_always_were() {
        assert_eq!(moli_i18n::tr!("assistant.auto.guide"), before::GUIDE);
        let inventory = "## Salon\n- hue:1 · Lampe · on✎=true\n## Sans pièce\n- x:1 · Truc\n";
        let meters = "Électricité (pour « la consommation ») :\n- Maison (toute la maison) : puissance en W = t/p\n";
        let date = "samedi 3 octobre 2026, 21h05";
        for protected in ["Chambre des enfants, Bureau", ""] {
            assert_eq!(
                builder_prompt(date, protected, inventory, meters),
                before::builder_prompt(date, protected, inventory, meters)
            );
        }
        for power in ["Électricité en direct : 940 W.", ""] {
            assert_eq!(
                compose_prompt(date, power, inventory),
                before::compose_prompt(date, power, inventory)
            );
        }
    }

    #[test]
    fn the_french_request_is_what_it_always_was() {
        assert_eq!(
            request_message("allume le garage", None),
            before::request_message("allume le garage", None)
        );
        let current: Automation = serde_json::from_value(json!({
            "name": "Garage la nuit",
            "graph": { "nodes": [{ "id": "a1", "type": "set", "point": "hue:2/on", "value": true }] },
            "author": "human"
        }))
        .unwrap();
        assert_eq!(
            request_message("plus tard", Some(&current)),
            before::request_message("plus tard", Some(&current))
        );
    }

    #[test]
    fn the_french_meters_and_messages_are_what_they_always_were() {
        for full in [true, false] {
            let summary = crate::tests::sample_summary(full);
            assert_eq!(meters_text(&summary), before::meters(&summary));
        }
        assert_eq!(
            moli_i18n::tr!("assistant.auto.unreadable", problems = "x"),
            before::unreadable("x")
        );
        assert_eq!(
            moli_i18n::tr!("assistant.auto.refused", problems = "x ; y"),
            before::refused("x ; y")
        );
        assert_eq!(
            moli_i18n::tr!("assistant.auto.unusable", problems = "x"),
            before::unusable("x")
        );
        assert_eq!(
            moli_i18n::tr!("assistant.auto.not_running"),
            "les automatisations ne tournent pas"
        );
        assert_eq!(
            moli_i18n::tr!("assistant.auto.saved"),
            "brouillon enregistré : un adulte doit le valider (bouton sur la carte)"
        );
        let bad = json!({ "name": "x", "mode": "restart", "note": null,
            "nodes": [{ "id": "t1", "type": "nope" }], "edges": [] })
        .to_string();
        let why = parse(&bad, None).unwrap_err();
        assert!(why.starts_with("nœud t1 : "), "{why}");
    }

    /// The model is told the engine's own words (node types, ports,
    /// channels, tokens) in both languages: only the prose is translated.
    #[test]
    fn the_english_guide_keeps_the_engines_words() {
        let (fr, en) = (crate::tests::french(), crate::tests::english());
        let (fr, en) = (&fr["assistant.auto.guide"], &en["assistant.auto.guide"]);
        let s = schema();
        let node = &s["properties"]["nodes"]["items"]["properties"];
        let mut words: Vec<String> = node.as_object().unwrap().keys().cloned().collect();
        for list in [
            &node["type"]["enum"],
            &node["channels"]["items"]["enum"],
            &s["properties"]["edges"]["items"]["properties"]["port"]["enum"],
            &rule_schema()["properties"]["kind"]["enum"],
        ] {
            words.extend(
                list.as_array()
                    .unwrap()
                    .iter()
                    .map(|w| w.as_str().unwrap().to_owned()),
            );
        }
        words.extend(["{{texte}}", "{{heure}}", "restart", "single", "queued"].map(String::from));
        for word in &words {
            assert_eq!(
                fr.contains(word.as_str()),
                en.contains(word.as_str()),
                "{word}"
            );
        }
        assert!(
            en.contains("{{texte}}") && en.contains("when_state") && en.contains("\"telegram\"")
        );
        assert!(en.contains("message (English, short)") && en.contains("short, in English"));
    }
}
