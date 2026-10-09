//! Light fixtures (D12): several smart bulbs and/or the relay that feeds
//! them, seen and commanded as **one** light — the three pendants of the
//! salon behind their wall relay, not five devices.
//!
//! Orders go to the bulbs first; the relay is the last resort: switching
//! off switches the bulbs off and keeps them powered (still reachable),
//! the relay only cuts when no bulb answers; switching on restores the
//! relay if it is cut (the order is answered as soon as the relay obeyed),
//! then lights and sets the bulbs once they are back. A bulb that does not
//! answer has no power (a wall switch): it never counts as lit.
//!
//! Moli runs it as instance [`INSTANCE`] whenever `moli.toml` declares
//! fixtures:
//!
//! ```toml
//! [[fixture]]
//! id = "suspensions"          # the device is lumieres:suspensions
//! name = "Suspensions"
//! room = "Salon"
//! bulbs = ["hue:…", "hue:…", "hue:…"]
//! power = "tuya:…/switch_1"   # the relay that feeds them (optional)
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use moli_core::{
    Access, Device, DeviceId, Event, InstanceId, Kind, Origin, PointId, PointSpec, Semantic, Unit,
    Value,
};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx, Hub};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;

/// The instance (and the devices' prefix: `lumieres:suspensions`).
pub const INSTANCE: &str = "lumieres";
/// How long bulbs may take to come back once their relay is on again.
const COMEBACK: Duration = Duration::from_secs(15);
const LOOK_AGAIN: Duration = Duration::from_millis(400);

/// One fixture, as `moli.toml` declares it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    /// Stable id: the device is `lumieres:<id>`. Never rename it (labels,
    /// plan, automations point at it).
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub room: Option<String>,
    /// The smart bulbs (device ids): orders go to them first.
    #[serde(default)]
    pub bulbs: Vec<String>,
    /// The relay that feeds them (its on/off point, `device/key`).
    #[serde(default)]
    pub power: Option<String>,
}

impl Fixture {
    fn power_point(&self) -> Option<PointId> {
        self.power.as_ref().map(|p| PointId::from(p.as_str()))
    }

    fn members(&self) -> Vec<DeviceId> {
        let mut out: Vec<DeviceId> = self
            .bulbs
            .iter()
            .map(|b| DeviceId::from(b.as_str()))
            .collect();
        if let Some((device, _)) = self.power_point().as_ref().and_then(PointId::split) {
            out.push(device);
        }
        out
    }
}

/// `[[fixture]]` checked: ids, members, nobody in two fixtures.
pub fn check(fixtures: &[Fixture]) -> Result<(), String> {
    let mut ids = HashSet::new();
    let mut taken = HashSet::new();
    for f in fixtures {
        let id_ok = !f.id.is_empty()
            && f.id.len() <= 32
            && f.id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !id_ok {
            return Err(format!("fixture id {:?}: a-z, 0-9 or '-' only", f.id));
        }
        if !ids.insert(&f.id) {
            return Err(format!("duplicate fixture id {:?}", f.id));
        }
        if f.name.trim().is_empty() {
            return Err(format!("fixture {:?}: a name", f.id));
        }
        if f.bulbs.is_empty() && f.power.is_none() {
            return Err(format!("fixture {:?}: bulbs, a power relay, or both", f.id));
        }
        for bulb in &f.bulbs {
            if !bulb.contains(':') || bulb.contains('/') {
                return Err(format!("fixture {:?}: {bulb:?} is not a device id", f.id));
            }
            if !taken.insert(bulb.clone()) {
                return Err(format!("{bulb:?} is in two fixtures"));
            }
        }
        if let Some(power) = &f.power {
            let device = match PointId::from(power.as_str()).split() {
                Some((device, _)) if device.as_str().contains(':') => device,
                _ => {
                    return Err(format!(
                        "fixture {:?}: power {power:?} is not a point id",
                        f.id
                    ));
                }
            };
            if f.bulbs.iter().any(|b| b == device.as_str()) {
                return Err(format!(
                    "fixture {:?}: a bulb cannot be its own power",
                    f.id
                ));
            }
            if !taken.insert(power.clone()) {
                return Err(format!("{power:?} feeds two fixtures"));
            }
        }
    }
    Ok(())
}

// ---- what the members say -------------------------------------------------------------

/// A bulb, now.
#[derive(Clone, Debug, Default, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Bulb {
    /// It answers (an unpowered bulb does not).
    pub answers: bool,
    pub on: Option<bool>,
    /// Percent.
    pub brightness: Option<f64>,
    pub color_temp: Option<f64>,
    pub color: Option<String>,
    pub dims: bool,
    pub warms: bool,
    pub tints: bool,
}

/// The relay that feeds the bulbs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Supply {
    pub answers: bool,
    pub on: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Reading {
    pub supply: Option<Supply>,
    pub bulbs: Vec<Bulb>,
}

/// What the fixture shows.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    /// Something can be done with it (its relay answers, or a bulb does).
    pub online: bool,
    pub on: bool,
    /// Its bulbs have power and answer (false: cut at the relay or at a
    /// wall switch).
    pub powered: bool,
    pub brightness: Option<f64>,
    pub color_temp: Option<f64>,
    pub color: Option<String>,
}

impl Reading {
    fn cut(&self) -> bool {
        self.supply.is_some_and(|s| s.on == Some(false))
    }

    fn answering(&self) -> impl Iterator<Item = (usize, &Bulb)> {
        self.bulbs.iter().enumerate().filter(|(_, b)| b.answers)
    }

    #[must_use]
    pub fn shown(&self) -> Shown {
        let cut = self.cut();
        let answering: Vec<&Bulb> = self.answering().map(|(_, b)| b).collect();
        let lit: Vec<&Bulb> = answering
            .iter()
            .copied()
            .filter(|b| b.on == Some(true))
            .collect();
        let on = if self.bulbs.is_empty() {
            self.supply.is_some_and(|s| s.answers && s.on == Some(true))
        } else {
            !cut && !lit.is_empty()
        };
        // The level it shines at, or comes back at.
        let pool = if lit.is_empty() { &answering } else { &lit };
        let levels: Vec<f64> = pool.iter().filter_map(|b| b.brightness).collect();
        #[allow(clippy::cast_precision_loss)]
        let brightness = (!levels.is_empty())
            .then(|| (levels.iter().sum::<f64>() / levels.len() as f64 * 10.0).round() / 10.0);
        Shown {
            online: self.supply.is_none_or(|s| s.answers) || !answering.is_empty(),
            on,
            powered: !cut && (self.bulbs.is_empty() || !answering.is_empty()),
            brightness,
            color_temp: pool.iter().find_map(|b| b.color_temp),
            color: pool.iter().find_map(|b| b.color.clone()),
        }
    }
}

// ---- what to do ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Setting {
    On(bool),
    /// Percent.
    Brightness(f64),
    ColorTemp(f64),
    Color(Arc<str>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    Nothing,
    /// Orders to bulbs (by index), each bulb's in turn.
    Bulbs(Vec<(usize, Vec<Setting>)>),
    /// Restore the relay, then give the bulbs that come back these.
    PowerUp(Vec<Setting>),
    /// Cut the relay (no bulb answers to be switched off).
    PowerDown,
    Refuse(String),
}

/// The bulbs first, the relay as a last resort.
#[must_use]
pub fn plan(r: &Reading, order: &Setting) -> Plan {
    let order = match order {
        Setting::Brightness(b) if *b <= 0.0 => Setting::On(false),
        o => o.clone(),
    };
    let answering: Vec<usize> = r.answering().map(|(i, _)| i).collect();
    if order == Setting::On(false) {
        if answering.is_empty() {
            let fed = r.supply.is_some_and(|s| s.on != Some(false));
            return if fed { Plan::PowerDown } else { Plan::Nothing };
        }
        let lit: Vec<(usize, Vec<Setting>)> = answering
            .into_iter()
            .filter(|i| r.bulbs[*i].on != Some(false))
            .map(|i| (i, vec![Setting::On(false)]))
            .collect();
        return if lit.is_empty() {
            Plan::Nothing
        } else {
            Plan::Bulbs(lit)
        };
    }
    if r.bulbs.is_empty() {
        return match r.supply {
            Some(s) if s.on != Some(true) => Plan::PowerUp(Vec::new()),
            _ => Plan::Nothing,
        };
    }
    if r.cut() {
        let mut then = vec![Setting::On(true)];
        if order != Setting::On(true) {
            then.push(order);
        }
        return Plan::PowerUp(then);
    }
    if answering.is_empty() {
        return Plan::Refuse(if r.supply.is_some() {
            moli_i18n::tr!("pilotes.lights.ampoules_muettes")
        } else {
            moli_i18n::tr!("pilotes.lights.coupee_interrupteur")
        });
    }
    let mut orders = Vec::new();
    for i in answering {
        let b = &r.bulbs[i];
        let mut settings = Vec::new();
        if b.on != Some(true) {
            settings.push(Setting::On(true));
        }
        let can = match &order {
            Setting::Brightness(_) => b.dims,
            Setting::ColorTemp(_) => b.warms,
            Setting::Color(_) => b.tints,
            Setting::On(_) => false,
        };
        if can {
            settings.push(order.clone());
        }
        if !settings.is_empty() {
            orders.push((i, settings));
        }
    }
    if orders.is_empty() {
        Plan::Nothing
    } else {
        Plan::Bulbs(orders)
    }
}

// ---- speaking to the members ------------------------------------------------------------

/// A member's points, by meaning (any brand: Hue's `on`, Zigbee2MQTT's
/// `state` « ON », a brightness out of 254…).
#[derive(Clone, Debug, Default, PartialEq)]
struct Keys {
    on: Option<Switch>,
    brightness: Option<(Arc<str>, f64)>,
    color_temp: Option<(Arc<str>, Option<f64>, Option<f64>)>,
    color: Option<Arc<str>>,
}

#[derive(Clone, Debug, PartialEq)]
struct Switch {
    key: Arc<str>,
    /// Takes « ON » / « OFF » rather than true / false.
    text: bool,
}

fn switch_of(spec: &PointSpec) -> Option<Switch> {
    let text = match &spec.kind {
        Kind::Binary => false,
        Kind::Enum { values } if values.iter().any(|v| v.eq_ignore_ascii_case("on")) => true,
        _ => return None,
    };
    Some(Switch {
        key: spec.key.clone(),
        text,
    })
}

fn keys(device: &Device) -> Keys {
    let writable = || device.points.iter().filter(|p| p.access.write);
    let by = |semantic: Semantic| writable().find(|p| p.semantic == semantic);
    Keys {
        on: by(Semantic::OnOff).and_then(switch_of),
        brightness: by(Semantic::Brightness).map(|p| {
            let max = match (&p.kind, &p.unit) {
                (_, Some(Unit::Percent)) => 100.0,
                (Kind::Numeric { max: Some(max), .. }, _) => *max,
                _ => 100.0,
            };
            (p.key.clone(), max)
        }),
        color_temp: by(Semantic::ColorTemp).map(|p| match &p.kind {
            Kind::Numeric { min, max, .. } => (p.key.clone(), *min, *max),
            _ => (p.key.clone(), None, None),
        }),
        color: by(Semantic::Color)
            .filter(|p| p.kind == Kind::Text)
            .map(|p| p.key.clone()),
    }
}

fn truthy(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::Int(i) => Some(*i != 0),
        Value::Text(t) => match t.to_ascii_lowercase().as_str() {
            "on" | "true" => Some(true),
            "off" | "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn on_value(text: bool, on: bool) -> Value {
    if text {
        Value::from(if on { "ON" } else { "OFF" })
    } else {
        Value::Bool(on)
    }
}

/// The fixture's members as the hub knows them now.
fn read(hub: &Hub, fixture: &Fixture) -> (Reading, Vec<Keys>) {
    let mut keyring = Vec::new();
    let bulbs = fixture
        .bulbs
        .iter()
        .map(|id| {
            let Some(view) = hub.device(&DeviceId::from(id.as_str())) else {
                keyring.push(Keys::default());
                return Bulb::default();
            };
            let k = keys(&view.device);
            let get = |key: &str| view.state.get(key).map(|s| &s.value);
            let bulb = Bulb {
                answers: view.online != Some(false),
                on: k.on.as_ref().and_then(|s| get(&s.key)).and_then(truthy),
                brightness: k
                    .brightness
                    .as_ref()
                    .and_then(|(key, max)| Some(get(key)?.as_f64()? * 100.0 / max)),
                color_temp: k
                    .color_temp
                    .as_ref()
                    .and_then(|(key, ..)| get(key)?.as_f64()),
                color: k.color.as_ref().and_then(|key| match get(key)? {
                    Value::Text(t) => Some(t.to_string()),
                    _ => None,
                }),
                dims: k.brightness.is_some(),
                warms: k.color_temp.is_some(),
                tints: k.color.is_some(),
            };
            keyring.push(k);
            bulb
        })
        .collect();
    let supply = fixture.power_point().map(|point| {
        let view = point.split().and_then(|(device, _)| hub.device(&device));
        Supply {
            answers: view.as_ref().is_some_and(|v| v.online != Some(false)),
            on: hub.state(&point).and_then(|s| truthy(&s.value)),
        }
    });
    (Reading { supply, bulbs }, keyring)
}

/// One bulb's settings as (point, value) orders.
fn orders(bulb: &str, keys: &Keys, settings: &[Setting]) -> Vec<(PointId, Value)> {
    let device = DeviceId::from(bulb);
    settings
        .iter()
        .filter_map(|s| {
            let (key, value) = match s {
                Setting::On(on) => {
                    let switch = keys.on.as_ref()?;
                    (switch.key.clone(), on_value(switch.text, *on))
                }
                Setting::Brightness(pct) => {
                    let (key, max) = keys.brightness.as_ref()?;
                    let v = if (*max - 100.0).abs() < f64::EPSILON {
                        *pct
                    } else {
                        (pct * max / 100.0).round().max(1.0)
                    };
                    (key.clone(), Value::Float(v))
                }
                Setting::ColorTemp(m) => {
                    let (key, min, max) = keys.color_temp.as_ref()?;
                    let v = m.max(min.unwrap_or(f64::MIN)).min(max.unwrap_or(f64::MAX));
                    (key.clone(), Value::Float(v))
                }
                Setting::Color(c) => (keys.color.clone()?, Value::Text(c.clone())),
            };
            Some((PointId::new(&device, &key), value))
        })
        .collect()
}

/// Who the journal says gave the order.
fn actor(fixture: &Fixture) -> String {
    moli_i18n::tr!("pilotes.lights.acteur", name = fixture.name)
}

/// Gives the bulbs their orders, the bulbs side by side. Fine if one bulb
/// obeyed entirely; otherwise the first error.
async fn give(
    hub: &Hub,
    fixture: &Fixture,
    keyring: &[Keys],
    plan: Vec<(usize, Vec<Setting>)>,
) -> Result<(), String> {
    let actor = actor(fixture);
    let runs = plan.into_iter().map(|(i, settings)| {
        let list = orders(&fixture.bulbs[i], &keyring[i], &settings);
        let actor = actor.clone();
        async move {
            for (point, value) in list {
                hub.command(&point, value, Origin::System, Some(actor.clone()))
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Ok::<(), String>(())
        }
    });
    let results = futures::future::join_all(runs).await;
    if results.is_empty() || results.iter().any(Result::is_ok) {
        Ok(())
    } else {
        results
            .into_iter()
            .find_map(Result::err)
            .map_or(Ok(()), Err)
    }
}

async fn power(hub: &Hub, fixture: &Fixture, on: bool) -> Result<(), String> {
    let Some(point) = fixture.power_point() else {
        return Ok(());
    };
    let text = point
        .split()
        .and_then(|(device, key)| {
            let view = hub.device(&device)?;
            switch_of(view.device.point(key)?)
        })
        .is_some_and(|s| s.text);
    let value = on_value(text, on);
    let actor = actor(fixture);
    hub.command(&point, value, Origin::System, Some(actor))
        .await
        .map_err(|e| e.to_string())
}

/// After the relay came back: once the bulbs answer (or time is up),
/// they get what was asked.
async fn after_power_up(hub: Hub, fixture: Fixture, settings: Vec<Setting>) {
    let until = Instant::now() + COMEBACK;
    let (reading, keyring) = loop {
        tokio::time::sleep(LOOK_AGAIN).await;
        let (reading, keyring) = read(&hub, &fixture);
        if reading.bulbs.iter().all(|b| b.answers) || Instant::now() >= until {
            break (reading, keyring);
        }
    };
    for setting in settings {
        match plan(&reading, &setting) {
            Plan::Bulbs(orders) => {
                if let Err(e) = give(&hub, &fixture, &keyring, orders).await {
                    tracing::warn!(fixture = %fixture.id, error = %e, "bulbs back, not set");
                }
            }
            Plan::Refuse(why) => {
                tracing::warn!(fixture = %fixture.id, %why, "bulbs did not come back");
                return;
            }
            _ => {}
        }
    }
}

// ---- the device -----------------------------------------------------------------------------

fn spec(
    key: &str,
    label: &str,
    kind: Kind,
    write: bool,
    unit: Option<Unit>,
    semantic: Semantic,
) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access { read: true, write },
        unit,
        semantic,
    }
}

fn device(id: DeviceId, instance: &InstanceId, fixture: &Fixture, keyring: &[Keys]) -> Device {
    let mut points = vec![
        spec(
            "on",
            &moli_i18n::tr!("pilotes.lights.allume"),
            Kind::Binary,
            true,
            None,
            Semantic::OnOff,
        ),
        spec(
            "powered",
            &moli_i18n::tr!("pilotes.lights.sous_tension"),
            Kind::Binary,
            false,
            None,
            Semantic::Other,
        ),
    ];
    if keyring.iter().any(|k| k.brightness.is_some()) {
        let numeric = Kind::Numeric {
            min: Some(0.0),
            max: Some(100.0),
            step: None,
        };
        points.push(spec(
            "brightness",
            &moli_i18n::tr!("pilotes.lights.luminosite"),
            numeric,
            true,
            Some(Unit::Percent),
            Semantic::Brightness,
        ));
    }
    let temps: Vec<(Option<f64>, Option<f64>)> = keyring
        .iter()
        .filter_map(|k| k.color_temp.as_ref().map(|(_, min, max)| (*min, *max)))
        .collect();
    if !temps.is_empty() {
        // What every warm bulb can take.
        let min = temps.iter().filter_map(|t| t.0).reduce(f64::max);
        let max = temps.iter().filter_map(|t| t.1).reduce(f64::min);
        let numeric = Kind::Numeric {
            min,
            max,
            step: None,
        };
        points.push(spec(
            "color_temp",
            &moli_i18n::tr!("pilotes.lights.temperature_blanc"),
            numeric,
            true,
            Some(Unit::Other("mired".into())),
            Semantic::ColorTemp,
        ));
    }
    if keyring.iter().any(|k| k.color.is_some()) {
        points.push(spec(
            "color",
            &moli_i18n::tr!("pilotes.lights.couleur"),
            Kind::Text,
            true,
            None,
            Semantic::Color,
        ));
    }
    Device {
        id,
        instance: instance.clone(),
        native_name: fixture.name.as_str().into(),
        manufacturer: Some("Moli".into()),
        // An identifier the dashboard, the assistant and the energy estimate
        // recognize a fixture by: never translated.
        model: Some("Luminaire".into()),
        description: None,
        native_room: fixture.room.as_deref().map(Into::into),
        members: fixture.members(),
        points,
    }
}

// ---- the driver ------------------------------------------------------------------------------

#[derive(Debug)]
pub struct Fixtures {
    hub: Hub,
    fixtures: Vec<Fixture>,
}

impl Fixtures {
    #[must_use]
    pub fn new(hub: Hub, fixtures: Vec<Fixture>) -> Self {
        Self { hub, fixtures }
    }
}

/// A published point and how to read it from what is shown.
type Field = (&'static str, fn(&Shown) -> Value);

/// What the driver last published for a fixture.
#[derive(Default)]
struct Published {
    device: Option<Device>,
    shown: Option<Shown>,
}

enum Next {
    Command(CommandRequest),
    Event(Result<Event, RecvError>),
    Stop,
}

impl Fixtures {
    fn refresh(&self, ctx: &DriverCtx, i: usize, published: &mut Published) {
        let fixture = &self.fixtures[i];
        let id = ctx.device_id(&fixture.id);
        let (reading, keyring) = read(&self.hub, fixture);
        let device = device(id.clone(), ctx.instance(), fixture, &keyring);
        if published.device.as_ref() != Some(&device) {
            ctx.upsert_device(device.clone());
            published.device = Some(device);
            published.shown = None;
        }
        let shown = reading.shown();
        let before = published.shown.take();
        let changed = |f: fn(&Shown) -> Value| before.as_ref().is_none_or(|b| f(b) != f(&shown));
        let fields: [Field; 5] = [
            ("on", |s| Value::Bool(s.on)),
            ("powered", |s| Value::Bool(s.powered)),
            ("brightness", |s| {
                s.brightness.map_or(Value::Null, Value::Float)
            }),
            ("color_temp", |s| {
                s.color_temp.map_or(Value::Null, Value::Float)
            }),
            ("color", |s| {
                s.color.as_deref().map_or(Value::Null, Value::from)
            }),
        ];
        for (key, f) in fields {
            let value = f(&shown);
            let declared = published
                .device
                .as_ref()
                .is_some_and(|d| d.point(key).is_some());
            if declared && value != Value::Null && changed(f) {
                ctx.set_state(&id, key, value);
            }
        }
        if before.as_ref().is_none_or(|b| b.online != shown.online) {
            ctx.set_availability(&id, shown.online);
        }
        published.shown = Some(shown);
    }

    async fn command(&self, command: CommandRequest) {
        if command.abandoned() {
            return;
        }
        let native = command.device.id.as_str().split_once(':').map(|(_, n)| n);
        let Some(fixture) = self.fixtures.iter().find(|f| native == Some(f.id.as_str())) else {
            command.reply(Err(moli_i18n::tr!("pilotes.lights.luminaire_inconnu")));
            return;
        };
        let setting = match (&*command.key, &command.value) {
            ("on", v) => truthy(v).map(Setting::On),
            ("brightness", v) => v.as_f64().map(Setting::Brightness),
            ("color_temp", v) => v.as_f64().map(Setting::ColorTemp),
            ("color", Value::Text(t)) => Some(Setting::Color(t.clone())),
            _ => None,
        };
        let Some(setting) = setting else {
            command.reply(Err(moli_i18n::tr!("pilotes.lights.ordre_inconnu")));
            return;
        };
        let (reading, keyring) = read(&self.hub, fixture);
        let result = match plan(&reading, &setting) {
            Plan::Nothing => Ok(()),
            Plan::Bulbs(orders) => give(&self.hub, fixture, &keyring, orders).await,
            Plan::PowerDown => power(&self.hub, fixture, false).await,
            Plan::PowerUp(then) => {
                let result = power(&self.hub, fixture, true).await;
                if result.is_ok() && !fixture.bulbs.is_empty() {
                    tokio::spawn(after_power_up(self.hub.clone(), fixture.clone(), then));
                }
                result
            }
            Plan::Refuse(why) => Err(why),
        };
        command.reply(result);
    }
}

impl Driver for Fixtures {
    fn kind(&self) -> &'static str {
        "lights"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            // Subscribe before reading: nothing falls in between.
            let mut events = self.hub.subscribe();
            let mut members: HashMap<DeviceId, Vec<usize>> = HashMap::new();
            for (i, f) in self.fixtures.iter().enumerate() {
                for m in f.members() {
                    members.entry(m).or_default().push(i);
                }
            }
            let mut published: Vec<Published> =
                self.fixtures.iter().map(|_| Published::default()).collect();
            for (i, p) in published.iter_mut().enumerate() {
                self.refresh(ctx, i, p);
            }
            ctx.ready();
            loop {
                let next = tokio::select! {
                    command = ctx.next_command() => command.map_or(Next::Stop, Next::Command),
                    event = events.recv() => Next::Event(event),
                };
                let touched: Vec<usize> = match next {
                    Next::Stop | Next::Event(Err(RecvError::Closed)) => return Ok(()),
                    Next::Command(command) => {
                        self.command(command).await;
                        Vec::new()
                    }
                    Next::Event(Err(RecvError::Lagged(_))) => (0..self.fixtures.len()).collect(),
                    Next::Event(Ok(event)) => {
                        let device = match &event {
                            Event::State { point, .. } => point.split().map(|(d, _)| d),
                            Event::Availability { device, .. }
                            | Event::DeviceRemoved { device } => Some(device.clone()),
                            Event::DeviceUpserted { device } => Some(device.id.clone()),
                            _ => None,
                        };
                        device
                            .and_then(|d| members.get(&d).cloned())
                            .unwrap_or_default()
                    }
                };
                for i in touched {
                    self.refresh(ctx, i, &mut published[i]);
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulb(answers: bool, on: bool) -> Bulb {
        Bulb {
            answers,
            on: Some(on),
            brightness: Some(80.0),
            dims: true,
            tints: true,
            ..Bulb::default()
        }
    }

    fn salon(relay: Option<bool>, bulbs: Vec<Bulb>) -> Reading {
        Reading {
            supply: relay.map(|on| Supply {
                answers: true,
                on: Some(on),
            }),
            bulbs,
        }
    }

    #[test]
    fn switching_off_keeps_the_bulbs_powered() {
        let r = salon(
            Some(true),
            vec![bulb(true, true), bulb(true, true), bulb(true, false)],
        );
        assert_eq!(
            plan(&r, &Setting::On(false)),
            Plan::Bulbs(vec![
                (0, vec![Setting::On(false)]),
                (1, vec![Setting::On(false)])
            ]),
            "the bulbs, never the relay"
        );
        assert_eq!(
            plan(&r, &Setting::Brightness(0.0)),
            plan(&r, &Setting::On(false))
        );
        // Nobody answers but the relay feeds them: the relay is all that is left.
        let lost = salon(Some(true), vec![bulb(false, true), bulb(false, true)]);
        assert_eq!(plan(&lost, &Setting::On(false)), Plan::PowerDown);
        assert_eq!(
            plan(
                &salon(Some(false), vec![bulb(false, true)]),
                &Setting::On(false)
            ),
            Plan::Nothing
        );
    }

    #[test]
    fn switching_on_restores_the_relay_first() {
        let cut = salon(Some(false), vec![bulb(false, true), bulb(false, true)]);
        assert_eq!(
            plan(&cut, &Setting::On(true)),
            Plan::PowerUp(vec![Setting::On(true)])
        );
        assert_eq!(
            plan(&cut, &Setting::Color("#ff0000".into())),
            Plan::PowerUp(vec![Setting::On(true), Setting::Color("#ff0000".into())])
        );
        let fed = salon(Some(true), vec![bulb(true, false), bulb(true, true)]);
        assert_eq!(
            plan(&fed, &Setting::On(true)),
            Plan::Bulbs(vec![(0, vec![Setting::On(true)])])
        );
        assert_eq!(
            plan(&fed, &Setting::Brightness(40.0)),
            Plan::Bulbs(vec![
                (0, vec![Setting::On(true), Setting::Brightness(40.0)]),
                (1, vec![Setting::Brightness(40.0)]),
            ])
        );
        // A bulb behind a wall switch: nothing Moli can do, and it says so.
        let wall = salon(None, vec![bulb(false, true)]);
        assert!(matches!(plan(&wall, &Setting::On(true)), Plan::Refuse(_)));
        // A plain light on a relay.
        assert_eq!(
            plan(&salon(Some(false), vec![]), &Setting::On(true)),
            Plan::PowerUp(vec![])
        );
        assert_eq!(
            plan(&salon(Some(true), vec![]), &Setting::On(false)),
            Plan::PowerDown
        );
    }

    #[test]
    fn a_bulb_without_power_is_never_lit() {
        // The bridge still says « on »: the wall switch cut it.
        let wall = salon(None, vec![bulb(false, true)]);
        let shown = wall.shown();
        assert!(!shown.on && !shown.powered && shown.online);
        let cut = salon(Some(false), vec![bulb(true, true)]);
        assert!(!cut.shown().on, "the relay is off");
        let half = salon(
            Some(true),
            vec![bulb(true, true), bulb(false, true), bulb(true, false)],
        );
        let shown = half.shown();
        assert!(shown.on && shown.powered);
        assert_eq!(shown.brightness, Some(80.0));
        assert!(salon(Some(true), vec![]).shown().on);
    }

    #[test]
    fn checks_the_declaration() {
        let parse = |text: &str| {
            #[derive(Deserialize)]
            struct File {
                fixture: Vec<Fixture>,
            }
            check(&toml::from_str::<File>(text).unwrap().fixture)
        };
        let ok = "[[fixture]]\nid = \"suspensions\"\nname = \"Suspensions\"\nbulbs = [\"hue:a\", \"hue:b\"]\npower = \"tuya:r/switch_1\"\n";
        parse(ok).unwrap();
        assert!(parse(&ok.replace("suspensions", "Suspensions")).is_err());
        assert!(parse(&ok.replace("tuya:r/switch_1", "switch_1")).is_err());
        assert!(
            parse(&ok.replace("hue:b", "hue:a")).is_err(),
            "a bulb twice"
        );
        assert!(
            parse(&format!("{ok}{}", ok.replace("suspensions", "autre"))).is_err(),
            "two fixtures share"
        );
        assert!(
            parse("[[fixture]]\nid = \"x\"\nname = \"X\"\n").is_err(),
            "nothing in it"
        );
        assert!(
            parse("[[fixture]]\nid = \"x\"\nname = \"X\"\npower = \"tuya:r/switch_1\"\n").is_ok()
        );
    }
}
