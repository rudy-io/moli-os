//! What the house model asks and Moli works out (contract v4): several devices at once,
//! a relative setting, which devices are in a state, what is wrong, the time, when a
//! device last changed, the weather. The model never sees a value: Moli reads them
//! here, commands under the guard and words the outcome.

use moli_core::{Kind, Origin, Semantic, Value};
use moli_runtime::DeviceView;
use serde_json::{Value as Json, json};

use crate::Assistant;
use crate::Run;
use crate::house::{self, Layout, name_of, norm};

/// What a device is, as « toutes les lumières » means it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Family {
    Light,
    Cover,
    Plug,
    Climate,
    /// Heating only (a radiator valve): « chauffage », never « clim ».
    Heating,
    Media,
    Other,
}

const LIGHT_WORDS: [&str; 14] = [
    "lamp",
    "lumi",
    "plafonnier",
    "ampoule",
    "applique",
    "spot",
    "suspension",
    "ruban",
    "led",
    "lampadaire",
    "guirlande",
    "light",
    "veilleuse",
    "eclairage",
];
const COVER_WORDS: [&str; 5] = ["volet", "store", "rideau", "shutter", "blind"];
/// Names listed in a sentence, at most.
const MAX_LISTED: usize = 10;
/// A battery below this is low (%).
const LOW_BATTERY: f64 = 20.0;

fn writes(v: &DeviceView, key: &str) -> bool {
    v.device
        .points
        .iter()
        .any(|p| p.access.write && &*p.key == key)
}

fn writes_semantic(v: &DeviceView, semantic: Semantic) -> bool {
    v.device
        .points
        .iter()
        .any(|p| p.access.write && p.semantic == semantic)
}

/// The order that lights a light up first, when a colour or a level is asked of one that is off
/// (« allume la cuisine en rouge »: a colour sent to a dark lamp leaves it dark). A level of zero is no wish to light it.
pub(crate) fn wake_order(v: &DeviceView, key: &str, value: &Json) -> Option<(String, Json)> {
    let asks = match key {
        "color" | "color_temp" => true,
        "brightness" => value.as_f64().is_some_and(|n| n > 0.0),
        _ => false,
    };
    let onoff = v
        .device
        .points
        .iter()
        .find(|p| p.access.write && p.semantic == Semantic::OnOff)?;
    (asks && is_on(v) == Some(false)).then(|| (onoff.key.to_string(), json!(true)))
}

pub(crate) fn family_of(v: &DeviceView) -> Family {
    let name = norm(name_of(v));
    // What opens the house (the guard's own reading: a lock's point, else its names), and the
    // house's own switches (helpers: « Alarme », « Mode vacances »), are never part of
    // « tous les volets » or « toutes les prises ».
    let names: Vec<&str> = std::iter::once(&*v.device.native_name)
        .chain(v.label.name.as_deref())
        .collect();
    let opens = moli_runtime::opener(&names, &v.device.points) != moli_runtime::Opener::None;
    let helper = v.device.model.as_deref() == Some("Aide")
        && v.device.manufacturer.as_deref() == Some("Moli");
    if opens || helper {
        return Family::Other;
    }
    let control_closes = v.device.points.iter().any(|p| {
        &*p.key == "control"
            && matches!(&p.kind, Kind::Enum { values } if values.iter().any(|x| &**x == "close"))
    });
    if writes(v, "percent_control")
        || control_closes
        || COVER_WORDS.iter().any(|w| name.contains(w))
    {
        Family::Cover
    } else if [
        "system_mode",
        "target_temperature",
        "occupied_heating_setpoint",
    ]
    .iter()
    .any(|k| writes(v, k))
    {
        let cools = writes(v, "occupied_cooling_setpoint")
            || writes(v, "on")
            || name.contains("clim")
            || v.device.points.iter().any(|p| {
                matches!(&p.kind, Kind::Enum { values } if values.iter().any(|x| matches!(&**x, "cool" | "froid")))
            });
        if cools {
            Family::Climate
        } else {
            Family::Heating
        }
    } else if ["volume", "announce", "play"].iter().any(|k| writes(v, k)) {
        Family::Media
    } else if [Semantic::Brightness, Semantic::Color, Semantic::ColorTemp]
        .into_iter()
        .any(|s| writes_semantic(v, s))
        || (writes_semantic(v, Semantic::OnOff) && LIGHT_WORDS.iter().any(|w| name.contains(w)))
    {
        Family::Light
    } else if writes_semantic(v, Semantic::OnOff) {
        Family::Plug
    } else {
        Family::Other
    }
}

/// The families a word names. « tout » is the lights and what plays: never the plugs
/// (a fridge) nor the heating, which are asked by name.
fn families(written: &str) -> Option<Vec<Family>> {
    let w = norm(written);
    let pick = |f: Family| Some(vec![f]);
    if w.starts_with("lumi")
        || w.starts_with("lamp")
        || w.starts_with("light")
        || w.starts_with("eclair")
    {
        pick(Family::Light)
    } else if w.starts_with("volet")
        || w.starts_with("store")
        || w.starts_with("shutter")
        || w.starts_with("cover")
    {
        pick(Family::Cover)
    } else if w.starts_with("prise") || w.starts_with("plug") || w.starts_with("inter") {
        pick(Family::Plug)
    } else if w.starts_with("clim") || w.starts_with("climate") {
        pick(Family::Climate)
    } else if w.starts_with("chauff") || w.starts_with("heat") || w.starts_with("radiat") {
        Some(vec![Family::Climate, Family::Heating])
    } else if w.starts_with("media")
        || w.starts_with("enceinte")
        || w.starts_with("tele")
        || w.starts_with("tv")
    {
        pick(Family::Media)
    } else if w.starts_with("tout") || w == "all" || w == "everything" {
        Some(vec![Family::Light, Family::Media])
    } else {
        None
    }
}

/// The first writable point with this semantic.
fn point_with(v: &DeviceView, semantic: Semantic) -> Option<String> {
    v.device
        .points
        .iter()
        .find(|p| p.access.write && p.semantic == semantic)
        .map(|p| p.key.to_string())
}

fn first_writable(v: &DeviceView, keys: &[&str]) -> Option<String> {
    keys.iter().find(|k| writes(v, k)).map(|k| (*k).to_owned())
}

/// What `value` comes to on this device: the point and the value to send.
fn order_for(v: &DeviceView, family: Family, value: &Json) -> Option<(String, Json)> {
    let on = value
        .as_bool()
        .or_else(|| match value.as_str().map(norm).as_deref() {
            Some("on" | "open" | "ouvert" | "ouvre" | "allume") => Some(true),
            Some("off" | "close" | "ferme" | "eteint" | "eteins") => Some(false),
            _ => None,
        });
    match family {
        Family::Light | Family::Plug => match (on, value.as_f64()) {
            (Some(on), _) => Some((point_with(v, Semantic::OnOff)?, json!(on))),
            (None, Some(level)) if family == Family::Light => Some((
                point_with(v, Semantic::Brightness)?,
                json!(level.clamp(0.0, 100.0)),
            )),
            _ => None,
        },
        Family::Cover => {
            let stop = value
                .as_str()
                .is_some_and(|s| norm(s).starts_with("stop") || norm(s) == "arrete");
            let has = |word: &str| {
                v.device.points.iter().any(|p| {
                    &*p.key == "control"
                        && matches!(&p.kind, Kind::Enum { values } if values.iter().any(|x| &**x == word))
                })
            };
            if stop {
                return has("stop").then(|| ("control".to_owned(), json!("stop")));
            }
            match (on, value.as_f64()) {
                (Some(open), _) if has(if open { "open" } else { "close" }) => Some((
                    "control".to_owned(),
                    json!(if open { "open" } else { "close" }),
                )),
                (Some(open), _) => Some((
                    first_writable(v, &["percent_control", "position"])?,
                    json!(if open { 100 } else { 0 }),
                )),
                (None, Some(level)) => Some((
                    first_writable(v, &["percent_control", "position"])?,
                    json!(level.clamp(0.0, 100.0)),
                )),
                _ => None,
            }
        }
        Family::Climate | Family::Heating => match (on, value.as_f64()) {
            (Some(on), _) if writes(v, "on") => Some(("on".to_owned(), json!(on))),
            (Some(false), _) if writes(v, "system_mode") => {
                Some(("system_mode".to_owned(), json!("off")))
            }
            (None, Some(degrees)) => Some((
                first_writable(v, &["target_temperature", "occupied_heating_setpoint"])?,
                json!(degrees),
            )),
            _ => None,
        },
        Family::Media => match (on, value.as_f64()) {
            (Some(on), _) => Some((point_with(v, Semantic::OnOff)?, json!(on))),
            (None, Some(volume)) => Some((
                first_writable(v, &["volume"])?,
                json!(volume.clamp(0.0, 100.0)),
            )),
            _ => None,
        },
        Family::Other => None,
    }
}

fn listed(names: &[String]) -> String {
    let mut out = names
        .iter()
        .take(MAX_LISTED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > MAX_LISTED {
        out.push_str(&moli_i18n::tr!(
            "assistant.house.and_others",
            count = names.len() - MAX_LISTED
        ));
    }
    out
}

/// « Plafonnier (Salon) »: names repeat from room to room.
fn shown(v: &DeviceView, layout: &Layout) -> String {
    let room = layout.room_of(v);
    if room.is_empty() || room == house::no_room() {
        name_of(v).to_owned()
    } else {
        format!("{} ({room})", name_of(v))
    }
}

fn number(v: &DeviceView, key: &str) -> Option<f64> {
    match &v.state.get(key)?.value {
        Value::Int(i) => i32::try_from(*i).ok().map(f64::from),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn truth(v: &DeviceView, key: &str) -> Option<bool> {
    match &v.state.get(key)?.value {
        Value::Bool(b) => Some(*b),
        Value::Text(t) => {
            let t = norm(t);
            Some(t == "on" || t == "open" || t.starts_with("ouv") || t == "true")
        }
        _ => None,
    }
}

/// Whether a device is on (lights, plugs, heating, media), open (covers, doors), or
/// neither known.
fn is_on(v: &DeviceView) -> Option<bool> {
    let onoff = v
        .device
        .points
        .iter()
        .find(|p| p.semantic == Semantic::OnOff)?;
    truth(v, &onoff.key)
}

fn is_open(v: &DeviceView) -> Option<bool> {
    if let Some(level) = number(v, "percent_control").or_else(|| number(v, "position")) {
        return Some(level > 0.0);
    }
    if let Some(control) = v.state.get("control").and_then(|s| match &s.value {
        Value::Text(t) => Some(norm(t)),
        _ => None,
    }) {
        return Some(control != "close" && control != "closed");
    }
    let contact = v
        .device
        .points
        .iter()
        .find(|p| p.semantic == Semantic::Contact)?;
    // A contact sensor says true when closed.
    match &v.state.get(&contact.key)?.value {
        Value::Bool(closed) => Some(!closed),
        Value::Text(t) => Some(norm(t).starts_with("ouv") || norm(t) == "open"),
        _ => None,
    }
}

fn low_battery(v: &DeviceView) -> bool {
    v.device.points.iter().any(|p| match p.semantic {
        Semantic::Battery => number(v, &p.key).is_some_and(|b| b < LOW_BATTERY),
        Semantic::BatteryLow => truth(v, &p.key) == Some(true),
        _ => false,
    })
}

fn alarm(v: &DeviceView) -> bool {
    v.device.points.iter().any(|p| {
        (matches!(p.semantic, Semantic::Smoke | Semantic::WaterLeak)
            || &*p.key == "carbon_monoxide")
            && truth(v, &p.key) == Some(true)
    })
}

/// « Il est 20 h 07, samedi 24 octobre. »
pub(crate) fn time_words(at: &jiff::Zoned, language: &str) -> String {
    const JOURS: [&str; 7] = [
        "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi", "dimanche",
    ];
    const MOIS: [&str; 12] = [
        "janvier",
        "février",
        "mars",
        "avril",
        "mai",
        "juin",
        "juillet",
        "août",
        "septembre",
        "octobre",
        "novembre",
        "décembre",
    ];
    const DAYS: [&str; 7] = [
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
    ];
    const MONTHS: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    let day = usize::try_from(at.weekday().to_monday_zero_offset()).unwrap_or(0);
    let month = usize::try_from(at.month() - 1).unwrap_or(0);
    if language.starts_with("en") {
        format!(
            "It is {:02}:{:02}, {} {} {}.",
            at.hour(),
            at.minute(),
            DAYS[day],
            at.day(),
            MONTHS[month]
        )
    } else {
        format!(
            "Il est {} h {:02}, {} {} {}.",
            at.hour(),
            at.minute(),
            JOURS[day],
            at.day(),
            MOIS[month]
        )
    }
}

/// The day asked (0 today, 1 tomorrow…, at most 6); none said: today.
pub(crate) fn day_of(args: &Json) -> usize {
    usize::try_from(args["day"].as_u64().unwrap_or(0).min(6)).unwrap_or(0)
}

/// The forecast of one day (`day`: 0 today, 1 tomorrow…) in a sentence.
pub(crate) fn weather_words(forecast: &Json, day: usize) -> String {
    let Some(d) = forecast["days"].get(day) else {
        return moli_i18n::tr!("assistant.house.weather_unavailable");
    };
    let when = match day {
        0 => moli_i18n::tr!("assistant.house.today"),
        1 => moli_i18n::tr!("assistant.house.tomorrow"),
        n => moli_i18n::tr!("assistant.house.in_days", count = n),
    };
    let round = |v: &Json| {
        v.as_f64()
            .map_or_else(|| "?".to_owned(), |x| format!("{}", x.round()))
    };
    moli_i18n::tr!(
        "assistant.house.weather_line",
        when = when,
        sky = d["sky"].as_str().unwrap_or_default(),
        min = round(&d["min_c"]),
        max = round(&d["max_c"]),
        rain = round(&d["rain_chance_pct"])
    )
}

/// Something Moli will do later (an order, a reminder): kept in memory only.
#[derive(Debug)]
pub(crate) struct Planned {
    id: u64,
    created: std::time::Instant,
    text: String,
    task: tokio::task::AbortHandle,
}

/// An order and the change it caused are this close (a device answers within seconds).
const ORDER_TO_CHANGE_MS: u64 = 30_000;

/// Planned at once, at most; the longest delay (a day); a reminder's length.
const MAX_PLANNED: usize = 20;
const MAX_MINUTES: u64 = 24 * 60;
const MAX_REMINDER: usize = 120;
/// « Annule » takes back what was planned this recently, before the changes of configuration.
const CANCEL_WINDOW: std::time::Duration = std::time::Duration::from_secs(10 * 60);

fn minutes_of(args: &Json) -> Option<u64> {
    let m = args["minutes"]
        .as_u64()
        .or_else(|| args["minutes"].as_str().and_then(|s| s.trim().parse().ok()))?;
    (1..=MAX_MINUTES).contains(&m).then_some(m)
}

impl Assistant {
    /// The devices of the house as the conversation sees them (hidden ones aside).
    fn visible(&self, layout: &Layout) -> Vec<DeviceView> {
        self.0
            .hub
            .snapshot()
            .devices
            .iter()
            .filter(|v| !layout.hides(v.device.id.as_str()))
            .cloned()
            .collect()
    }

    /// A room the model wrote, as the house names it; `Err` when the house has none such.
    fn room_filter(&self, layout: &Layout, args: &Json) -> Result<Option<String>, String> {
        match args["room"]
            .as_str()
            .map(str::trim)
            .filter(|r| !r.is_empty())
        {
            None => Ok(None),
            Some(_) => self
                .known_room(layout, &args["room"])
                .map(|r| Some(norm(&r)))
                .ok_or_else(|| moli_i18n::tr!("assistant.house.not_found")),
        }
    }

    /// `set_all`: one order to every device of a kind (in a room, or the whole house).
    pub(crate) async fn set_all(&self, args: &Json, layout: &Layout, run: &mut Run) -> String {
        let (Some(mut wanted), Some(value)) =
            (args["kind"].as_str().and_then(families), args.get("value"))
        else {
            return crate::house_model::unknown_words();
        };
        // « Allume tout », « tout à 60 % »: the lights, never the speakers (they would start playing).
        if wanted.len() > 1 && wanted.contains(&Family::Media) && value != &json!(false) {
            wanted = vec![Family::Light];
        }
        let room = match self.room_filter(layout, args) {
            Ok(room) => room,
            Err(words) => return words,
        };
        let mut orders = Vec::new();
        for v in self.visible(layout) {
            let family = family_of(&v);
            if !wanted.contains(&family)
                || room
                    .as_ref()
                    .is_some_and(|r| norm(&layout.room_of(&v)) != *r)
            {
                continue;
            }
            if v.online == Some(false) {
                continue;
            }
            if let Some((key, value)) = order_for(&v, family, value) {
                if let Some((on_key, on)) = wake_order(&v, &key, &value) {
                    orders.push((format!("{}/{on_key}", v.device.id.as_str()), on, false));
                }
                let already = v
                    .state
                    .get(key.as_str())
                    .is_some_and(|s| crate::house_model::same(&s.value, &value));
                orders.push((format!("{}/{key}", v.device.id.as_str()), value, already));
            }
        }
        if orders.is_empty() {
            return moli_i18n::tr!("assistant.house.all_none");
        }
        let total = orders.len();
        let (mut done, mut held, mut failed) = (0, 0, 0);
        for (point, value, already) in orders {
            if already {
                done += 1;
                continue;
            }
            let result = self
                .set(&json!({ "point": point, "value": value }), run)
                .await;
            match result["status"].as_str() {
                Some("done") => done += 1,
                Some("awaiting_approval") => held += 1,
                _ => failed += 1,
            }
        }
        let mut said = moli_i18n::tr!("assistant.house.all_done", count = done, total = total);
        if held > 0 {
            said.push(' ');
            said.push_str(&moli_i18n::tr!("assistant.house.all_held", count = held));
        }
        if failed > 0 {
            said.push(' ');
            said.push_str(&moli_i18n::tr!(
                "assistant.house.all_failed",
                count = failed
            ));
        }
        said
    }

    /// `adjust`: from the current value, by `by`, within the point's bounds.
    pub(crate) async fn adjust(&self, point: Option<String>, args: &Json, run: &mut Run) -> String {
        let (Some(point), Some(by)) = (point, args["by"].as_f64()) else {
            return crate::house_model::unknown_words();
        };
        let (device, key) = point.split_once('/').unwrap_or((&point, ""));
        let Some(v) = self.0.hub.device(&device.into()) else {
            return moli_i18n::tr!("assistant.house.not_found");
        };
        let Some(spec) = v
            .device
            .points
            .iter()
            .find(|p| &*p.key == key && p.access.write)
        else {
            return moli_i18n::tr!("assistant.house.not_found");
        };
        let (Kind::Numeric { min, max, step }, Some(now)) = (&spec.kind, number(&v, key)) else {
            return moli_i18n::tr!("assistant.house.adjust_refused");
        };
        if house::setting(&spec.key, spec.semantic) {
            return moli_i18n::tr!("assistant.house.setting_refused");
        }
        let mut next = now + by;
        if let Some(step) = step.filter(|s| *s > 0.0) {
            next = (next / step).round() * step;
        }
        next = next.clamp(min.unwrap_or(f64::MIN), max.unwrap_or(f64::MAX));
        if (next - now).abs() < 1e-9 {
            return moli_i18n::tr!("assistant.house.adjust_limit", device = name_of(&v));
        }
        let result = self
            .set(&json!({ "point": point, "value": next }), run)
            .await;
        let unit = spec
            .unit
            .as_ref()
            .map(|u| format!(" {}", u.symbol()))
            .unwrap_or_default();
        match result["status"].as_str() {
            Some("done") => moli_i18n::tr!(
                "assistant.house.adjust_done",
                device = name_of(&v),
                value = format!("{}{unit}", (next * 10.0).round() / 10.0)
            ),
            Some("awaiting_approval") => {
                moli_i18n::tr!("assistant.house.held", device = name_of(&v))
            }
            _ => moli_i18n::tr!("assistant.house.failed", device = name_of(&v)),
        }
    }

    /// `find`: the devices in a state, named.
    pub(crate) fn find(&self, args: &Json, layout: &Layout) -> String {
        let state = norm(args["state"].as_str().unwrap_or_default());
        let kinds = args["kind"].as_str().and_then(families);
        let room = match self.room_filter(layout, args) {
            Ok(room) => room,
            Err(words) => return words,
        };
        let test: fn(&DeviceView) -> bool = match state.as_str() {
            "allume" | "on" | "allumee" => |v| is_on(v) == Some(true),
            "eteint" | "off" | "eteinte" => |v| is_on(v) == Some(false),
            "ouvert" | "open" | "ouverte" => |v| is_open(v) == Some(true),
            "ferme" | "closed" | "fermee" => |v| is_open(v) == Some(false),
            "injoignable" | "offline" => |v| v.online == Some(false),
            "batterie_faible" | "low_battery" => low_battery,
            _ => return crate::house_model::unknown_words(),
        };
        let names: Vec<String> = self
            .visible(layout)
            .iter()
            .filter(|v| kinds.as_ref().is_none_or(|k| k.contains(&family_of(v))))
            .filter(|v| room.as_ref().is_none_or(|r| norm(&layout.room_of(v)) == *r))
            // A device that does not answer has no state now: only « injoignable » finds it.
            .filter(|v| {
                v.online != Some(false) || matches!(state.as_str(), "injoignable" | "offline")
            })
            .filter(|v| test(v))
            .map(|v| shown(v, layout))
            .collect();
        if names.is_empty() {
            moli_i18n::tr!("assistant.house.find_none")
        } else {
            format!("{}.", listed(&names))
        }
    }

    /// `problems`: who does not answer, which battery is low, which alarm rings, what waits.
    pub(crate) fn problems(&self, layout: &Layout) -> String {
        let devices = self.visible(layout);
        let names = |test: &dyn Fn(&DeviceView) -> bool| -> Vec<String> {
            devices
                .iter()
                .filter(|v| test(v))
                .map(|v| shown(v, layout))
                .collect()
        };
        let mut said = Vec::new();
        let alarms = names(&alarm);
        if !alarms.is_empty() {
            said.push(moli_i18n::tr!(
                "assistant.house.problems_alarm",
                names = listed(&alarms)
            ));
        }
        let offline = names(&|v: &DeviceView| v.online == Some(false));
        if !offline.is_empty() {
            said.push(moli_i18n::tr!(
                "assistant.house.problems_offline",
                names = listed(&offline)
            ));
        }
        let low = names(&low_battery);
        if !low.is_empty() {
            said.push(moli_i18n::tr!(
                "assistant.house.problems_battery",
                names = listed(&low)
            ));
        }
        let waiting = self.0.hub.approvals().len();
        if waiting > 0 {
            said.push(moli_i18n::tr!(
                "assistant.house.problems_waiting",
                count = waiting
            ));
        }
        if said.is_empty() {
            moli_i18n::tr!("assistant.house.problems_none")
        } else {
            said.join(" ")
        }
    }

    /// `now`: the time and the day where the house is.
    pub(crate) fn now_words(&self) -> String {
        let at = jiff::Timestamp::now().to_zoned(self.0.tz.clone());
        time_words(&at, &moli_i18n::language())
    }

    /// `last_change`: when the device's main value last changed, and who ordered it.
    pub(crate) async fn last_change(&self, device: Option<&moli_core::DeviceId>) -> String {
        const MAIN: [&str; 9] = [
            "state",
            "on",
            "state_l1",
            "control",
            "percent_control",
            "contact",
            "occupancy",
            "temperature",
            "system_mode",
        ];
        let Some(v) = device.and_then(|d| self.0.hub.device(d)) else {
            return moli_i18n::tr!("assistant.house.not_found");
        };
        let Some((key, sample)) = MAIN.iter().find_map(|k| v.state.get(*k).map(|s| (*k, s))) else {
            return moli_i18n::tr!("assistant.house.last_change_unknown", device = name_of(&v));
        };
        let point = format!("{}/{key}", v.device.id.as_str());
        // A device reports its value again and again: the time it changed is the history's
        // (its last change); without a history, the last report is all there is.
        let mut changed = sample.ts;
        if let Some(history) = &self.0.history {
            let now = moli_core::now_ms();
            if let Ok(series) = history
                .series(&point, now.saturating_sub(30 * 24 * 3_600_000), now, 1)
                .await
                && let Some((ts, _)) = series.last
            {
                changed = ts;
            }
        }
        // Who: an order on that point a moment before the change; none, the device itself.
        let by = self
            .0
            .hub
            .journal(2000)
            .iter()
            .find(|e| {
                matches!(&e.action, moli_core::Action::Command { point: p, .. } if p.as_str() == point)
                    && e.ts <= changed + 2_000
                    && changed.saturating_sub(e.ts) <= ORDER_TO_CHANGE_MS
            })
            .map_or_else(
                || moli_i18n::tr!("assistant.house.by_device"),
                |e| match e.origin {
                    Origin::Ui | Origin::Household => moli_i18n::tr!("assistant.house.by_dashboard"),
                    Origin::Assistant => moli_i18n::tr!("assistant.house.by_moli"),
                    Origin::Automation => moli_i18n::tr!("assistant.house.by_automation"),
                    _ => moli_i18n::tr!("assistant.house.by_agent"),
                },
            );
        let at = jiff::Timestamp::from_millisecond(i64::try_from(changed).unwrap_or(0))
            .unwrap_or_default()
            .to_zoned(self.0.tz.clone());
        let today = jiff::Timestamp::now().to_zoned(self.0.tz.clone()).date();
        let when = if at.date() == today {
            moli_i18n::tr!(
                "assistant.house.at_time",
                time = format!("{} h {:02}", at.hour(), at.minute())
            )
        } else {
            moli_i18n::tr!(
                "assistant.house.on_day",
                date = format!("{:02}/{:02}", at.day(), at.month()),
                time = format!("{} h {:02}", at.hour(), at.minute())
            )
        };
        moli_i18n::tr!(
            "assistant.house.last_change",
            device = name_of(&v),
            when = when,
            by = by
        )
    }

    /// `ambiance`: a look (« cinéma », « feu ») or a colour on the lights of a room.
    pub(crate) async fn house_ambiance(
        &self,
        args: &Json,
        layout: &Layout,
        run: &mut Run,
    ) -> String {
        if args["room"].as_str().is_none_or(|r| r.trim().is_empty()) {
            return crate::house_model::unknown_words();
        }
        let room = match self.room_filter(layout, args) {
            Ok(room) => room,
            Err(words) => return words,
        };
        let lights: Vec<String> = self
            .visible(layout)
            .iter()
            .filter(|v| family_of(v) == Family::Light && v.online != Some(false))
            .filter(|v| room.as_ref().is_none_or(|r| norm(&layout.room_of(v)) == *r))
            .map(|v| v.device.id.as_str().to_owned())
            .collect();
        if lights.is_empty() {
            return moli_i18n::tr!("assistant.house.all_none");
        }
        let look = args["look"]
            .as_str()
            .map(|l| norm(l).trim().replace(' ', "-"));
        let order = match (look, args["color"].as_str()) {
            (Some(id), _) if moli_ambiance::find(&id).is_some() => {
                json!({ "lights": lights, "ambiance": id })
            }
            (_, Some(said)) => match crate::house_model::colour(said) {
                Some(crate::house_model::Colour::Hex(hex)) => {
                    json!({ "lights": lights, "color": hex })
                }
                Some(crate::house_model::Colour::White(mired)) => {
                    json!({ "lights": lights, "white": 1_000_000 / mired })
                }
                None => return moli_i18n::tr!("assistant.house.colour_unknown"),
            },
            _ => return moli_i18n::tr!("assistant.house.ambiance_unknown"),
        };
        let before = run.actions.len();
        self.ambiance(&order, run).await;
        // Counted by lamp (a held lamp may hold several orders), and what did not answer said too.
        let lamps = |status: &str| {
            run.actions[before..]
                .iter()
                .filter(|a| a.status == status)
                .map(|a| a.device.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        };
        let (done, held) = (lamps("done"), lamps("held"));
        let failed = lights.len().saturating_sub(done + held);
        let mut said = moli_i18n::tr!(
            "assistant.house.all_done",
            count = done,
            total = lights.len()
        );
        if held > 0 {
            said.push(' ');
            said.push_str(&moli_i18n::tr!("assistant.house.all_held", count = held));
        }
        if failed > 0 {
            said.push(' ');
            said.push_str(&moli_i18n::tr!(
                "assistant.house.all_failed",
                count = failed
            ));
        }
        said
    }

    /// Keeps `task` to run later, under `text` (what « annule » says it took back).
    fn plan(
        &self,
        text: String,
        task: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Option<u64> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let mut list = self
            .0
            .planned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if list.len() >= MAX_PLANNED {
            return None;
        }
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let me = self.clone();
        let handle = tokio::spawn(async move {
            task.await;
            me.0.planned
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|p| p.id != id);
        });
        list.push(Planned {
            id,
            created: std::time::Instant::now(),
            text,
            task: handle.abort_handle(),
        });
        Some(id)
    }

    /// « Annule » right after planning something: the latest planned goes, unsaid.
    /// Only what was planned after `since` (the last confirmed change): « annule » takes back the latest.
    pub(crate) fn cancel_planned(&self, since: Option<std::time::Instant>) -> Option<String> {
        let mut list = self
            .0
            .planned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let latest = list.iter().rposition(|p| {
            p.created.elapsed() < CANCEL_WINDOW && since.is_none_or(|s| p.created > s)
        })?;
        let gone = list.remove(latest);
        gone.task.abort();
        Some(gone.text)
    }

    /// `schedule`: an order sent in `minutes`, under the guard as if said then.
    pub(crate) fn schedule(&self, point: Option<String>, args: &Json) -> String {
        let (Some(point), Some(minutes), Some(value)) =
            (point, minutes_of(args), args.get("value").cloned())
        else {
            return crate::house_model::unknown_words();
        };
        let (device, key) = point.split_once('/').unwrap_or((&point, ""));
        let Some(v) = self.0.hub.device(&device.into()) else {
            return moli_i18n::tr!("assistant.house.not_found");
        };
        let Some(spec) = v
            .device
            .points
            .iter()
            .find(|p| &*p.key == key && p.access.write)
        else {
            return moli_i18n::tr!("assistant.house.not_found");
        };
        if house::setting(&spec.key, spec.semantic) {
            return moli_i18n::tr!("assistant.house.setting_refused");
        }
        // Checked now, not at the time it goes: a wrong value would only fail unseen.
        let mut value = value;
        if key == "color"
            && let Some(said) = value.as_str()
            && !said.starts_with('#')
        {
            match crate::house_model::colour(said) {
                Some(crate::house_model::Colour::Hex(hex)) => value = json!(hex),
                _ => return moli_i18n::tr!("assistant.house.colour_unknown"),
            }
        }
        let fits = match &spec.kind {
            Kind::Binary => value.is_boolean(),
            Kind::Numeric { min, max, .. } => value
                .as_f64()
                .is_some_and(|n| min.is_none_or(|m| n >= m) && max.is_none_or(|m| n <= m)),
            Kind::Enum { values } => value
                .as_str()
                .is_some_and(|x| values.iter().any(|v| &**v == x)),
            Kind::Text => value.is_string(),
        };
        if !fits {
            return crate::house_model::unknown_words();
        }
        let name = name_of(&v).to_owned();
        let hub = self.0.hub.clone();
        let target = moli_core::PointId::from(point.clone());
        // The time is counted from now, not from when the task first runs.
        let due = tokio::time::Instant::now() + std::time::Duration::from_secs(minutes * 60);
        let order = async move {
            tokio::time::sleep_until(due).await;
            let done = hub
                .command(
                    &target,
                    Value::from_json(&value),
                    Origin::Assistant,
                    Some("Moli".into()),
                )
                .await;
            if let Err(e) = done {
                tracing::warn!(point = %target, error = %e, "a planned order did not go");
            }
        };
        let text = moli_i18n::tr!("assistant.house.planned_what", device = name.clone());
        match self.plan(text, order) {
            Some(_) => moli_i18n::tr!(
                "assistant.house.planned_order",
                device = name,
                minutes = minutes
            ),
            None => moli_i18n::tr!("assistant.house.planned_full"),
        }
    }

    /// `remind`: a word on the dashboards and the household's phones in `minutes`.
    pub(crate) fn remind(&self, args: &Json) -> String {
        let text = args["text"]
            .as_str()
            .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|t| {
                !t.is_empty()
                    && t.chars().count() <= MAX_REMINDER
                    && !t.chars().any(char::is_control)
            });
        let (Some(text), Some(minutes)) = (text, minutes_of(args)) else {
            return crate::house_model::unknown_words();
        };
        let hub = self.0.hub.clone();
        let said = text.clone();
        let due = tokio::time::Instant::now() + std::time::Duration::from_secs(minutes * 60);
        let reminder = async move {
            tokio::time::sleep_until(due).await;
            hub.notice(moli_core::Notice {
                title: Some(moli_i18n::tr!("assistant.house.reminder_title")),
                message: said.clone(),
                from: Some("Moli".into()),
                ts: moli_core::now_ms(),
            });
            for phone in hub.writable_points("push") {
                let _ = hub
                    .command(
                        &phone,
                        Value::Text(said.clone().into()),
                        Origin::Assistant,
                        Some("Moli".into()),
                    )
                    .await;
            }
        };
        match self.plan(text, reminder) {
            Some(_) => moli_i18n::tr!("assistant.house.planned_reminder", minutes = minutes),
            None => moli_i18n::tr!("assistant.house.planned_full"),
        }
    }

    /// `weather`: the forecast for one day, where the house is.
    pub(crate) async fn weather(&self, args: &Json) -> String {
        let Some((latitude, longitude)) = self.position() else {
            return moli_i18n::tr!("assistant.house.weather_no_place");
        };
        let day = day_of(args);
        match crate::outside::forecast(latitude, longitude, (day + 1) as u64).await {
            Ok(forecast) => weather_words(&forecast, day),
            Err(e) => {
                tracing::warn!(error = %e, "forecast unavailable");
                moli_i18n::tr!("assistant.house.weather_unavailable")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use moli_core::{Kind, Semantic, Value};
    use serde_json::json;

    use crate::house_model::tests::{ask, call, house_model_over};
    use crate::tests::{Fake, hub_with};

    fn spec(key: &str, kind: Kind, write: bool, semantic: Semantic) -> moli_core::PointSpec {
        moli_core::PointSpec {
            key: key.into(),
            label: key.into(),
            kind,
            access: moli_core::Access { read: true, write },
            unit: None,
            semantic,
        }
    }

    fn level(min: f64, max: f64) -> Kind {
        Kind::Numeric {
            min: Some(min),
            max: Some(max),
            step: None,
        }
    }

    fn choice(values: &[&str]) -> Kind {
        Kind::Enum {
            values: values.iter().map(|v| (*v).into()).collect(),
        }
    }

    /// Salon: Plafonnier (on, 50 %), Volet (open), Prise (on), Clim (on, 21 °C).
    /// Cuisine: Lampe (off), Capteur (battery 10 %). Garage: Détecteur (smoke).
    async fn house() -> moli_runtime::Hub {
        let onoff = || spec("state", Kind::Binary, true, Semantic::OnOff);
        let hub = hub_with(Fake(vec![
            (
                "Plafonnier",
                vec![
                    onoff(),
                    spec("brightness", level(0.0, 100.0), true, Semantic::Brightness),
                ],
                vec![
                    ("state", Value::Bool(true)),
                    ("brightness", Value::Float(50.0)),
                ],
            ),
            (
                "Volet",
                vec![
                    spec(
                        "control",
                        choice(&["open", "stop", "close"]),
                        true,
                        Semantic::Other,
                    ),
                    spec("percent_control", level(0.0, 100.0), true, Semantic::Other),
                ],
                vec![
                    ("control", Value::Text("open".into())),
                    ("percent_control", Value::Float(100.0)),
                ],
            ),
            ("Prise", vec![onoff()], vec![("state", Value::Bool(true))]),
            (
                "Clim",
                vec![
                    spec("on", Kind::Binary, true, Semantic::OnOff),
                    spec(
                        "target_temperature",
                        level(16.0, 30.0),
                        true,
                        Semantic::Temperature,
                    ),
                ],
                vec![
                    ("on", Value::Bool(true)),
                    ("target_temperature", Value::Float(21.0)),
                ],
            ),
            ("Lampe", vec![onoff()], vec![("state", Value::Bool(false))]),
            (
                "Capteur",
                vec![spec("battery", level(0.0, 100.0), false, Semantic::Battery)],
                vec![("battery", Value::Float(10.0))],
            ),
            (
                "Détecteur",
                vec![spec("smoke", Kind::Binary, false, Semantic::Smoke)],
                vec![("smoke", Value::Bool(true))],
            ),
        ]))
        .await;
        for (device, room) in [
            ("Plafonnier", "Salon"),
            ("Volet", "Salon"),
            ("Prise", "Salon"),
            ("Clim", "Salon"),
            ("Lampe", "Cuisine"),
            ("Capteur", "Cuisine"),
            ("Détecteur", "Garage"),
        ] {
            let room = moli_runtime::LabelPatch {
                name: None,
                room: Some(room.into()),
            };
            hub.set_label(
                &format!("fake:{device}").as_str().into(),
                room,
                moli_core::Origin::Ui,
                None,
            )
            .await
            .unwrap();
        }
        hub
    }

    /// The model's number for a device of `house()`.
    fn alias(moli: &crate::Assistant, name: &str) -> String {
        let layout = crate::house::Layout::default();
        let (_, aliases) = moli.house_prompt(&layout).unwrap();
        aliases
            .into_iter()
            .find(|(_, id)| id.as_str() == format!("fake:{name}"))
            .map(|(a, _)| a)
            .unwrap()
    }

    #[tokio::test]
    async fn all_the_lights_of_a_room_are_switched_off_and_nothing_else() {
        let answer = call(
            "set_all",
            r#"{"kind":"lumières","room":"Salon","value":false}"#,
        );
        let hub = house().await;
        let (moli, _) = house_model_over(hub.clone(), vec![&answer]).await;
        let reply = moli
            .turn(ask("Éteins les lumières du salon"))
            .await
            .unwrap();
        let points: Vec<&str> = reply.actions.iter().map(|a| a.point.as_str()).collect();
        assert_eq!(
            points,
            vec!["fake:Plafonnier/state"],
            "not the plug, not the kitchen"
        );
        assert!(reply.reply.contains('1'), "{}", reply.reply);
    }

    #[tokio::test]
    async fn all_the_shutters_close_by_their_own_order() {
        let answer = call("set_all", r#"{"kind":"volets","value":"close"}"#);
        let (moli, _) = house_model_over(house().await, vec![&answer]).await;
        let reply = moli.turn(ask("Ferme tous les volets")).await.unwrap();
        assert_eq!(reply.actions.len(), 1);
        assert_eq!(reply.actions[0].point.as_str(), "fake:Volet/control");
        assert_eq!(reply.actions[0].value, json!("close"));
    }

    #[tokio::test]
    async fn everything_off_means_the_lights_never_the_plugs() {
        let answer = call("set_all", r#"{"kind":"tout","value":false}"#);
        let (moli, _) = house_model_over(house().await, vec![&answer]).await;
        let reply = moli.turn(ask("Éteins tout")).await.unwrap();
        let points: Vec<&str> = reply.actions.iter().map(|a| a.point.as_str()).collect();
        assert_eq!(
            points,
            vec!["fake:Plafonnier/state"],
            "the kitchen lamp is already off"
        );
    }

    #[tokio::test]
    async fn a_kind_nobody_has_in_a_room_is_said() {
        let answer = call(
            "set_all",
            r#"{"kind":"volets","room":"Cuisine","value":"open"}"#,
        );
        let (moli, _) = house_model_over(house().await, vec![&answer]).await;
        let reply = moli
            .turn(ask("Ouvre les volets de la cuisine"))
            .await
            .unwrap();
        assert!(reply.actions.is_empty());
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.all_none"));
    }

    #[tokio::test]
    async fn a_relative_setting_moves_from_the_current_value_within_bounds() {
        let hub = house().await;
        let (probe, _) = house_model_over(hub.clone(), vec![]).await;
        let (clim, lamp) = (alias(&probe, "Clim"), alias(&probe, "Plafonnier"));
        let warmer = call(
            "adjust",
            &json!({ "point": format!("{clim}/target_temperature"), "by": 1 }).to_string(),
        );
        let darker = call(
            "adjust",
            &json!({ "point": format!("{lamp}/brightness"), "by": -80 }).to_string(),
        );
        let (moli, _) = house_model_over(hub, vec![&warmer, &darker]).await;
        let reply = moli.turn(ask("Un peu plus chaud")).await.unwrap();
        assert_eq!(reply.actions[0].value, json!(22.0));
        let reply = moli.turn(ask("Baisse beaucoup la lumière")).await.unwrap();
        assert_eq!(reply.actions[0].value, json!(0.0), "bounded at the minimum");
    }

    #[tokio::test]
    async fn a_relative_setting_on_something_not_numeric_is_refused() {
        let hub = house().await;
        let (probe, _) = house_model_over(hub.clone(), vec![]).await;
        let lamp = alias(&probe, "Lampe");
        let answer = call(
            "adjust",
            &json!({ "point": format!("{lamp}/state"), "by": 1 }).to_string(),
        );
        let (moli, _) = house_model_over(hub, vec![&answer]).await;
        let reply = moli.turn(ask("Plus de lampe")).await.unwrap();
        assert!(reply.actions.is_empty());
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.adjust_refused")
        );
    }

    #[tokio::test]
    async fn the_devices_in_a_state_are_named() {
        let on = call("find", r#"{"kind":"lumières","state":"allumé"}"#);
        let open = call("find", r#"{"state":"ouvert"}"#);
        let low = call("find", r#"{"state":"batterie_faible"}"#);
        let none = call(
            "find",
            r#"{"kind":"lumières","state":"allumé","room":"Cuisine"}"#,
        );
        let (moli, _) = house_model_over(house().await, vec![&on, &open, &low, &none]).await;
        let reply = moli
            .turn(ask("Quelles lumières sont allumées ?"))
            .await
            .unwrap();
        assert!(
            reply.reply.contains("Plafonnier") && !reply.reply.contains("Lampe"),
            "{}",
            reply.reply
        );
        let reply = moli.turn(ask("Qu'est-ce qui est ouvert ?")).await.unwrap();
        assert!(reply.reply.contains("Volet"), "{}", reply.reply);
        let reply = moli.turn(ask("Des piles à changer ?")).await.unwrap();
        assert!(reply.reply.contains("Capteur"), "{}", reply.reply);
        let reply = moli
            .turn(ask("Une lumière allumée dans la cuisine ?"))
            .await
            .unwrap();
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.find_none"));
    }

    #[tokio::test]
    async fn the_problems_of_the_house_are_listed() {
        let answer = call("problems", "{}");
        let (moli, _) = house_model_over(house().await, vec![&answer]).await;
        let reply = moli.turn(ask("Y a-t-il un problème ?")).await.unwrap();
        assert!(reply.reply.contains("Détecteur"), "smoke: {}", reply.reply);
        assert!(reply.reply.contains("Capteur"), "battery: {}", reply.reply);
        assert!(reply.actions.is_empty());
    }

    #[tokio::test]
    async fn a_calm_house_says_all_is_well() {
        let answer = call("problems", "{}");
        let (moli, _) = house_model_over(crate::tests::lamp_hub().await, vec![&answer]).await;
        let reply = moli.turn(ask("Tout va bien ?")).await.unwrap();
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.problems_none"));
    }

    #[test]
    fn the_time_is_said_in_words() {
        let at: jiff::Zoned = "2026-10-24T20:07:00+02:00[Europe/Paris]".parse().unwrap();
        assert_eq!(
            super::time_words(&at, "fr"),
            "Il est 20 h 07, samedi 24 octobre."
        );
        assert_eq!(
            super::time_words(&at, "en"),
            "It is 20:07, Saturday 24 October."
        );
    }

    #[tokio::test]
    async fn the_last_change_says_when_and_who() {
        let hub = house().await;
        let (probe, _) = house_model_over(hub.clone(), vec![]).await;
        let lamp = alias(&probe, "Lampe");
        hub.command(
            &"fake:Lampe/state".into(),
            Value::Bool(true),
            moli_core::Origin::Ui,
            None,
        )
        .await
        .unwrap();
        let answer = call("last_change", &json!({ "device": lamp }).to_string());
        let (moli, _) = house_model_over(hub, vec![&answer]).await;
        let reply = moli.turn(ask("Qui a allumé la lampe ?")).await.unwrap();
        assert!(reply.reply.contains("Lampe"), "{}", reply.reply);
        assert!(
            reply
                .reply
                .contains(&moli_i18n::tr!("assistant.house.by_dashboard")),
            "{}",
            reply.reply
        );
    }

    #[test]
    fn a_forecast_is_said_for_the_day_asked() {
        let forecast = json!({ "days": [
            { "date": "2026-10-24", "sky": "ensoleillé", "min_c": 8.2, "max_c": 17.6, "rain_chance_pct": 5 },
            { "date": "2026-10-25", "sky": "pluvieux", "min_c": 9.0, "max_c": 14.4, "rain_chance_pct": 80 },
        ] });
        let said = super::weather_words(&forecast, 1);
        assert!(
            said.contains("pluvieux")
                && said.contains('9')
                && said.contains("14")
                && said.contains("80"),
            "{said}"
        );
        assert!(
            said.starts_with(&moli_i18n::tr!("assistant.house.tomorrow")),
            "{said}"
        );
        assert_eq!(
            super::weather_words(&forecast, 5),
            moli_i18n::tr!("assistant.house.weather_unavailable")
        );
    }

    #[tokio::test]
    async fn an_ambiance_dresses_the_lights_of_a_room() {
        let look = call("ambiance", r#"{"room":"Salon","look":"cinéma"}"#);
        let unknown = call("ambiance", r#"{"room":"Salon","look":"disco"}"#);
        let (moli, _) = house_model_over(house().await, vec![&look, &unknown]).await;
        let reply = moli.turn(ask("Ambiance cinéma au salon")).await.unwrap();
        let devices: Vec<&str> = reply.actions.iter().map(|a| a.device.as_str()).collect();
        assert_eq!(devices, vec!["Plafonnier"], "{}", reply.reply);
        let reply = moli.turn(ask("Ambiance disco")).await.unwrap();
        assert!(reply.actions.is_empty());
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.ambiance_unknown")
        );
    }

    #[tokio::test]
    async fn an_order_for_later_goes_at_its_time_and_not_before() {
        let hub = house().await;
        let (probe, _) = house_model_over(hub.clone(), vec![]).await;
        let lamp = alias(&probe, "Lampe");
        let answer = call(
            "schedule",
            &json!({ "minutes": 2, "point": format!("{lamp}/state"), "value": true }).to_string(),
        );
        let (moli, _) = house_model_over(hub.clone(), vec![&answer]).await;
        let reply = moli
            .turn(ask("Allume la lampe de la cuisine dans 2 minutes"))
            .await
            .unwrap();
        assert!(reply.actions.is_empty(), "nothing now");
        assert!(reply.reply.contains('2'), "{}", reply.reply);
        let lamp_on = || {
            hub.device(&"fake:Lampe".into())
                .unwrap()
                .state
                .get("state")
                .map(|s| s.value.clone())
        };
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert_eq!(lamp_on(), Some(Value::Bool(false)), "not yet");
        tokio::time::advance(std::time::Duration::from_secs(61)).await;
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        tokio::time::resume();
        for _ in 0..100 {
            if lamp_on() == Some(Value::Bool(true)) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert_eq!(lamp_on(), Some(Value::Bool(true)));
        let entry = hub.journal(1)[0].clone();
        assert_eq!(entry.origin, moli_core::Origin::Assistant);
    }

    #[tokio::test]
    async fn a_reminder_is_shown_at_its_time() {
        let hub = crate::tests::lamp_hub().await;
        let mut events = hub.subscribe();
        let answer = call("remind", r#"{"minutes":1,"text":"Sortir le linge"}"#);
        let (moli, _) = house_model_over(hub.clone(), vec![&answer]).await;
        let reply = moli
            .turn(ask("Rappelle-moi de sortir le linge dans une minute"))
            .await
            .unwrap();
        assert!(reply.reply.contains('1'), "{}", reply.reply);
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(61)).await;
        tokio::time::resume();
        let notice = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Ok(moli_core::Event::Notice { notice }) = events.recv().await {
                    return notice;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(notice.message, "Sortir le linge");
    }

    #[tokio::test]
    async fn cancel_takes_back_what_was_just_planned() {
        let plan = call("remind", r#"{"minutes":30,"text":"Four"}"#);
        let undo = call("undo", "{}");
        let (moli, _) = house_model_over(crate::tests::lamp_hub().await, vec![&plan, &undo]).await;
        moli.turn(ask("Minuteur de 30 minutes pour le four"))
            .await
            .unwrap();
        let reply = moli.turn(ask("Annule le minuteur")).await.unwrap();
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.planned_cancelled", text = "Four")
        );
        assert!(moli.0.planned.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_delay_out_of_range_or_a_setting_is_not_planned() {
        let far = call("remind", r#"{"minutes":5000,"text":"Plus tard"}"#);
        let (moli, _) = house_model_over(crate::tests::lamp_hub().await, vec![&far]).await;
        let reply = moli.turn(ask("Dans quatre jours")).await.unwrap();
        assert_eq!(reply.reply, crate::house_model::unknown_words());
        assert!(moli.0.planned.lock().unwrap().is_empty());
    }

    async fn with_garage_and_lock() -> moli_runtime::Hub {
        hub_with(Fake(vec![
            (
                "Porte de garage",
                vec![spec(
                    "control",
                    choice(&["open", "stop", "close"]),
                    true,
                    Semantic::Other,
                )],
                vec![("control", Value::Text("open".into()))],
            ),
            (
                "Volet",
                vec![spec(
                    "control",
                    choice(&["open", "stop", "close"]),
                    true,
                    Semantic::Other,
                )],
                vec![("control", Value::Text("open".into()))],
            ),
            (
                "Entrée",
                vec![spec("state", Kind::Binary, true, Semantic::Lock)],
                vec![("state", Value::Bool(true))],
            ),
            (
                "Plafonnier",
                vec![
                    spec("state", Kind::Binary, true, Semantic::OnOff),
                    spec("brightness", level(0.0, 100.0), true, Semantic::Brightness),
                ],
                vec![
                    ("state", Value::Bool(false)),
                    ("brightness", Value::Float(20.0)),
                ],
            ),
            (
                "Enceinte",
                vec![spec("volume", level(0.0, 100.0), true, Semantic::Other)],
                vec![("volume", Value::Float(20.0))],
            ),
        ]))
        .await
    }

    #[tokio::test]
    async fn closing_the_shutters_or_cutting_the_plugs_never_touches_a_garage_door_or_a_lock() {
        let shut = call("set_all", r#"{"kind":"volets","value":"close"}"#);
        let plugs = call("set_all", r#"{"kind":"prises","value":false}"#);
        let (moli, _) = house_model_over(with_garage_and_lock().await, vec![&shut, &plugs]).await;
        let reply = moli.turn(ask("Ferme tous les volets")).await.unwrap();
        let points: Vec<&str> = reply.actions.iter().map(|a| a.point.as_str()).collect();
        assert_eq!(points, vec!["fake:Volet/control"]);
        let reply = moli.turn(ask("Coupe toutes les prises")).await.unwrap();
        assert!(
            reply.actions.is_empty(),
            "the lock is not a plug: {:?}",
            reply.actions
        );
    }

    #[tokio::test]
    async fn everything_on_or_to_a_level_is_the_lights_never_the_speakers() {
        let on = call("set_all", r#"{"kind":"tout","value":true}"#);
        let level = call("set_all", r#"{"kind":"tout","value":60}"#);
        let (moli, _) = house_model_over(with_garage_and_lock().await, vec![&on, &level]).await;
        let reply = moli.turn(ask("Allume tout")).await.unwrap();
        let points: Vec<&str> = reply.actions.iter().map(|a| a.point.as_str()).collect();
        assert_eq!(points, vec!["fake:Plafonnier/state"]);
        let reply = moli.turn(ask("Tout à 60 %")).await.unwrap();
        let points: Vec<&str> = reply.actions.iter().map(|a| a.point.as_str()).collect();
        assert_eq!(points, vec!["fake:Plafonnier/brightness"]);
    }

    #[tokio::test]
    async fn an_order_for_later_is_never_carried_out_now_whatever_the_tool() {
        for answer in [
            call("set_all", r#"{"kind":"lumières","value":false}"#),
            call("ambiance", r#"{"room":"Salon","look":"cinema"}"#),
        ] {
            let (moli, _) = house_model_over(house().await, vec![&answer]).await;
            let reply = moli.turn(ask("Éteins tout dans 10 minutes")).await.unwrap();
            assert!(reply.actions.is_empty(), "{:?}", reply.actions);
            assert_eq!(
                reply.reply,
                moli_i18n::tr!("assistant.house.later_unsupported")
            );
        }
    }

    #[tokio::test]
    async fn an_ambiance_needs_a_room() {
        let answer = call("ambiance", r#"{"look":"cinema"}"#);
        let (moli, _) = house_model_over(house().await, vec![&answer]).await;
        let reply = moli.turn(ask("Ambiance cinéma")).await.unwrap();
        assert!(reply.actions.is_empty());
    }

    #[test]
    fn the_weather_without_a_day_is_today() {
        assert_eq!(super::day_of(&json!({})), 0);
        assert_eq!(super::day_of(&json!({ "day": 1 })), 1);
        assert_eq!(super::day_of(&json!({ "day": 40 })), 6);
    }

    #[tokio::test]
    async fn a_planned_order_is_checked_when_planned() {
        let hub = house().await;
        let (probe, _) = house_model_over(hub.clone(), vec![]).await;
        let lamp = alias(&probe, "Lampe");
        for (point, value) in [
            (format!("{lamp}/state"), json!("on")),
            (format!("{lamp}/nope"), json!(true)),
        ] {
            let answer = call(
                "schedule",
                &json!({ "minutes": 5, "point": point, "value": value }).to_string(),
            );
            let (moli, _) = house_model_over(hub.clone(), vec![&answer]).await;
            moli.turn(ask("Plus tard")).await.unwrap();
            assert!(moli.0.planned.lock().unwrap().is_empty(), "{point} {value}");
        }
    }

    #[tokio::test]
    async fn cancel_takes_back_the_latest_of_a_plan_and_a_confirmed_change() {
        let plan = call("remind", r#"{"minutes":30,"text":"Four"}"#);
        let rename = call(
            "rename_device",
            r#"{"device":"d1","new_name":"Lampe du salon"}"#,
        );
        let undo = call("undo", "{}");
        let (moli, _) =
            house_model_over(crate::tests::lamp_hub().await, vec![&plan, &rename, &undo]).await;
        moli.turn(ask("Minuteur de 30 minutes")).await.unwrap();
        let reply = moli.turn(ask("Renomme la lampe")).await.unwrap();
        moli.decide_proposal(reply.proposals[0].id, true, moli_core::Origin::Ui, None)
            .await
            .unwrap();
        let reply = moli.turn(ask("Annule")).await.unwrap();
        assert_eq!(
            reply.proposals.len(),
            1,
            "the rename is the latest: {}",
            reply.reply
        );
        assert_eq!(
            moli.0.planned.lock().unwrap().len(),
            1,
            "the reminder stays"
        );
    }

    #[tokio::test]
    async fn an_order_on_a_device_of_another_room_than_the_one_said_is_not_sent() {
        let hub = house().await;
        let (probe, _) = house_model_over(hub.clone(), vec![]).await;
        let salon = alias(&probe, "Plafonnier");
        for answer in [
            call(
                "set",
                &json!({ "point": format!("{salon}/state"), "value": false }).to_string(),
            ),
            call(
                "schedule",
                &json!({ "minutes": 5, "point": format!("{salon}/state"), "value": false })
                    .to_string(),
            ),
        ] {
            let (moli, _) = house_model_over(hub.clone(), vec![&answer]).await;
            let reply = moli
                .turn(ask("Éteins la lumière de la cuisine"))
                .await
                .unwrap();
            assert!(reply.actions.is_empty(), "{:?}", reply.actions);
            assert!(moli.0.planned.lock().unwrap().is_empty());
            assert!(reply.reply.contains("Salon"), "{}", reply.reply);
        }
        let answer = call(
            "set",
            &json!({ "point": format!("{salon}/state"), "value": false }).to_string(),
        );
        let (moli, _) = house_model_over(hub, vec![&answer]).await;
        let reply = moli.turn(ask("Éteins la lumière du salon")).await.unwrap();
        assert_eq!(reply.actions.len(), 1, "the room said is the device's");
    }

    #[tokio::test]
    async fn a_device_that_does_not_answer_is_never_said_on_or_off() {
        let on = call("find", r#"{"kind":"lumières","state":"allumé"}"#);
        let (moli, _) = house_model_over(crate::tests::lamp_hub_with(true).await, vec![&on]).await;
        let reply = moli
            .turn(ask("Quelles lumières sont allumées ?"))
            .await
            .unwrap();
        assert_eq!(reply.reply, moli_i18n::tr!("assistant.house.find_none"));
    }

    #[tokio::test]
    async fn the_weather_of_a_house_without_a_place_says_how_to_give_it() {
        let answer = call("weather", r#"{"day":1}"#);
        let (moli, _) = house_model_over(crate::tests::lamp_hub().await, vec![&answer]).await;
        let reply = moli.turn(ask("Quel temps demain ?")).await.unwrap();
        assert_eq!(
            reply.reply,
            moli_i18n::tr!("assistant.house.weather_no_place")
        );
    }

    async fn dark_ribbons() -> moli_runtime::Hub {
        hub_with(Fake(vec![(
            "Ruban",
            vec![
                spec("state", Kind::Binary, true, Semantic::OnOff),
                spec("brightness", level(0.0, 100.0), true, Semantic::Brightness),
                spec("color", Kind::Text, true, Semantic::Color),
            ],
            vec![
                ("state", Value::Bool(false)),
                ("brightness", Value::Float(40.0)),
                ("color", Value::Text("#ffffff".into())),
            ],
        )]))
        .await
    }

    #[tokio::test]
    async fn a_colour_or_a_level_on_a_light_that_is_off_lights_it_up() {
        let hub = dark_ribbons().await;
        let (probe, _) = house_model_over(hub.clone(), vec![]).await;
        let id = alias(&probe, "Ruban");
        let red = call(
            "set",
            &json!({ "point": format!("{id}/color"), "value": "rouge" }).to_string(),
        );
        let (moli, _) = house_model_over(hub.clone(), vec![&red]).await;
        let reply = moli.turn(ask("Allume le ruban en rouge")).await.unwrap();
        let points: Vec<&str> = reply.actions.iter().map(|a| a.point.as_str()).collect();
        assert_eq!(
            points,
            vec!["fake:Ruban/state", "fake:Ruban/color"],
            "on, then the colour"
        );
        assert_eq!(reply.actions[0].value, json!(true));
        // a level of zero is no wish to light it
        hub.command(
            &"fake:Ruban/state".into(),
            Value::Bool(false),
            moli_core::Origin::Ui,
            None,
        )
        .await
        .unwrap();
        let zero = call(
            "set",
            &json!({ "point": format!("{id}/brightness"), "value": 0 }).to_string(),
        );
        let (moli, _) = house_model_over(hub, vec![&zero]).await;
        let reply = moli.turn(ask("Mets le ruban à zéro")).await.unwrap();
        assert_eq!(reply.actions.len(), 1, "{:?}", reply.actions);
    }

    #[tokio::test]
    async fn a_level_for_every_light_lights_up_the_ones_that_are_off() {
        let hub = dark_ribbons().await;
        let answer = call("set_all", r#"{"kind":"lumières","value":30}"#);
        let (moli, _) = house_model_over(hub, vec![&answer]).await;
        let reply = moli.turn(ask("Toutes les lumières à 30 %")).await.unwrap();
        let points: Vec<&str> = reply.actions.iter().map(|a| a.point.as_str()).collect();
        assert_eq!(points, vec!["fake:Ruban/state", "fake:Ruban/brightness"]);
    }
}
