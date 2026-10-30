//! The conversation with a model trained on Moli's house contract
//! (`contract = "moli-v4"` on its provider): a prompt that never moves (devices
//! numbered `d1…dN`, no value, no date: the provider keeps it in cache) and the
//! contract's tools, in one round trip; Moli itself reads the numbers back,
//! reads the values and words the outcome. The model was trained on single
//! turns where its answer is the call, so it never reads a tool result.

use std::collections::BTreeMap;

use moli_core::{DeviceId, Origin};
use moli_runtime::{LabelError, LabelPatch, MAX_LABEL};
use serde_json::{Value as Json, json};

use super::{
    Assistant, AssistantError, MAX_MESSAGE_CHARS, Reply, Run, Turn, contract, house, providers,
    show,
};
use crate::house::{Layout, name_of, norm};
use crate::layout_edit;
use crate::proposals::{Change, Pending};

impl Assistant {
    /// Whether the chat provider speaks the house-model contract.
    pub(crate) fn speaks_contract(&self) -> bool {
        self.0
            .providers
            .resolve(providers::Route::Chat)
            .is_some_and(|p| p.contract.is_some())
    }

    /// The contract's prompt for the house as it is now: its rooms, its
    /// protected rooms and the static inventory (what the model was trained on).
    pub(crate) fn house_prompt(
        &self,
        layout: &Layout,
    ) -> Result<(String, BTreeMap<String, DeviceId>), AssistantError> {
        contract::ensure_french().map_err(|e| AssistantError::NotConfigured(e.to_string()))?;
        let snapshot = self.0.hub.snapshot();
        let protected: Vec<String> = snapshot
            .guard
            .protected_rooms
            .iter()
            .map(|r| layout.room(r))
            .collect();
        let rooms: Vec<&str> = layout.rooms.iter().map(|r| r.name.as_str()).collect();
        let (inventory, aliases) =
            house::inventory_static(&snapshot.devices, layout, &snapshot.guard.protected_rooms);
        let prompt = contract::prompt(&rooms, &protected, &inventory)
            .map_err(|e| AssistantError::Invalid(e.to_string()))?;
        Ok((prompt, aliases.into_iter().collect()))
    }

    /// Has the provider read the prompt once, before the first question: a small
    /// model on a processor reads 3 000 tokens in half a minute, and a prompt that
    /// never moves is kept afterwards. Silent: a provider that is not there yet
    /// (it starts after Moli) is not an error, the first question just waits.
    pub async fn warm_up(&self) {
        if !self.speaks_contract() {
            return;
        }
        let layout = Layout::load(self.0.layout_path.as_deref()).await;
        let Ok((prompt, _)) = self.house_prompt(&layout) else {
            return;
        };
        let Ok(key) = self.key() else {
            return;
        };
        let body = json!({
            "model": self.0.config.model,
            "messages": [
                { "role": "system", "content": prompt },
                { "role": "user", "content": "." },
            ],
            "tools": contract_tools(),
            "temperature": 0,
            "max_tokens": 1,
        });
        match self.0.endpoint.chat(key.as_deref(), &body).await {
            Ok(_) => tracing::info!("house model warmed up"),
            Err(e) => tracing::debug!(error = %e, "house model not warmed up"),
        }
    }

    /// One round trip: the model answers with words or with one call, and
    /// Moli words what came of it (the model never reads a tool result).
    pub(crate) async fn converse_house(
        &self,
        turn: &Turn,
        layout: &Layout,
        key: Option<&str>,
        stop: &tokio_util::sync::CancellationToken,
    ) -> Result<Reply, AssistantError> {
        let question = turn
            .messages
            .last()
            .map(|m| {
                m.content
                    .chars()
                    .take(MAX_MESSAGE_CHARS)
                    .collect::<String>()
            })
            .unwrap_or_default();
        let (prompt, aliases) = self.house_prompt(layout)?;
        // What the house was taught answers before the model.
        if let Some(reply) = self.apply_rule(turn, &question, layout, &aliases).await {
            return Ok(reply);
        }
        let tools = contract_tools();
        // The prompt first and unchanged (its reading is kept), then the dialogue.
        let mut messages = vec![json!({ "role": "system", "content": prompt })];
        messages.extend(dialogue(
            turn,
            &question,
            contract::version().is_ok_and(|v| v >= 4),
        ));
        let body = json!({
            "model": self.0.config.model,
            "messages": messages,
            "tools": tools,
            "temperature": 0,
            "max_tokens": self.house_room(&question),
        });
        let Some(answer) = self.ask_model(key, &body, stop).await? else {
            return Ok(self.interrupted(Run::default()));
        };
        let mut run = Run {
            asker: turn.asker.clone().unwrap_or_default(),
            satellite: turn.surface.as_deref() == Some("satellite"),
            ..Run::default()
        };
        run.usage.steps = 1;
        run.usage.prompt_tokens = answer["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
        run.usage.completion_tokens = answer["usage"]["completion_tokens"].as_u64().unwrap_or(0);
        let message = &answer["choices"][0]["message"];
        let calls = message["tool_calls"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        // A change of configuration is decided alone: the other calls of the answer wait.
        let proposal = calls
            .iter()
            .find(|c| PROPOSING.contains(&c["function"]["name"].as_str().unwrap_or_default()));
        let chosen: Vec<&Json> = match proposal {
            Some(one) => vec![one],
            None => calls.iter().take(MAX_CALLS).collect(),
        };
        if chosen.len() < calls.len() {
            tracing::warn!(
                calls = calls.len(),
                carried_out = chosen.len(),
                "house model made more calls than are carried out"
            );
        }
        let said = if chosen.is_empty() {
            plain_words(message["content"].as_str().unwrap_or_default())
        } else {
            let mut said = Vec::new();
            for call in chosen {
                let words = self
                    .house_call(call, layout, turn.spoken, &aliases, &question, &mut run)
                    .await;
                if !words.is_empty() {
                    said.push(words);
                }
            }
            said.join(" ")
        };
        // No question to guess cards from: the model's words stand as they are.
        let reply = self.finish(&said, run, layout, "");
        Ok(self.seal(reply, turn, &question))
    }

    /// What the house model's call comes to, in Moli's words. The model
    /// writes device numbers (`d3/state`): Moli reads the real identifiers.
    pub(crate) async fn house_call(
        &self,
        call: &Json,
        layout: &Layout,
        spoken: bool,
        aliases: &BTreeMap<String, DeviceId>,
        question: &str,
        run: &mut Run,
    ) -> String {
        let name = call["function"]["name"].as_str().unwrap_or_default();
        if run.tools.len() < 16 {
            run.tools.push(name.chars().take(32).collect());
        }
        let args: Json = call["function"]["arguments"]
            .as_str()
            .and_then(|a| serde_json::from_str(a).ok())
            .or_else(|| {
                call["function"]["arguments"]
                    .as_object()
                    .map(|_| call["function"]["arguments"].clone())
            })
            .unwrap_or_else(|| json!({}));
        // The model sometimes writes a point (`d3/state`) where a device is asked: the device is before the `/`.
        let device_of = |written: &Json| {
            let written = written.as_str()?.trim();
            aliases.get(
                written
                    .split_once('/')
                    .map_or(written, |(device, _)| device),
            )
        };
        let point_of = |written: &Json| {
            let (alias, key) = written.as_str()?.trim().split_once('/')?;
            Some(format!("{}/{key}", aliases.get(alias)?.as_str()))
        };
        match name {
            // The person named a room and the model picked a device elsewhere: nothing is sent.
            "set" | "adjust" | "schedule"
                if let Some(words) =
                    self.elsewhere(point_of(&args["point"]).as_deref(), layout, question) =>
            {
                words
            }
            "set" => {
                self.house_set(point_of(&args["point"]), &args, question, run)
                    .await
            }
            "get_device" => {
                let Some(id) = device_of(&args["device"]) else {
                    return moli_i18n::tr!("assistant.house.not_found");
                };
                let card = json!({ "kind": "device", "id": id.as_str() });
                show(&self.0.hub, &json!({ "cards": [card] }), layout, run);
                // The values are Moli's to read, not the model's to guess.
                reading_words(
                    &self.get_device(&json!({ "device": id.as_str() }), layout),
                    question,
                )
            }
            "energy" | "history" => {
                let card = if name == "energy" {
                    json!({ "kind": "energy" })
                } else {
                    let Some(point) = point_of(&args["point"]) else {
                        return moli_i18n::tr!("assistant.house.not_found");
                    };
                    json!({ "kind": "history", "point": point, "hours": args["hours"] })
                };
                show(&self.0.hub, &json!({ "cards": [card] }), layout, run);
                // A satellite has no screen: the figures are said.
                if name == "energy" && spoken {
                    self.power_now().await
                } else {
                    String::new()
                }
            }
            // A satellite has no screen to confirm on: the change is asked for on the dashboard.
            "rename_device" | "move_device" | "set_device_enabled" | "rename_room"
            | "add_person" | "set_home_location"
                if run.satellite =>
            {
                moli_i18n::tr!("assistant.house.propose_on_screen")
            }
            "rename_device" | "move_device" | "set_device_enabled" | "rename_room" => {
                let device = device_of(&args["device"]).cloned();
                self.propose(name, &args, device, layout, spoken, run)
            }
            "add_person" => self.propose_person(&args, question, spoken, run),
            // An order for later is never carried out now (`schedule` and `remind` are the tools for it).
            "set_all" | "adjust" | "ambiance" if later(question) => {
                moli_i18n::tr!("assistant.house.later_unsupported")
            }
            "set_all" => self.set_all(&args, layout, run).await,
            "adjust" => self.adjust(point_of(&args["point"]), &args, run).await,
            "find" => self.find(&args, layout),
            "problems" => self.problems(layout),
            "now" => self.now_words(),
            "last_change" => self.last_change(device_of(&args["device"])).await,
            "weather" => self.weather(&args).await,
            "ambiance" => self.house_ambiance(&args, layout, run).await,
            "schedule" => self.schedule(point_of(&args["point"]), &args, run.caller()),
            "remind" => self.remind(&args, run.caller()),
            "set_home_location" => self.propose_location(&args, question, spoken, run).await,
            "delegate" => self.delegate_question(&args, run).await,
            "undo" => self.propose_undo(spoken, run),
            "refresh_weather" => self.refresh_weather().await,
            "request_driver" => self.request_driver(&args).await,
            "draft_automation" => self.request_automation(&args).await,
            _ => moli_i18n::tr!("assistant.house.unavailable"),
        }
    }

    /// `set`: checked (later, a setting, unreachable), a colour's name translated, an
    /// order already true skipped, then sent under the guard.
    async fn house_set(
        &self,
        point: Option<String>,
        args: &Json,
        question: &str,
        run: &mut Run,
    ) -> String {
        let Some(point) = point else {
            return if args["point"].is_string() && args.get("value").is_some() {
                moli_i18n::tr!("assistant.house.not_found")
            } else {
                unknown_words()
            };
        };
        let Some(value) = args.get("value") else {
            return unknown_words();
        };
        let (device, key) = point.split_once('/').unwrap_or((&point, ""));
        let view = self.0.hub.device(&DeviceId::from(device));
        if later(question) {
            return moli_i18n::tr!("assistant.house.later_unsupported");
        }
        // A setting of the device (a delay, what it does at power-up) is the dashboard's.
        if view.as_ref().is_some_and(|v| {
            v.device
                .points
                .iter()
                .any(|p| &*p.key == key && house::setting(&p.key, p.semantic))
        }) {
            return moli_i18n::tr!("assistant.house.setting_refused");
        }
        let label = view.as_ref().map_or("?", |v| name_of(v)).to_owned();
        // Nobody answers there: say so, send nothing.
        if view.as_ref().is_some_and(|v| v.online == Some(false)) {
            return moli_i18n::tr!("assistant.house.unreachable", device = label);
        }
        let (mut point, mut value) = (point.clone(), value.clone());
        // A colour by its name (« rouge », « blanc chaud »): the house's to translate.
        if key == "color"
            && let Some(said) = value.as_str()
            && !said.starts_with('#')
        {
            let warm = view.as_ref().is_some_and(|v| {
                v.device
                    .points
                    .iter()
                    .any(|p| &*p.key == "color_temp" && p.access.write)
            });
            match colour(said) {
                Some(Colour::Hex(hex)) => value = json!(hex),
                Some(Colour::White(mired)) if warm => {
                    point = format!("{device}/color_temp");
                    value = json!(mired);
                }
                Some(Colour::White(_)) => value = json!("#ffffff"),
                None => return moli_i18n::tr!("assistant.house.colour_unknown"),
            }
        }
        // Already so: nothing to send.
        let point_key = point.split_once('/').map_or("", |(_, k)| k);
        if let Some(now) = view.as_ref().and_then(|v| v.state.get(point_key))
            && same(&now.value, &value)
        {
            return moli_i18n::tr!("assistant.house.already", device = label);
        }
        // A colour or a level for a light that is off: it is lit first.
        if let Some(v) = &view
            && let Some((on_key, on)) = crate::house_tools::wake_order(v, point_key, &value)
        {
            self.set(
                &json!({ "point": format!("{}/{on_key}", v.device.id.as_str()), "value": on }),
                run,
            )
            .await;
        }
        let result = self
            .set(&json!({ "point": point, "value": value }), run)
            .await;
        order_words(&result, &label)
    }

    /// A question outside the house's frame (a joke, an explanation, an opinion).
    /// The tool stays in the contract (no retraining), but nothing leaves the house
    /// until handing a question to another assistant has a decided form (#17).
    /// Room for the house model's answer: one call or a few words, and the
    /// texts its tools carry are bounded by `MAX_REQUEST`. Once an agent can
    /// take a question, `delegate` writes it out again, whole: room for it
    /// too, or a long one is cut mid-call and lost (the rule of `output_room`).
    fn house_room(&self, question: &str) -> usize {
        if self.0.hermes.is_some() {
            HOUSE_WORDS + question.chars().count()
        } else {
            HOUSE_WORDS
        }
    }

    /// `delegate`: a question beyond the house, handed to the outside agent
    /// when there is one; its answer is announced when it comes. Once a turn,
    /// as for the general model (`MAX_DELEGATIONS`): one answer here makes up
    /// to `MAX_CALLS` calls, as many tasks otherwise.
    async fn delegate_question(&self, args: &Json, run: &mut Run) -> String {
        if self.0.hermes.is_none() {
            return moli_i18n::tr!("assistant.house.delegate_later");
        }
        if run.delegations == crate::MAX_DELEGATIONS {
            return moli_i18n::tr!("assistant.house.delegate_once");
        }
        run.delegations += 1;
        let Some(question) = args["question"]
            .as_str()
            .map(str::trim)
            .filter(|q| !q.is_empty())
        else {
            return unknown_words();
        };
        match self.hand_over(question, false, &run.asker).await {
            Ok(_) => moli_i18n::tr!("assistant.house.delegated"),
            Err(why) => {
                tracing::warn!(error = %why, "handing a question over");
                moli_i18n::tr!("assistant.house.delegate_failed")
            }
        }
    }

    /// The rooms a sentence names (whole words, accents and case aside), as the house writes them.
    pub(crate) fn rooms_said(&self, layout: &Layout, text: &str) -> Vec<String> {
        let words = format!(
            " {} ",
            norm(text)
                .replace(|c: char| !c.is_alphanumeric(), " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        );
        let mut rooms: Vec<(String, String)> = layout
            .rooms
            .iter()
            .flat_map(|r| {
                std::iter::once(r.name.clone())
                    .chain(r.aliases.iter().cloned())
                    .map(|n| (layout.room(&r.name), n))
            })
            .collect();
        rooms.extend(
            self.0
                .hub
                .snapshot()
                .devices
                .iter()
                .map(|d| (layout.room_of(d), layout.room_of(d))),
        );
        let mut said: Vec<String> = rooms
            .into_iter()
            .filter(|(_, name)| {
                let n = norm(name).replace(|c: char| !c.is_alphanumeric(), " ");
                let n = n.split_whitespace().collect::<Vec<_>>().join(" ");
                n.chars().count() >= 3 && words.contains(&format!(" {n} "))
            })
            .map(|(room, _)| norm(&room))
            .collect();
        said.sort();
        said.dedup();
        said
    }

    /// When the sentence names rooms and the device written is in none of them: what Moli says
    /// instead of acting (« éteins la lumière de l'entrée » must never reach the salon's).
    fn elsewhere(&self, point: Option<&str>, layout: &Layout, question: &str) -> Option<String> {
        let device = point?.split('/').next()?;
        let view = self.0.hub.device(&DeviceId::from(device))?;
        let room = layout.room_of(&view);
        let said = self.rooms_said(layout, question);
        (!said.is_empty() && !said.contains(&norm(&room))).then(|| {
            moli_i18n::tr!(
                "assistant.house.room_mismatch",
                device = name_of(&view),
                room = room
            )
        })
    }

    /// « Actualise la météo »: what the dashboard kept is forgotten and asked again.
    async fn refresh_weather(&self) -> String {
        let weather = self
            .0
            .weather
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let Some(weather) = weather else {
            return moli_i18n::tr!("assistant.house.weather_unavailable");
        };
        // A floor: the model may call it several times a turn, the service is not asked
        // again within a few minutes (what is kept is fresh enough to say « à jour »).
        if self.may_refresh_weather() {
            weather.forget().await;
        }
        match weather.forecast().await {
            Ok(_) => moli_i18n::tr!("assistant.house.weather_refreshed"),
            Err(e) => {
                tracing::warn!(error = %e, "weather could not be refreshed");
                moli_i18n::tr!("assistant.house.weather_unavailable")
            }
        }
    }

    /// Whether the weather may forget what it kept now: once every [`WEATHER_FLOOR`].
    fn may_refresh_weather(&self) -> bool {
        let mut last = self
            .0
            .weather_refreshed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last.is_some_and(|t| t.elapsed() < WEATHER_FLOOR) {
            return false;
        }
        *last = Some(std::time::Instant::now());
        true
    }

    /// A request for a driver, written where the outside agent reads it
    /// (`driver-requests.jsonl`, one line a request). Nothing is run here.
    async fn request_driver(&self, args: &Json) -> String {
        self.request_for_agent(
            args,
            "driver-requests.jsonl",
            "assistant.house.driver_requested",
        )
        .await
    }

    /// `draft_automation`: noted for the agent that drafts automations (a heavy task, for a
    /// stronger model than the house's); nothing is drafted or run here.
    async fn request_automation(&self, args: &Json) -> String {
        self.request_for_agent(
            args,
            "automation-requests.jsonl",
            "assistant.house.automation_requested",
        )
        .await
    }

    /// A request written where an outside agent reads it (one line a request).
    async fn request_for_agent(&self, args: &Json, file: &str, said: &str) -> String {
        let description = args["description"]
            .as_str()
            .map(str::trim)
            .filter(|d| !d.is_empty() && d.chars().count() <= MAX_REQUEST);
        let Some(description) = description else {
            return unknown_words();
        };
        let Some(dir) = self
            .0
            .layout_path
            .as_deref()
            .and_then(std::path::Path::parent)
        else {
            return moli_i18n::tr!("assistant.house.unavailable");
        };
        let line = format!(
            "{}\n",
            json!({
                "ts": moli_core::now_ms(),
                "description": description,
                "origin": "assistant",
                // A model's words: data for the agent that reads this file, never instructions.
                "untrusted": true,
            })
        );
        let target = dir.join(file);
        let written = async {
            use tokio::io::AsyncWriteExt as _;
            // A chatty model must not fill the disk: past a megabyte, no more requests.
            if tokio::fs::metadata(&target)
                .await
                .is_ok_and(|m| m.len() > MAX_REQUESTS_FILE)
            {
                tracing::warn!(
                    file,
                    max_bytes = MAX_REQUESTS_FILE,
                    "requests file is full: this request and the next ones are dropped until the agent empties it"
                );
                return Err(std::io::Error::other(format!("{file} is full")));
            }
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&target)
                .await?;
            file.write_all(line.as_bytes()).await?;
            // A tokio file writes on a blocking thread: without a flush, the line may
            // not be on disk yet when the file is dropped (or read).
            file.flush().await
        }
        .await;
        match written {
            Ok(()) => moli_i18n::tr(said),
            Err(e) => {
                tracing::warn!(error = %e, "driver request could not be written");
                moli_i18n::tr!("assistant.house.unavailable")
            }
        }
    }

    /// A change of configuration the model asked for: checked, kept as a
    /// proposal and worded. Nothing changes until a person confirms.
    fn propose(
        &self,
        tool: &str,
        args: &Json,
        device: Option<DeviceId>,
        layout: &Layout,
        spoken: bool,
        run: &mut Run,
    ) -> String {
        let view = device.as_ref().and_then(|id| self.0.hub.device(id));
        let protected = self.0.hub.snapshot().guard.protected_rooms.clone();
        let current = view.as_ref().map(|v| name_of(v).to_owned());
        let (change, text) = match tool {
            "rename_device" => {
                let (Some(id), Some(from)) = (device, current) else {
                    return moli_i18n::tr!("assistant.house.not_found");
                };
                let Some(to) = valid_name(&args["new_name"], &from) else {
                    return unknown_words();
                };
                let text = moli_i18n::tr!("assistant.house.p_rename_device", from = from, to = to);
                (
                    Change::RenameDevice {
                        device: id,
                        name: to,
                    },
                    text,
                )
            }
            "move_device" => {
                let (Some(id), Some(name)) = (device, current) else {
                    return moli_i18n::tr!("assistant.house.not_found");
                };
                let Some(room) = self.known_room(layout, &args["room"]) else {
                    return moli_i18n::tr!("assistant.house.unknown_room");
                };
                // A protected room is written as the guard writes it: the guard
                // does not read home.json, and a label it cannot match protects nothing.
                let room = protected
                    .iter()
                    .find(|p| layout.same_room(p, &room))
                    .cloned()
                    .unwrap_or(room);
                let text =
                    moli_i18n::tr!("assistant.house.p_move_device", device = name, room = room);
                // Leaving a protected room drops its protection: the person confirms knowing it.
                let leaves = view.as_ref().map(|v| layout.room_of(v)).filter(|from| {
                    protected.iter().any(|p| layout.same_room(p, from))
                        && !layout.same_room(from, &room)
                });
                let text = match leaves {
                    Some(from) => format!(
                        "{text} {}",
                        moli_i18n::tr!("assistant.house.p_leaves_protected", room = from)
                    ),
                    None => text,
                };
                (Change::MoveDevice { device: id, room }, text)
            }
            "set_device_enabled" => {
                let (Some(id), Some(name), Some(enabled)) =
                    (device, current, args["enabled"].as_bool())
                else {
                    return moli_i18n::tr!("assistant.house.not_found");
                };
                let text = if enabled {
                    moli_i18n::tr!("assistant.house.p_show", device = name)
                } else {
                    moli_i18n::tr!("assistant.house.p_hide", device = name)
                };
                (
                    Change::SetEnabled {
                        device: id,
                        enabled,
                    },
                    text,
                )
            }
            _ => {
                let Some(from) = self.known_room(layout, &args["room"]) else {
                    return moli_i18n::tr!("assistant.house.unknown_room");
                };
                // The guard names protected rooms by their name: renaming one would let it go.
                if protected.iter().any(|p| layout.same_room(p, &from)) {
                    return moli_i18n::tr!("assistant.house.room_protected_kept");
                }
                let Some(to) = valid_name(&args["new_name"], &from) else {
                    return unknown_words();
                };
                // Never into another room: two rooms would become one without a word.
                if self
                    .known_room(layout, &json!(to))
                    .is_some_and(|other| norm(&other) != norm(&from))
                {
                    return moli_i18n::tr!("assistant.house.room_taken");
                }
                let text = moli_i18n::tr!("assistant.house.p_rename_room", from = from, to = to);
                (Change::RenameRoom { from, to }, text)
            }
        };
        self.queue(change, &text, spoken, run)
    }

    /// Keeps a change for a person to decide, and words it.
    fn queue(&self, change: Change, text: &str, spoken: bool, run: &mut Run) -> String {
        let proposal = self
            .0
            .proposals
            .add(change, text.to_owned(), None, Some(run.asker.clone()));
        run.proposals.push(proposal);
        propose_words(text, spoken)
    }

    /// « Ajoute Léa au foyer »: a name, nothing else. The e-mail, the password and the
    /// rights stay with the People screen.
    fn propose_person(&self, args: &Json, question: &str, spoken: bool, run: &mut Run) -> String {
        let Some(directory) = self.directory() else {
            return moli_i18n::tr!("assistant.house.unavailable");
        };
        let Some(name) = valid_name(&args["name"], "").filter(|n| n.chars().count() <= MAX_PERSON)
        else {
            return unknown_words();
        };
        let name = name_from_words(&name, question);
        if directory.names().iter().any(|n| norm(n) == norm(&name)) {
            return moli_i18n::tr!("assistant.house.person_known", name = name);
        }
        let text = moli_i18n::tr!("assistant.house.p_add_person", name = name);
        self.queue(Change::AddPerson { name }, &text, spoken, run)
    }

    /// « J'habite à Lyon »: the town is looked up now, so that the proposal says which
    /// one was found. Only the town's name goes out, to the geocoding service.
    async fn propose_location(
        &self,
        args: &Json,
        question: &str,
        spoken: bool,
        run: &mut Run,
    ) -> String {
        if self.0.layout_path.is_none() {
            return moli_i18n::tr!("assistant.house.unavailable");
        }
        let Some(written) = valid_name(&args["place"], "") else {
            return unknown_words();
        };
        let places = self
            .0
            .places
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let names = crate::places::candidates(&written, question);
        let found = crate::places::first_known(places.as_ref(), &names).await;
        let mut failed = false;
        match found {
            Ok(Some(place)) => {
                let text = moli_i18n::tr!("assistant.house.p_set_location", place = place.label);
                return self.queue(
                    Change::SetLocation { place: Some(place) },
                    &text,
                    spoken,
                    run,
                );
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, "a town could not be looked up");
                failed = true;
            }
        }
        if failed {
            moli_i18n::tr!("assistant.house.place_unavailable")
        } else {
            moli_i18n::tr!("assistant.house.place_unknown")
        }
    }

    fn directory(&self) -> Option<std::sync::Arc<dyn crate::Directory>> {
        self.0
            .directory
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// « Annule », « fais l'inverse »: the inverse of the last change a person
    /// confirmed, proposed like any other change. The model has no memory of the
    /// dialogue: Moli keeps what was done.
    fn propose_undo(&self, spoken: bool, run: &mut Run) -> String {
        // « Non, annule ! » right after a proposal means: not that one. Nothing was changed.
        if let Some(waiting) = self.0.proposals.newest_pending(Some(&run.asker)) {
            self.0.proposals.take(waiting);
            return moli_i18n::tr!("assistant.house.cancelled");
        }
        // « Annule » takes back the latest: what was just planned, or the last confirmed change.
        let since = self.0.proposals.last_applied().map(|a| a.at);
        if let Some(text) = self.cancel_planned(since) {
            return moli_i18n::tr!("assistant.house.planned_cancelled", text = text);
        }
        let Some(applied) = self.0.proposals.last_applied() else {
            return moli_i18n::tr!("assistant.house.undo_nothing");
        };
        if run.satellite {
            return moli_i18n::tr!("assistant.house.propose_on_screen");
        }
        let text = moli_i18n::tr!("assistant.house.p_undo", text = applied.text);
        let proposal = self.0.proposals.add(
            applied.inverse,
            text.clone(),
            Some(applied.id),
            Some(run.asker.clone()),
        );
        run.proposals.push(proposal);
        propose_words(&text, spoken)
    }

    /// What takes `change` back, worked out before it is applied (the names and
    /// rooms it replaces are still there).
    async fn inverse_of(&self, change: &Change) -> Option<(Change, String)> {
        let (inverse, text) = match change {
            Change::RenameDevice {
                device,
                name: new_name,
            } => {
                let view = self.0.hub.device(device)?;
                let before = view.label.name.clone();
                let shown = before
                    .clone()
                    .unwrap_or_else(|| view.device.native_name.to_string());
                (
                    Change::RenameDevice {
                        device: device.clone(),
                        name: before.unwrap_or_default(),
                    },
                    moli_i18n::tr!(
                        "assistant.house.p_rename_device",
                        from = new_name,
                        to = shown
                    ),
                )
            }
            Change::MoveDevice { device, .. } => {
                let view = self.0.hub.device(device)?;
                let before = view.label.room.clone();
                let shown = before
                    .clone()
                    .or_else(|| view.device.native_room.as_deref().map(str::to_owned))
                    .unwrap_or_else(house::no_room);
                (
                    Change::MoveDevice {
                        device: device.clone(),
                        room: before.unwrap_or_default(),
                    },
                    moli_i18n::tr!(
                        "assistant.house.p_move_device",
                        device = name_of(&view),
                        room = shown
                    ),
                )
            }
            Change::SetEnabled { device, enabled } => {
                let view = self.0.hub.device(device)?;
                // Already as asked: nothing changes, so there is nothing to take back.
                let layout = Layout::load(self.0.layout_path.as_deref()).await;
                if layout.hides(device.as_str()) != *enabled {
                    return None;
                }
                let text = if *enabled {
                    moli_i18n::tr!("assistant.house.p_hide", device = name_of(&view))
                } else {
                    moli_i18n::tr!("assistant.house.p_show", device = name_of(&view))
                };
                (
                    Change::SetEnabled {
                        device: device.clone(),
                        enabled: !enabled,
                    },
                    text,
                )
            }
            Change::RenameRoom { from, to } => (
                Change::RenameRoom {
                    from: to.clone(),
                    to: from.clone(),
                },
                moli_i18n::tr!("assistant.house.p_rename_room", from = to, to = from),
            ),
            // The id is known once the person exists: `decide_proposal` fills it in.
            Change::AddPerson { name } => (
                Change::RemovePerson {
                    id: String::new(),
                    name: name.clone(),
                },
                moli_i18n::tr!("assistant.house.p_remove_person", name = name),
            ),
            Change::RemovePerson { name, .. } => (
                Change::AddPerson { name: name.clone() },
                moli_i18n::tr!("assistant.house.p_add_person", name = name),
            ),
            Change::SetLocation { .. } => {
                let path = self.0.layout_path.clone()?;
                let before = tokio::task::spawn_blocking(move || layout_edit::saved_place(&path))
                    .await
                    .ok()?;
                let text = match &before {
                    Some(p) => moli_i18n::tr!("assistant.house.p_set_location", place = p.label),
                    None => moli_i18n::tr!("assistant.house.p_clear_location"),
                };
                (Change::SetLocation { place: before }, text)
            }
        };
        Some((inverse, text))
    }

    /// A room as the house names it, when the house has it: listed in
    /// `home.json`, or the room of one of its devices.
    pub(crate) fn known_room(&self, layout: &Layout, written: &Json) -> Option<String> {
        let room = layout.room(written.as_str()?.trim());
        let wanted = norm(&room);
        let listed = layout.rooms.iter().any(|r| norm(&r.name) == wanted);
        let of_a_device = || {
            self.0
                .hub
                .snapshot()
                .devices
                .iter()
                .any(|d| norm(&layout.room_of(d)) == wanted)
        };
        (!room.is_empty() && (listed || of_a_device())).then_some(room)
    }

    /// A person's answer to a proposal. Only a person decides (the dashboard,
    /// never an agent or a script); a proposal is decided once, and waits ten
    /// minutes.
    pub async fn decide_proposal(
        &self,
        id: u64,
        approve: bool,
        origin: Origin,
        actor: Option<String>,
    ) -> Result<String, ProposalError> {
        self.decide_proposal_by(id, approve, origin, actor, true)
            .await
    }

    /// [`Self::decide_proposal`], saying whether the person deciding is an owner:
    /// adding someone to the household is an owner's, as `POST /api/people` is.
    pub async fn decide_proposal_by(
        &self,
        id: u64,
        approve: bool,
        origin: Origin,
        actor: Option<String>,
        owner: bool,
    ) -> Result<String, ProposalError> {
        // Releasing what the model asked for is a decision, like releasing an order
        // the guard held: a person with a session, nothing less (the household
        // alone is not a person).
        if origin != Origin::Ui {
            return Err(ProposalError::HumanOnly);
        }
        // One confirmed change at a time: the inverse is worked out, the change applied
        // and remembered together, or two at once would remember the wrong ones.
        let _one_at_a_time = self.0.changing.lock().await;
        let pending = self.0.proposals.take(id).ok_or(ProposalError::Unknown)?;
        if approve && !owner && matches!(pending.change, Change::AddPerson { .. }) {
            // Refused, not lost: an owner may still confirm it.
            self.0.proposals.put_back(id, pending);
            return Err(ProposalError::OwnersOnly);
        }
        if !approve {
            return Ok(moli_i18n::tr!("assistant.house.cancelled"));
        }
        // An undo only goes through while the change it takes back is still the latest.
        if let Some(undone) = pending.undo
            && self.0.proposals.last_applied().map(|a| a.id) != Some(undone)
        {
            return Err(ProposalError::Unknown);
        }
        // Worked out before the change: the names it replaces are still there.
        let inverse = if pending.undo.is_some() {
            None
        } else {
            self.inverse_of(&pending.change).await
        };
        let created = match self.apply(&pending.change, origin, actor.clone()).await {
            Ok(created) => created,
            Err(e) => {
                // Not applied: it waits for another try, it is not lost.
                self.0.proposals.put_back(
                    id,
                    Pending {
                        change: pending.change,
                        text: pending.text,
                        undo: pending.undo,
                        asker: pending.asker,
                    },
                );
                return Err(e);
            }
        };
        if pending.undo.is_some() {
            self.0.proposals.pop_applied(); // taken back: not to be undone twice
        } else if let Some((mut inverse, text)) = inverse {
            if let (Change::RemovePerson { id, .. }, Some(new)) = (&mut inverse, created) {
                *id = new;
            }
            self.0.proposals.record(inverse, text);
        }
        // A new name or room changes the prompt: read it again now, not at the next
        // question. One read at a time: a second change while one runs only marks it stale.
        if self
            .0
            .warm_after_change
            .load(std::sync::atomic::Ordering::Relaxed)
            && self.0.warm.request()
        {
            let warming = self.clone();
            tokio::spawn(async move {
                loop {
                    warming.warm_up().await;
                    if !warming.0.warm.finished() {
                        break;
                    }
                }
            });
        }
        tracing::info!(
            proposal = id,
            change = kind_of(&pending.change),
            actor = actor.as_deref().unwrap_or("?"),
            "configuration changed on a person's say"
        );
        Ok(moli_i18n::tr!(
            "assistant.house.done_change",
            text = pending.text
        ))
    }

    async fn apply(
        &self,
        change: &Change,
        origin: Origin,
        actor: Option<String>,
    ) -> Result<Option<String>, ProposalError> {
        let layout_file = || {
            self.0
                .layout_path
                .clone()
                .ok_or_else(|| ProposalError::Failed("no home.json to write".into()))
        };
        let failed = |e: &dyn std::fmt::Display| ProposalError::Failed(e.to_string());
        match change {
            Change::RenameDevice { device, name } => {
                let patch = LabelPatch {
                    name: Some(name.clone()),
                    room: None,
                };
                let done = self.0.hub.set_label(device, patch, origin, actor).await;
                done.map(drop).map_err(label_error)?;
            }
            Change::MoveDevice { device, room } => {
                let patch = LabelPatch {
                    name: None,
                    room: Some(room.clone()),
                };
                let done = self.0.hub.set_label(device, patch, origin, actor).await;
                done.map(drop).map_err(label_error)?;
            }
            Change::SetEnabled { device, enabled } => {
                let (path, id, hidden) = (layout_file()?, device.as_str().to_owned(), !*enabled);
                tokio::task::spawn_blocking(move || layout_edit::set_hidden(&path, &id, hidden))
                    .await
                    .map_err(|e| failed(&e))?
                    .map_err(|e| failed(&e))?;
            }
            Change::RenameRoom { from, to } => {
                let (path, from, to) = (layout_file()?, from.clone(), to.clone());
                tokio::task::spawn_blocking(move || layout_edit::rename_room(&path, &from, &to))
                    .await
                    .map_err(|e| failed(&e))?
                    .map_err(|e| failed(&e))?;
            }
            Change::AddPerson { name } => {
                let directory = self.directory().ok_or_else(|| failed(&"no household"))?;
                return directory.add(name).await.map(Some).map_err(|e| failed(&e));
            }
            Change::RemovePerson { id, name } => {
                let directory = self.directory().ok_or_else(|| failed(&"no household"))?;
                directory
                    .remove_unused(id, name)
                    .await
                    .map_err(|e| failed(&e))?;
            }
            Change::SetLocation { place } => {
                let (path, kept) = (layout_file()?, place.clone());
                tokio::task::spawn_blocking(move || {
                    layout_edit::set_location(&path, kept.as_ref())
                })
                .await
                .map_err(|e| failed(&e))?
                .map_err(|e| failed(&e))?;
                // Forgotten: `moli.toml` speaks again.
                let at = place.as_ref().map(|p| (p.latitude, p.longitude)).or(self
                    .0
                    .config
                    .latitude
                    .zip(self.0.config.longitude));
                *self
                    .0
                    .location
                    .write()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = at;
                let weather = self
                    .0
                    .weather
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if let Some(weather) = weather {
                    // Now for the position (placed, moved or no longer placed); the old
                    // forecast goes in the background (it may wait on a download, and the
                    // other confirmations wait on this one).
                    weather.move_to(at);
                    tokio::spawn(async move { weather.forget_all().await });
                }
                // The sun of the automations follows at once.
                if let Some(automations) = self
                    .0
                    .automations
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .as_ref()
                {
                    automations.set_position(at);
                }
            }
        }
        Ok(None)
    }
}

/// One prompt read at a time after changes: while one runs, further changes only
/// mark the prompt stale and one more read follows (Ollama would queue them all).
#[derive(Debug, Default)]
pub(crate) struct WarmGate(std::sync::Mutex<(bool, bool)>);

impl WarmGate {
    /// A change happened: `true` when the caller is the one to read the prompt now.
    pub(crate) fn request(&self) -> bool {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.0 {
            state.1 = true;
            false
        } else {
            state.0 = true;
            true
        }
    }

    /// A read ended: `true` when the prompt went stale meanwhile, one more read is due.
    pub(crate) fn finished(&self) -> bool {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.1 {
            state.1 = false;
            true
        } else {
            state.0 = false;
            false
        }
    }
}

/// A name as the person wrote it. The small model garbles a name it has not seen
/// (« Rafaël » for « Raphaël »): when a word of the sentence is a close spelling of what
/// it wrote, the person's word wins (a first letter in capital if they typed none).
fn name_from_words(model: &str, question: &str) -> String {
    let wanted = norm(model);
    let limit = (wanted.chars().count() / 3).max(1);
    let best = question
        .split(|c: char| !c.is_alphabetic() && c != '-' && c != '\'')
        .filter(|w| w.chars().count() >= 2)
        .map(|w| (distance(&norm(w), &wanted), w))
        .min_by_key(|(d, _)| *d);
    match best {
        // Same letters: a name the person capitalised is theirs, else the model's accents stand.
        Some((0, word)) if word.chars().next().is_some_and(char::is_uppercase) => word.to_owned(),
        Some((0, _)) | None => model.to_owned(),
        Some((d, word)) if d <= limit => {
            let mut letters = word.chars();
            match letters.next() {
                Some(first) if first.is_lowercase() => {
                    first.to_uppercase().chain(letters).collect()
                }
                _ => word.to_owned(),
            }
        }
        Some(_) => model.to_owned(),
    }
}

/// Edits (insert, delete, change one letter) from one word to the other.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.chars().enumerate() {
        let mut last = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (last + usize::from(x != *y)).min(above + 1).min(row[j] + 1);
            last = above;
        }
    }
    row[b.len()]
}

/// Whether the person asks for something later (« dans 5 minutes », « minuteur »,
/// « rappelle-moi », « à 18 h »). Until Moli can schedule, such an order is not carried
/// out now: the small model has no clock and would act at once.
// ponytail: word list, replaced by `schedule`/`remind` in contract v4.
fn later(question: &str) -> bool {
    const WORDS: [&str; 6] = [
        "minuteur", "rappelle", "rappel ", "timer", "remind", "chrono",
    ];
    const UNITS: [&str; 8] = [
        "min", "minute", "minutes", "h", "heure", "heures", "seconde", "secondes",
    ];
    const COUNTS: [&str; 14] = [
        "un",
        "une",
        "deux",
        "trois",
        "cinq",
        "dix",
        "quinze",
        "vingt",
        "trente",
        "quart",
        "demi",
        "demi-heure",
        "ten",
        "five",
    ];
    let text = norm(question).replace(['\'', ',', '.', '!', '?'], " ");
    if WORDS.iter().any(|w| text.contains(w)) {
        return true;
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    words.windows(3).any(|w| {
        let count = w[1].chars().all(|c| c.is_ascii_digit()) || COUNTS.contains(&w[1]);
        let unit =
            UNITS.contains(&w[2]) || matches!(w[2], "quart" | "quarts" | "demi" | "demi-heure");
        matches!(w[0], "dans" | "in" | "ici") && count && (unit || w[1] == "demi-heure")
    }) || words.windows(2).any(|w| {
        // « dans 1h », « dans 10min »
        matches!(w[0], "dans" | "in" | "ici")
            && (w[1].strip_suffix('h').or_else(|| w[1].strip_suffix("min")))
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    }) || words.windows(2).any(|w| {
        // « à 18h », « à 21 h 30 »
        w[0] == "a"
            && (w[1].ends_with('h')
                && w[1].len() > 1
                && w[1][..w[1].len() - 1].chars().all(|c| c.is_ascii_digit()))
    }) || words.windows(3).any(|w| {
        w[0] == "a" && w[1].chars().all(|c| c.is_ascii_digit()) && !w[1].is_empty() && w[2] == "h"
    })
}

/// What a change is, for the log (never the names and places it carries).
fn kind_of(change: &Change) -> &'static str {
    match change {
        Change::RenameDevice { .. } => "rename_device",
        Change::MoveDevice { .. } => "move_device",
        Change::SetEnabled { .. } => "set_device_enabled",
        Change::RenameRoom { .. } => "rename_room",
        Change::AddPerson { .. } => "add_person",
        Change::RemovePerson { .. } => "remove_person",
        Change::SetLocation { .. } => "set_home_location",
    }
}

/// What the model is shown after the prompt: the question, and from contract v4 on (a
/// model trained on dialogues) the last exchanges before it: « éteins-la », « et dans
/// la chambre ? » need what was just said.
fn dialogue(turn: &Turn, question: &str, with_context: bool) -> Vec<Json> {
    let mut out = Vec::new();
    if with_context {
        let before = &turn.messages[..turn.messages.len().saturating_sub(1)];
        out.extend(
            before[before.len().saturating_sub(2 * CONTEXT_EXCHANGES)..]
                .iter()
                .map(|m| {
                    let content: String = m.content.chars().take(MAX_CONTEXT_CHARS).collect();
                    json!({ "role": m.role, "content": content })
                }),
        );
    }
    out.push(json!({ "role": "user", "content": question }));
    out
}

/// A colour as a lamp takes it: a hex code, or a white as a temperature (mired).
pub(crate) enum Colour {
    Hex(&'static str),
    White(u32),
}

/// French and English colour names, accents and case aside.
pub(crate) fn colour(said: &str) -> Option<Colour> {
    const NAMES: [(&str, &str); 22] = [
        ("rouge", "#ff0000"),
        ("red", "#ff0000"),
        ("vert", "#00ff00"),
        ("green", "#00ff00"),
        ("bleu", "#0000ff"),
        ("blue", "#0000ff"),
        ("jaune", "#ffff00"),
        ("yellow", "#ffff00"),
        ("orange", "#ff8000"),
        ("violet", "#8000ff"),
        ("purple", "#8000ff"),
        ("rose", "#ff4080"),
        ("pink", "#ff4080"),
        ("cyan", "#00ffff"),
        ("magenta", "#ff00ff"),
        ("turquoise", "#40e0d0"),
        ("indigo", "#4b0082"),
        ("ambre", "#ffbf00"),
        ("amber", "#ffbf00"),
        ("blanc", "#ffffff"),
        ("white", "#ffffff"),
        ("or", "#ffd700"),
    ];
    const WHITES: [(&str, u32); 6] = [
        ("blanc chaud", 370),
        ("warm white", 370),
        ("blanc froid", 200),
        ("cool white", 200),
        ("blanc neutre", 285),
        ("neutral white", 285),
    ];
    let said = norm(said).trim().to_owned();
    if let Some((_, mired)) = WHITES.iter().find(|(n, _)| *n == said) {
        return Some(Colour::White(*mired));
    }
    NAMES
        .iter()
        .find(|(n, _)| *n == said)
        .map(|(_, hex)| Colour::Hex(hex))
}

/// Whether a device already has the value asked (numbers within a hair, words aside case).
pub(crate) fn same(now: &moli_core::Value, asked: &Json) -> bool {
    match (now, asked) {
        (moli_core::Value::Bool(a), Json::Bool(b)) => a == b,
        (moli_core::Value::Int(a), Json::Number(b)) => b.as_i64() == Some(*a),
        (moli_core::Value::Float(a), Json::Number(b)) => {
            b.as_f64().is_some_and(|b| (a - b).abs() < 1e-6)
        }
        (moli_core::Value::Text(a), Json::String(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

/// The weather service is asked again at most this often by « actualise la météo ».
const WEATHER_FLOOR: std::time::Duration = std::time::Duration::from_secs(5 * 60);
/// Calls of one answer carried out, at most (« allume le salon et ferme le volet »).
const MAX_CALLS: usize = 4;
/// Exchanges of the thread the model is shown before the question, and their length.
const CONTEXT_EXCHANGES: usize = 2;
const MAX_CONTEXT_CHARS: usize = 300;
/// Calls that propose a change of configuration (a person decides).
const PROPOSING: [&str; 7] = [
    "rename_device",
    "move_device",
    "set_device_enabled",
    "rename_room",
    "add_person",
    "set_home_location",
    "undo",
];

/// A person's name, as the People screen takes it.
const MAX_PERSON: usize = 40;
/// Longest request for a driver, in characters.
const MAX_REQUEST: usize = 500;
/// What the house model may write in one step without carrying a question.
const HOUSE_WORDS: usize = 200;
/// The requests file stops growing past this many bytes.
const MAX_REQUESTS_FILE: u64 = 1024 * 1024;

/// Why a decision did not go through.
#[derive(Debug)]
pub enum ProposalError {
    /// No such proposal: decided already, expired, or never made.
    Unknown,
    /// An agent or a script tried to decide: only a person does.
    HumanOnly,
    /// A person who is not an owner tried to add someone to the household.
    OwnersOnly,
    /// The house refused the change (its guard, its rules).
    Refused(String),
    /// The change could not be written.
    Failed(String),
}

impl std::fmt::Display for ProposalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unknown => f.write_str("this proposal is not there any more"),
            Self::HumanOnly => f.write_str("only a person decides, from the dashboard"),
            Self::OwnersOnly => f.write_str("only an owner adds someone to the household"),
            Self::Refused(why) | Self::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for ProposalError {}

fn label_error(e: LabelError) -> ProposalError {
    match e {
        LabelError::Guarded(why) => ProposalError::Refused(why),
        other => ProposalError::Failed(other.to_string()),
    }
}

/// A name a person will read (and the model reads on every turn): not empty,
/// not too long, no line break or control character (a name is never a place to
/// hide an instruction), and not what it already is.
fn valid_name(written: &Json, current: &str) -> Option<String> {
    let raw = written.as_str()?;
    if raw.chars().any(char::is_control) {
        return None;
    }
    let name = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    (!name.is_empty() && name.chars().count() <= MAX_LABEL && norm(&name) != norm(current))
        .then_some(name)
}

/// The contract's tools, as the model is sent them (embedded at build time).
fn contract_tools() -> Json {
    serde_json::from_str::<Json>(contract::TOOLS_JSON)
        .map_or_else(|_| json!([]), |d| d["tools"].clone())
}

/// « Je propose : … » (confirmed on the screen; a satellite has none: the dashboard).
fn propose_words(text: &str, spoken: bool) -> String {
    if spoken {
        moli_i18n::tr!("assistant.house.propose_spoken", text = text)
    } else {
        moli_i18n::tr!("assistant.house.propose", text = text)
    }
}

pub(crate) fn unknown_words() -> String {
    moli_i18n::tr!("assistant.reply.unknown")
}

/// The model's words, as long as they are words: a thinking block is dropped,
/// and a call the server could not parse (cut short, badly formed) is not read
/// out loud: nothing is better than `<tool_call>{"name"…`.
fn plain_words(content: &str) -> String {
    let mut text = content.to_owned();
    while let (Some(start), Some(end)) = (text.find("<think>"), text.find("</think>")) {
        if end < start {
            break;
        }
        text.replace_range(start..end + "</think>".len(), "");
    }
    let text = text.trim();
    if text.contains("<tool_call>") || text.starts_with('{') {
        return String::new();
    }
    text.to_owned()
}

/// Values Moli never reads aloud: radio, calibration and update noise (a label
/// matches whatever its case or wording: `Linkquality`, `Temperature calibration`).
const NOISE: [&str; 4] = ["linkquality", "calibration", "rssi", "update"];

fn is_noise(label: &str) -> bool {
    let label = label.to_lowercase();
    NOISE.iter().any(|n| label.contains(n)) || house::noise(&label)
}

/// What a point is called in French, and the stems of the words that ask for it.
const POINTS: [(&str, &str, &[&str]); 12] = [
    (
        "temperature",
        "température",
        &["temperat", "degre", "chaud", "froid"],
    ),
    ("humidity", "humidité", &["humid"]),
    ("battery", "batterie", &["batter", "=pile"]),
    ("power", "puissance", &["puissance", "consomm", "watt"]),
    ("energy", "énergie", &["energie", "kwh"]),
    ("brightness", "luminosité", &["lumin", "intensit"]),
    ("occupancy", "présence", &["presen", "mouvement", "=quelqu"]),
    ("pressure", "pression", &["pression"]),
    ("co2", "CO₂", &["co2", "=air"]),
    ("illuminance", "éclairement", &["eclair", "=lux"]),
    ("contact", "contact", &["ouvert", "ferme"]),
    ("voltage", "tension", &["tension", "volt"]),
];
/// Points that say whether a thing is on: said « allumé » or « éteint ».
const STATES: [&str; 2] = ["state", "on"];
const STATE_STEMS: [&str; 4] = ["allum", "eteint", "etat", "=marche"];

/// A device's current values, said in French: only what the question asks for
/// (the temperature when it asks for the temperature), else the main values.
/// `Capteur salon : température 19,1 °C.`
fn reading_words(device: &Json, question: &str) -> String {
    if device.get("error").is_some() {
        return moli_i18n::tr!("assistant.house.not_found");
    }
    let name = device["name"].as_str().unwrap_or_default();
    if device["online"] == false {
        return moli_i18n::tr!("assistant.house.unreachable", device = name);
    }
    let asked = norm(question);
    let asked_words: Vec<&str> = asked
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let points: Vec<&Json> = device["points"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| !p["value"].is_null())
        .filter(|p| !is_noise(p["label"].as_str().unwrap_or_default()))
        .filter(|p| !is_noise(p["key"].as_str().unwrap_or_default()))
        .collect();
    let key = |p: &Json| p["key"].as_str().unwrap_or_default().to_lowercase();
    let wanted = |p: &&Json| {
        let key = key(p);
        let stems: &[&str] = POINTS.iter().find(|(k, _, _)| *k == key).map_or(
            if STATES.contains(&key.as_str()) {
                &STATE_STEMS
            } else {
                &[]
            },
            |(_, _, s)| s,
        );
        // A stem opens a word (« temperat » in « température »); `=` wants the whole
        // word (« air » is not in « faire » or « éclairage »).
        stems.iter().any(|stem| match stem.strip_prefix('=') {
            Some(whole) => asked_words.contains(&whole),
            None => asked_words.iter().any(|w| w.starts_with(stem)),
        })
    };
    let asked_for: Vec<&Json> = points.iter().copied().filter(|p| wanted(p)).collect();
    let chosen = if asked_for.is_empty() {
        points.iter().copied().take(6).collect()
    } else {
        asked_for
    };
    let parts: Vec<String> = chosen.iter().map(|p| point_words(p, &key(p))).collect();
    if parts.is_empty() {
        return moli_i18n::tr!("assistant.house.no_reading", device = name);
    }
    format!("{name} : {}.", parts.join(", "))
}

/// One value: `température 19,1 °C`, `allumé`, `batterie 100 %`.
fn point_words(p: &Json, key: &str) -> String {
    if STATES.contains(&key)
        && let Some(on) = p["value"].as_bool()
    {
        return moli_i18n::tr!(if on {
            "assistant.house.is_on"
        } else {
            "assistant.house.is_off"
        });
    }
    let label = POINTS.iter().find(|(k, _, _)| *k == key).map_or_else(
        || p["label"].as_str().unwrap_or_default().to_lowercase(),
        |(_, fr, _)| (*fr).to_owned(),
    );
    let value = match &p["value"] {
        Json::Bool(true) => moli_i18n::tr!("assistant.house.yes"),
        Json::Bool(false) => moli_i18n::tr!("assistant.house.no"),
        Json::String(text) => text.clone(),
        Json::Number(n) => {
            let figure = n.as_f64().map_or_else(|| n.to_string(), tenths);
            format!("{figure} {}", p["unit"].as_str().unwrap_or_default())
                .trim()
                .to_owned()
        }
        other => other.to_string(),
    };
    format!("{label} {value}")
}

/// A figure to the tenth, the way it is said: `21,3`, `60`.
fn tenths(x: f64) -> String {
    let rounded = (x * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.1}").replace('.', ",")
    }
}

/// What became of a `set`, in Moli's words (the model writes none).
fn order_words(result: &Json, device: &str) -> String {
    match result["status"].as_str() {
        Some("done") => moli_i18n::tr!("assistant.house.done", device = device),
        Some("awaiting_approval") => moli_i18n::tr!("assistant.house.held", device = device),
        // The Hub's reason is English and stays in the action, which the screen shows.
        Some(_) => moli_i18n::tr!("assistant.house.failed", device = device),
        // Not even an order (a call cut short, no point): nothing to explain.
        None => unknown_words(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::directory::Directory as _;
    use std::sync::Arc;

    use super::*;
    use crate::Config;
    use crate::llm::tests::fake_provider;

    /// A house-model assistant over the lamp hub (`d1` is the lamp); its provider answers `answers` in turn.
    pub(crate) async fn house_model_over(
        hub: moli_runtime::Hub,
        answers: Vec<&str>,
    ) -> (Assistant, Arc<std::sync::Mutex<Vec<String>>>) {
        house_model_in(hub, None, answers).await
    }

    pub(crate) async fn house_model_in(
        hub: moli_runtime::Hub,
        layout: Option<std::path::PathBuf>,
        answers: Vec<&str>,
    ) -> (Assistant, Arc<std::sync::Mutex<Vec<String>>>) {
        let (url, asked) = fake_provider(answers.into_iter().map(str::to_owned).collect()).await;
        let config: Config = toml::from_str(&format!(
            "[[provider]]\nid = \"house\"\nbase_url = \"{url}\"\nmodel = \"moli-maison\"\ntier = 1\ncontract = \"moli-v4\"\n\n[routes]\nchat = \"house\"\n"
        ))
        .unwrap();
        let moli = Assistant::new(hub, None, None, layout, config).unwrap();
        // Scripted providers answer in arrival order: no background warm-up here.
        moli.0
            .warm_after_change
            .store(false, std::sync::atomic::Ordering::Relaxed);
        (moli, asked)
    }

    #[tokio::test]
    async fn an_old_contract_puts_the_conversation_out_of_service_with_its_reason() {
        let (url, asked) = fake_provider(vec![words("bonjour")]).await;
        let config: Config = toml::from_str(&format!(
            "[[provider]]\nid = \"house\"\nbase_url = \"{url}\"\nmodel = \"moli-maison\"\ntier = 1\ncontract = \"moli-v1\"\n\n[routes]\nchat = \"house\"\n"
        ))
        .unwrap();
        let moli =
            Assistant::new(crate::tests::lamp_hub().await, None, None, None, config).unwrap();
        let status = moli.status();
        assert_eq!(status["ready"], false);
        let why = status["refused"].as_str().unwrap().to_owned();
        assert!(why.contains("moli-v1") && why.contains("house"), "{why}");
        // No fallback to a general conversation: the turn says why, nothing is asked.
        let e = moli.turn(ask("Allume la lampe")).await.unwrap_err();
        assert!(
            matches!(&e, AssistantError::NotConfigured(w) if *w == why),
            "{e}"
        );
        assert!(asked.lock().unwrap().is_empty());
    }

    pub(crate) async fn house_model(
        answers: Vec<&str>,
    ) -> (Assistant, Arc<std::sync::Mutex<Vec<String>>>) {
        house_model_over(crate::tests::lamp_hub().await, answers).await
    }

    pub(crate) fn call(name: &str, arguments: &str) -> String {
        json!({ "choices": [{ "message": { "role": "assistant", "content": null,
            "tool_calls": [{ "id": "c1", "type": "function",
                "function": { "name": name, "arguments": arguments } }] } }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 } })
        .to_string()
    }

    pub(crate) fn words(text: &str) -> String {
        json!({ "choices": [{ "message": { "role": "assistant", "content": text } }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5 } })
        .to_string()
    }

    pub(crate) fn ask(question: &str) -> Turn {
        serde_json::from_value(json!({ "messages": [{ "role": "user", "content": question }] }))
            .unwrap()
    }

    fn spoken_ask(question: &str) -> Turn {
        serde_json::from_value(
            json!({ "spoken": true, "messages": [{ "role": "user", "content": question }] }),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn with_an_agent_a_long_question_has_room_to_be_handed_over() {
        let long = "trouve-moi une recette de rougail saucisse pour huit personnes ; ".repeat(12);
        let (moli, asked) = house_model(vec![&words("D'accord.")]).await;
        moli.turn(ask(&long)).await.unwrap();
        assert_eq!(body_of(&asked.lock().unwrap()[0])["max_tokens"], 200);
        let (url, asked) = fake_provider(vec![words("D'accord.")]).await;
        // An agent at home, never asked here (the model answers in words).
        let config: Config = toml::from_str(&format!(
            "[[provider]]\nid = \"house\"\nbase_url = \"{url}\"\nmodel = \"moli-maison\"\ntier = 1\ncontract = \"moli-v4\"\n\n[routes]\nchat = \"house\"\n\n[hermes]\nurl = \"http://127.0.0.1:9\"\n"
        ))
        .unwrap();
        let moli = Assistant::new(
            crate::tests::hub_with_hermes_token("maison").await,
            None,
            None,
            None,
            config,
        )
        .unwrap();
        moli.0
            .warm_after_change
            .store(false, std::sync::atomic::Ordering::Relaxed);
        moli.turn(ask(&long)).await.unwrap();
        assert_eq!(
            body_of(&asked.lock().unwrap()[0])["max_tokens"],
            json!(200 + long.chars().count())
        );
    }

    fn body_of(request: &str) -> Json {
        serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap()
    }

    fn unknown() -> String {
        moli_i18n::tr!("assistant.reply.unknown")
    }

    #[tokio::test]
    async fn the_model_is_sent_the_static_prompt_and_the_twenty_five_tools() {
        let reply = words("Bonjour !");
        let (moli, asked) = house_model(vec![&reply]).await;
        moli.turn(ask("Bonjour")).await.unwrap();
        let body = body_of(&asked.lock().unwrap()[0]);
        assert_eq!(body["model"], "moli-maison");
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        let system = body["messages"][0]["content"].as_str().unwrap();
        assert!(system.starts_with("Contrat Moli v4."), "{system}");
        assert!(system.contains("Appareils :"), "{system}");
        assert!(
            system.contains("- d1 · lamp · on✎"),
            "the numbered inventory: {system}"
        );
        assert!(
            !system.contains("fake:lamp"),
            "real identifiers stay in Moli: {system}"
        );
        assert!(!system.contains("Date"), "nothing that moves: {system}");
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 25);
        let set = tools
            .iter()
            .find(|t| t["function"]["name"] == "set")
            .unwrap();
        assert!(
            set["function"]["parameters"]["properties"]
                .get("say")
                .is_none()
        );
    }

    #[tokio::test]
    async fn the_prompt_does_not_move_when_the_house_does_its_things() {
        let order = call("set", r#"{"point":"d1/on","value":true}"#);
        let after = words("Deux");
        let (moli, asked) = house_model(vec![&order, &after]).await;
        moli.turn(ask("Allume la lampe")).await.unwrap(); // the lamp's state changes here
        moli.turn(ask("Salut")).await.unwrap();
        let asked = asked.lock().unwrap();
        assert_eq!(
            body_of(&asked[0])["messages"][0],
            body_of(&asked[1])["messages"][0],
            "a prompt that never moves is a prompt the provider keeps in cache"
        );
    }

    #[tokio::test]
    async fn warming_up_reads_the_prompt_once_so_the_first_question_is_not_slow() {
        let answer = words("ok");
        let (moli, asked) = house_model(vec![&answer, &answer]).await;
        moli.warm_up().await;
        moli.turn(ask("Bonjour")).await.unwrap();
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 2);
        let (warm, turn) = (body_of(&asked[0]), body_of(&asked[1]));
        assert_eq!(
            warm["max_tokens"], 1,
            "only the prompt is read, nothing is generated"
        );
        assert_eq!(
            warm["messages"][0], turn["messages"][0],
            "the very prompt a turn sends: that is what the provider keeps"
        );
        assert_eq!(warm["tools"], turn["tools"]);
    }

    #[tokio::test]
    async fn warming_up_a_house_model_that_does_not_answer_is_silent() {
        let (moli, _) = house_model(vec![]).await; // the provider answers 500
        moli.warm_up().await; // nothing to report, nothing to crash
    }

    /// Several `delegate` in one answer: one task handed over, the others
    /// declined in words.
    #[tokio::test]
    async fn the_house_model_hands_over_once_a_turn() {
        let started = |id: &str| json!({ "id": id, "status": "started" }).to_string();
        let (agent, agent_asked) = fake_provider(vec![started("run-a"), started("run-b")]).await;
        let (house, _) = fake_provider(vec![calls(&[
            ("delegate", r#"{"question":"Un vol pour Paris ?"}"#),
            ("delegate", r#"{"question":"Et un hôtel ?"}"#),
        ])])
        .await;
        let config: Config = toml::from_str(&format!(
            "[[provider]]
id = \"house\"
base_url = \"{house}\"
model = \"moli-maison\"
tier = 1
contract = \"moli-v4\"

[routes]
chat = \"house\"

[hermes]
url = \"{agent}\"
"
        ))
        .unwrap();
        let moli = Assistant::new(
            crate::tests::hub_with_hermes_token("une-fois").await,
            None,
            None,
            None,
            config,
        )
        .unwrap();
        moli.0
            .warm_after_change
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let reply = moli
            .turn(ask("Trouve-moi un vol et un hôtel"))
            .await
            .unwrap();
        assert_eq!(agent_asked.lock().unwrap().len(), 1);
        assert!(
            reply
                .reply
                .contains(&moli_i18n::tr!("assistant.house.delegate_once")),
            "{}",
            reply.reply
        );
    }

    #[tokio::test]
    async fn a_general_model_is_not_warmed_up() {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let moli = Assistant::new(hub, None, None, None, Config::default()).unwrap();
        moli.warm_up().await; // no contract: returns at once, no request
    }

    #[tokio::test]
    async fn a_question_out_of_frame_is_not_yet_handed_to_another_assistant() {
        // Even with an `audit` provider configured, nothing leaves the house (#17).
        let (house_url, _) = fake_provider(vec![call(
            "delegate",
            r#"{"question":"Raconte-moi une blague."}"#,
        )])
        .await;
        let (general_url, general_asked) = fake_provider(vec![words("une blague")]).await;
        let config: Config = toml::from_str(&format!(
            "[[provider]]\nid = \"house\"\nbase_url = \"{house_url}\"\nmodel = \"moli-maison\"\ntier = 1\ncontract = \"moli-v4\"\n\n\
             [[provider]]\nid = \"general\"\nbase_url = \"{general_url}\"\nmodel = \"gros-modele\"\ntier = 3\n\n\
             [routes]\nchat = \"house\"\naudit = \"general\"\n"
        ))
        .unwrap();
        let moli =
            Assistant::new(crate::tests::lamp_hub().await, None, None, None, config).unwrap();
        moli.0
            .warm_after_change
            .store(false, std::sync::atomic::Ordering::Relaxed);
        let reply = moli.turn(ask("Raconte-moi une blague")).await.unwrap();
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.delegate_later")
        );
        assert!(
            general_asked.lock().unwrap().is_empty(),
            "nothing left the house"
        );
    }

    #[tokio::test]
    async fn the_thread_reaches_the_model_after_the_prompt() {
        let reply = words("Salut");
        let (moli, asked) = house_model(vec![&reply]).await;
        let turn: Turn = serde_json::from_value(json!({ "messages": [
            { "role": "user", "content": "Allume la lampe" },
            { "role": "assistant", "content": "C'est fait." },
            { "role": "user", "content": "Éteins-la" } ] }))
        .unwrap();
        moli.turn(turn).await.unwrap();
        let body = body_of(&asked.lock().unwrap()[0]);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[3]["content"], "Éteins-la");
    }

    #[tokio::test]
    async fn an_order_is_written_with_a_number_and_carried_out_on_the_real_device() {
        let answer = call("set", r#"{"point":"d1/on","value":true}"#);
        let (moli, asked) = house_model(vec![&answer]).await;
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        assert_eq!(reply.actions[0].status, "done");
        assert_eq!(reply.actions[0].point.as_str(), "fake:lamp/on");
        assert!(
            reply.reply.contains("C’est fait") && reply.reply.contains("lamp"),
            "{}",
            reply.reply
        );
        assert_eq!(
            reply.cards,
            vec![json!({ "kind": "device", "id": "fake:lamp" })]
        );
        assert_eq!(reply.usage.steps, 1);
        assert_eq!(asked.lock().unwrap().len(), 1, "one round trip");
    }

    #[tokio::test]
    async fn an_order_that_failed_is_said_by_moli_not_sent_back_to_the_model() {
        let answer = call("set", r#"{"point":"d1/nothing","value":true}"#);
        let (moli, asked) = house_model(vec![&answer]).await;
        let reply = moli.turn(ask("Allume le rien")).await.unwrap();
        assert!(
            reply.reply.contains("pas pu") && reply.reply.contains("lamp"),
            "{}",
            reply.reply
        );
        assert_eq!(reply.actions[0].status, "failed");
        assert_eq!(asked.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_number_that_is_no_device_is_not_found() {
        let answer = call("set", r#"{"point":"d9/on","value":true}"#);
        let (moli, _) = house_model(vec![&answer]).await;
        let reply = moli.turn(ask("Allume la lampe fantôme")).await.unwrap();
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.not_found"));
        assert!(reply.actions.is_empty());
    }

    #[tokio::test]
    async fn an_unreachable_device_is_said_unreachable_and_nothing_is_sent_to_it() {
        let answer = call("set", r#"{"point":"d1/on","value":true}"#);
        let hub = crate::tests::lamp_hub_with(true).await;
        let (moli, _) = house_model_over(hub, vec![&answer]).await;
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.unreachable", device = "lamp")
        );
        assert!(reply.actions.is_empty());
    }

    #[tokio::test]
    async fn words_alone_are_the_answer_a_refusal_or_a_question() {
        let answer = words("Chambre 1 est une pièce protégée, je ne la commande pas.");
        let (moli, _) = house_model(vec![&answer]).await;
        let reply = moli
            .turn(ask("Allume la lampe de la chambre 1"))
            .await
            .unwrap();
        assert_eq!(
            reply.reply,
            "Chambre 1 est une pièce protégée, je ne la commande pas."
        );
        assert!(reply.actions.is_empty() && reply.cards.is_empty());
    }

    #[derive(Debug, Default)]
    struct FakeDirectory {
        people: std::sync::Mutex<Vec<(String, String)>>,
        busy: std::sync::Mutex<Vec<String>>,
    }

    impl crate::Directory for FakeDirectory {
        fn names(&self) -> Vec<String> {
            self.people
                .lock()
                .unwrap()
                .iter()
                .map(|p| p.1.clone())
                .collect()
        }
        fn add<'a>(&'a self, name: &'a str) -> crate::DirectoryDone<'a, String> {
            Box::pin(async move {
                let id = name.to_lowercase();
                self.people
                    .lock()
                    .unwrap()
                    .push((id.clone(), name.to_owned()));
                Ok(id)
            })
        }
        fn remove_unused<'a>(&'a self, id: &'a str, _: &'a str) -> crate::DirectoryDone<'a, ()> {
            Box::pin(async move {
                if self.busy.lock().unwrap().iter().any(|b| b == id) {
                    return Err("set up".into());
                }
                self.people.lock().unwrap().retain(|p| p.0 != id);
                Ok(())
            })
        }
    }

    #[derive(Debug)]
    struct FakePlaces(Option<crate::Place>);

    impl crate::Places for FakePlaces {
        fn find<'a>(&'a self, _name: &'a str) -> crate::places::Found<'a> {
            let found = self.0.clone();
            Box::pin(async move { Ok(found) })
        }
    }

    #[derive(Debug)]
    struct Down;

    impl crate::Places for Down {
        fn find<'a>(&'a self, _: &'a str) -> crate::places::Found<'a> {
            Box::pin(async { Err(anyhow::anyhow!("offline")) })
        }
    }

    /// A service that knows one town, by its exact (accent- and case-blind) name.
    #[derive(Debug)]
    struct KnowsOnly(crate::Place, &'static str);

    impl crate::Places for KnowsOnly {
        fn find<'a>(&'a self, name: &'a str) -> crate::places::Found<'a> {
            let known = norm(name) == norm(self.1);
            let found = known.then(|| self.0.clone());
            Box::pin(async move { Ok(found) })
        }
    }

    fn lyon() -> crate::Place {
        crate::Place {
            label: "Lyon (Auvergne-Rhône-Alpes, France)".into(),
            latitude: 45.76,
            longitude: 4.84,
        }
    }

    #[tokio::test]
    async fn only_an_owner_confirms_a_new_person_and_the_proposal_waits_for_one() {
        let answer = call("add_person", r#"{"name":"Léa"}"#);
        let (moli, _) = house_model(vec![&answer]).await;
        let people = Arc::new(FakeDirectory::default());
        moli.set_directory(people.clone());
        let reply = moli.turn(ask("Ajoute Léa au foyer")).await.unwrap();
        let id = reply.proposals[0].id;
        assert!(matches!(
            moli.decide_proposal_by(id, true, Origin::Ui, None, false)
                .await,
            Err(ProposalError::OwnersOnly)
        ));
        assert!(people.names().is_empty());
        // Not consumed: an owner still confirms it.
        moli.decide_proposal_by(id, true, Origin::Ui, None, true)
            .await
            .unwrap();
        assert_eq!(people.names(), vec!["Léa".to_owned()]);
    }

    #[tokio::test]
    async fn a_person_is_added_only_when_confirmed_and_taken_back_by_an_undo() {
        let answer = call("add_person", r#"{"name":"Léa"}"#);
        let undo = call("undo", "{}");
        let (moli, _) = house_model(vec![&answer, &undo]).await;
        let people = Arc::new(FakeDirectory::default());
        moli.set_directory(people.clone());
        let reply = moli.turn(ask("Ajoute Léa au foyer")).await.unwrap();
        assert_eq!(reply.proposals.len(), 1);
        assert!(
            reply.proposals[0].text.contains("« Léa »"),
            "{}",
            reply.proposals[0].text
        );
        assert!(
            people.names().is_empty(),
            "nothing before a person says yes"
        );
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(people.names(), vec!["Léa".to_owned()]);
        let reply = moli.turn(ask("Fait l'inverse")).await.unwrap();
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert!(people.names().is_empty());
    }

    #[tokio::test]
    async fn a_name_the_model_garbled_is_taken_from_the_persons_own_words() {
        for (said, written, shown) in [
            ("Ajoute Raphaël dans la maison", "Rafaël", "« Raphaël »"),
            ("Ajoute Michael à la maison", "Michaël", "« Michael »"),
            ("ajoute michael à la maison", "Michaël", "« Michaël »"),
            ("Ajoute lea au foyer", "Léa", "« Léa »"),
            ("Il y a un nouveau, Hugo", "Hugo", "« Hugo »"),
            ("Ajoute quelqu'un au foyer", "Zoé", "« Zoé »"),
        ] {
            let answer = call("add_person", &json!({ "name": written }).to_string());
            let (moli, _) = house_model(vec![&answer]).await;
            moli.set_directory(Arc::new(FakeDirectory::default()));
            let reply = moli.turn(ask(said)).await.unwrap();
            assert!(
                reply.proposals[0].text.contains(shown),
                "{said}: {}",
                reply.proposals[0].text
            );
        }
    }

    #[tokio::test]
    async fn a_person_already_in_the_household_or_with_a_bad_name_is_not_proposed() {
        for (name, expected) in [
            (
                "léa",
                moli_i18n::tr!("assistant.house.person_known", name = "léa"),
            ),
            ("", unknown()),
            ("Lé\na", unknown()),
        ] {
            let answer = call("add_person", &json!({ "name": name }).to_string());
            let (moli, _) = house_model(vec![&answer]).await;
            let people = Arc::new(FakeDirectory::default());
            people.add("Léa").await.unwrap();
            moli.set_directory(people);
            let reply = moli.turn(ask("Ajoute")).await.unwrap();
            assert!(reply.proposals.is_empty(), "{name:?}");
            assert_eq!(reply.reply, expected, "{name:?}");
        }
    }

    #[tokio::test]
    async fn without_the_household_a_person_cannot_be_added() {
        let answer = call("add_person", r#"{"name":"Léa"}"#);
        let (moli, _) = house_model(vec![&answer]).await;
        let reply = moli.turn(ask("Ajoute Léa")).await.unwrap();
        assert!(reply.proposals.is_empty());
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.unavailable"));
    }

    #[tokio::test]
    async fn an_addition_somebody_built_on_is_not_undone() {
        let answer = call("add_person", r#"{"name":"Léa"}"#);
        let undo = call("undo", "{}");
        let (moli, _) = house_model(vec![&answer, &undo]).await;
        let people = Arc::new(FakeDirectory::default());
        moli.set_directory(people.clone());
        let reply = moli.turn(ask("Ajoute Léa")).await.unwrap();
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        people.busy.lock().unwrap().push("léa".into());
        let reply = moli.turn(ask("Annule")).await.unwrap();
        let refused = moli
            .decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await;
        assert!(
            matches!(refused, Err(ProposalError::Failed(_))),
            "{refused:?}"
        );
        assert_eq!(people.names(), vec!["Léa".to_owned()]);
    }

    #[tokio::test]
    async fn the_house_moves_only_when_confirmed_and_an_undo_forgets_the_choice() {
        let answer = call("set_home_location", r#"{"place":"Lyon"}"#);
        let undo = call("undo", "{}");
        let (moli, path) =
            with_layout(crate::tests::lamp_hub().await, "{}", vec![&answer, &undo]).await;
        moli.set_places(Arc::new(FakePlaces(Some(lyon()))));
        // Moli started with no position at all: no sky, no sun.
        let weather = moli_weather::Weather::unplaced(None);
        moli.set_weather(weather.clone());
        let (autos, _engine) = moli_automation::Automations::start(
            &crate::tests::lamp_hub().await,
            moli_automation::Config::default(),
            None,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        moli.set_automations(autos.clone());
        let reply = moli.turn(ask("J'habite à Lyon")).await.unwrap();
        assert!(
            reply.proposals[0]
                .text
                .contains("Lyon (Auvergne-Rhône-Alpes, France)")
        );
        assert_eq!(moli.position(), None, "nothing before a person says yes");
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(moli.position(), Some((45.76, 4.84)));
        // The dashboard's sky and the automations' sun follow, without a restart.
        assert_eq!(weather.position(), Some((45.76, 4.84)));
        assert_eq!(autos.position(), Some((45.76, 4.84)));
        assert_eq!(crate::layout_edit::saved_place(&path), Some(lyon()));
        let reply = moli.turn(ask("Fait l'inverse")).await.unwrap();
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(moli.position(), None);
        assert_eq!(weather.position(), None, "no position any more");
        assert_eq!(autos.position(), None);
        assert_eq!(crate::layout_edit::saved_place(&path), None);
    }

    #[tokio::test]
    async fn a_second_choice_of_place_is_undone_back_to_the_first() {
        let first = call("set_home_location", r#"{"place":"Lyon"}"#);
        let second = call("set_home_location", r#"{"place":"Paris"}"#);
        let undo = call("undo", "{}");
        let (moli, path) = with_layout(
            crate::tests::lamp_hub().await,
            "{}",
            vec![&first, &second, &undo],
        )
        .await;
        moli.set_places(Arc::new(FakePlaces(Some(lyon()))));
        let reply = moli.turn(ask("Lyon")).await.unwrap();
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        moli.set_places(Arc::new(FakePlaces(Some(crate::Place {
            label: "Paris (Île-de-France, France)".into(),
            latitude: 48.85,
            longitude: 2.35,
        }))));
        let reply = moli.turn(ask("Paris")).await.unwrap();
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(moli.position(), Some((48.85, 2.35)));
        let reply = moli.turn(ask("Fait l'inverse")).await.unwrap();
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(crate::layout_edit::saved_place(&path), Some(lyon()));
        assert_eq!(moli.position(), Some((45.76, 4.84)));
    }

    #[tokio::test]
    async fn a_garbled_town_is_found_again_from_the_persons_own_words() {
        let villeneuve = crate::Place {
            label: "Villeneuve-d'Ormeval (Région fictive, France)".into(),
            latitude: 46.12,
            longitude: 2.34,
        };
        for (said, written) in [
            ("J'habite a villeneuve d'ormeval", "Villeneuve d'Ormevaal"),
            (
                "Villeneuve d'ormeval haute Brume",
                "Villeneuve d'ormeval haute Brume",
            ),
        ] {
            let answer = call(
                "set_home_location",
                &json!({ "place": written }).to_string(),
            );
            let (moli, _) = with_layout(crate::tests::lamp_hub().await, "{}", vec![&answer]).await;
            moli.set_places(Arc::new(KnowsOnly(
                villeneuve.clone(),
                "Villeneuve d'Ormeval",
            )));
            let reply = moli.turn(ask(said)).await.unwrap();
            assert_eq!(reply.proposals.len(), 1, "{said}: {}", reply.reply);
            assert!(reply.proposals[0].text.contains("Villeneuve-d'Ormeval"));
        }
    }

    #[tokio::test]
    async fn a_town_nobody_finds_or_a_service_that_fails_proposes_nothing() {
        for (places, expected) in [(FakePlaces(None), "assistant.house.place_unknown")] {
            let answer = call("set_home_location", r#"{"place":"Nullepart"}"#);
            let (moli, _) = with_layout(crate::tests::lamp_hub().await, "{}", vec![&answer]).await;
            moli.set_places(Arc::new(places));
            let reply = moli.turn(ask("J'habite à Nullepart")).await.unwrap();
            assert!(reply.proposals.is_empty());
            assert_eq!(reply.reply, moli_i18n::tr!(expected));
        }
        let answer = call("set_home_location", r#"{"place":"Lyon"}"#);
        let (moli, _) = with_layout(crate::tests::lamp_hub().await, "{}", vec![&answer]).await;
        moli.set_places(Arc::new(Down));
        let reply = moli.turn(ask("Lyon")).await.unwrap();
        assert!(reply.proposals.is_empty());
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.place_unavailable")
        );
    }

    #[tokio::test]
    async fn a_place_with_a_control_character_or_too_long_is_not_even_searched() {
        for place in ["Ly\non", "", &"x".repeat(81)] {
            let answer = call("set_home_location", &json!({ "place": place }).to_string());
            let (moli, _) = with_layout(crate::tests::lamp_hub().await, "{}", vec![&answer]).await;
            moli.set_places(Arc::new(FakePlaces(Some(lyon()))));
            let reply = moli.turn(ask("Lyon")).await.unwrap();
            assert!(reply.proposals.is_empty(), "{place:?}");
            assert_eq!(reply.reply, unknown());
        }
    }

    #[test]
    fn a_sentence_that_asks_for_later_is_recognised() {
        for q in [
            "Mets un minuteur de 10 minutes.",
            "Éteins la lumière de l'entrée dans 5 minutes.",
            "Rappelle-moi de sortir le linge",
            "Allume le salon dans une heure",
            "éteins tout dans un quart d'heure",
            "Allume le chauffage à 18h",
            "Ferme les volets à 21 h 30",
            "Set a timer for ten minutes",
            "turn off the light in 5 minutes",
        ] {
            assert!(later(q), "{q}");
        }
        for q in [
            "Allume la lumière du salon",
            "Mets la clim à 22",
            "Baisse la lumière à 30 %",
            "Allume la lampe dans le salon",
            "Il fait 18 degrés ?",
            "Monte le volet à 50",
        ] {
            assert!(!later(q), "{q}");
        }
    }

    #[tokio::test]
    async fn an_order_for_later_is_not_carried_out_now() {
        let answer = call("set", r#"{"point":"d1/on","value":false}"#);
        let (moli, _) = house_model(vec![&answer]).await;
        let reply = moli
            .turn(ask("Éteins la lampe dans 5 minutes"))
            .await
            .unwrap();
        assert!(reply.actions.is_empty(), "{:?}", reply.actions);
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.later_unsupported")
        );
    }

    #[tokio::test]
    async fn an_order_already_true_is_not_sent_and_said_so() {
        let answer = call("set", r#"{"point":"d1/on","value":false}"#);
        let (moli, _) = house_model(vec![&answer]).await;
        let reply = moli.turn(ask("Éteins la lampe")).await.unwrap();
        assert!(reply.actions.is_empty(), "{:?}", reply.actions);
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.already", device = "lamp")
        );
    }

    async fn colour_lamp() -> moli_runtime::Hub {
        use crate::tests::{Fake, hub_with, point};
        hub_with(Fake(vec![(
            "Ruban",
            vec![
                point("state", moli_core::Kind::Binary, true),
                point("color", moli_core::Kind::Text, true),
                point(
                    "color_temp",
                    moli_core::Kind::Numeric {
                        min: Some(150.0),
                        max: Some(500.0),
                        step: None,
                    },
                    true,
                ),
            ],
            vec![
                ("state", moli_core::Value::Bool(true)),
                ("color", moli_core::Value::Text("#ffffff".into())),
                ("color_temp", moli_core::Value::Float(250.0)),
            ],
        )]))
        .await
    }

    #[tokio::test]
    async fn a_colour_said_by_its_name_becomes_the_lamps_colour() {
        for (said, point, value) in [
            ("rouge", "fake:Ruban/color", json!("#ff0000")),
            ("Blue", "fake:Ruban/color", json!("#0000ff")),
            ("blanc chaud", "fake:Ruban/color_temp", json!(370)),
        ] {
            let answer = call(
                "set",
                &json!({ "point": "d1/color", "value": said }).to_string(),
            );
            let (moli, _) = house_model_over(colour_lamp().await, vec![&answer]).await;
            let reply = moli.turn(ask("Change la couleur")).await.unwrap();
            assert_eq!(reply.actions.len(), 1, "{said}: {}", reply.reply);
            assert_eq!(reply.actions[0].point.as_str(), point, "{said}");
            assert_eq!(reply.actions[0].value, value, "{said}");
        }
        let answer = call("set", r#"{"point":"d1/color","value":"plaid"}"#);
        let (moli, _) = house_model_over(colour_lamp().await, vec![&answer]).await;
        let reply = moli.turn(ask("Mets la couleur plaid")).await.unwrap();
        assert!(reply.actions.is_empty());
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.colour_unknown")
        );
    }

    #[tokio::test]
    async fn the_model_never_writes_a_devices_setting() {
        use crate::tests::{Fake, hub_with, point};
        let hub = hub_with(Fake(vec![(
            "Ampoule",
            vec![
                point("state", moli_core::Kind::Binary, true),
                point(
                    "off_on_duration",
                    moli_core::Kind::Numeric {
                        min: Some(0.0),
                        max: Some(10.0),
                        step: None,
                    },
                    true,
                ),
            ],
            vec![
                ("state", moli_core::Value::Bool(true)),
                ("off_on_duration", moli_core::Value::Float(0.0)),
            ],
        )]))
        .await;
        let answer = call("set", r#"{"point":"d1/off_on_duration","value":10}"#);
        let (moli, _) = house_model_over(hub.clone(), vec![&answer]).await;
        let reply = moli
            .turn(ask("Règle la durée d'allumage de l'ampoule à 10"))
            .await
            .unwrap();
        assert!(reply.actions.is_empty(), "{:?}", reply.actions);
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.setting_refused")
        );
        let kept = hub.snapshot().devices[0]
            .state
            .get("off_on_duration")
            .cloned();
        assert_eq!(kept.map(|v| v.value), Some(moli_core::Value::Float(0.0)));
    }

    #[tokio::test]
    async fn a_reading_is_read_by_moli_from_the_real_device_and_shown_as_a_card() {
        let answer = call("get_device", r#"{"device":"d1"}"#);
        let (moli, asked) = house_model(vec![&answer]).await;
        let reply = moli.turn(ask("La lampe est allumée ?")).await.unwrap();
        assert!(reply.reply.contains("éteint"), "{}", reply.reply);
        assert_eq!(
            reply.cards,
            vec![json!({ "kind": "device", "id": "fake:lamp" })]
        );
        assert_eq!(asked.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn energy_and_history_are_shown_as_cards() {
        for (name, arguments, card) in [
            ("energy", "{}", json!({ "kind": "energy" })),
            (
                "history",
                r#"{"point":"d1/on","hours":6}"#,
                json!({ "kind": "history", "point": "fake:lamp/on", "hours": 6.0 }),
            ),
        ] {
            let answer = call(name, arguments);
            let (moli, asked) = house_model(vec![&answer]).await;
            let reply = moli.turn(ask("montre")).await.unwrap();
            assert_eq!(reply.cards, vec![card], "{name}");
            assert!(!reply.reply.is_empty(), "{name}");
            assert_eq!(asked.lock().unwrap().len(), 1, "{name}");
        }
    }

    #[tokio::test]
    async fn a_call_the_server_could_not_parse_is_never_shown_as_words() {
        for raw in [
            "<tool_call>\n{\"name\": \"set\", \"arguments\": {\"poi",
            "{\"name\": \"set\", \"arguments\": {}}",
        ] {
            let answer = words(raw);
            let (moli, _) = house_model(vec![&answer]).await;
            let reply = moli.turn(ask("Allume")).await.unwrap();
            assert_eq!(reply.reply, unknown(), "{raw}");
        }
    }

    #[tokio::test]
    async fn a_thinking_block_left_in_the_answer_is_dropped() {
        let answer = words("<think>\nhm\n</think>\n\nBonjour !");
        let (moli, _) = house_model(vec![&answer]).await;
        assert_eq!(moli.turn(ask("Salut")).await.unwrap().reply, "Bonjour !");
    }

    #[tokio::test]
    async fn arguments_cut_short_execute_nothing() {
        let answer = call("set", r#"{"point":"d1/on","va"#);
        let (moli, _) = house_model(vec![&answer]).await;
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        assert!(reply.actions.is_empty());
        assert_eq!(reply.reply, unknown());
    }

    #[tokio::test]
    async fn an_answer_with_nothing_in_it_is_not_understood() {
        let (moli, _) = house_model(vec![r#"{"usage":{}}"#]).await;
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        assert_eq!(reply.reply, unknown());
    }

    pub(crate) fn calls(list: &[(&str, &str)]) -> String {
        let tool_calls: Vec<Json> = list
            .iter()
            .enumerate()
            .map(|(i, (name, arguments))| {
                json!({ "id": format!("c{i}"), "type": "function",
                    "function": { "name": name, "arguments": arguments } })
            })
            .collect();
        json!({ "choices": [{ "message": { "role": "assistant", "content": null,
            "tool_calls": tool_calls } }], "usage": { "prompt_tokens": 10, "completion_tokens": 5 } })
        .to_string()
    }

    #[tokio::test]
    async fn several_calls_are_carried_out_in_order_up_to_four() {
        let on = r#"{"point":"d1/on","value":true}"#;
        let off = r#"{"point":"d1/on","value":false}"#;
        let six = calls(&[
            ("set", on),
            ("set", off),
            ("set", on),
            ("set", off),
            ("set", on),
            ("set", off),
        ]);
        let (moli, _) = house_model(vec![&six]).await;
        let reply = moli
            .turn(ask("Allume puis éteins, plusieurs fois"))
            .await
            .unwrap();
        let values: Vec<Json> = reply.actions.iter().map(|a| a.value.clone()).collect();
        assert_eq!(
            values,
            vec![json!(true), json!(false), json!(true), json!(false)]
        );
    }

    #[tokio::test]
    async fn a_proposal_among_several_calls_stands_alone() {
        let both = calls(&[
            ("set", r#"{"point":"d1/on","value":true}"#),
            (
                "rename_device",
                r#"{"device":"d1","new_name":"Lampe du salon"}"#,
            ),
        ]);
        let (moli, _) = house_model(vec![&both]).await;
        let reply = moli.turn(ask("Allume et renomme la lampe")).await.unwrap();
        assert!(reply.actions.is_empty(), "{:?}", reply.actions);
        assert_eq!(reply.proposals.len(), 1);
    }

    #[test]
    fn from_contract_v4_the_last_two_exchanges_come_before_the_question() {
        let long = "x".repeat(400);
        let thread: Turn = serde_json::from_value(json!({ "messages": [
            { "role": "user", "content": "Bonjour" },
            { "role": "assistant", "content": "Bonjour !" },
            { "role": "user", "content": "Allume la lampe" },
            { "role": "assistant", "content": long },
            { "role": "user", "content": "Éteins-la" },
            { "role": "assistant", "content": "C'est fait." },
            { "role": "user", "content": "Et rallume-la" },
        ] }))
        .unwrap();
        let sent: Vec<(String, String)> = dialogue(&thread, "Et rallume-la", true)
            .iter()
            .map(|m| {
                (
                    m["role"].as_str().unwrap().to_owned(),
                    m["content"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        assert_eq!(sent.len(), 5);
        assert_eq!(sent[0], ("user".into(), "Allume la lampe".into()));
        assert_eq!(sent[1].1.chars().count(), 300, "a long answer is cut");
        assert_eq!(sent[2], ("user".into(), "Éteins-la".into()));
        assert_eq!(sent[3], ("assistant".into(), "C'est fait.".into()));
        assert_eq!(sent[4], ("user".into(), "Et rallume-la".into()));
        assert_eq!(
            dialogue(&thread, "Et rallume-la", false).len(),
            1,
            "v3 never saw a dialogue"
        );
    }

    #[tokio::test]
    async fn an_order_on_a_device_nobody_may_command_alone_is_held_and_said_so() {
        let answer = call("set", r#"{"point":"d1/on","value":true}"#);
        let guard = moli_runtime::guard::GuardPolicy {
            protect_unassigned: true,
            ..moli_runtime::guard::GuardPolicy::default()
        };
        let hub = crate::tests::lamp_hub_guarded(false, guard).await;
        let (moli, _) = house_model_over(hub, vec![&answer]).await;
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        assert_eq!(reply.actions[0].status, "held");
        assert!(reply.reply.contains("approbation"), "{}", reply.reply);
    }

    #[tokio::test]
    async fn a_real_identifier_is_not_a_number_and_is_not_found() {
        let answer = call("set", r#"{"point":"fake:lamp/on","value":true}"#);
        let (moli, _) = house_model(vec![&answer]).await;
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.not_found"));
        assert!(reply.actions.is_empty());
    }

    #[tokio::test]
    async fn a_hidden_device_has_no_number() {
        let dir = std::env::temp_dir().join(format!("moli-house-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let layout = dir.join("home.json");
        std::fs::write(&layout, r#"{"rooms":[],"hidden":["fake:lamp"]}"#).unwrap();
        let (a, b) = (
            words("Bonjour"),
            call("set", r#"{"point":"d1/on","value":true}"#),
        );
        let (moli, asked) =
            house_model_in(crate::tests::lamp_hub().await, Some(layout), vec![&a, &b]).await;
        moli.turn(ask("Bonjour")).await.unwrap();
        let system = body_of(&asked.lock().unwrap()[0])["messages"][0]["content"].to_string();
        assert!(
            !system.contains("- d1"),
            "a hidden device is not offered: {system}"
        );
        let reply = moli.turn(ask("Allume la lampe")).await.unwrap();
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.not_found"));
        assert!(reply.actions.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn reading_an_unreachable_device_says_so() {
        let answer = call("get_device", r#"{"device":"d1"}"#);
        let hub = crate::tests::lamp_hub_with(true).await;
        let (moli, _) = house_model_over(hub, vec![&answer]).await;
        let reply = moli.turn(ask("La lampe est allumée ?")).await.unwrap();
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.unreachable", device = "lamp")
        );
    }

    #[tokio::test]
    async fn a_point_written_where_a_device_was_asked_still_finds_the_device() {
        let answer = call("get_device", r#"{"device":"d1/on"}"#);
        let (moli, _) = house_model(vec![&answer]).await;
        let reply = moli.turn(ask("La lampe est allumée ?")).await.unwrap();
        assert_eq!(
            reply.cards,
            vec![json!({ "kind": "device", "id": "fake:lamp" })]
        );
    }

    fn sensor() -> Json {
        json!({ "name": "Capteur salon", "online": true, "points": [
            { "key": "battery", "label": "Battery", "value": 100, "unit": "%" },
            { "key": "temperature", "label": "Temperature", "value": 19.099_999, "unit": "°C" },
            { "key": "humidity", "label": "Humidity", "value": 60.8, "unit": "%" },
            { "key": "temperature_calibration", "label": "Temperature calibration", "value": 0, "unit": "°C" },
            { "key": "linkquality", "label": "Linkquality", "value": 132, "unit": "lqi" } ] })
    }

    #[test]
    fn a_reading_answers_what_was_asked_in_french_and_nothing_else() {
        let said = reading_words(&sensor(), "Quelle température fait-il dans le salon ?");
        assert_eq!(said, "Capteur salon : température 19,1 °C.");
        let said = reading_words(&sensor(), "Quelle est l'humidité dans le salon ?");
        assert_eq!(said, "Capteur salon : humidité 60,8 %.");
        let said = reading_words(&sensor(), "Il reste de la pile ?");
        assert_eq!(said, "Capteur salon : batterie 100 %.");
    }

    #[test]
    fn a_reading_without_a_cue_gives_the_main_values_in_french_without_the_noise() {
        let said = reading_words(&sensor(), "Dis-moi l'état du capteur.");
        assert_eq!(
            said,
            "Capteur salon : batterie 100 %, température 19,1 °C, humidité 60,8 %."
        );
        assert!(
            !said.to_lowercase().contains("linkquality") && !said.contains("calibration"),
            "{said}"
        );
    }

    #[test]
    fn a_lamp_is_said_lit_or_off_not_yes_or_no() {
        let lamp = json!({ "name": "Lampe", "online": true, "points": [
            { "key": "state", "label": "State", "value": true },
            { "key": "brightness", "label": "Brightness", "value": 60, "unit": "%" } ] });
        assert_eq!(
            reading_words(&lamp, "La lampe est allumée ?"),
            "Lampe : allumé."
        );
        assert_eq!(
            reading_words(&lamp, "Quelle luminosité ?"),
            "Lampe : luminosité 60 %."
        );
        let off = json!({ "name": "Lampe", "online": true, "points": [{ "key": "on", "label": "On", "value": false }] });
        assert_eq!(reading_words(&off, "C'est allumé ?"), "Lampe : éteint.");
    }

    #[test]
    fn a_device_with_no_value_yet_is_said_so() {
        let device = json!({ "name": "Lampe", "online": true, "points": [{ "key": "on", "label": "on", "value": null }] });
        assert_eq!(
            reading_words(&device, "C'est allumé ?"),
            moli_i18n::tr!("assistant.house.no_reading", device = "Lampe")
        );
        assert_eq!(
            reading_words(&json!({ "error": "unknown device" }), "?"),
            moli_i18n::tr!("assistant.house.not_found")
        );
    }

    /// A house-model assistant over `hub`, with a `home.json` in a fresh directory.
    pub(crate) async fn with_layout(
        hub: moli_runtime::Hub,
        layout: &str,
        answers: Vec<&str>,
    ) -> (Assistant, std::path::PathBuf) {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "moli-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("home.json");
        std::fs::write(&path, layout).unwrap();
        let (moli, _) = house_model_in(hub, Some(path.clone()), answers).await;
        (moli, path)
    }

    fn label_of(hub: &moli_runtime::Hub) -> moli_core::Label {
        hub.device(&moli_core::DeviceId::from("fake:lamp"))
            .unwrap()
            .label
            .clone()
    }

    #[tokio::test]
    async fn a_rename_is_proposed_not_done_and_done_when_a_person_confirms() {
        let answer = call(
            "rename_device",
            r#"{"device":"d1","new_name":"Lampe de lecture"}"#,
        );
        let hub = crate::tests::lamp_hub().await;
        let (moli, _) = with_layout(hub.clone(), "{}", vec![&answer]).await;
        let reply = moli
            .turn(ask("Appelle la lampe Lampe de lecture"))
            .await
            .unwrap();
        assert!(
            reply.reply.contains("Je propose") && reply.reply.contains("Lampe de lecture"),
            "{}",
            reply.reply
        );
        assert_eq!(reply.proposals.len(), 1);
        assert!(
            label_of(&hub).name.is_none(),
            "nothing changes before a person says so"
        );
        let said = moli
            .decide_proposal(
                reply.proposals[0].id,
                true,
                Origin::Ui,
                Some("Maman".into()),
            )
            .await
            .unwrap();
        assert!(said.contains("Lampe de lecture"), "{said}");
        assert_eq!(label_of(&hub).name.as_deref(), Some("Lampe de lecture"));
    }

    #[tokio::test]
    async fn a_proposal_that_is_refused_changes_nothing_and_is_gone() {
        let answer = call("rename_device", r#"{"device":"d1","new_name":"Autre"}"#);
        let hub = crate::tests::lamp_hub().await;
        let (moli, _) = with_layout(hub.clone(), "{}", vec![&answer]).await;
        let id = moli.turn(ask("Renomme")).await.unwrap().proposals[0].id;
        let said = moli
            .decide_proposal(id, false, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(said, moli_i18n::tr!("assistant.house.cancelled"));
        assert!(label_of(&hub).name.is_none());
        assert!(matches!(
            moli.decide_proposal(id, true, Origin::Ui, None).await,
            Err(ProposalError::Unknown)
        ));
    }

    #[tokio::test]
    async fn only_a_person_with_a_session_confirms_never_an_agent_a_script_or_the_household_alone()
    {
        let answer = call("rename_device", r#"{"device":"d1","new_name":"Autre"}"#);
        let hub = crate::tests::lamp_hub().await;
        let (moli, _) = with_layout(hub.clone(), "{}", vec![&answer]).await;
        let id = moli.turn(ask("Renomme")).await.unwrap().proposals[0].id;
        for origin in [
            Origin::Api,
            Origin::Assistant,
            Origin::Cli,
            Origin::Household,
        ] {
            assert!(
                matches!(
                    moli.decide_proposal(id, true, origin, None).await,
                    Err(ProposalError::HumanOnly)
                ),
                "{origin:?}"
            );
        }
        assert!(label_of(&hub).name.is_none());
        assert!(
            moli.decide_proposal(id, true, Origin::Ui, None)
                .await
                .is_ok(),
            "still there for a person"
        );
    }

    #[tokio::test]
    async fn a_proposal_that_could_not_be_applied_stays_for_another_try() {
        // No home.json to write: the hide fails, the proposal is not lost.
        let answer = call("set_device_enabled", r#"{"device":"d1","enabled":false}"#);
        let (moli, _) = house_model(vec![&answer]).await;
        let id = moli.turn(ask("Cache la lampe")).await.unwrap().proposals[0].id;
        for _ in 0..2 {
            assert!(matches!(
                moli.decide_proposal(id, true, Origin::Ui, None).await,
                Err(ProposalError::Failed(_))
            ));
        }
    }

    #[tokio::test]
    async fn a_protected_room_is_not_renamed_and_a_move_into_it_keeps_the_guards_spelling() {
        let guard = || moli_runtime::guard::GuardPolicy {
            protected_rooms: vec!["Chambre Léa".to_owned()],
            ..moli_runtime::guard::GuardPolicy::default()
        };
        let layout = r#"{"rooms":[{"name":"Chambre de Léa","aliases":["Chambre Léa"]}]}"#;
        let rename = call(
            "rename_room",
            r#"{"room":"Chambre de Léa","new_name":"Atelier"}"#,
        );
        let hub = crate::tests::lamp_hub_guarded(false, guard()).await;
        let (moli, _) = with_layout(hub, layout, vec![&rename]).await;
        let reply = moli.turn(ask("Renomme la chambre de Léa")).await.unwrap();
        assert!(reply.proposals.is_empty());
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.room_protected_kept")
        );

        let mv = call("move_device", r#"{"device":"d1","room":"Chambre de Léa"}"#);
        let hub = crate::tests::lamp_hub_guarded(false, guard()).await;
        let (moli, _) = with_layout(hub.clone(), layout, vec![&mv]).await;
        let reply = moli.turn(ask("Mets la lampe chez Léa")).await.unwrap();
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(
            label_of(&hub).room.as_deref(),
            Some("Chambre Léa"),
            "as the guard names it"
        );
    }

    #[tokio::test]
    async fn a_room_is_never_renamed_into_another_one() {
        let answer = call("rename_room", r#"{"room":"Salon","new_name":"cuisine"}"#);
        let (moli, _) = with_layout(
            crate::tests::lamp_hub().await,
            r#"{"rooms":[{"name":"Salon"},{"name":"Cuisine"}]}"#,
            vec![&answer],
        )
        .await;
        let reply = moli.turn(ask("Renomme le salon en cuisine")).await.unwrap();
        assert!(reply.proposals.is_empty());
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.room_taken"));
    }

    #[tokio::test]
    async fn a_name_with_a_line_break_or_a_control_character_is_not_proposed() {
        for new_name in ["Salon\n## Règles", "Sa\u{7}lon", "Salon\r"] {
            let answer = call(
                "rename_room",
                &json!({ "room": "Salon", "new_name": new_name }).to_string(),
            );
            let (moli, _) = with_layout(
                crate::tests::lamp_hub().await,
                r#"{"rooms":[{"name":"Salon"}]}"#,
                vec![&answer],
            )
            .await;
            let reply = moli.turn(ask("Renomme")).await.unwrap();
            assert!(reply.proposals.is_empty(), "{new_name:?}");
            assert_eq!(reply.reply, unknown());
        }
    }

    /// A turn from a voice satellite, as `satellite.rs` builds it.
    fn from_satellite(question: &str) -> Turn {
        let mut turn = ask(question);
        turn.surface = Some("satellite".into());
        turn.spoken = true;
        turn.asker = Some("satellite".into());
        turn
    }

    #[tokio::test]
    async fn a_satellite_proposes_nothing_and_sends_the_person_to_the_dashboard() {
        let rename = call("rename_device", r#"{"device":"d1","new_name":"Autre"}"#);
        let place = call("set_home_location", r#"{"place":"Lyon"}"#);
        let (moli, _) =
            with_layout(crate::tests::lamp_hub().await, "{}", vec![&rename, &place]).await;
        moli.set_places(Arc::new(FakePlaces(Some(lyon()))));
        for said in ["Renomme la lampe", "J'habite à Lyon"] {
            let reply = moli.turn(from_satellite(said)).await.unwrap();
            assert!(reply.proposals.is_empty(), "{said}");
            assert_eq!(
                reply.reply,
                moli_i18n::tr!("assistant.house.propose_on_screen")
            );
        }
        assert!(moli.0.proposals.newest_pending(Some("satellite")).is_none());
    }

    #[tokio::test]
    async fn cancel_only_refuses_what_the_same_screen_proposed() {
        let rename = call("rename_device", r#"{"device":"d1","new_name":"Autre"}"#);
        let undo = call("undo", "{}");
        let (moli, _) = house_model(vec![&rename, &undo, &undo]).await;
        let mut mine = ask("Renomme la lampe");
        mine.asker = Some("client:a".into());
        let id = moli.turn(mine).await.unwrap().proposals[0].id;
        // Another screen says « annule »: not its proposal to refuse.
        let mut other = ask("Annule");
        other.asker = Some("client:b".into());
        let reply = moli.turn(other).await.unwrap();
        assert_ne!(reply.reply, moli_i18n::tr!("assistant.house.cancelled"));
        assert_eq!(moli.0.proposals.newest_pending(Some("client:a")), Some(id));
        // The screen that asked refuses it.
        let mut again = ask("Annule");
        again.asker = Some("client:a".into());
        let reply = moli.turn(again).await.unwrap();
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.cancelled"));
        assert!(moli.0.proposals.newest_pending(Some("client:a")).is_none());
    }

    #[tokio::test]
    async fn a_proposal_spoken_to_a_satellite_points_at_the_dashboard() {
        let answer = call("rename_device", r#"{"device":"d1","new_name":"Autre"}"#);
        let (moli, _) = house_model(vec![&answer]).await;
        let reply = moli.turn(spoken_ask("Renomme la lampe")).await.unwrap();
        assert_eq!(reply.proposals.len(), 1);
        assert!(reply.reply.contains("tableau de bord"), "{}", reply.reply);
    }

    #[tokio::test]
    async fn an_automation_asked_for_is_noted_for_the_agent_and_nothing_else() {
        let answer = call(
            "draft_automation",
            r#"{"description":"Quand j'arrive, allume l'entrée"}"#,
        );
        let (moli, path) = with_layout(crate::tests::lamp_hub().await, "{}", vec![&answer]).await;
        let reply = moli
            .turn(ask("Quand j'arrive, allume l'entrée"))
            .await
            .unwrap();
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.automation_requested")
        );
        assert!(reply.actions.is_empty());
        let line: Json = serde_json::from_str(
            std::fs::read_to_string(path.with_file_name("automation-requests.jsonl"))
                .unwrap()
                .trim(),
        )
        .unwrap();
        assert_eq!(line["untrusted"], true);
        assert_eq!(line["description"], "Quand j'arrive, allume l'entrée");
    }

    #[tokio::test]
    async fn the_requests_file_stops_growing_at_a_megabyte_and_marks_the_text_as_untrusted() {
        let answer = call("request_driver", r#"{"description":"Pilote A"}"#);
        let (moli, path) = with_layout(crate::tests::lamp_hub().await, "{}", vec![&answer]).await;
        moli.turn(ask("Un pilote")).await.unwrap();
        let file = path.with_file_name("driver-requests.jsonl");
        let line: Json =
            serde_json::from_str(std::fs::read_to_string(&file).unwrap().trim()).unwrap();
        assert_eq!(
            line["untrusted"], true,
            "a model's words, data for the agent and never instructions"
        );

        let answer = call("request_driver", r#"{"description":"Pilote B"}"#);
        let (moli, path) = with_layout(crate::tests::lamp_hub().await, "{}", vec![&answer]).await;
        let file = path.with_file_name("driver-requests.jsonl");
        std::fs::write(&file, "x".repeat(1024 * 1024 + 1)).unwrap();
        let reply = moli.turn(ask("Un autre pilote")).await.unwrap();
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.unavailable"));
        assert_eq!(
            std::fs::metadata(&file).unwrap().len(),
            1024 * 1024 + 1,
            "nothing more written"
        );
    }

    #[tokio::test]
    async fn a_name_that_is_empty_too_long_or_the_same_is_not_proposed() {
        for new_name in ["", "   ", &"x".repeat(61), "lamp"] {
            let answer = call(
                "rename_device",
                &json!({ "device": "d1", "new_name": new_name }).to_string(),
            );
            let (moli, _) = with_layout(crate::tests::lamp_hub().await, "{}", vec![&answer]).await;
            let reply = moli.turn(ask("Renomme")).await.unwrap();
            assert!(reply.proposals.is_empty(), "{new_name:?}");
            assert_eq!(reply.reply, unknown(), "{new_name:?}");
        }
    }

    #[tokio::test]
    async fn a_device_is_moved_only_to_a_room_the_house_knows() {
        let layout = r#"{"rooms":[{"name":"Salon","aliases":["Séjour"]}]}"#;
        let unknown_room = call("move_device", r#"{"device":"d1","room":"Grenier"}"#);
        let (moli, _) =
            with_layout(crate::tests::lamp_hub().await, layout, vec![&unknown_room]).await;
        let reply = moli.turn(ask("Mets la lampe au grenier")).await.unwrap();
        assert!(reply.proposals.is_empty());
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.unknown_room"));

        let known = call("move_device", r#"{"device":"d1","room":"séjour"}"#);
        let hub = crate::tests::lamp_hub().await;
        let (moli, _) = with_layout(hub.clone(), layout, vec![&known]).await;
        let reply = moli.turn(ask("Mets la lampe au séjour")).await.unwrap();
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(
            label_of(&hub).room.as_deref(),
            Some("Salon"),
            "the room as the house names it"
        );
    }

    #[tokio::test]
    async fn a_device_is_hidden_in_the_layout_and_leaves_the_prompt() {
        let answer = call("set_device_enabled", r#"{"device":"d1","enabled":false}"#);
        let (moli, path) = with_layout(
            crate::tests::lamp_hub().await,
            "{}",
            vec![&answer, &words("Salut")],
        )
        .await;
        let reply = moli.turn(ask("Cache la lampe")).await.unwrap();
        assert!(reply.reply.contains("masquer"), "{}", reply.reply);
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        let file: Json = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(file["hidden"], json!(["fake:lamp"]));
    }

    #[tokio::test]
    async fn a_room_is_renamed_in_the_layout_with_the_old_name_as_alias() {
        let answer = call("rename_room", r#"{"room":"Salon","new_name":"Séjour"}"#);
        let (moli, path) = with_layout(
            crate::tests::lamp_hub().await,
            r#"{"rooms":[{"name":"Salon"}]}"#,
            vec![&answer],
        )
        .await;
        let reply = moli.turn(ask("Renomme le salon en séjour")).await.unwrap();
        assert!(reply.reply.contains("Séjour"), "{}", reply.reply);
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        let file: Json = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            file["rooms"][0],
            json!({ "name": "Séjour", "aliases": ["Salon"] })
        );
    }

    #[tokio::test]
    async fn a_room_nobody_knows_cannot_be_renamed() {
        let answer = call("rename_room", r#"{"room":"Grenier","new_name":"Atelier"}"#);
        let (moli, _) = with_layout(
            crate::tests::lamp_hub().await,
            r#"{"rooms":[{"name":"Salon"}]}"#,
            vec![&answer],
        )
        .await;
        let reply = moli.turn(ask("Renomme le grenier")).await.unwrap();
        assert!(reply.proposals.is_empty());
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.unknown_room"));
    }

    #[tokio::test]
    async fn a_driver_request_is_written_for_the_outside_agent_and_nothing_is_run() {
        let (a, b) = (
            call(
                "request_driver",
                r#"{"description":"Pilote pour un aspirateur Dreame"}"#,
            ),
            call(
                "request_driver",
                r#"{"description":"Intégration d'un compteur d'eau"}"#,
            ),
        );
        let (moli, path) = with_layout(crate::tests::lamp_hub().await, "{}", vec![&a, &b]).await;
        let reply = moli
            .turn(ask("Écris un pilote pour mon aspirateur"))
            .await
            .unwrap();
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.driver_requested")
        );
        assert!(reply.actions.is_empty() && reply.proposals.is_empty());
        moli.turn(ask("Et pour le compteur d'eau")).await.unwrap();
        let file = std::fs::read_to_string(path.with_file_name("driver-requests.jsonl")).unwrap();
        let lines: Vec<Json> = file
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2, "one line a request: {file}");
        assert_eq!(lines[0]["description"], "Pilote pour un aspirateur Dreame");
        assert_eq!(lines[0]["origin"], "assistant");
        assert!(lines[1]["ts"].as_u64().unwrap() > 0);
    }

    #[tokio::test]
    async fn an_empty_or_huge_driver_request_is_not_written() {
        for description in ["", "   ", &"x".repeat(501)] {
            let answer = call(
                "request_driver",
                &json!({ "description": description }).to_string(),
            );
            let (moli, path) =
                with_layout(crate::tests::lamp_hub().await, "{}", vec![&answer]).await;
            let reply = moli.turn(ask("Un pilote")).await.unwrap();
            assert_eq!(reply.reply, unknown());
            assert!(!path.with_file_name("driver-requests.jsonl").exists());
        }
    }

    #[tokio::test]
    async fn the_weather_is_forgotten_once_every_few_minutes_at_most() {
        let (moli, _) = house_model(vec![]).await;
        assert!(moli.may_refresh_weather(), "the first time");
        assert!(!moli.may_refresh_weather(), "again within the floor");
        assert!(!moli.may_refresh_weather());
    }

    #[tokio::test]
    async fn refreshing_the_weather_without_a_weather_says_it_cannot() {
        let answer = call("refresh_weather", "{}");
        let (moli, _) = house_model(vec![&answer]).await;
        let reply = moli.turn(ask("Actualise la météo")).await.unwrap();
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.weather_unavailable")
        );
    }

    #[tokio::test]
    async fn an_undo_proposes_the_inverse_of_the_last_confirmed_change_and_confirming_it_restores()
    {
        let (rename, undo) = (
            call(
                "rename_device",
                r#"{"device":"d1","new_name":"Lampe de lecture"}"#,
            ),
            call("undo", "{}"),
        );
        let hub = crate::tests::lamp_hub().await;
        let (moli, _) = with_layout(hub.clone(), "{}", vec![&rename, &undo]).await;
        let id = moli
            .turn(ask("Appelle la lampe Lampe de lecture"))
            .await
            .unwrap()
            .proposals[0]
            .id;
        moli.decide_proposal(id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(label_of(&hub).name.as_deref(), Some("Lampe de lecture"));
        let reply = moli
            .turn(ask("Fait l'inverse dsl je me suis trompé"))
            .await
            .unwrap();
        assert_eq!(reply.proposals.len(), 1);
        assert!(
            reply.reply.contains("Je propose")
                && reply.reply.contains("Lampe de lecture")
                && reply.reply.contains("lamp")
                && reply.reply.contains("annul"),
            "{}",
            reply.reply
        );
        assert_eq!(
            label_of(&hub).name.as_deref(),
            Some("Lampe de lecture"),
            "still a proposal: nothing changes until a person says so"
        );
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert!(
            label_of(&hub).name.is_none(),
            "the name is back to what it was"
        );
    }

    #[tokio::test]
    async fn with_nothing_confirmed_there_is_nothing_to_undo() {
        let (rename, undo) = (
            call("rename_device", r#"{"device":"d1","new_name":"Autre"}"#),
            call("undo", "{}"),
        );
        let (moli, _) = with_layout(
            crate::tests::lamp_hub().await,
            "{}",
            vec![&undo, &rename, &undo, &undo],
        )
        .await;
        let nothing = moli_i18n::tr!("assistant.house.undo_nothing");
        assert_eq!(moli.turn(ask("Annule")).await.unwrap().reply, nothing);
        // A proposal nobody confirmed is not a change: « annule » refuses that proposal.
        let waiting = moli.turn(ask("Renomme la lampe")).await.unwrap().proposals[0].id;
        let reply = moli.turn(ask("Annule")).await.unwrap();
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.cancelled"));
        assert!(matches!(
            moli.decide_proposal(waiting, true, Origin::Ui, None).await,
            Err(ProposalError::Unknown)
        ));
        // And there is still nothing to take back.
        let reply = moli.turn(ask("Annule encore")).await.unwrap();
        assert_eq!(reply.reply, nothing);
        assert!(reply.proposals.is_empty());
    }

    #[tokio::test]
    async fn undoing_goes_back_one_change_at_a_time_and_an_undo_is_not_itself_undone() {
        let calls = [
            call("rename_device", r#"{"device":"d1","new_name":"A"}"#),
            call("rename_device", r#"{"device":"d1","new_name":"B"}"#),
            call("undo", "{}"),
            call("undo", "{}"),
            call("undo", "{}"),
        ];
        let hub = crate::tests::lamp_hub().await;
        let (moli, _) = with_layout(
            hub.clone(),
            "{}",
            calls.iter().map(String::as_str).collect(),
        )
        .await;
        for _ in 0..2 {
            let id = moli.turn(ask("Renomme")).await.unwrap().proposals[0].id;
            moli.decide_proposal(id, true, Origin::Ui, None)
                .await
                .unwrap();
        }
        assert_eq!(label_of(&hub).name.as_deref(), Some("B"));
        let id = moli.turn(ask("Annule")).await.unwrap().proposals[0].id;
        moli.decide_proposal(id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(label_of(&hub).name.as_deref(), Some("A"));
        let id = moli.turn(ask("Annule encore")).await.unwrap().proposals[0].id;
        moli.decide_proposal(id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert!(label_of(&hub).name.is_none());
        let reply = moli.turn(ask("Annule encore")).await.unwrap();
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.undo_nothing"));
    }

    #[tokio::test]
    async fn hiding_a_device_and_renaming_a_room_are_undone_too() {
        let calls = [
            call("set_device_enabled", r#"{"device":"d1","enabled":false}"#),
            call("rename_room", r#"{"room":"Salon","new_name":"Séjour"}"#),
            call("undo", "{}"),
            call("undo", "{}"),
        ];
        let (moli, path) = with_layout(
            crate::tests::lamp_hub().await,
            r#"{"rooms":[{"name":"Salon"}]}"#,
            calls.iter().map(String::as_str).collect(),
        )
        .await;
        for _ in 0..2 {
            let id = moli.turn(ask("Change")).await.unwrap().proposals[0].id;
            moli.decide_proposal(id, true, Origin::Ui, None)
                .await
                .unwrap();
        }
        let file = |p: &std::path::Path| -> Json {
            serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
        };
        assert_eq!(file(&path)["rooms"][0]["name"], "Séjour");
        let reply = moli.turn(ask("Annule")).await.unwrap();
        assert!(
            reply.reply.contains("Salon") && reply.reply.contains("Séjour"),
            "{}",
            reply.reply
        );
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(file(&path)["rooms"][0]["name"], "Salon");
        let reply = moli.turn(ask("Annule encore")).await.unwrap();
        assert!(reply.reply.contains("afficher"), "{}", reply.reply);
        moli.decide_proposal(reply.proposals[0].id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(file(&path)["hidden"], json!([]));
    }

    #[tokio::test]
    async fn after_a_confirmed_change_the_prompt_is_read_again_in_the_background() {
        // A new name changes the prompt: the next question would wait half a minute.
        let (rename, warm) = (
            call("rename_device", r#"{"device":"d1","new_name":"Autre"}"#),
            words("ok"),
        );
        let hub = crate::tests::lamp_hub().await;
        let (url, asked) = fake_provider(vec![rename.clone(), warm.clone()]).await;
        let config: Config = toml::from_str(&format!(
            "[[provider]]\nid = \"house\"\nbase_url = \"{url}\"\nmodel = \"moli-maison\"\ntier = 1\ncontract = \"moli-v4\"\n\n[routes]\nchat = \"house\"\n"
        ))
        .unwrap();
        let moli = Assistant::new(hub, None, None, None, config).unwrap();
        let id = moli.turn(ask("Renomme la lampe")).await.unwrap().proposals[0].id;
        assert_eq!(asked.lock().unwrap().len(), 1);
        moli.decide_proposal(id, true, Origin::Ui, None).await.ok(); // no home.json: a label change does not need one
        for _ in 0..100 {
            if asked.lock().unwrap().len() == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 2, "a warm-up request followed the change");
        assert_eq!(body_of(&asked[1])["max_tokens"], 1);
        assert!(
            body_of(&asked[1])["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("Autre"),
            "the new name is in the prompt it warms"
        );
    }

    #[tokio::test]
    async fn a_second_cancel_refuses_the_undo_waiting_and_the_history_keeps_every_change() {
        let calls = [
            call("rename_device", r#"{"device":"d1","new_name":"A"}"#),
            call("rename_device", r#"{"device":"d1","new_name":"B"}"#),
            call("undo", "{}"),
            call("undo", "{}"),
            call("undo", "{}"),
        ];
        let hub = crate::tests::lamp_hub().await;
        let (moli, _) = with_layout(
            hub.clone(),
            "{}",
            calls.iter().map(String::as_str).collect(),
        )
        .await;
        for _ in 0..2 {
            let id = moli.turn(ask("Renomme")).await.unwrap().proposals[0].id;
            moli.decide_proposal(id, true, Origin::Ui, None)
                .await
                .unwrap();
        }
        let first = moli.turn(ask("Annule")).await.unwrap().proposals[0].id;
        // Asked again before anyone decided: that is « not that one », not a second proposal.
        let again = moli.turn(ask("Annule vraiment")).await.unwrap();
        assert_eq!(again.reply, moli_i18n::tr!("assistant.house.cancelled"));
        assert!(again.proposals.is_empty());
        assert!(matches!(
            moli.decide_proposal(first, true, Origin::Ui, None).await,
            Err(ProposalError::Unknown)
        ));
        assert_eq!(
            label_of(&hub).name.as_deref(),
            Some("B"),
            "nothing was undone"
        );
        // Asked a third time with nothing waiting: the undo is proposed again, history intact.
        let third = moli.turn(ask("Annule")).await.unwrap().proposals[0].id;
        moli.decide_proposal(third, true, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(label_of(&hub).name.as_deref(), Some("A"));
        assert!(
            moli.0.proposals.last_applied().is_some(),
            "A can still be taken back"
        );
    }

    #[tokio::test]
    async fn an_undo_made_stale_by_a_newer_change_is_refused() {
        let calls = [
            call("rename_device", r#"{"device":"d1","new_name":"A"}"#),
            call("undo", "{}"),
            call("rename_device", r#"{"device":"d1","new_name":"D"}"#),
        ];
        let hub = crate::tests::lamp_hub().await;
        let (moli, _) = with_layout(
            hub.clone(),
            "{}",
            calls.iter().map(String::as_str).collect(),
        )
        .await;
        let id = moli.turn(ask("Renomme")).await.unwrap().proposals[0].id;
        moli.decide_proposal(id, true, Origin::Ui, None)
            .await
            .unwrap();
        let stale = moli.turn(ask("Annule")).await.unwrap().proposals[0].id;
        let d = moli.turn(ask("Renomme encore")).await.unwrap().proposals[0].id;
        moli.decide_proposal(d, true, Origin::Ui, None)
            .await
            .unwrap();
        assert!(matches!(
            moli.decide_proposal(stale, true, Origin::Ui, None).await,
            Err(ProposalError::Unknown)
        ));
        assert_eq!(
            label_of(&hub).name.as_deref(),
            Some("D"),
            "the newer change was not overwritten"
        );
    }

    #[tokio::test]
    async fn showing_a_device_that_was_already_visible_leaves_nothing_to_undo() {
        let calls = [
            call("set_device_enabled", r#"{"device":"d1","enabled":true}"#),
            call("undo", "{}"),
        ];
        let (moli, _) = with_layout(
            crate::tests::lamp_hub().await,
            "{}",
            calls.iter().map(String::as_str).collect(),
        )
        .await;
        let id = moli.turn(ask("Affiche la lampe")).await.unwrap().proposals[0].id;
        moli.decide_proposal(id, true, Origin::Ui, None)
            .await
            .unwrap();
        let reply = moli.turn(ask("Annule")).await.unwrap();
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.undo_nothing"),
            "it changed nothing: nothing to take back"
        );
    }

    #[tokio::test]
    async fn saying_cancel_after_a_proposal_refuses_that_proposal_instead_of_undoing_the_last_change()
     {
        let calls = [
            call("rename_device", r#"{"device":"d1","new_name":"A"}"#),
            call("rename_device", r#"{"device":"d1","new_name":"B"}"#),
            call("undo", "{}"),
        ];
        let hub = crate::tests::lamp_hub().await;
        let (moli, _) = with_layout(
            hub.clone(),
            "{}",
            calls.iter().map(String::as_str).collect(),
        )
        .await;
        let id = moli.turn(ask("Renomme")).await.unwrap().proposals[0].id;
        moli.decide_proposal(id, true, Origin::Ui, None)
            .await
            .unwrap();
        let waiting = moli.turn(ask("Renomme encore")).await.unwrap().proposals[0].id;
        let reply = moli.turn(ask("Non, annule !")).await.unwrap();
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.cancelled"));
        assert!(reply.proposals.is_empty());
        assert!(
            matches!(
                moli.decide_proposal(waiting, true, Origin::Ui, None).await,
                Err(ProposalError::Unknown)
            ),
            "the waiting proposal is gone"
        );
        assert_eq!(
            label_of(&hub).name.as_deref(),
            Some("A"),
            "the confirmed change is untouched"
        );
    }

    #[tokio::test]
    async fn an_undo_that_cannot_be_applied_stays_and_loses_nothing() {
        // No home.json to write: hiding fails; the undo of a hide cannot be written either.
        let calls = [
            call("set_device_enabled", r#"{"device":"d1","enabled":false}"#),
            call("undo", "{}"),
        ];
        let dir = std::env::temp_dir().join(format!("moli-undo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (moli, path) = with_layout(
            crate::tests::lamp_hub().await,
            "{}",
            calls.iter().map(String::as_str).collect(),
        )
        .await;
        let id = moli.turn(ask("Cache la lampe")).await.unwrap().proposals[0].id;
        moli.decide_proposal(id, true, Origin::Ui, None)
            .await
            .unwrap();
        let undo = moli.turn(ask("Annule")).await.unwrap().proposals[0].id;
        std::fs::write(&path, "{ pas du json").unwrap(); // the file cannot be edited any more
        for _ in 0..2 {
            assert!(matches!(
                moli.decide_proposal(undo, true, Origin::Ui, None).await,
                Err(ProposalError::Failed(_))
            ));
        }
        assert!(
            moli.0.proposals.last_applied().is_some(),
            "the history is intact"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_device_moved_from_no_room_goes_back_to_no_room() {
        let calls = [
            call("move_device", r#"{"device":"d1","room":"Salon"}"#),
            call("undo", "{}"),
        ];
        let hub = crate::tests::lamp_hub().await;
        let (moli, _) = with_layout(
            hub.clone(),
            r#"{"rooms":[{"name":"Salon"}]}"#,
            calls.iter().map(String::as_str).collect(),
        )
        .await;
        let id = moli
            .turn(ask("Mets la lampe au salon"))
            .await
            .unwrap()
            .proposals[0]
            .id;
        moli.decide_proposal(id, true, Origin::Ui, None)
            .await
            .unwrap();
        assert_eq!(label_of(&hub).room.as_deref(), Some("Salon"));
        let undo = moli.turn(ask("Annule")).await.unwrap().proposals[0].id;
        moli.decide_proposal(undo, true, Origin::Ui, None)
            .await
            .unwrap();
        assert!(
            label_of(&hub).room.is_none(),
            "the label is cleared: the device is where it was"
        );
    }

    #[test]
    fn a_reading_matches_whole_words_not_the_middle_of_other_ones() {
        let sensor = json!({ "name": "Capteur", "online": true, "points": [
            { "key": "co2", "label": "CO2", "value": 600, "unit": "ppm" },
            { "key": "temperature", "label": "Temperature", "value": 21.0, "unit": "°C" },
            { "key": "illuminance", "label": "Illuminance", "value": 120, "unit": "lx" } ] });
        // « faire » and « éclairage » hold « air » without asking for the air.
        let said = reading_words(
            &sensor,
            "Qu'est-ce que tu peux me faire voir sur le capteur ?",
        );
        assert!(
            said.contains("température") && said.contains("CO₂"),
            "{said}"
        );
        assert_eq!(
            reading_words(&sensor, "Quel est l'éclairement ?"),
            "Capteur : éclairement 120 lx."
        );
        assert_eq!(
            reading_words(&sensor, "L'air est chaud ?"),
            "Capteur : CO₂ 600 ppm, température 21 °C."
        );
        assert_eq!(
            reading_words(&sensor, "L'air de la pièce est bon ?"),
            "Capteur : CO₂ 600 ppm."
        );
    }

    #[test]
    fn two_warm_ups_close_together_become_one_more_not_a_queue() {
        let gate = WarmGate::default();
        assert!(gate.request(), "the first one runs");
        assert!(
            !gate.request(),
            "a second while one runs only marks the prompt as stale"
        );
        assert!(!gate.request());
        assert!(
            gate.finished(),
            "one more run for everything that came meanwhile"
        );
        assert!(!gate.finished(), "and then it rests");
        assert!(gate.request(), "ready for the next change");
    }

    #[test]
    fn the_outcomes_of_an_order_are_worded() {
        assert_eq!(
            order_words(&json!({ "status": "done" }), "Lampe"),
            moli_i18n::tr!("assistant.house.done", device = "Lampe")
        );
        let held = order_words(&json!({ "status": "awaiting_approval" }), "Lampe");
        assert!(
            held.contains("Lampe") && held.contains("approbation"),
            "{held}"
        );
        let failed = order_words(
            &json!({ "status": "failed", "error": "unreachable" }),
            "Lampe",
        );
        assert!(
            failed.contains("pas pu") && failed.contains("Lampe"),
            "{failed}"
        );
        assert!(
            !failed.contains("unreachable"),
            "the Hub's English stays in the action: {failed}"
        );
        assert_eq!(
            order_words(&json!({ "error": "point and value are required" }), "?"),
            unknown()
        );
    }
}
