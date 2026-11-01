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
    /// Creality's firmware (K1, K2…): its touch probe gives it away.
    pub creality: bool,
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
            "temperature_sensor chamber_temp",
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
            creality: objects.iter().any(|o| o.starts_with("prtouch")),
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

/// « Creality K2 » from the host name Creality gives it (`K2-1234`).
#[must_use]
pub fn creality_machine(host: &str) -> String {
    match host.split_once('-').map(|(m, _)| m) {
        Some(m) if m.starts_with('K') && m.len() <= 8 => format!("Creality {m}"),
        _ => "Creality".to_owned(),
    }
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
    let color = at("filament_color_rgba")
        .get(..6)
        .filter(|rgb| rgb.chars().all(|c| c.is_ascii_hexdigit()))
        .map_or_else(String::new, |rgb| format!("#{}", rgb.to_ascii_lowercase()));
    (!text.is_empty()).then_some((text, color))
}

/// Values from one `objects/query` answer (`result.status`).
/// Layer and total: Klipper's own, else Creality's when they hold (a total
/// above zero, the layer within it).
fn layers(job_stats: &Json, sdcard: &Json) -> [Option<f64>; 2] {
    let creality = match (sdcard["layer"].as_f64(), sdcard["layer_count"].as_f64()) {
        (Some(layer), Some(total)) if total > 0.0 && (0.0..=total).contains(&layer) => {
            [Some(layer), Some(total)]
        }
        _ => [None, None],
    };
    let info = &job_stats["info"];
    [
        info["current_layer"].as_f64().or(creality[0]),
        info["total_layer"].as_f64().or(creality[1]),
    ]
}

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
    let sdcard = &status["virtual_sdcard"];
    // Creality's firmware tells its own print (`cur_print_data`): its
    // `display_status` lags far behind, the file's progress is the one its
    // screen shows. Moonraker does not read its slicer: the estimate is here.
    // Each of its values is taken only when it holds, else the usual
    // reading stands.
    let creality = &sdcard["cur_print_data"];
    let progress = sdcard["progress"]
        .as_f64()
        .filter(|p| creality.is_object() && (0.0..=1.0).contains(p))
        .or_else(|| {
            status["display_status"]["progress"]
                .as_f64()
                .filter(|p| *p > 0.0)
        })
        .or_else(|| sdcard["progress"].as_f64())
        .unwrap_or(0.0);
    add(
        &mut found,
        "progress",
        Value::Float(round(progress * 100.0, 1)),
    );
    let duration = job_stats["print_duration"].as_f64().unwrap_or(0.0);
    if active {
        let estimated = job.and_then(|j| j.estimated).or_else(|| {
            creality["metadata"]["estimated_time"]
                .as_f64()
                .filter(|t| *t > 0.0)
        });
        if let Some(left) = remaining(duration, progress, estimated) {
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
    for (key, n) in ["layer", "total_layers"]
        .into_iter()
        .zip(layers(job_stats, sdcard))
    {
        if let Some(n) = n {
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

    fn k2() -> Json {
        serde_json::from_slice(include_bytes!(
            "../../../integrations/klipper/fixtures/k2.json"
        ))
        .unwrap()
    }

    #[test]
    fn the_k2_is_known_by_its_probe() {
        let objects: Vec<String> = serde_json::from_value(k2()["objects"].clone()).unwrap();
        let shape = Shape::of(&objects);
        assert!(shape.creality && shape.bed && !shape.filaments);
        assert_eq!(shape.tools, ["extruder"]);
        assert_eq!(
            shape.chamber.as_deref(),
            Some("temperature_sensor chamber_temp")
        );
        assert_eq!(shape.light, None);
        assert!(!self::shape().creality);
        assert_eq!(creality_machine("K2-0000"), "Creality K2");
        assert_eq!(creality_machine("K1C-0000"), "Creality K1C");
        assert_eq!(creality_machine("atelier"), "Creality");
    }

    #[test]
    fn a_k2_print_reads_as_its_screen() {
        // Creality's screen, at that moment: 44 %, layer 353 / 1092, about
        // 3 h 12 left (its slicer's 6 h 13 m 43 s less the 3 h elapsed).
        let f = k2();
        let objects: Vec<String> = serde_json::from_value(f["objects"].clone()).unwrap();
        let shape = Shape::of(&objects);
        let job = Job::from_metadata("Exemple_PLA_6h13m43s.gcode", &f["metadata"]["result"]);
        assert_eq!((job.estimated, job.thumbnail.as_deref()), (None, None));
        let v = values(&shape, &f["query"]["result"]["status"], Some(&job));
        assert_eq!(get(&v, "state"), &Value::from("printing"));
        assert_eq!(get(&v, "progress"), &Value::Float(44.0));
        assert_eq!(get(&v, "layer"), &Value::Float(353.0));
        assert_eq!(get(&v, "total_layers"), &Value::Float(1092.0));
        assert_eq!(get(&v, "print_duration"), &Value::Float(181.0));
        // Slicer 193 min left, file 230: blended, as for any Klipper printer.
        assert_eq!(get(&v, "remaining"), &Value::Float(212.0));
    }

    /// The Snapmaker U1 reads exactly as before Creality was known: every
    /// point, every value, finished or under way (taken from `main` before
    /// the change).
    #[test]
    fn the_u1_reads_exactly_as_before() {
        let f = fixture();
        let shape = shape();
        let keys: Vec<String> = shape.points().iter().map(|p| p.key.to_string()).collect();
        let job = Job::from_metadata("Calibration_cube_PLA_8h16m.gcode", &f["metadata"]["result"]);
        let done = values(&shape, &f["query"]["result"]["status"], Some(&job));
        let mut status = f["query"]["result"]["status"].clone();
        status["print_stats"]["state"] = Json::from("printing");
        status["virtual_sdcard"]["progress"] = Json::from(0.25);
        status["display_status"]["progress"] = Json::from(0.3);
        let printing = values(&shape, &status, Some(&job));
        let before = |text: &str| serde_json::from_str::<Json>(text).unwrap();
        assert_eq!(
            serde_json::to_value(&keys).unwrap(),
            before(
                r#"["state","printing","progress","remaining","job","layer","total_layers","print_duration","filament_used","nozzle_temperature","nozzle_target","active_tool","tool1_temperature","tool1_target","tool1_filament","tool1_color","tool2_temperature","tool2_target","tool2_filament","tool2_color","tool3_temperature","tool3_target","tool3_filament","tool3_color","tool4_temperature","tool4_target","tool4_filament","tool4_color","bed_temperature","bed_target","chamber_temperature","message","light","control","jobs_total","print_hours_total","filament_total"]"#
            )
        );
        assert_eq!(
            serde_json::to_value(&done).unwrap(),
            before(
                r##"[["state","complete"],["printing",false],["progress",100.0],["remaining",0.0],["job","Calibration_cube_PLA_8h16m"],["layer",1002.0],["total_layers",1002.0],["print_duration",462.0],["filament_used",29.1],["nozzle_temperature",30.0],["nozzle_target",0.0],["active_tool",4.0],["tool1_temperature",30.0],["tool1_target",0.0],["tool1_filament","PLA"],["tool1_color","#6c5bb1"],["tool2_temperature",30.0],["tool2_target",0.0],["tool2_filament","PLA"],["tool2_color","#8bd5ee"],["tool3_temperature",30.0],["tool3_target",0.0],["tool3_filament","PLA Basic · Snapmaker"],["tool3_color","#ffffff"],["tool4_temperature",30.0],["tool4_target",0.0],["tool4_filament","PLA"],["tool4_color","#000000"],["bed_temperature",28.0],["bed_target",0.0],["chamber_temperature",30.0],["message",""],["light",false]]"##
            )
        );
        assert_eq!(
            serde_json::to_value(&printing).unwrap(),
            before(
                r##"[["state","printing"],["printing",true],["progress",30.0],["remaining",556.0],["job","Calibration_cube_PLA_8h16m"],["layer",1002.0],["total_layers",1002.0],["print_duration",462.0],["filament_used",29.1],["nozzle_temperature",30.0],["nozzle_target",0.0],["active_tool",4.0],["tool1_temperature",30.0],["tool1_target",0.0],["tool1_filament","PLA"],["tool1_color","#6c5bb1"],["tool2_temperature",30.0],["tool2_target",0.0],["tool2_filament","PLA"],["tool2_color","#8bd5ee"],["tool3_temperature",30.0],["tool3_target",0.0],["tool3_filament","PLA Basic · Snapmaker"],["tool3_color","#ffffff"],["tool4_temperature",30.0],["tool4_target",0.0],["tool4_filament","PLA"],["tool4_color","#000000"],["bed_temperature",28.0],["bed_target",0.0],["chamber_temperature",30.0],["message",""],["light",false]]"##
            )
        );
    }

    /// A Creality value that does not hold is left aside, and the usual
    /// reading stands: no layer 0 of 0, no progress above 100 %.
    #[test]
    fn creality_values_that_do_not_hold_fall_back() {
        let f = k2();
        let objects: Vec<String> = serde_json::from_value(f["objects"].clone()).unwrap();
        let shape = Shape::of(&objects);
        let mut status = f["query"]["result"]["status"].clone();
        status["virtual_sdcard"]["layer"] = Json::from(0);
        status["virtual_sdcard"]["layer_count"] = Json::from(0);
        status["virtual_sdcard"]["progress"] = Json::from(2.0);
        let v = values(&shape, &status, None);
        assert!(v.iter().all(|(k, _)| k != "layer" && k != "total_layers"));
        // Its own progress gone wrong: the display's, as for any Klipper.
        assert_eq!(get(&v, "progress"), &Value::Float(19.0));
        // A layer past the total does not hold either.
        status["virtual_sdcard"]["layer"] = Json::from(1200);
        status["virtual_sdcard"]["layer_count"] = Json::from(1092);
        let v = values(&shape, &status, None);
        assert!(v.iter().all(|(k, _)| k != "layer"));
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
    fn a_colour_cut_inside_a_letter_is_no_colour() {
        // The 6th byte of `12345é00` falls inside the `é` (UTF-8).
        let table = serde_json::json!({
            "filament_type": ["PLA", "PETG"],
            "filament_color_rgba": ["12345é00", "6C5BB1FF"]
        });
        assert_eq!(filament(&table, 0), Some(("PLA".into(), String::new())));
        assert_eq!(filament(&table, 1), Some(("PETG".into(), "#6c5bb1".into())));
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
