//! What Proxmox VE says, as Moli devices and points.
//!
//! Every field is optional: a version that leaves one out loses that point
//! (its value goes `Null`), never the device. A value Moli does not know is
//! never guessed.

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde_json::Value as Json;

pub(crate) type States = Vec<(&'static str, Value)>;

/// A device with the values to publish for it.
pub(crate) struct Entity {
    pub device: Device,
    pub states: States,
}

/// The node itself: always published, even when it does not answer.
pub(crate) const NODE: &str = "node";
pub(crate) const BACKUP: &str = "backup";

/// What the house reads as « fine » for a disk's SMART health.
const HEALTH_FINE: [&str; 2] = ["passed", "ok"];
/// A disk with less life left than this (Proxmox's `wearout`, 100 = new) is
/// worth replacing soon.
const WEAROUT_LOW: f64 = 10.0;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// The thresholds a house may change (`moli.toml`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// Memory, disk or storage use above this, in percent: a problem.
    pub full: f64,
    /// A virtual machine's memory above this: a problem (none by default).
    pub vm_full: Option<f64>,
    /// No successful backup for this long, in hours: a problem.
    pub backup_hours: f64,
}

fn num(value: &Json) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|n| n.is_finite())
}

/// The longest name or word taken from Proxmox.
const MAX_TEXT: usize = 80;

/// The most entries taken from one list (containers, virtual machines,
/// storages, disks, tasks): a node that answers with more is cut short,
/// never trusted to stay small.
pub(crate) const MAX_ENTRIES: usize = 256;

/// The list in an answer, at most [`MAX_ENTRIES`] long (none if it is not
/// a list).
pub(crate) fn entries(value: &Json) -> &[Json] {
    let list = value.as_array().map_or(&[][..], Vec::as_slice);
    &list[..list.len().min(MAX_ENTRIES)]
}

/// A word from Proxmox (a name, a status): too long or holding a control
/// character, it is not taken, since it ends up in identifiers and messages.
fn text(value: &Json) -> Option<&str> {
    value.as_str().map(str::trim).filter(|s| {
        !s.is_empty() && s.chars().count() <= MAX_TEXT && !s.chars().any(char::is_control)
    })
}

/// A guest's id (`vmid`), a positive whole number.
fn vmid_of(value: &Json) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|n| *n >= 1)
}

fn round1(n: f64) -> f64 {
    (n * 10.0).round() / 10.0
}

fn float(n: Option<f64>) -> Value {
    n.map_or(Value::Null, |n| Value::Float(round1(n)))
}

fn word(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |s| Value::Text(s.into()))
}

/// `used` out of `total`, in percent (none when the total is unknown or 0).
fn share(used: Option<f64>, total: Option<f64>) -> Option<f64> {
    match (used, total) {
        (Some(used), Some(total)) if total > 0.0 => Some((used / total * 100.0).clamp(0.0, 100.0)),
        _ => None,
    }
}

fn spec(key: &str, label: String, kind: Kind, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
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

fn percent(key: &str, label: String) -> PointSpec {
    let kind = Kind::Numeric {
        min: Some(0.0),
        max: Some(100.0),
        step: None,
    };
    spec(key, label, kind, Some(Unit::Percent), Semantic::Other)
}

fn reading(key: &str, label: String, unit: Unit) -> PointSpec {
    let kind = Kind::Numeric {
        min: None,
        max: None,
        step: None,
    };
    spec(key, label, kind, Some(unit), Semantic::Other)
}

fn gib(key: &str, label: String) -> PointSpec {
    reading(key, label, Unit::Other("Gio".into()))
}

fn words(key: &str, label: String) -> PointSpec {
    spec(key, label, Kind::Text, None, Semantic::Other)
}

fn flag(key: &str, label: String) -> PointSpec {
    spec(key, label, Kind::Binary, None, Semantic::Other)
}

/// The two points every part with something to watch carries: a flag (the
/// dashboard's « Parc » page lists what is wrong) and its words.
fn alert_points() -> [PointSpec; 2] {
    [
        spec(
            "problem",
            moli_i18n::tr!("pilotes.proxmox.probleme"),
            Kind::Binary,
            None,
            Semantic::Alert,
        ),
        words("detail", moli_i18n::tr!("pilotes.proxmox.detail")),
    ]
}

fn alert(reasons: &[String]) -> States {
    vec![
        ("problem", Value::Bool(!reasons.is_empty())),
        ("detail", Value::Text(reasons.join(", ").into())),
    ]
}

/// A use above the threshold, said in words.
fn too_full(what: &str, share: Option<f64>, limits: Limits) -> Option<String> {
    above(what, share, limits.full)
}

fn above(what: &str, share: Option<f64>, limit: f64) -> Option<String> {
    let share = share.filter(|s| *s > limit)?;
    Some(moli_i18n::tr!(
        "pilotes.proxmox.detail_plein",
        quoi = what,
        pct = format!("{share:.0}")
    ))
}

fn device(
    instance: &InstanceId,
    native: &str,
    name: String,
    model: Option<&str>,
    points: Vec<PointSpec>,
) -> Device {
    Device {
        id: DeviceId::new(instance, native),
        instance: instance.clone(),
        native_name: name.into(),
        manufacturer: Some("Proxmox".into()),
        model: model.map(Into::into),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

/// The node. `version`: Proxmox's own (`pve-manager/9.2.31/…`, its number kept).
pub(crate) fn node_device(instance: &InstanceId, node: &str, version: Option<&str>) -> Device {
    let mut points = vec![
        flag("up", moli_i18n::tr!("pilotes.proxmox.en_marche")),
        percent("cpu", moli_i18n::tr!("pilotes.proxmox.processeur")),
        percent("memory", moli_i18n::tr!("pilotes.proxmox.memoire")),
        gib(
            "memory_used",
            moli_i18n::tr!("pilotes.proxmox.memoire_utilisee"),
        ),
        gib(
            "memory_total",
            moli_i18n::tr!("pilotes.proxmox.memoire_totale"),
        ),
        percent("root", moli_i18n::tr!("pilotes.proxmox.disque_systeme")),
        reading(
            "uptime",
            moli_i18n::tr!("pilotes.proxmox.allume_depuis"),
            Unit::Hour,
        ),
        reading(
            "updates",
            moli_i18n::tr!("pilotes.proxmox.mises_a_jour"),
            Unit::Other("".into()),
        ),
        words("version", moli_i18n::tr!("pilotes.proxmox.version")),
    ];
    points.extend(alert_points());
    let model = version.map(|v| format!("Proxmox VE {v}"));
    device(
        instance,
        NODE,
        moli_i18n::tr!("pilotes.proxmox.noeud", node = node),
        model.as_deref(),
        points,
    )
}

/// `pve-manager/9.2.31/30a00e…` → `9.2.31`.
pub(crate) fn version(status: &Json) -> Option<&str> {
    let full = text(&status["pveversion"])?;
    full.split('/').nth(1).filter(|v| !v.is_empty())
}

/// The node's values from `/nodes/{node}/status`; `updates`: the length of
/// `/apt/update` when it could be read.
pub(crate) fn node_states(status: &Json, updates: Option<usize>, limits: Limits) -> States {
    let memory = share(
        num(&status["memory"]["used"]),
        num(&status["memory"]["total"]),
    );
    let root = share(
        num(&status["rootfs"]["used"]),
        num(&status["rootfs"]["total"]),
    );
    let mut reasons = Vec::new();
    reasons.extend(too_full(
        &moli_i18n::tr!("pilotes.proxmox.memoire"),
        memory,
        limits,
    ));
    reasons.extend(too_full(
        &moli_i18n::tr!("pilotes.proxmox.disque_systeme"),
        root,
        limits,
    ));
    let mut values: States = vec![
        ("up", Value::Bool(true)),
        (
            "cpu",
            float(num(&status["cpu"]).map(|c| (c * 100.0).clamp(0.0, 100.0))),
        ),
        ("memory", float(memory)),
        (
            "memory_used",
            float(num(&status["memory"]["used"]).map(|b| b / GIB)),
        ),
        (
            "memory_total",
            float(num(&status["memory"]["total"]).map(|b| b / GIB)),
        ),
        ("root", float(root)),
        ("uptime", float(num(&status["uptime"]).map(|s| s / 3600.0))),
        (
            "updates",
            updates.map_or(Value::Null, |n| {
                Value::Float(f64::from(u32::try_from(n).unwrap_or(u32::MAX)))
            }),
        ),
        ("version", word(version(status))),
    ];
    values.extend(alert(&reasons));
    values
}

/// The node does not answer: nothing is known, except that.
pub(crate) fn node_states_down() -> States {
    let mut values: States = vec![("up", Value::Bool(false))];
    for key in [
        "cpu",
        "memory",
        "memory_used",
        "memory_total",
        "root",
        "uptime",
        "updates",
        "version",
    ] {
        values.push((key, Value::Null));
    }
    values.push(("problem", Value::Bool(true)));
    values.push((
        "detail",
        Value::Text(moli_i18n::tr!("pilotes.proxmox.detail_injoignable").into()),
    ));
    values
}

/// A guest (container or virtual machine) from `/nodes/{node}/lxc` or `/qemu`.
/// `kind`: `lxc` or `qemu`. `starts_at_boot`: whether its configuration says
/// `onboot` (read only for a stopped guest; `None` when unknown).
pub(crate) fn guest(
    instance: &InstanceId,
    kind: &str,
    guest: &Json,
    starts_at_boot: Option<bool>,
    limits: Limits,
) -> Option<Entity> {
    let vmid = vmid_of(&guest["vmid"])?;
    let name = text(&guest["name"]).map_or_else(|| format!("{kind} {vmid}"), str::to_owned);
    let running = text(&guest["status"]).map(|s| s == "running");
    let memory = share(num(&guest["mem"]), num(&guest["maxmem"]));
    // A virtual machine's disk use is not known from outside (Proxmox says 0).
    let disk = if kind == "lxc" {
        share(num(&guest["disk"]), num(&guest["maxdisk"]))
    } else {
        None
    };
    let mut reasons = Vec::new();
    if running == Some(false) && starts_at_boot == Some(true) {
        reasons.push(moli_i18n::tr!("pilotes.proxmox.detail_arrete"));
    }
    // A virtual machine's `mem` counts the guest's disk cache as used: its
    // own threshold, none by default (a container's figures are right).
    let memory_limit = if kind == "lxc" {
        Some(limits.full)
    } else {
        limits.vm_full
    };
    if let (Some(true), Some(limit)) = (running, memory_limit) {
        reasons.extend(above(
            &moli_i18n::tr!("pilotes.proxmox.memoire"),
            memory,
            limit,
        ));
    }
    reasons.extend(too_full(
        &moli_i18n::tr!("pilotes.proxmox.disque"),
        disk,
        limits,
    ));
    let mut points = vec![
        flag("running", moli_i18n::tr!("pilotes.proxmox.en_marche")),
        percent("cpu", moli_i18n::tr!("pilotes.proxmox.processeur")),
        percent("memory", moli_i18n::tr!("pilotes.proxmox.memoire")),
        gib(
            "memory_used",
            moli_i18n::tr!("pilotes.proxmox.memoire_utilisee"),
        ),
        gib(
            "memory_total",
            moli_i18n::tr!("pilotes.proxmox.memoire_totale"),
        ),
        reading(
            "uptime",
            moli_i18n::tr!("pilotes.proxmox.allume_depuis"),
            Unit::Hour,
        ),
    ];
    if kind == "lxc" {
        points.push(percent("disk", moli_i18n::tr!("pilotes.proxmox.disque")));
    }
    points.extend(alert_points());
    // Proxmox gives the share of the guest's own processors: shown as it is.
    let cpu = num(&guest["cpu"]).map(|c| (c * 100.0).clamp(0.0, 100.0));
    let mut values: States = vec![
        ("running", running.map_or(Value::Null, Value::Bool)),
        ("cpu", float(cpu.filter(|_| running == Some(true)))),
        ("memory", float(memory.filter(|_| running == Some(true)))),
        (
            "memory_used",
            float(
                num(&guest["mem"])
                    .filter(|_| running == Some(true))
                    .map(|b| b / GIB),
            ),
        ),
        (
            "memory_total",
            float(num(&guest["maxmem"]).map(|b| b / GIB)),
        ),
        (
            "uptime",
            float(
                num(&guest["uptime"])
                    .filter(|_| running == Some(true))
                    .map(|s| s / 3600.0),
            ),
        ),
    ];
    if kind == "lxc" {
        values.push(("disk", float(disk)));
    }
    values.extend(alert(&reasons));
    let model = if kind == "lxc" {
        moli_i18n::tr!("pilotes.proxmox.conteneur", vmid = vmid)
    } else {
        moli_i18n::tr!("pilotes.proxmox.machine_virtuelle", vmid = vmid)
    };
    Some(Entity {
        device: device(
            instance,
            &format!("{kind}_{vmid}"),
            name,
            Some(&model),
            points,
        ),
        states: values,
    })
}

/// The guests' ids that are stopped: their configuration says whether they
/// should run (`onboot`).
pub(crate) fn stopped(guests: &Json) -> Vec<u64> {
    entries(guests)
        .iter()
        .filter(|g| text(&g["status"]) == Some("stopped"))
        .filter_map(|g| vmid_of(&g["vmid"]))
        .collect()
}

/// `onboot` in a guest's configuration (`1` = starts with the node).
pub(crate) fn starts_at_boot(config: &Json) -> bool {
    num(&config["onboot"]) == Some(1.0)
}

/// The storages from `/nodes/{node}/storage`, those enabled and active.
pub(crate) fn storages(instance: &InstanceId, list: &Json, limits: Limits) -> Vec<Entity> {
    entries(list)
        .iter()
        .filter(|s| num(&s["active"]) == Some(1.0) && num(&s["enabled"]) != Some(0.0))
        .filter_map(|s| {
            let id = text(&s["storage"])?;
            if !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            {
                return None;
            }
            let used = share(num(&s["used"]), num(&s["total"]));
            let reasons: Vec<String> = too_full(id, used, limits).into_iter().collect();
            let mut points = vec![
                percent("used", moli_i18n::tr!("pilotes.proxmox.occupation")),
                gib("free", moli_i18n::tr!("pilotes.proxmox.libre")),
                gib("total", moli_i18n::tr!("pilotes.proxmox.taille")),
                words("content", moli_i18n::tr!("pilotes.proxmox.contenu")),
            ];
            points.extend(alert_points());
            let mut values: States = vec![
                ("used", float(used)),
                ("free", float(num(&s["avail"]).map(|b| b / GIB))),
                ("total", float(num(&s["total"]).map(|b| b / GIB))),
                ("content", word(text(&s["content"]))),
            ];
            values.extend(alert(&reasons));
            Some(Entity {
                device: device(
                    instance,
                    &format!("storage_{id}"),
                    moli_i18n::tr!("pilotes.proxmox.stockage", id = id),
                    text(&s["type"]),
                    points,
                ),
                states: values,
            })
        })
        .collect()
}

/// The last backup, from `/nodes/{node}/tasks?typefilter=vzdump` (newest
/// first). `now`: seconds since the epoch.
pub(crate) fn backup(instance: &InstanceId, tasks: &Json, now: f64, limits: Limits) -> Entity {
    let list: Vec<&Json> = entries(tasks)
        .iter()
        .filter(|t| text(&t["type"]) == Some("vzdump"))
        .collect();
    // A backup still running has no status yet: the one before tells.
    let latest = list.iter().find(|t| text(&t["status"]).is_some());
    let running = list
        .iter()
        .any(|t| text(&t["status"]).is_none() && num(&t["endtime"]).is_none());
    let success = list.iter().find(|t| text(&t["status"]) == Some("OK"));
    let result = latest.and_then(|t| text(&t["status"]));
    let started = latest.and_then(|t| num(&t["starttime"]));
    let ended = latest.and_then(|t| num(&t["endtime"]));
    let ok_age = success
        .and_then(|t| num(&t["endtime"]).or_else(|| num(&t["starttime"])))
        .map(|at| ((now - at) / 3600.0).max(0.0));
    let mut reasons = Vec::new();
    if let Some(result) = result.filter(|r| *r != "OK") {
        reasons.push(moli_i18n::tr!(
            "pilotes.proxmox.detail_sauvegarde",
            result = result
        ));
    }
    if ok_age.is_none_or(|h| h > limits.backup_hours) && !running {
        reasons.push(moli_i18n::tr!(
            "pilotes.proxmox.detail_sauvegarde_vieille",
            heures = format!("{:.0}", limits.backup_hours)
        ));
    }
    let mut points = vec![
        words(
            "last_result",
            moli_i18n::tr!("pilotes.proxmox.dernier_resultat"),
        ),
        words(
            "last_run",
            moli_i18n::tr!("pilotes.proxmox.derniere_execution"),
        ),
        reading(
            "duration",
            moli_i18n::tr!("pilotes.proxmox.duree"),
            Unit::Minute,
        ),
        reading(
            "age",
            moli_i18n::tr!("pilotes.proxmox.depuis_la_derniere"),
            Unit::Hour,
        ),
        flag("running", moli_i18n::tr!("pilotes.proxmox.en_cours")),
    ];
    points.extend(alert_points());
    let mut values: States = vec![
        ("last_result", word(result)),
        (
            "last_run",
            started.map_or(Value::Null, |s| Value::Text(rfc3339(s).into())),
        ),
        (
            "duration",
            float(match (started, ended) {
                (Some(s), Some(e)) if e >= s => Some((e - s) / 60.0),
                _ => None,
            }),
        ),
        ("age", float(ok_age)),
        ("running", Value::Bool(running)),
    ];
    values.extend(alert(&reasons));
    Entity {
        device: device(
            instance,
            BACKUP,
            moli_i18n::tr!("pilotes.proxmox.sauvegardes"),
            None,
            points,
        ),
        states: values,
    }
}

/// Seconds since the epoch → `2026-11-20T02:30:01Z`.
#[allow(clippy::cast_possible_truncation)] // a date: whole seconds, far below 2^63
fn rfc3339(seconds: f64) -> String {
    let secs = seconds.max(0.0).floor() as i64;
    let (days, rest) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from the day count (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The physical disks from `/nodes/{node}/disks/list`.
pub(crate) fn disks(instance: &InstanceId, list: &Json) -> Vec<Entity> {
    entries(list)
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let health = text(&d["health"]);
            let wearout = num(&d["wearout"]).filter(|w| (0.0..=100.0).contains(w));
            let mut reasons = Vec::new();
            if let Some(health) =
                health.filter(|h| !HEALTH_FINE.contains(&h.to_ascii_lowercase().as_str()))
            {
                reasons.push(moli_i18n::tr!(
                    "pilotes.proxmox.detail_smart",
                    health = health
                ));
            }
            if wearout.is_some_and(|w| w < WEAROUT_LOW) {
                reasons.push(moli_i18n::tr!(
                    "pilotes.proxmox.detail_usure",
                    pct = format!("{:.0}", wearout.unwrap_or_default())
                ));
            }
            let mut points = vec![
                words("health", moli_i18n::tr!("pilotes.proxmox.sante")),
                percent("life", moli_i18n::tr!("pilotes.proxmox.vie_restante")),
                gib("size", moli_i18n::tr!("pilotes.proxmox.taille")),
                words("type", moli_i18n::tr!("pilotes.proxmox.type")),
            ];
            points.extend(alert_points());
            let mut values: States = vec![
                ("health", word(health)),
                ("life", float(wearout)),
                ("size", float(num(&d["size"]).map(|b| b / GIB))),
                ("type", word(text(&d["type"]))),
            ];
            values.extend(alert(&reasons));
            // The disk's place in the list: its serial number stays out of
            // identifiers and names (it is personal enough to be left out).
            Entity {
                device: device(
                    instance,
                    &format!("disk_{i}"),
                    moli_i18n::tr!("pilotes.proxmox.disque_n", n = i + 1),
                    text(&d["model"]),
                    points,
                ),
                states: values,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(text: &str) -> Json {
        serde_json::from_str::<Json>(text).unwrap()["data"].clone()
    }

    const LIMITS: Limits = Limits {
        full: 90.0,
        vm_full: None,
        backup_hours: 26.0,
    };

    fn value<'a>(states: &'a States, key: &str) -> &'a Value {
        &states.iter().find(|(k, _)| *k == key).unwrap().1
    }

    fn instance() -> InstanceId {
        InstanceId::from("pve")
    }

    #[test]
    fn the_node_reads_from_its_status() {
        let answer = fixture(include_str!(
            "../../../integrations/proxmox/fixtures/status.json"
        ));
        assert_eq!(version(&answer), Some("9.2.31"));
        let states = node_states(&answer, Some(0), LIMITS);
        assert_eq!(value(&states, "up"), &Value::Bool(true));
        assert_eq!(value(&states, "updates"), &Value::Float(0.0));
        assert_eq!(value(&states, "version"), &Value::Text("9.2.31".into()));
        let Value::Float(memory) = value(&states, "memory") else {
            panic!("memory")
        };
        assert!((0.0..=100.0).contains(memory));
        assert_eq!(value(&states, "problem"), &Value::Bool(false));
        let down = node_states_down();
        assert_eq!(value(&down, "up"), &Value::Bool(false));
        assert_eq!(value(&down, "memory"), &Value::Null);
        assert_eq!(value(&down, "problem"), &Value::Bool(true));
    }

    #[test]
    fn running_containers_are_fine() {
        let lxc = fixture(include_str!(
            "../../../integrations/proxmox/fixtures/lxc.json"
        ));
        let all: Vec<Entity> = lxc
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|g| guest(&instance(), "lxc", g, None, LIMITS))
            .collect();
        assert_eq!(all.len(), 6);
        assert_eq!(&*all[0].device.id.to_string(), "pve:lxc_100");
        assert_eq!(&*all[0].device.native_name, "medias");
        for e in &all {
            assert_eq!(
                value(&e.states, "running"),
                &Value::Bool(true),
                "{}",
                e.device.native_name
            );
            assert_eq!(
                value(&e.states, "problem"),
                &Value::Bool(false),
                "{}",
                e.device.native_name
            );
        }
        assert!(stopped(&lxc).is_empty());
    }

    #[test]
    fn a_stopped_container_that_should_run_and_a_full_one_are_problems() {
        let lxc = fixture(include_str!(
            "../../../integrations/proxmox/fixtures/lxc-faulty.json"
        ));
        assert_eq!(stopped(&lxc), vec![101]);
        let config = fixture(include_str!(
            "../../../integrations/proxmox/fixtures/lxc-config.json"
        ));
        assert!(starts_at_boot(&config));
        let find = |vmid: u64| {
            lxc.as_array()
                .unwrap()
                .iter()
                .find(|g| g["vmid"] == vmid)
                .unwrap()
                .clone()
        };
        let wifi = guest(&instance(), "lxc", &find(101), Some(true), LIMITS).unwrap();
        assert_eq!(value(&wifi.states, "running"), &Value::Bool(false));
        assert_eq!(
            value(&wifi.states, "memory"),
            &Value::Null,
            "a stopped guest uses nothing"
        );
        assert_eq!(value(&wifi.states, "problem"), &Value::Bool(true));
        // Stopped on purpose (no `onboot`): nothing to say.
        let resting = guest(&instance(), "lxc", &find(101), Some(false), LIMITS).unwrap();
        assert_eq!(value(&resting.states, "problem"), &Value::Bool(false));
        let proxy = guest(&instance(), "lxc", &find(103), None, LIMITS).unwrap();
        assert_eq!(value(&proxy.states, "problem"), &Value::Bool(true));
        let Value::Text(detail) = value(&proxy.states, "detail") else {
            panic!("detail")
        };
        assert!(detail.contains("95"), "{detail}");
    }

    #[test]
    fn a_virtual_machine_has_no_disk_use() {
        let vm = serde_json::json!({"vmid": 200, "name": "vm", "status": "running", "mem": 1, "maxmem": 4, "disk": 0, "maxdisk": 10});
        let e = guest(&instance(), "qemu", &vm, None, LIMITS).unwrap();
        assert_eq!(&*e.device.id.to_string(), "pve:qemu_200");
        assert!(e.states.iter().all(|(k, _)| *k != "disk"));
        assert!(
            guest(
                &instance(),
                "qemu",
                &serde_json::json!({"vmid": "x"}),
                None,
                LIMITS
            )
            .is_none()
        );
    }

    #[test]
    fn a_virtual_machine_full_of_cache_is_no_problem_unless_asked() {
        // A Home Assistant VM as Proxmox sees it: 94 %, mostly the guest's cache.
        let vm = serde_json::json!({"vmid": 201, "name": "ha", "status": "running", "mem": 4_041_060_352_u64, "maxmem": 4_294_967_296_u64});
        let quiet = guest(&instance(), "qemu", &vm, None, LIMITS).unwrap();
        assert_eq!(value(&quiet.states, "problem"), &Value::Bool(false));
        assert!(matches!(value(&quiet.states, "memory"), Value::Float(m) if *m > 93.0));
        let asked = Limits {
            vm_full: Some(90.0),
            ..LIMITS
        };
        let loud = guest(&instance(), "qemu", &vm, None, asked).unwrap();
        assert_eq!(value(&loud.states, "problem"), &Value::Bool(true));
        // A container's memory keeps the common threshold.
        let ct = serde_json::json!({"vmid": 101, "name": "ct", "status": "running", "mem": 98, "maxmem": 100, "disk": 1, "maxdisk": 10});
        let full = guest(&instance(), "lxc", &ct, None, LIMITS).unwrap();
        assert_eq!(value(&full.states, "problem"), &Value::Bool(true));
    }

    #[test]
    fn a_backup_running_is_said_and_the_one_before_tells() {
        let tasks = fixture(include_str!(
            "../../../integrations/proxmox/fixtures/tasks-running.json"
        ));
        let start = num(&tasks[0]["starttime"]).unwrap();
        let e = backup(&instance(), &tasks, start + 600.0, LIMITS);
        assert_eq!(value(&e.states, "running"), &Value::Bool(true));
        assert_eq!(value(&e.states, "last_result"), &Value::Text("OK".into()));
        assert_eq!(value(&e.states, "problem"), &Value::Bool(false));
    }

    #[test]
    fn storages_read_and_a_full_one_is_a_problem() {
        let fine = storages(
            &instance(),
            &fixture(include_str!(
                "../../../integrations/proxmox/fixtures/storage.json"
            )),
            LIMITS,
        );
        assert_eq!(fine.len(), 3);
        assert!(
            fine.iter()
                .all(|e| value(&e.states, "problem") == &Value::Bool(false))
        );
        let full = storages(
            &instance(),
            &fixture(include_str!(
                "../../../integrations/proxmox/fixtures/storage-faulty.json"
            )),
            LIMITS,
        );
        let lvm = full
            .iter()
            .find(|e| &*e.device.id.to_string() == "pve:storage_local-lvm")
            .unwrap();
        assert_eq!(value(&lvm.states, "problem"), &Value::Bool(true));
    }

    #[test]
    fn the_last_backup_and_its_failures() {
        let tasks = fixture(include_str!(
            "../../../integrations/proxmox/fixtures/tasks.json"
        ));
        let end = num(&tasks[0]["endtime"]).unwrap();
        let fresh = backup(&instance(), &tasks, end + 3600.0, LIMITS);
        assert_eq!(
            value(&fresh.states, "last_result"),
            &Value::Text("OK".into())
        );
        assert_eq!(value(&fresh.states, "age"), &Value::Float(1.0));
        assert_eq!(value(&fresh.states, "problem"), &Value::Bool(false));
        let Value::Text(when) = value(&fresh.states, "last_run") else {
            panic!("last_run")
        };
        assert!(when.ends_with('Z') && when.len() == 20, "{when}");
        // Two days without a backup.
        let old = backup(&instance(), &tasks, end + 48.0 * 3600.0, LIMITS);
        assert_eq!(value(&old.states, "problem"), &Value::Bool(true));
        let failed = backup(
            &instance(),
            &fixture(include_str!(
                "../../../integrations/proxmox/fixtures/tasks-faulty.json"
            )),
            end + 3600.0,
            LIMITS,
        );
        assert_eq!(value(&failed.states, "problem"), &Value::Bool(true));
        // Never any backup: a problem too.
        let none = backup(&instance(), &serde_json::json!([]), end, LIMITS);
        assert_eq!(value(&none.states, "problem"), &Value::Bool(true));
    }

    #[test]
    fn disks_by_place_never_by_serial() {
        let fine = disks(
            &instance(),
            &fixture(include_str!(
                "../../../integrations/proxmox/fixtures/disks.json"
            )),
        );
        assert_eq!(fine.len(), 1);
        assert_eq!(&*fine[0].device.id.to_string(), "pve:disk_0");
        assert!(!fine[0].device.native_name.contains("SERIAL"));
        assert_eq!(value(&fine[0].states, "problem"), &Value::Bool(false));
        assert_eq!(value(&fine[0].states, "life"), &Value::Float(100.0));
        let bad = disks(
            &instance(),
            &fixture(include_str!(
                "../../../integrations/proxmox/fixtures/disks-faulty.json"
            )),
        );
        assert_eq!(value(&bad[0].states, "problem"), &Value::Bool(true));
    }

    #[test]
    fn dates_are_utc() {
        assert_eq!(rfc3339(0.0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_795_141_801.0), "2026-11-20T02:30:01Z");
        assert_eq!(rfc3339(951_782_400.0), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn a_long_list_is_cut_short() {
        let many: Vec<Json> = (1..=1000)
            .map(|n| serde_json::json!({"vmid": n, "status": "stopped", "name": format!("ct{n}")}))
            .collect();
        let many = Json::Array(many);
        assert_eq!(entries(&many).len(), MAX_ENTRIES);
        assert_eq!(stopped(&many).len(), MAX_ENTRIES);
        let disks = Json::Array(vec![serde_json::json!({"health": "PASSED"}); 1000]);
        assert_eq!(super::disks(&instance(), &disks).len(), MAX_ENTRIES);
        assert!(entries(&serde_json::json!({"not": "a list"})).is_empty());
        assert!(entries(&Json::Null).is_empty());
    }
}
