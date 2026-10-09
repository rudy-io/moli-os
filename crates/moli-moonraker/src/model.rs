//! Pure translation between Moonraker's answers and Moli's points. No IO.
//!
//! The printer's shape (how many tool heads, a bed, a chamber sensor, a
//! light, Snapmaker's filament table) is read once from the list of Klipper
//! objects; every poll then reads the same objects in one query.

use std::fmt::Write as _;

use moli_core::{Access, Kind, PointSpec, Semantic, Unit, Value};
use serde_json::Value as Json;

/// What the printer is doing (`print_stats.state`).
pub const STATES: [&str; 6] = [
    "standby",
    "printing",
    "paused",
    "complete",
    "cancelled",
    "error",
];

/// What can be asked of a print. Agents need a human for these.
pub const ORDERS: [&str; 3] = ["pause", "resume", "cancel"];

/// What this printer has, from `/printer/objects/list`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Shape {
    /// `extruder`, `extruder1`… in order: one per tool head.
    pub tools: Vec<String>,
    pub bed: bool,
    /// The object that gives the chamber's temperature.
    pub chamber: Option<String>,
    /// The light's object (`led cavity_led`).
    pub light: Option<String>,
    /// Snapmaker's filament table, one entry per tool.
    pub filaments: bool,
}

impl Shape {
    #[must_use]
    pub fn of(objects: &[String]) -> Self {
        let has = |name: &str| objects.iter().any(|o| o == name);
        let mut tools: Vec<String> = objects
            .iter()
            .filter(|o| {
                o.strip_prefix("extruder")
                    .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit()))
            })
            .cloned()
            .collect();
        tools.sort_by_key(|t| t["extruder".len()..].parse::<u32>().unwrap_or(0));
        let chamber = [
            "temperature_sensor chamber",
            "temperature_sensor cavity",
            "heater_generic chamber",
        ]
        .into_iter()
        .find(|o| has(o))
        .map(str::to_owned);
        let light = objects
            .iter()
            .find(|o| o.starts_with("led ") || o.starts_with("neopixel "))
            .cloned();
        Self {
            tools,
            bed: has("heater_bed"),
            chamber,
            light,
            filaments: has("print_task_config"),
        }
    }

    /// The objects read at each poll (`/printer/objects/query?…`).
    #[must_use]
    pub fn query(&self) -> String {
        let mut objects = vec![
            "webhooks",
            "print_stats",
            "display_status",
            "virtual_sdcard",
            "toolhead",
        ];
        objects.extend(self.tools.iter().map(String::as_str));
        if self.bed {
            objects.push("heater_bed");
        }
        objects.extend(self.chamber.as_deref());
        objects.extend(self.light.as_deref());
        if self.filaments {
            objects.push("print_task_config");
        }
        let list: Vec<String> = objects.iter().map(|o| o.replace(' ', "%20")).collect();
        format!("/printer/objects/query?{}", list.join("&"))
    }

    fn multi(&self) -> bool {
        self.tools.len() > 1
    }

    #[must_use]
    pub fn points(&self) -> Vec<PointSpec> {
        let mut p = print_points();
        p.extend(self.head_points());
        if self.bed {
            p.push(point(
                "bed_temperature",
                &moli_i18n::tr!("pilotes.moonraker.plateau"),
                numeric(),
                celsius(),
                Semantic::Temperature,
            ));
            p.push(point(
                "bed_target",
                &moli_i18n::tr!("pilotes.moonraker.plateau_consigne"),
                numeric(),
                celsius(),
                Semantic::Config,
            ));
        }
        if self.chamber.is_some() {
            p.push(point(
                "chamber_temperature",
                &moli_i18n::tr!("pilotes.moonraker.enceinte"),
                numeric(),
                celsius(),
                Semantic::Temperature,
            ));
        }
        p.push(point(
            "message",
            &moli_i18n::tr!("pilotes.moonraker.message"),
            Kind::Text,
            None,
            Semantic::Other,
        ));
        if self.light.is_some() {
            p.push(writable(point(
                "light",
                &moli_i18n::tr!("pilotes.moonraker.lumiere"),
                Kind::Binary,
                None,
                Semantic::Other,
            )));
        }
        p.push(writable(point(
            "control",
            &moli_i18n::tr!("pilotes.moonraker.commande"),
            enumeration(&ORDERS),
            None,
            Semantic::Control,
        )));
        p.push(point(
            "jobs_total",
            &moli_i18n::tr!("pilotes.moonraker.impressions_total"),
            counter(),
            None,
            Semantic::Other,
        ));
        p.push(point(
            "print_hours_total",
            &moli_i18n::tr!("pilotes.moonraker.heures_total"),
            counter(),
            Some(Unit::Hour),
            Semantic::Other,
        ));
        p.push(point(
            "filament_total",
            &moli_i18n::tr!("pilotes.moonraker.filament_total"),
            counter(),
            metres(),
            Semantic::Other,
        ));
        p
    }

    /// The nozzle in use, then each tool head and its filament.
    fn head_points(&self) -> Vec<PointSpec> {
        let mut p = vec![
            point(
                "nozzle_temperature",
                &moli_i18n::tr!("pilotes.moonraker.buse"),
                numeric(),
                celsius(),
                Semantic::Temperature,
            ),
            point(
                "nozzle_target",
                &moli_i18n::tr!("pilotes.moonraker.buse_consigne"),
                numeric(),
                celsius(),
                Semantic::Config,
            ),
        ];
        if self.multi() {
            let n = count(self.tools.len());
            p.push(point(
                "active_tool",
                &moli_i18n::tr!("pilotes.moonraker.tete_active"),
                range(1.0, n),
                None,
                Semantic::Other,
            ));
        }
        if self.multi() || self.filaments {
            for i in 1..=self.tools.len() {
                p.push(point(
                    &format!("tool{i}_temperature"),
                    &moli_i18n::tr!("pilotes.moonraker.tete", n = i),
                    numeric(),
                    celsius(),
                    Semantic::Temperature,
                ));
                p.push(point(
                    &format!("tool{i}_target"),
                    &moli_i18n::tr!("pilotes.moonraker.tete_consigne", n = i),
                    numeric(),
                    celsius(),
                    Semantic::Config,
                ));
                if self.filaments {
                    p.push(point(
                        &format!("tool{i}_filament"),
                        &moli_i18n::tr!("pilotes.moonraker.filament", n = i),
                        Kind::Text,
                        None,
                        Semantic::Other,
                    ));
                    p.push(point(
                        &format!("tool{i}_color"),
                        &moli_i18n::tr!("pilotes.moonraker.couleur", n = i),
                        Kind::Text,
                        None,
                        Semantic::Other,
                    ));
                }
            }
        }
        p
    }
}

/// The print itself: every Klipper printer has these.
fn print_points() -> Vec<PointSpec> {
    vec![
        point(
            "state",
            &moli_i18n::tr!("pilotes.moonraker.etat"),
            enumeration(&STATES),
            None,
            Semantic::Other,
        ),
        point(
            "printing",
            &moli_i18n::tr!("pilotes.moonraker.en_impression"),
            Kind::Binary,
            None,
            Semantic::Other,
        ),
        point(
            "progress",
            &moli_i18n::tr!("pilotes.moonraker.progression"),
            range(0.0, 100.0),
            Some(Unit::Percent),
            Semantic::Other,
        ),
        point(
            "remaining",
            &moli_i18n::tr!("pilotes.moonraker.temps_restant"),
            counter(),
            Some(Unit::Minute),
            Semantic::Other,
        ),
        point(
            "job",
            &moli_i18n::tr!("pilotes.moonraker.impression"),
            Kind::Text,
            None,
            Semantic::Other,
        ),
        point(
            "layer",
            &moli_i18n::tr!("pilotes.moonraker.couche"),
            counter(),
            None,
            Semantic::Other,
        ),
        point(
            "total_layers",
            &moli_i18n::tr!("pilotes.moonraker.couches"),
            counter(),
            None,
            Semantic::Other,
        ),
        point(
            "print_duration",
            &moli_i18n::tr!("pilotes.moonraker.duree"),
            counter(),
            Some(Unit::Minute),
            Semantic::Other,
        ),
        point(
            "filament_used",
            &moli_i18n::tr!("pilotes.moonraker.filament_utilise"),
            counter(),
            metres(),
            Semantic::Other,
        ),
    ]
}

#[allow(clippy::unnecessary_wraps)]
fn celsius() -> Option<Unit> {
    Some(Unit::Celsius)
}

/// A count or an amount: never negative, no upper bound.
fn counter() -> Kind {
    Kind::Numeric {
        min: Some(0.0),
        max: None,
        step: None,
    }
}

fn point(key: &str, label: &str, kind: Kind, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind,
        access: Access {
            read: true,
            write: false,
        },
        unit,
        semantic,
    }
}

fn writable(mut p: PointSpec) -> PointSpec {
    p.access.write = true;
    p
}

fn enumeration(values: &[&str]) -> Kind {
    Kind::Enum {
        values: values.iter().map(|v| (*v).into()).collect(),
    }
}

fn numeric() -> Kind {
    Kind::Numeric {
        min: None,
        max: None,
        step: None,
    }
}

fn range(min: f64, max: f64) -> Kind {
    Kind::Numeric {
        min: Some(min),
        max: Some(max),
        step: None,
    }
}

fn count(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

fn metres() -> Option<Unit> {
    Unit::parse("m")
}

/// The slicer's facts about a file (`/server/files/metadata`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Job {
    pub file: String,
    /// Seconds, as the slicer estimated.
    pub estimated: Option<f64>,
    /// The largest thumbnail, as a path under `gcodes/`.
    pub thumbnail: Option<String>,
}

impl Job {
    #[must_use]
    pub fn from_metadata(file: &str, meta: &Json) -> Self {
        let dir = file.rsplit_once('/').map(|(d, _)| format!("{d}/"));
        let thumbnail = meta["thumbnails"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| Some((t["width"].as_u64()?, t["relative_path"].as_str()?)))
            .max_by_key(|(w, _)| *w)
            .map(|(_, path)| format!("{}{path}", dir.as_deref().unwrap_or("")));
        Self {
            file: file.to_owned(),
            estimated: meta["estimated_time"].as_f64(),
            thumbnail,
        }
    }
}

fn round(v: f64, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    (v * f).round() / f
}

/// Minutes left: the slicer's estimate and the file's progress, averaged
/// when both are known (each alone drifts: the slicer is optimistic, the
/// file's bytes are not its time).
#[must_use]
pub fn remaining(duration: f64, progress: f64, estimated: Option<f64>) -> Option<f64> {
    let by_file = (progress >= 0.02).then(|| duration / progress - duration);
    let by_slicer = estimated.map(|e| (e - duration).max(0.0));
    let seconds = match (by_file, by_slicer) {
        (Some(f), Some(s)) => f64::midpoint(f, s),
        (Some(f), None) => f,
        (None, Some(s)) => s,
        (None, None) => return None,
    };
    Some((seconds.max(0.0) / 60.0).round())
}

fn filament(table: &Json, i: usize) -> Option<(String, String)> {
    let at = |key: &str| table[key][i].as_str().map_or("", str::trim);
    if table["filament_exist"][i] == Json::Bool(false) {
        return Some((moli_i18n::tr!("pilotes.moonraker.vide"), String::new()));
    }
    let kind = [at("filament_type"), at("filament_sub_type")]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let vendor = at("filament_vendor");
    let mut text = kind;
    if !vendor.is_empty() && !vendor.eq_ignore_ascii_case("generic") {
        text = format!("{text} · {vendor}");
    }
    let rgba = at("filament_color_rgba");
    let color = if rgba.len() >= 6 && rgba[..6].chars().all(|c| c.is_ascii_hexdigit()) {
        format!("#{}", rgba[..6].to_ascii_lowercase())
    } else {
        String::new()
    };
    (!text.is_empty()).then_some((text, color))
}

/// Values from one `objects/query` answer (`result.status`).
#[must_use]
pub fn values(shape: &Shape, status: &Json, job: Option<&Job>) -> Vec<(String, Value)> {
    let mut found: Vec<(String, Value)> = Vec::new();
    let job_stats = &status["print_stats"];
    let ready = status["webhooks"]["state"]
        .as_str()
        .is_none_or(|s| s == "ready");
    let state = if ready {
        job_stats["state"].as_str().unwrap_or("standby")
    } else {
        "error"
    };
    add(&mut found, "state", Value::from(state));
    let active = matches!(state, "printing" | "paused");
    add(&mut found, "printing", Value::Bool(active));
    let progress = status["display_status"]["progress"]
        .as_f64()
        .filter(|p| *p > 0.0)
        .or_else(|| status["virtual_sdcard"]["progress"].as_f64())
        .unwrap_or(0.0);
    add(
        &mut found,
        "progress",
        Value::Float(round(progress * 100.0, 1)),
    );
    let duration = job_stats["print_duration"].as_f64().unwrap_or(0.0);
    if active {
        if let Some(left) = remaining(duration, progress, job.and_then(|j| j.estimated)) {
            add(&mut found, "remaining", Value::Float(left));
        }
    } else if state == "complete" {
        add(&mut found, "remaining", Value::Float(0.0));
    }
    let file = job_stats["filename"].as_str().unwrap_or("");
    add(
        &mut found,
        "job",
        Value::from(file.strip_suffix(".gcode").unwrap_or(file)),
    );
    for (key, field) in [("layer", "current_layer"), ("total_layers", "total_layer")] {
        if let Some(n) = job_stats["info"][field].as_f64() {
            add(&mut found, key, Value::Float(n));
        }
    }
    add(
        &mut found,
        "print_duration",
        Value::Float((duration / 60.0).round()),
    );
    if let Some(mm) = job_stats["filament_used"].as_f64() {
        add(
            &mut found,
            "filament_used",
            Value::Float(round(mm / 1000.0, 1)),
        );
    }
    heads(shape, status, &mut found);
    if shape.bed {
        let bed = &status["heater_bed"];
        if let Some(t) = bed["temperature"].as_f64() {
            add(&mut found, "bed_temperature", Value::Float(round(t, 1)));
        }
        if let Some(t) = bed["target"].as_f64() {
            add(&mut found, "bed_target", Value::Float(t));
        }
    }
    if let Some(chamber) = &shape.chamber
        && let Some(t) = status[chamber.as_str()]["temperature"].as_f64()
    {
        add(&mut found, "chamber_temperature", Value::Float(round(t, 1)));
    }
    let message = if ready {
        job_stats["message"].as_str().unwrap_or("")
    } else {
        status["webhooks"]["state_message"].as_str().unwrap_or("")
    };
    add(&mut found, "message", Value::from(message.trim()));
    if let Some(light) = &shape.light
        && let Some(channels) = status[light.as_str()]["color_data"][0].as_array()
    {
        let on = channels.iter().any(|c| c.as_f64().unwrap_or(0.0) > 0.0);
        add(&mut found, "light", Value::Bool(on));
    }
    found
}

/// The nozzle in use, then each tool head and its filament.
fn heads(shape: &Shape, status: &Json, found: &mut Vec<(String, Value)>) {
    let active_tool = status["toolhead"]["extruder"]
        .as_str()
        .and_then(|name| shape.tools.iter().position(|t| t == name))
        .unwrap_or(0);
    if let Some(tool) = shape.tools.get(active_tool) {
        let heater = &status[tool.as_str()];
        if let Some(t) = heater["temperature"].as_f64() {
            add(found, "nozzle_temperature", Value::Float(round(t, 1)));
        }
        if let Some(t) = heater["target"].as_f64() {
            add(found, "nozzle_target", Value::Float(t));
        }
    }
    if shape.multi() {
        add(found, "active_tool", Value::Float(count(active_tool + 1)));
    }
    if shape.multi() || shape.filaments {
        for (i, tool) in shape.tools.iter().enumerate() {
            let n = i + 1;
            if let Some(t) = status[tool.as_str()]["temperature"].as_f64() {
                add(
                    found,
                    &format!("tool{n}_temperature"),
                    Value::Float(round(t, 1)),
                );
            }
            if let Some(t) = status[tool.as_str()]["target"].as_f64() {
                add(found, &format!("tool{n}_target"), Value::Float(t));
            }
            if shape.filaments
                && let Some((text, color)) = filament(&status["print_task_config"], i)
            {
                add(
                    found,
                    &format!("tool{n}_filament"),
                    Value::from(text.as_str()),
                );
                add(
                    found,
                    &format!("tool{n}_color"),
                    Value::from(color.as_str()),
                );
            }
        }
    }
}

fn add(found: &mut Vec<(String, Value)>, key: &str, value: Value) {
    found.push((key.to_owned(), value));
}

/// Lifetime figures (`/server/history/totals`, `result`).
#[must_use]
pub fn totals(result: &Json) -> Vec<(String, Value)> {
    let t = &result["job_totals"];
    let mut out = Vec::new();
    if let Some(n) = t["total_jobs"].as_f64() {
        out.push(("jobs_total".to_owned(), Value::Float(n)));
    }
    if let Some(s) = t["total_print_time"].as_f64() {
        out.push((
            "print_hours_total".to_owned(),
            Value::Float(round(s / 3600.0, 1)),
        ));
    }
    if let Some(mm) = t["total_filament_used"].as_f64() {
        out.push((
            "filament_total".to_owned(),
            Value::Float((mm / 1000.0).round()),
        ));
    }
    out
}

/// The request (a `POST` path) a command becomes, or why it cannot.
pub fn order(shape: &Shape, key: &str, value: &Value) -> Result<String, String> {
    match (key, value) {
        ("control", Value::Text(o)) if ORDERS.contains(&&**o) => Ok(format!("/printer/print/{o}")),
        ("light", Value::Bool(on)) => {
            let Some(light) = &shape.light else {
                return Err(moli_i18n::tr!("pilotes.moonraker.pas_de_lumiere"));
            };
            let name = light.split_once(' ').map_or(light.as_str(), |(_, n)| n);
            let v = u8::from(*on);
            let script = format!("SET_LED LED={name} RED={v} GREEN={v} BLUE={v} WHITE={v}");
            Ok(format!("/printer/gcode/script?script={}", encode(&script)))
        }
        (key, _) => Err(moli_i18n::tr!(
            "pilotes.moonraker.ne_se_regle_pas",
            point = key
        )),
    }
}

/// Percent-encoding for a URL path or query value (`/` kept in paths).
#[must_use]
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
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

    fn fixture() -> Json {
        serde_json::from_slice(include_bytes!(
            "../../../integrations/klipper/fixtures/u1.json"
        ))
        .unwrap()
    }

    fn shape() -> Shape {
        let objects: Vec<String> = serde_json::from_value(fixture()["objects"].clone()).unwrap();
        Shape::of(&objects)
    }

    fn get<'a>(values: &'a [(String, Value)], key: &str) -> &'a Value {
        &values.iter().find(|(k, _)| k == key).unwrap().1
    }

    #[test]
    fn the_u1_is_read_as_it_is() {
        let shape = shape();
        assert_eq!(
            shape.tools,
            ["extruder", "extruder1", "extruder2", "extruder3"]
        );
        assert!(shape.bed && shape.filaments);
        assert_eq!(shape.chamber.as_deref(), Some("temperature_sensor cavity"));
        assert_eq!(shape.light.as_deref(), Some("led cavity_led"));
        assert!(
            shape
                .query()
                .contains("temperature_sensor%20cavity&led%20cavity_led")
        );
        let keys: Vec<String> = shape.points().iter().map(|p| p.key.to_string()).collect();
        for k in [
            "state",
            "active_tool",
            "tool4_filament",
            "chamber_temperature",
            "light",
            "control",
        ] {
            assert!(keys.contains(&k.to_owned()), "{k}");
        }
        let control = shape
            .points()
            .into_iter()
            .find(|p| &*p.key == "control")
            .unwrap();
        assert!(control.access.write);
        assert_eq!(control.semantic, Semantic::Control);
    }

    #[test]
    fn a_finished_print_reads_back() {
        let f = fixture();
        let shape = shape();
        let job = Job::from_metadata("Calibration_cube_PLA_8h16m.gcode", &f["metadata"]["result"]);
        assert_eq!(
            job.thumbnail.as_deref(),
            Some(".thumbs/Calibration_cube_PLA_8h16m-300x300.png")
        );
        assert_eq!(job.estimated, Some(29732.0));
        let v = values(&shape, &f["query"]["result"]["status"], Some(&job));
        assert_eq!(get(&v, "state"), &Value::from("complete"));
        assert_eq!(get(&v, "printing"), &Value::Bool(false));
        assert_eq!(get(&v, "progress"), &Value::Float(100.0));
        assert_eq!(get(&v, "remaining"), &Value::Float(0.0));
        assert_eq!(get(&v, "job"), &Value::from("Calibration_cube_PLA_8h16m"));
        assert_eq!(get(&v, "layer"), &Value::Float(1002.0));
        assert_eq!(get(&v, "print_duration"), &Value::Float(462.0));
        assert_eq!(get(&v, "filament_used"), &Value::Float(29.1));
        assert_eq!(get(&v, "active_tool"), &Value::Float(4.0));
        assert_eq!(get(&v, "tool1_filament"), &Value::from("PLA"));
        assert_eq!(get(&v, "tool1_color"), &Value::from("#6c5bb1"));
        assert_eq!(
            get(&v, "tool3_filament"),
            &Value::from("PLA Basic · Snapmaker")
        );
        assert_eq!(get(&v, "chamber_temperature"), &Value::Float(30.0));
        assert_eq!(get(&v, "light"), &Value::Bool(false));
        let t = totals(&f["totals"]["result"]);
        assert_eq!(get(&t, "jobs_total"), &Value::Float(83.0));
        assert_eq!(get(&t, "print_hours_total"), &Value::Float(244.6));
        assert_eq!(get(&t, "filament_total"), &Value::Float(1787.0));
    }

    #[test]
    fn time_left_blends_slicer_and_file() {
        // Half way after an hour, the slicer said 7000 s in all.
        assert_eq!(remaining(3600.0, 0.5, Some(7000.0)), Some(58.0));
        assert_eq!(remaining(3600.0, 0.5, None), Some(60.0));
        assert_eq!(remaining(60.0, 0.01, Some(7260.0)), Some(120.0));
        assert_eq!(remaining(60.0, 0.0, None), None);
    }

    #[test]
    fn klipper_down_is_an_error_with_its_reason() {
        let shape = shape();
        let status = serde_json::json!({
            "webhooks": {"state": "shutdown", "state_message": "MCU 'e1' shutdown"},
            "print_stats": {"state": "printing", "filename": "a.gcode"}
        });
        let v = values(&shape, &status, None);
        assert_eq!(get(&v, "state"), &Value::from("error"));
        assert_eq!(get(&v, "message"), &Value::from("MCU 'e1' shutdown"));
    }

    #[test]
    fn orders_become_requests() {
        let shape = shape();
        assert_eq!(
            order(&shape, "control", &Value::from("pause")).unwrap(),
            "/printer/print/pause"
        );
        assert!(order(&shape, "control", &Value::from("start")).is_err());
        assert_eq!(
            order(&shape, "light", &Value::Bool(true)).unwrap(),
            "/printer/gcode/script?script=SET_LED%20LED%3Dcavity_led%20RED%3D1%20GREEN%3D1%20BLUE%3D1%20WHITE%3D1"
        );
        assert!(order(&Shape::default(), "light", &Value::Bool(true)).is_err());
        assert!(order(&shape, "progress", &Value::Float(1.0)).is_err());
        assert_eq!(
            encode("a b/.thumbs/x é.png"),
            "a%20b/.thumbs/x%20%C3%A9.png"
        );
    }
}
