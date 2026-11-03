//! Optional resources. Failure of one section never grants stale capabilities.
use crate::{
    Config,
    api::{self, ReadMethod, ReadRequest},
    fields::{self, Scope},
    model::{Entity, Snapshot},
};
use anyhow::{Context as _, ensure};
use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Value};
use serde_json::{Value as Json, json};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

const MAX_RECORDS: usize = 32;
const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Default)]
struct SnapshotBudget {
    used: usize,
}
impl SnapshotBudget {
    fn reserve(&mut self, values: &[Json]) -> bool {
        let mut counter = ByteCounter { used: self.used };
        if serde_json::to_writer(&mut counter, values).is_err() {
            return false;
        }
        self.used = counter.used;
        true
    }
}

struct ByteCounter {
    used: usize,
}
impl std::io::Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.used = self
            .used
            .checked_add(bytes.len())
            .filter(|total| *total <= MAX_SNAPSHOT_BYTES)
            .ok_or_else(|| std::io::Error::other("Airzone snapshot limit"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct ResourceSnapshot {
    webserver: Json,
    version: Option<Json>,
    integration: Option<Json>,
    systems: Vec<Json>,
    dhw: Option<Json>,
    iaq: Vec<Json>,
    read_only: bool,
    systems_available: bool,
    iaq_available: bool,
    failures: u32,
    unmapped_systems: BTreeSet<u64>,
    incomplete_iaq: BTreeSet<u64>,
    budget: SnapshotBudget,
}

impl Default for ResourceSnapshot {
    fn default() -> Self {
        Self {
            webserver: json!({}),
            version: None,
            integration: None,
            systems: Vec::new(),
            dhw: None,
            iaq: Vec::new(),
            read_only: true,
            systems_available: false,
            iaq_available: false,
            failures: 0,
            unmapped_systems: BTreeSet::new(),
            incomplete_iaq: BTreeSet::new(),
            budget: SnapshotBudget::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    System,
    Dhw,
    Iaq(u64),
}

fn invalid() -> String {
    moli_i18n::tr!("airzone.invalid")
}

fn id(value: &Json, allow_zero: bool) -> anyhow::Result<u64> {
    value
        .as_u64()
        .filter(|v| (u64::from(!allow_zero)..=32).contains(v))
        .context(invalid())
}

fn append_rows<'a>(value: &'a Json, rows: &mut Vec<&'a Json>) -> anyhow::Result<()> {
    match value {
        Json::Object(_) => rows.push(value),
        Json::Array(values) if values.len() <= MAX_RECORDS => {
            ensure!(values.iter().all(Json::is_object), invalid());
            rows.extend(values);
        }
        _ => anyhow::bail!(invalid()),
    }
    ensure!(rows.len() <= MAX_RECORDS, invalid());
    Ok(())
}

fn records(value: &Json, family: Family) -> anyhow::Result<Vec<Json>> {
    let mut rows = Vec::new();
    if let Some(systems) = value.get("systems") {
        ensure!(family == Family::System, invalid());
        let systems = systems
            .as_array()
            .filter(|v| v.len() <= MAX_RECORDS)
            .context(invalid())?;
        for system in systems {
            if let Some(data) = system.get("data") {
                let start = rows.len();
                append_rows(data, &mut rows)?;
                if let Some(parent) = system.get("systemID") {
                    let parent = id(parent, false)?;
                    ensure!(
                        rows[start..]
                            .iter()
                            .all(|r| r["systemID"].as_u64() == Some(parent)),
                        invalid()
                    );
                }
            } else {
                append_rows(system, &mut rows)?;
            }
        }
    } else {
        append_rows(value.get("data").context(invalid())?, &mut rows)?;
    }
    if family == Family::Dhw {
        ensure!(rows.len() == 1, invalid());
    }
    let mut seen = BTreeSet::new();
    let mut projected = Vec::with_capacity(rows.len());
    for raw in rows {
        let raw = fields::project(raw)?;
        let system = id(&raw["systemID"], family == Family::Dhw)?;
        let sensor = match family {
            Family::System => {
                ensure!(raw.get("iaqsensorID").is_none(), invalid());
                if let Some(zone) = raw.get("zoneID") {
                    id(zone, false)?;
                }
                0
            }
            Family::Dhw => {
                ensure!(
                    system == 0 && raw.get("zoneID").is_none() && raw.get("iaqsensorID").is_none(),
                    invalid()
                );
                0
            }
            Family::Iaq(requested) => {
                ensure!(
                    system == requested && raw.get("zoneID").is_none(),
                    invalid()
                );
                id(&raw["iaqsensorID"], false)?
            }
        };
        ensure!(seen.insert((system, sensor)), invalid());
        projected.push(raw);
    }
    projected.sort_by_key(|raw| (raw["systemID"].as_u64(), raw["iaqsensorID"].as_u64()));
    Ok(projected)
}

async fn fetch(config: &Config, request: ReadRequest, end: Instant) -> anyhow::Result<Json> {
    let budget = end
        .checked_duration_since(Instant::now())
        .filter(|v| !v.is_zero())
        .context(api::Failure::Expired)?;
    api::read(
        config,
        &request,
        ReadMethod::Post,
        budget.min(Duration::from_secs(4)),
    )
    .await
}

async fn read_system(config: &Config, system: u64, end: Instant) -> anyhow::Result<Vec<Json>> {
    let value = fetch(
        config,
        ReadRequest::Hvac {
            system: u8::try_from(system)?,
            zone: None,
        },
        end,
    )
    .await?;
    let rows = records(&value, Family::System)?;
    ensure!(
        rows.len() == 1 && rows[0]["systemID"].as_u64() == Some(system),
        invalid()
    );
    Ok(rows)
}

fn metadata(value: &Json, key: &str) -> anyhow::Result<Json> {
    let data = value.get("data").unwrap_or(value);
    if key == "version"
        && let (Some(version), Some(alias)) = (data.get("version"), data.get("schema"))
    {
        ensure!(version == alias, invalid());
    }
    let value = data
        .get(key)
        .or_else(|| (key == "version").then(|| data.get("schema")).flatten())
        .context(invalid())?;
    let text = value
        .as_str()
        .filter(|s| !s.is_empty() && s.chars().count() <= 256 && !s.chars().any(char::is_control))
        .context(invalid())?;
    Ok(json!({key:text}))
}

impl ResourceSnapshot {
    pub(crate) async fn read_for(
        config: &Config,
        zones: &Snapshot,
        webserver: &Json,
        native: &str,
        mac: &str,
        end: Instant,
    ) -> Self {
        let gateway = native == format!("{mac}_gateway");
        let target = Target::from_native(native, mac);
        if !gateway && target.is_none() {
            return Self::default();
        }
        let mut result = Self::from_webserver(config, webserver);
        if gateway {
            result.read_systems(config, zones, end).await;
            result.read_integration(config, end).await;
        } else if let Some(target) = target {
            match target {
                Target::Dhw => {
                    result.read_systems(config, zones, end).await;
                    result.read_dhw(config, end).await;
                }
                Target::System(system) | Target::Iaq(system, _) => {
                    match read_system(config, system, end).await {
                        Ok(rows) if result.budget.reserve(&rows) => result.systems = rows,
                        _ => result.failures += 1,
                    }
                    if matches!(target, Target::Iaq(..)) {
                        result.read_iaq(config, end).await;
                    }
                }
            }
        }
        result
    }
    fn from_webserver(config: &Config, webserver: &Json) -> Self {
        let mut result = Self {
            read_only: config.read_only,
            ..Self::default()
        };
        match fields::project(webserver) {
            Ok(value) if result.budget.reserve(std::slice::from_ref(&value)) => {
                result.webserver = value;
            }
            _ => result.failures += 1,
        }
        result
    }
    pub(crate) fn retain_known_scope(
        &mut self,
        instance: &InstanceId,
        mac: &str,
        zones: &Snapshot,
        known_devices: &[DeviceId],
    ) {
        let prefix = format!("{instance}:{mac}_s");
        let mapped = zones.system_ids();
        for device in known_devices {
            let system = device
                .as_str()
                .strip_prefix(&prefix)
                .and_then(|native| native.strip_suffix("_system"))
                .and_then(|system| system.parse::<u64>().ok())
                .filter(|system| (1..=32).contains(system));
            if let Some(system) = system.filter(|system| !mapped.contains(system)) {
                self.unmapped_systems.insert(system);
            }
        }
    }

    fn installation_complete(&self, zones: &Snapshot) -> bool {
        let mapped = zones.system_ids();
        self.systems_available
            && self.unmapped_systems.is_empty()
            && self
                .systems
                .iter()
                .all(|r| r["systemID"].as_u64().is_some_and(|s| mapped.contains(&s)))
    }
    pub(crate) async fn read(
        config: &Config,
        zones: &Snapshot,
        webserver: &Json,
        end: Instant,
    ) -> Self {
        let mut result = Self::from_webserver(config, webserver);
        result.version = fetch(config, ReadRequest::Version, end)
            .await
            .and_then(|v| metadata(&v, "version"))
            .ok()
            .filter(|value| result.budget.reserve(std::slice::from_ref(value)));
        result.failures += u32::from(result.version.is_none());
        result.read_integration(config, end).await;

        result.read_systems(config, zones, end).await;
        result.read_dhw(config, end).await;
        result.read_iaq(config, end).await;
        result
    }

    async fn read_integration(&mut self, config: &Config, end: Instant) {
        self.integration = fetch(config, ReadRequest::Integration, end)
            .await
            .and_then(|v| metadata(&v, "driver"))
            .ok()
            .filter(|value| self.budget.reserve(std::slice::from_ref(value)));
        self.failures += u32::from(self.integration.is_none());
    }

    async fn read_systems(&mut self, config: &Config, zones: &Snapshot, end: Instant) {
        let response = fetch(
            config,
            ReadRequest::Hvac {
                system: 127,
                zone: None,
            },
            end,
        )
        .await;
        // An unsupported broadcast can use the documented targeted reads.
        // Malformed or oversized content is not evidence of a smaller system
        // inventory: retain useful targeted reads but disable global writes.
        let legacy_fallback = response.as_ref().err().is_some_and(|e| {
            matches!(
                e.downcast_ref::<api::Failure>(),
                Some(api::Failure::Http(404 | 405 | 500 | 501) | api::Failure::Api)
            )
        });
        let broadcast = response
            .and_then(|v| records(&v, Family::System))
            .and_then(|rows| {
                ensure!(self.budget.reserve(&rows), invalid());
                Ok(rows)
            });
        let broadcast_ok = broadcast.is_ok();
        self.systems = broadcast.unwrap_or_default();
        // Some controllers implement only per-system reads. A missing known
        // system is retried through that documented read form, never a PUT.
        for system in zones.system_ids() {
            if self
                .systems
                .iter()
                .any(|r| r["systemID"].as_u64() == Some(system))
            {
                continue;
            }
            let queried = read_system(config, system, end).await;
            if let Ok(rows) = queried
                && self.budget.reserve(&rows)
            {
                self.systems.extend(rows);
            }
        }
        self.systems.sort_by_key(|r| r["systemID"].as_u64());
        self.systems_available = (broadcast_ok || (legacy_fallback && !self.systems.is_empty()))
            && zones.system_ids().iter().all(|system| {
                self.systems
                    .iter()
                    .any(|r| r["systemID"].as_u64() == Some(*system))
            });
        self.failures += u32::from(!self.systems_available);
    }

    async fn read_dhw(&mut self, config: &Config, end: Instant) {
        self.dhw = fetch(
            config,
            ReadRequest::Hvac {
                system: 0,
                zone: None,
            },
            end,
        )
        .await
        .and_then(|v| records(&v, Family::Dhw))
        .ok()
        .and_then(|v| v.into_iter().next());
        if let Some(raw) = &mut self.dhw
            && raw.get("units").is_none()
            && let Some(unit) = config.dhw_units
        {
            raw["units"] = json!(unit);
        }
        if self
            .dhw
            .as_ref()
            .is_some_and(|raw| !self.budget.reserve(std::slice::from_ref(raw)))
        {
            self.dhw = None;
        }
        self.failures += u32::from(self.dhw.is_none());
    }

    async fn read_iaq(&mut self, config: &Config, end: Instant) {
        let mut requested = 0;
        let mut succeeded = 0;
        for system in &self.systems {
            let Some(count) = system["num_airqsensor"]
                .as_u64()
                .filter(|v| (1..=32).contains(v))
            else {
                continue;
            };
            requested += 1;
            let system = system["systemID"].as_u64().expect("validated system");
            let read = fetch(
                config,
                ReadRequest::Iaq {
                    system: u8::try_from(system).expect("bounded system ID"),
                    sensor: 0,
                },
                end,
            )
            .await
            .and_then(|v| records(&v, Family::Iaq(system)));
            let read = read.and_then(|rows| {
                ensure!(self.budget.reserve(&rows), invalid());
                Ok(rows)
            });
            if let Ok(rows) = read {
                // A missing advertised sensor is an incomplete section,
                // even if the remaining records are still useful to read.
                let complete = rows.len() == usize::try_from(count).expect("bounded count");
                succeeded += usize::from(complete);
                if !complete {
                    self.incomplete_iaq.insert(system);
                    self.failures += 1;
                }
                self.iaq.extend(rows);
            } else {
                self.incomplete_iaq.insert(system);
                self.failures += 1;
            }
        }
        self.iaq_available = requested > 0 && requested == succeeded;
    }

    pub(crate) fn entities(
        &self,
        instance: &InstanceId,
        mac: &str,
        zones: &Snapshot,
        read_only: bool,
    ) -> Vec<Entity> {
        let locked = self.read_only || read_only;
        let mut entities = vec![self.gateway(instance, mac, zones, locked)];
        for raw in &self.systems {
            let system = raw["systemID"].as_u64().expect("validated system");
            entities.push(resource(
                instance,
                mac,
                zones,
                Target::System(system),
                raw,
                locked,
            ));
        }
        if let Some(raw) = &self.dhw {
            entities.push(resource(
                instance,
                mac,
                zones,
                Target::Dhw,
                raw,
                locked || !self.installation_complete(zones),
            ));
        }
        for raw in &self.iaq {
            let system = raw["systemID"].as_u64().expect("validated system");
            let sensor = raw["iaqsensorID"].as_u64().expect("validated sensor");
            entities.push(resource(
                instance,
                mac,
                zones,
                Target::Iaq(system, sensor),
                raw,
                locked || self.incomplete_iaq.contains(&system),
            ));
        }
        entities
    }

    fn gateway(
        &self,
        instance: &InstanceId,
        mac: &str,
        zones: &Snapshot,
        read_only: bool,
    ) -> Entity {
        let members = zones.scope_members(instance, mac, None);
        let locked = read_only || members.is_none() || !self.installation_complete(zones);
        let mut values = fields::points(Scope::Webserver, &self.webserver, locked);
        if let Some(raw) = &self.version {
            values.extend(fields::points(Scope::Version, raw, locked));
        }
        if let Some(raw) = &self.integration {
            values.extend(fields::points(Scope::Integration, raw, locked));
        }
        for (key, value) in [
            ("version_available", self.version.is_some()),
            ("integration_available", self.integration.is_some()),
            ("systems_available", self.systems_available),
            ("dhw_available", self.dhw.is_some()),
            ("iaq_available", self.iaq_available),
            ("read_only_status", read_only),
        ] {
            values.push((diagnostic(key, Kind::Binary), Value::Bool(value)));
        }
        values.push((
            diagnostic(
                "failures_count",
                Kind::Numeric {
                    min: Some(0.),
                    max: Some(68.),
                    step: Some(1.),
                },
            ),
            Value::Int(i64::from(self.failures)),
        ));
        entity(
            instance,
            &format!("{mac}_gateway"),
            moli_i18n::tr!("airzone.gateway"),
            "Airzone Webserver",
            members.unwrap_or_default(),
            values,
        )
    }

    pub(crate) fn encode(
        &self,
        instance: &InstanceId,
        mac: &str,
        zones: &Snapshot,
        device: &DeviceId,
        key: &str,
        value: &Value,
    ) -> anyhow::Result<(&'static str, Json)> {
        ensure!(!self.read_only, moli_i18n::tr!("airzone.read_only"));
        let entities = self.entities(instance, mac, zones, false);
        let (current, _) = entities
            .iter()
            .find(|(d, _)| &d.id == device)
            .context(invalid())?;
        ensure!(
            current.point(key).is_some_and(|p| p.access.write),
            invalid()
        );
        if *device == DeviceId::new(instance, &format!("{mac}_gateway")) {
            let (wire, value) = fields::encode(
                Scope::Integration,
                self.integration.as_ref().context(invalid())?,
                key,
                value,
            )?;
            return Ok(("integration", json!({wire:value})));
        }
        for (target, raw) in self.targets() {
            if *device != DeviceId::new(instance, &target.native(mac)) {
                continue;
            }
            let (wire, value) = fields::encode(target.scope(), raw, key, value)?;
            let mut body = target.address(raw);
            body[wire] = value;
            return Ok((target.path(), body));
        }
        anyhow::bail!(invalid())
    }

    fn targets(&self) -> impl Iterator<Item = (Target, &Json)> {
        self.systems
            .iter()
            .map(|r| {
                (
                    Target::System(r["systemID"].as_u64().expect("validated system")),
                    r,
                )
            })
            .chain(self.dhw.iter().map(|r| (Target::Dhw, r)))
            .chain(self.iaq.iter().map(|r| {
                (
                    Target::Iaq(
                        r["systemID"].as_u64().expect("validated system"),
                        r["iaqsensorID"].as_u64().expect("validated sensor"),
                    ),
                    r,
                )
            }))
    }
}

#[derive(Clone, Copy)]
enum Target {
    System(u64),
    Dhw,
    Iaq(u64, u64),
}
impl Target {
    fn from_native(native: &str, mac: &str) -> Option<Self> {
        let suffix = native.strip_prefix(&format!("{mac}_"))?;
        let target = if suffix == "dhw" {
            Self::Dhw
        } else {
            let (system, kind) = suffix.strip_prefix('s')?.split_once('_')?;
            let system = system
                .parse::<u64>()
                .ok()
                .filter(|s| (1..=32).contains(s))?;
            if kind == "system" {
                Self::System(system)
            } else {
                let sensor = kind
                    .strip_prefix("iaq")?
                    .parse::<u64>()
                    .ok()
                    .filter(|s| (1..=32).contains(s))?;
                Self::Iaq(system, sensor)
            }
        };
        (target.native(mac) == native).then_some(target)
    }
    fn native(self, mac: &str) -> String {
        match self {
            Self::System(id) => format!("{mac}_s{id}_system"),
            Self::Dhw => format!("{mac}_dhw"),
            Self::Iaq(system, sensor) => format!("{mac}_s{system}_iaq{sensor}"),
        }
    }
    fn system(self) -> Option<u64> {
        match self {
            Self::System(s) | Self::Iaq(s, _) => Some(s),
            Self::Dhw => None,
        }
    }
    fn scope(self) -> Scope {
        match self {
            Self::System(_) => Scope::System,
            Self::Dhw => Scope::Dhw,
            Self::Iaq(_, _) => Scope::Iaq,
        }
    }
    fn path(self) -> &'static str {
        match self {
            Self::Iaq(_, _) => "iaq",
            _ => "hvac",
        }
    }
    fn name(self) -> String {
        match self {
            Self::System(id) => moli_i18n::tr!("airzone.system", id = id),
            Self::Dhw => moli_i18n::tr!("airzone.dhw"),
            Self::Iaq(system, sensor) => {
                moli_i18n::tr!("airzone.iaq", id = format!("{system}/{sensor}"))
            }
        }
    }
    fn address(self, raw: &Json) -> Json {
        match self {
            Self::System(system) => {
                let mut body = json!({"systemID":system});
                if let Some(zone) = raw.get("zoneID") {
                    body["zoneID"] = zone.clone();
                }
                body
            }
            Self::Dhw => json!({"systemID":0}),
            Self::Iaq(system, sensor) => json!({"systemID":system,"iaqsensorID":sensor}),
        }
    }
}

fn resource(
    instance: &InstanceId,
    mac: &str,
    zones: &Snapshot,
    target: Target,
    raw: &Json,
    read_only: bool,
) -> Entity {
    let members = zones.scope_members(instance, mac, target.system());
    let values = fields::points(target.scope(), raw, read_only || members.is_none());
    let name = raw["name"]
        .as_str()
        .map(|s| {
            s.chars()
                .filter(|c| !c.is_control())
                .take(120)
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| target.name());
    entity(
        instance,
        &target.native(mac),
        name,
        "Airzone Local API",
        members.unwrap_or_default(),
        values,
    )
}

fn entity(
    instance: &InstanceId,
    native: &str,
    name: String,
    model: &str,
    members: Vec<DeviceId>,
    values: Vec<(PointSpec, Value)>,
) -> Entity {
    let (points, states): (Vec<_>, Vec<_>) = values
        .into_iter()
        .map(|(point, value)| {
            let key = point.key.to_string();
            (point, (key, value))
        })
        .unzip();
    (
        Device {
            id: DeviceId::new(instance, native),
            instance: instance.clone(),
            native_name: name.into(),
            manufacturer: Some("Airzone".into()),
            model: Some(model.into()),
            description: None,
            native_room: None,
            members,
            points,
        },
        states,
    )
}

fn diagnostic(key: &str, kind: Kind) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: moli_i18n::tr(&format!("airzone.{key}")).into(),
        kind,
        unit: None,
        semantic: Semantic::Other,
        access: Access {
            read: true,
            write: false,
        },
    }
}

#[cfg(test)]
#[path = "resources_tests.rs"]
mod tests;
