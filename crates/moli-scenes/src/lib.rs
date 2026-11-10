//! Scenes: the lights of several rooms set to a look in one gesture (an
//! evening by the fire, everything bright, a party), and the rhythm of the
//! day ([`rhythm`]): the white and the level a light takes by the hour.
//!
//! The house declares them in `moli.toml`; Moli runs them as one device,
//! `scenes:maison`, that a page, an automation, an agent or a wall switch
//! commands like any other:
//!
//! - `scene`: the scene in place (`none` when there is none); writing one
//!   puts the lights in it.
//! - `order`: `next` or `previous`, to go round the scenes from a button.
//! - `rhythm` (with a `[rhythm]`): whether lights follow the day.
//!
//! ```toml
//! [[scene]]
//! id = "soiree"               # never rename it: automations point at it
//! name = "Soirée"
//! cycle = true                # one of those `order` goes round
//! lights = [
//!   { id = "lumieres:suspensions", off = true },
//!   { id = "z2m:0x…", color = "#ffb060", brightness = 22, effect = "flame" },
//!   { id = "hue:…", white = 2400, brightness = 30 },
//! ]
//! ```
//!
//! A light a scene does not name stays as it is. Orders go through the hub
//! as the house's own (like a fixture's), one light after the other.

pub mod rhythm;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use moli_ambiance::{Lamp, Look, Setting};
use moli_core::{
    Access, Device, DeviceId, Event, Kind, Origin, PointId, PointSpec, Semantic, Value,
};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx, Hub};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

pub use rhythm::{Rhythm, Stop};

/// The instance (and the device's prefix: `scenes:maison`).
pub const INSTANCE: &str = "scenes";
/// The one device.
const DEVICE: &str = "maison";
/// The value of `scene` when no scene is in place.
pub const NONE: &str = "none";
const NEXT: &str = "next";
const PREVIOUS: &str = "previous";
const NO_EFFECT: &str = "no_effect";

/// A scene just chosen is not judged « over » before its lights had time to
/// come on.
const SETTLE: Duration = Duration::from_secs(30);
/// How often lights that follow the day are brought up to date.
const FOLLOW_EVERY: Duration = Duration::from_secs(600);
/// A level this far (percent) from what the rhythm set: a person chose it.
const TAKEN_OVER: f64 = 4.0;

/// One scene, as `moli.toml` declares it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scene {
    /// Stable id: the value of `scenes:maison/scene`.
    pub id: String,
    pub name: String,
    /// One of the scenes `order` goes round (all of them when none says so).
    #[serde(default)]
    pub cycle: bool,
    pub lights: Vec<Step>,
}

/// What a scene asks of one light.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// A device id (a fixture counts as one light).
    pub id: String,
    /// Switched off; nothing else may be asked then.
    #[serde(default)]
    pub off: bool,
    /// `#rrggbb`, for a lamp that takes colours.
    #[serde(default)]
    pub color: Option<String>,
    /// Kelvins: for a lamp without colour, or when no colour is given.
    #[serde(default)]
    pub white: Option<u32>,
    /// Percent.
    #[serde(default)]
    pub brightness: Option<f64>,
    /// An animation, played where the lamp (or a fixture's bulb) has it.
    #[serde(default)]
    pub effect: Option<String>,
}

impl Step {
    fn device(&self) -> DeviceId {
        DeviceId::from(self.id.as_str())
    }

    fn setting(&self) -> Setting {
        Setting {
            color: self.color.clone(),
            white: self.white,
            brightness: self.brightness,
            effect: self.effect.clone(),
        }
    }
}

/// `[[scene]]` and `[rhythm]` checked.
pub fn check(scenes: &[Scene], rhythm: Option<&Rhythm>) -> Result<(), String> {
    let mut ids = HashSet::new();
    for scene in scenes {
        let id_ok = !scene.id.is_empty()
            && scene.id.len() <= 32
            && scene
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !id_ok {
            return Err(format!("scene id {:?}: a-z, 0-9 or '-' only", scene.id));
        }
        if [NONE, NEXT, PREVIOUS].contains(&scene.id.as_str()) {
            return Err(format!("scene id {:?} is reserved", scene.id));
        }
        if !ids.insert(&scene.id) {
            return Err(format!("duplicate scene id {:?}", scene.id));
        }
        if scene.name.trim().is_empty() {
            return Err(format!("scene {:?}: a name", scene.id));
        }
        if scene.lights.is_empty() {
            return Err(format!("scene {:?}: at least one light", scene.id));
        }
        let mut lights = HashSet::new();
        for step in &scene.lights {
            let wrong = |what: &str| Err(format!("scene {:?}, {:?}: {what}", scene.id, step.id));
            if !step.id.contains(':') || step.id.contains('/') {
                return wrong("not a device id");
            }
            if !lights.insert(&step.id) {
                return wrong("named twice");
            }
            let asks = step.color.is_some()
                || step.white.is_some()
                || step.brightness.is_some()
                || step.effect.is_some();
            if step.off && asks {
                return wrong("off, or a look, not both");
            }
            if let Some(color) = &step.color
                && moli_core::color::hex_to_xy(color).is_none()
            {
                return wrong("color is « #rrggbb »");
            }
            if step.white.is_some_and(|k| !(1500..=6500).contains(&k)) {
                return wrong("white from 1500 to 6500 K");
            }
            if step.brightness.is_some_and(|b| !(1.0..=100.0).contains(&b)) {
                return wrong("brightness from 1 to 100");
            }
            if step.effect.as_deref().is_some_and(|e| e.trim().is_empty()) {
                return wrong("an empty effect");
            }
        }
    }
    rhythm.map_or(Ok(()), Rhythm::check)
}

/// The scene after (or before) `current` among those a button goes round.
/// From no scene (or one outside the round): the first one.
fn step_from<'a>(scenes: &'a [Scene], current: &str, forward: bool) -> Option<&'a Scene> {
    let any = scenes.iter().any(|s| s.cycle);
    let round: Vec<&Scene> = scenes.iter().filter(|s| s.cycle || !any).collect();
    let here = round.iter().position(|s| s.id == current);
    let next = match (here, forward) {
        (None, _) => 0,
        (Some(i), true) => (i + 1) % round.len(),
        (Some(i), false) => (i + round.len() - 1) % round.len(),
    };
    round.get(next).copied()
}

// ---- reading and commanding the lights --------------------------------------------------

fn truthy(value: &Value) -> bool {
    match value {
        Value::Bool(on) => *on,
        Value::Text(text) => text.eq_ignore_ascii_case("on"),
        _ => false,
    }
}

/// Lit, as the house knows it: an unreachable light is not.
fn lit(hub: &Hub, id: &DeviceId) -> bool {
    let Some(view) = hub.device(id) else {
        return false;
    };
    if view.online == Some(false) {
        return false;
    }
    view.device
        .points
        .iter()
        .filter(|p| p.access.write && p.semantic == Semantic::OnOff)
        .find_map(|p| view.state.get(&p.key))
        .is_some_and(|sample| truthy(&sample.value))
}

async fn send(hub: &Hub, point: &PointId, value: Value) -> bool {
    let actor = moli_i18n::tr!("pilotes.scenes.nom");
    match hub.command(point, value, Origin::System, Some(actor)).await {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%point, %error, "scene order refused");
            false
        }
    }
}

/// Puts one light where the scene wants it.
async fn set_light(hub: &Hub, step: &Step) {
    let Some(view) = hub.device(&step.device()) else {
        tracing::warn!(light = %step.id, "scene: unknown light");
        return;
    };
    if step.off {
        if let Some((point, value)) = moli_ambiance::switch(&view, false) {
            send(hub, &point, value).await;
        }
        return;
    }
    let setting = step.setting();
    match Lamp::of(&view) {
        Some(lamp) => {
            let orders = moli_ambiance::plan(Look::Set(&setting), std::slice::from_ref(&lamp));
            for (point, value) in orders.into_iter().flatten() {
                if !send(hub, &point, value).await {
                    return;
                }
            }
        }
        // A plain switch: on is all it knows.
        None => match moli_ambiance::switch(&view, true) {
            Some((point, value)) => {
                if !send(hub, &point, value).await {
                    return;
                }
            }
            None => return,
        },
    }
    // A fixture has no animation of its own: its bulbs play it (or stop
    // theirs when the scene asks for none).
    for member in &view.device.members {
        let Some(bulb) = hub
            .device(member)
            .filter(|v| v.online != Some(false))
            .and_then(|v| Lamp::of(&v))
        else {
            continue;
        };
        let point = PointId::new(&bulb.id, "effect");
        match setting.effect.as_deref().filter(|e| bulb.can_play(e)) {
            Some(effect) => {
                send(hub, &point, Value::from(effect)).await;
            }
            None if bulb.playing() => {
                send(hub, &point, Value::from(NO_EFFECT)).await;
            }
            None => {}
        }
    }
}

/// Puts the lights in the scene, one after the other. Gives up as soon as
/// another scene is chosen (`generation` moved on).
async fn apply(hub: Hub, scene: Scene, generation: Arc<AtomicU64>, mine: u64) {
    for step in &scene.lights {
        if generation.load(Ordering::Relaxed) != mine {
            return;
        }
        set_light(&hub, step).await;
    }
}

/// Gives a lit light the white and the level of the hour (never switches it on).
async fn follow(hub: Hub, id: DeviceId, white: u32, brightness: f64) {
    let Some(lamp) = hub.device(&id).and_then(|v| Lamp::of(&v)) else {
        return;
    };
    let setting = Setting {
        white: Some(white),
        brightness: Some(brightness),
        ..Setting::default()
    };
    let orders = moli_ambiance::plan(Look::Set(&setting), std::slice::from_ref(&lamp));
    // The first order switches on: the light is on already, and one switched
    // off in the meantime must stay off.
    for (point, value) in orders.into_iter().flatten().skip(1) {
        if !lit(&hub, &id) || !send(&hub, &point, value).await {
            return;
        }
    }
}

// ---- the device -----------------------------------------------------------------------------

fn device(ctx: &DriverCtx, scenes: &[Scene], rhythm: bool) -> Device {
    let spec = |key: &str, label: String, kind: Kind, read: bool| PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access { read, write: true },
        unit: None,
        semantic: Semantic::Other,
    };
    let mut values: Vec<Arc<str>> = vec![NONE.into()];
    values.extend(scenes.iter().map(|s| Arc::from(s.id.as_str())));
    let mut points = vec![
        spec(
            "scene",
            moli_i18n::tr!("pilotes.scenes.scene"),
            Kind::Enum { values },
            true,
        ),
        spec(
            "order",
            moli_i18n::tr!("pilotes.scenes.ordre"),
            Kind::Enum {
                values: vec![NEXT.into(), PREVIOUS.into()],
            },
            false,
        ),
    ];
    if rhythm {
        points.push(spec(
            "rhythm",
            moli_i18n::tr!("pilotes.scenes.rythme"),
            Kind::Binary,
            true,
        ));
    }
    Device {
        id: ctx.device_id(DEVICE),
        instance: ctx.instance().clone(),
        native_name: moli_i18n::tr!("pilotes.scenes.nom").into(),
        manufacturer: Some("Moli".into()),
        // An identifier pages recognize the device by: never translated.
        model: Some("Scenes".into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

// ---- the driver ------------------------------------------------------------------------------

#[derive(Debug)]
pub struct Scenes {
    hub: Hub,
    list: Vec<Scene>,
    rhythm: Option<Rhythm>,
    zone: jiff::tz::TimeZone,
}

impl Scenes {
    /// `zone`: the house's time zone (the rhythm follows local time).
    #[must_use]
    pub fn new(
        hub: Hub,
        scenes: Vec<Scene>,
        rhythm: Option<Rhythm>,
        zone: jiff::tz::TimeZone,
    ) -> Self {
        Self {
            hub,
            list: scenes,
            rhythm,
            zone,
        }
    }

    /// Minutes since local midnight.
    fn minute(&self) -> u32 {
        let now = jiff::Timestamp::now().to_zoned(self.zone.clone());
        u32::from(now.hour().unsigned_abs()) * 60 + u32::from(now.minute().unsigned_abs())
    }

    fn follows(&self, id: &DeviceId) -> bool {
        self.rhythm
            .as_ref()
            .is_some_and(|r| r.lights.iter().any(|l| l == id.as_str()))
    }

    /// Every light a scene or the rhythm names.
    fn watched(&self) -> HashSet<DeviceId> {
        let scenes = self
            .list
            .iter()
            .flat_map(|s| s.lights.iter().map(Step::device));
        let rhythm = self
            .rhythm
            .iter()
            .flat_map(|r| r.lights.iter().map(|l| DeviceId::from(l.as_str())));
        scenes.chain(rhythm).collect()
    }
}

/// What the driver keeps between two events.
struct Run {
    id: DeviceId,
    /// The scene in place (`none`), and since when.
    scene: String,
    since: Instant,
    /// Moves on at each scene chosen: a scene still being applied gives up.
    generation: Arc<AtomicU64>,
    rhythm_on: bool,
    lit: HashMap<DeviceId, bool>,
    /// The lights following the day, and what they were last given.
    following: HashMap<DeviceId, (u32, f64)>,
}

impl Scenes {
    fn choose(&self, ctx: &DriverCtx, run: &mut Run, scene: &Scene) {
        run.scene.clone_from(&scene.id);
        run.since = Instant::now();
        run.following.clear();
        ctx.set_state(&run.id, "scene", Value::from(scene.id.as_str()));
        let mine = run.generation.fetch_add(1, Ordering::Relaxed) + 1;
        tokio::spawn(apply(
            self.hub.clone(),
            scene.clone(),
            run.generation.clone(),
            mine,
        ));
    }

    fn clear(ctx: &DriverCtx, run: &mut Run) {
        run.scene = NONE.into();
        ctx.set_state(&run.id, "scene", Value::from(NONE));
    }

    fn command(&self, ctx: &DriverCtx, run: &mut Run, command: CommandRequest) {
        if command.abandoned() {
            return;
        }
        let text = match &command.value {
            Value::Text(text) => Some(&**text),
            _ => None,
        };
        let result = match (&*command.key, text, &command.value) {
            ("scene", Some(NONE), _) => {
                // The lights stay as they are: only the name goes.
                run.generation.fetch_add(1, Ordering::Relaxed);
                Self::clear(ctx, run);
                Ok(())
            }
            ("scene", Some(id), _) => match self.list.iter().find(|s| s.id == id) {
                Some(scene) => {
                    self.choose(ctx, run, scene);
                    Ok(())
                }
                None => Err(moli_i18n::tr!("pilotes.scenes.inconnue")),
            },
            ("order", Some(way @ (NEXT | PREVIOUS)), _) => {
                match step_from(&self.list, &run.scene, way == NEXT) {
                    Some(scene) => {
                        self.choose(ctx, run, scene);
                        Ok(())
                    }
                    None => Err(moli_i18n::tr!("pilotes.scenes.inconnue")),
                }
            }
            ("rhythm", _, Value::Bool(on)) if self.rhythm.is_some() => {
                run.rhythm_on = *on;
                if !on {
                    run.following.clear();
                }
                ctx.set_state(&run.id, "rhythm", Value::Bool(*on));
                Ok(())
            }
            _ => Err(moli_i18n::tr!("pilotes.scenes.ordre_inconnu")),
        };
        command.reply(result);
    }

    /// A watched light may have changed: switched on, it takes the white of
    /// the hour; once every light of the scene is off, the scene is over.
    fn look(&self, ctx: &DriverCtx, run: &mut Run, light: &DeviceId) {
        let now = lit(&self.hub, light);
        let before = run.lit.insert(light.clone(), now);
        if !now {
            run.following.remove(light);
        }
        if before == Some(false)
            && now
            && run.rhythm_on
            && run.scene == NONE
            && self.follows(light)
            && let Some(rhythm) = &self.rhythm
        {
            let (white, brightness) = rhythm.at(self.minute());
            run.following.insert(light.clone(), (white, brightness));
            tokio::spawn(follow(self.hub.clone(), light.clone(), white, brightness));
        }
        if run.scene != NONE
            && run.since.elapsed() > SETTLE
            && let Some(scene) = self.list.iter().find(|s| s.id == run.scene)
            && !scene
                .lights
                .iter()
                .any(|step| !step.off && run.lit.get(&step.device()) == Some(&true))
        {
            Self::clear(ctx, run);
        }
    }

    /// The day moved on: the lights still as the rhythm left them slide to
    /// the white of the hour. One a person dimmed or coloured is theirs
    /// until it is switched off.
    fn keep_up(&self, run: &mut Run) {
        let Some(rhythm) = &self.rhythm else {
            return;
        };
        let (white, brightness) = rhythm.at(self.minute());
        let lights: Vec<DeviceId> = run.following.keys().cloned().collect();
        for light in lights {
            let Some((given_white, given_level)) = run.following.get(&light).copied() else {
                continue;
            };
            let state = |key: &str| {
                self.hub
                    .state(&PointId::new(&light, key))
                    .map(|sample| sample.value)
            };
            let dimmed = state("brightness")
                .and_then(|v| v.as_f64())
                .is_some_and(|level| level <= 100.0 && (level - given_level).abs() > TAKEN_OVER);
            let coloured = state("color_temp") == Some(Value::Null);
            if dimmed || coloured || !lit(&self.hub, &light) {
                run.following.remove(&light);
                continue;
            }
            if white.abs_diff(given_white) >= 50 || (brightness - given_level).abs() >= 2.0 {
                run.following.insert(light.clone(), (white, brightness));
                tokio::spawn(follow(self.hub.clone(), light, white, brightness));
            }
        }
    }
}

enum Next {
    Command(CommandRequest),
    Event(Result<Event, RecvError>),
    Tick,
    Stop,
}

impl Driver for Scenes {
    fn kind(&self) -> &'static str {
        "scenes"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            // Subscribe before reading: nothing falls in between.
            let mut events = self.hub.subscribe();
            let watched = self.watched();
            let published = device(ctx, &self.list, self.rhythm.is_some());
            let id = published.id.clone();
            ctx.upsert_device(published);
            // What was in place before a restart, if it still exists.
            let scene = match ctx.current(&id, "scene") {
                Some(Value::Text(text)) if self.list.iter().any(|s| s.id == *text) => {
                    text.to_string()
                }
                _ => NONE.into(),
            };
            let rhythm_on = ctx.current(&id, "rhythm") != Some(Value::Bool(false));
            ctx.set_state(&id, "scene", Value::from(scene.as_str()));
            if self.rhythm.is_some() {
                ctx.set_state(&id, "rhythm", Value::Bool(rhythm_on));
            }
            let mut run = Run {
                id,
                scene,
                since: Instant::now(),
                generation: Arc::new(AtomicU64::new(0)),
                rhythm_on,
                lit: watched
                    .iter()
                    .map(|light| (light.clone(), lit(&self.hub, light)))
                    .collect(),
                following: HashMap::new(),
            };
            let mut tick = tokio::time::interval(FOLLOW_EVERY);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ctx.ready();
            loop {
                let next = tokio::select! {
                    command = ctx.next_command() => command.map_or(Next::Stop, Next::Command),
                    event = events.recv() => Next::Event(event),
                    _ = tick.tick(), if !run.following.is_empty() => Next::Tick,
                };
                match next {
                    Next::Stop | Next::Event(Err(RecvError::Closed)) => return Ok(()),
                    Next::Command(command) => self.command(ctx, &mut run, command),
                    Next::Tick => self.keep_up(&mut run),
                    // Events were missed: read every light again, without
                    // taking a difference for someone switching on.
                    Next::Event(Err(RecvError::Lagged(_))) => {
                        for light in &watched {
                            run.lit.insert(light.clone(), lit(&self.hub, light));
                        }
                    }
                    Next::Event(Ok(event)) => {
                        let light = match &event {
                            Event::State { point, .. } => point.split().map(|(d, _)| d),
                            Event::Availability { device, .. } => Some(device.clone()),
                            _ => None,
                        };
                        if let Some(light) = light.filter(|l| watched.contains(l)) {
                            self.look(ctx, &mut run, &light);
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUSE: &str = r##"
        [[scene]]
        id = "soiree"
        name = "Soirée"
        cycle = true
        lights = [
          { id = "lumieres:suspensions", off = true },
          { id = "z2m:0x01", color = "#ffb060", brightness = 22, effect = "flame" },
          { id = "hue:a", white = 2400, brightness = 30 },
        ]

        [[scene]]
        id = "plein-feu"
        name = "Plein feu"
        cycle = true
        lights = [{ id = "lumieres:suspensions", white = 4000, brightness = 100 }]

        [[scene]]
        id = "fete"
        name = "Fête"
        lights = [{ id = "z2m:0x01", effect = "colorloop" }]
    "##;

    #[derive(Deserialize)]
    struct File {
        scene: Vec<Scene>,
    }

    fn house() -> Vec<Scene> {
        toml::from_str::<File>(HOUSE).unwrap().scene
    }

    #[test]
    fn a_house_declares_its_scenes() {
        let scenes = house();
        check(&scenes, None).unwrap();
        assert_eq!(scenes[0].lights.len(), 3);
        assert!(scenes[0].lights[0].off);
        let strip = scenes[0].lights[1].setting();
        assert_eq!(strip.color.as_deref(), Some("#ffb060"));
        assert_eq!(strip.brightness, Some(22.0));
        assert_eq!(strip.effect.as_deref(), Some("flame"));
        assert_eq!(scenes[0].lights[2].setting().white, Some(2400));
    }

    #[test]
    fn a_button_goes_round_the_chosen_scenes() {
        let scenes = house();
        let id = |current: &str, forward| step_from(&scenes, current, forward).unwrap().id.clone();
        assert_eq!(id(NONE, true), "soiree");
        assert_eq!(id("soiree", true), "plein-feu");
        assert_eq!(id("plein-feu", true), "soiree");
        assert_eq!(id("soiree", false), "plein-feu");
        // From a scene outside the round: back to the first one.
        assert_eq!(id("fete", true), "soiree");
        // No scene marked: all of them go round.
        let mut all = house();
        for scene in &mut all {
            scene.cycle = false;
        }
        assert_eq!(step_from(&all, "plein-feu", true).unwrap().id, "fete");
        assert!(step_from(&[], NONE, true).is_none());
    }

    #[test]
    fn a_wrong_scene_is_refused() {
        let broken = |change: fn(&mut Vec<Scene>)| {
            let mut scenes = house();
            change(&mut scenes);
            check(&scenes, None)
        };
        assert!(broken(|s| s[0].id = "Soirée".into()).is_err());
        assert!(broken(|s| s[0].id = "none".into()).is_err());
        assert!(broken(|s| s[1].id = "soiree".into()).is_err());
        assert!(broken(|s| s[0].name = " ".into()).is_err());
        assert!(broken(|s| s[0].lights.clear()).is_err());
        assert!(broken(|s| s[0].lights[1].id = "strip".into()).is_err());
        assert!(broken(|s| s[0].lights[1].id = "hue:a".into()).is_err());
        assert!(broken(|s| s[0].lights[0].brightness = Some(50.0)).is_err());
        assert!(broken(|s| s[0].lights[1].color = Some("orange".into())).is_err());
        assert!(broken(|s| s[0].lights[2].white = Some(900)).is_err());
        assert!(broken(|s| s[0].lights[2].brightness = Some(0.0)).is_err());
        let rhythm = Rhythm {
            lights: Vec::new(),
            curve: rhythm::default_curve(),
        };
        assert!(check(&house(), Some(&rhythm)).is_err());
    }
}
