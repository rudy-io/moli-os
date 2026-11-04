//! Explicit, scope-bound consent for saved scenarios. No provider I/O.
use super::Error;
use crate::inventory::{
    self, Catalog, CatalogIdentity, CollectionKind, CollectionStatus, Definition,
};
use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointId, PointSpec, Semantic, Value};
use moli_runtime::DriverCtx;
use ring::digest;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::{collections::BTreeMap, fmt};

pub const POLICY_KEY: &str = "cloud_scenario_permissions_v1";
const POLICY_LIMIT: usize = 262_144;
const NATIVE_PREFIX: &str = "cloud:scenario:";

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    version: u8,
    identity: Option<CatalogIdentity>,
    allowed: BTreeMap<String, String>,
}
impl fmt::Debug for Policy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ScenarioPolicy([redacted])")
    }
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            version: 1,
            identity: None,
            allowed: BTreeMap::new(),
        }
    }
}
impl Policy {
    pub fn decode(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > POLICY_LIMIT {
            return Err(Error::InvalidInput);
        }
        let policy: Self = serde_json::from_slice(bytes).map_err(|_| Error::InvalidInput)?;
        if policy.version != 1
            || policy.allowed.len() > inventory::MAX_OBJECTS
            || policy
                .allowed
                .iter()
                .any(|(id, fingerprint)| !valid_oid(id) || !valid_hash(fingerprint))
            || (!policy.allowed.is_empty() && policy.identity.is_none())
        {
            return Err(Error::InvalidInput);
        }
        if let Some(identity) = &policy.identity {
            Catalog::new(identity.clone()).map_err(|_| Error::InvalidInput)?;
        }
        Ok(policy)
    }
    pub fn encode(&self) -> Result<String, Error> {
        let body = serde_json::to_string(self).map_err(|_| Error::InvalidInput)?;
        Self::decode(body.as_bytes())?;
        Ok(body)
    }
    pub fn change(
        &mut self,
        catalog: &Catalog,
        scenario: &str,
        enabled: bool,
    ) -> Result<(), Error> {
        if !valid_oid(scenario) {
            return Err(Error::InvalidInput);
        }
        if !enabled {
            self.allowed.remove(scenario);
            return Ok(());
        }
        let definition = definition(catalog, scenario).ok_or(Error::InvalidInput)?;
        let fingerprint = fingerprint(definition)?;
        if self.identity.as_ref() != Some(&catalog.identity()) {
            self.allowed.clear();
            self.identity = Some(catalog.identity());
        }
        self.allowed.insert(scenario.into(), fingerprint);
        Ok(())
    }
    pub fn permits(&self, catalog: &Catalog, scenario: &str) -> bool {
        self.identity.as_ref() == Some(&catalog.identity())
            && definition(catalog, scenario)
                .and_then(|d| fingerprint(d).ok())
                .is_some_and(|hash| self.allowed.get(scenario) == Some(&hash))
    }
}

pub fn revision(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in digest::digest(&digest::SHA256, bytes).as_ref() {
        write!(encoded, "{byte:02x}").expect("writing into a String");
    }
    encoded
}
pub fn catalog_revision(catalog: &Catalog) -> Result<String, Error> {
    inventory::encode_catalog(catalog)
        .map(|bytes| revision(&bytes))
        .map_err(|_| Error::InvalidResponse)
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
pub(crate) fn valid_oid(oid: &str) -> bool {
    !oid.is_empty()
        && oid.len() <= 256
        && ![
            "apply",
            "schedule",
            "currentExecutions",
            "history",
            "executions",
            "delayedTriggerSchedule",
            "stop",
            "cancel",
        ]
        .iter()
        .any(|reserved| oid.eq_ignore_ascii_case(reserved))
        && oid
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
fn definition<'a>(catalog: &'a Catalog, oid: &str) -> Option<&'a Definition> {
    if !valid_oid(oid) {
        return None;
    }
    catalog
        .collections
        .get(&CollectionKind::Scenarios)?
        .data
        .as_ref()?
        .definitions
        .iter()
        .find(|d| {
            d.id.as_deref() == Some(oid)
                && d.raw
                    .as_value()
                    .get("oid")
                    .and_then(serde_json::Value::as_str)
                    == Some(oid)
        })
}
pub(crate) fn fingerprint(definition: &Definition) -> Result<String, Error> {
    serde_json::to_vec(&definition.raw)
        .map(|bytes| revision(&bytes))
        .map_err(|_| Error::InvalidResponse)
}
fn device_id(catalog: &Catalog, oid: &str) -> DeviceId {
    let scope = serde_json::to_vec(&(&catalog.instance, &catalog.scope, oid))
        .expect("bounded scalar scope");
    DeviceId::new(
        &InstanceId::from(catalog.instance.clone()),
        &format!("{NATIVE_PREFIX}{}", revision(&scope)),
    )
}
fn command_value(catalog: &Catalog, revision_id: &str, oid: &str) -> Result<String, Error> {
    let bytes = serde_json::to_vec(&(catalog.identity(), revision_id, oid))
        .map_err(|_| Error::InvalidResponse)?;
    Ok(revision(&bytes))
}
pub fn owns_scenario(id: &DeviceId) -> bool {
    id.as_str()
        .split_once(':')
        .is_some_and(|(_, native)| native.starts_with(NATIVE_PREFIX))
}

#[derive(Serialize)]
pub struct ScenarioStatus {
    pub id: String,
    pub permitted: bool,
    pub executable: bool,
    pub point: PointId,
    pub value: String,
    pub reason: Option<&'static str>,
}
impl fmt::Debug for ScenarioStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ScenarioStatus([redacted])")
    }
}
#[derive(Debug, Serialize)]
pub struct ExecutionView {
    pub read_only: bool,
    pub scenarios: Vec<ScenarioStatus>,
}
pub fn view(
    catalog: &Catalog,
    policy: &Policy,
    read_only: bool,
    online: bool,
    fresh: bool,
) -> ExecutionView {
    view_at_revision(
        catalog,
        policy,
        read_only,
        online,
        fresh,
        &catalog_revision(catalog).unwrap_or_default(),
    )
}
pub fn view_at_revision(
    catalog: &Catalog,
    policy: &Policy,
    read_only: bool,
    online: bool,
    fresh: bool,
    revision: &str,
) -> ExecutionView {
    if !valid_hash(revision) {
        return ExecutionView {
            read_only,
            scenarios: vec![],
        };
    }
    let scenarios = catalog
        .collections
        .get(&CollectionKind::Scenarios)
        .and_then(|c| c.data.as_ref())
        .into_iter()
        .flat_map(|c| &c.definitions)
        .filter_map(|d| {
            let id = d.id.as_ref()?;
            let supported = definition(catalog, id).is_some();
            let permitted = policy.permits(catalog, id);
            let reason = if read_only {
                Some("global_read_only")
            } else if !supported {
                Some("not_supported")
            } else if !online {
                Some("offline")
            } else if !fresh {
                Some("stale")
            } else if !permitted {
                Some("permission_required")
            } else {
                None
            };
            Some(ScenarioStatus {
                id: id.clone(),
                permitted,
                executable: reason.is_none(),
                point: PointId::new(&device_id(catalog, id), "run"),
                value: command_value(catalog, revision, id).ok()?,
                reason,
            })
        })
        .collect();
    ExecutionView {
        read_only,
        scenarios,
    }
}
pub(crate) fn fresh(catalog: &Catalog, poll_secs: u64) -> bool {
    catalog
        .collections
        .get(&CollectionKind::Scenarios)
        .is_some_and(|collection| {
            collection.status == CollectionStatus::Available
                && collection
                    .last_observation_ms
                    .and_then(|time| u64::try_from(time).ok())
                    .is_some_and(|time| {
                        let now = moli_core::now_ms();
                        time <= now
                            && now - time <= poll_secs.saturating_mul(1000).saturating_add(90_000)
                    })
        })
}

pub(crate) struct Ticket {
    pub identity: CatalogIdentity,
    pub oid: String,
    pub fingerprint: String,
}
impl fmt::Debug for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ScenarioTicket([redacted])")
    }
}
pub(crate) fn resolve(
    catalog: &Catalog,
    policy: &Policy,
    revision: &str,
    device: &DeviceId,
    key: &str,
    value: &Value,
) -> Result<Ticket, Error> {
    if key != "run" {
        return Err(Error::InvalidInput);
    }
    let item = view_at_revision(catalog, policy, false, true, true, revision)
        .scenarios
        .into_iter()
        .find(|s| {
            s.executable
                && s.point == PointId::new(device, key)
                && value == &Value::from(s.value.as_str())
        })
        .ok_or(Error::ScopeMismatch)?;
    Ok(Ticket {
        identity: catalog.identity(),
        fingerprint: fingerprint(definition(catalog, &item.id).ok_or(Error::InvalidInput)?)?,
        oid: item.id,
    })
}

pub(crate) fn publish(
    ctx: &DriverCtx,
    catalog: Option<&Catalog>,
    read_only: bool,
    online: bool,
    poll_secs: u64,
) {
    let policy = ctx
        .secret(POLICY_KEY)
        .and_then(|s| Policy::decode(s.as_bytes()).ok())
        .unwrap_or_default();
    let statuses = catalog
        .map(|c| {
            let revision = ctx
                .secret(inventory::INVENTORY_KEY)
                .map(|bytes| self::revision(bytes.as_bytes()))
                .unwrap_or_default();
            view_at_revision(
                c,
                &policy,
                read_only,
                online,
                fresh(c, poll_secs),
                &revision,
            )
            .scenarios
        })
        .unwrap_or_default();
    let mut present = Vec::new();
    for status in statuses {
        let Some((id, _)) = status.point.split() else {
            continue;
        };
        present.push(id.clone());
        ctx.upsert_device(scenario_device(
            ctx.instance(),
            id.clone(),
            Some(&status.value),
            status.executable,
        ));
        ctx.set_availability(&id, online);
    }
    for old in ctx.devices().into_iter().filter(owns_scenario) {
        if !present.contains(&old) {
            // Retain offline cards through failed reads, never retain writable cached capabilities.
            if catalog.is_none() {
                ctx.upsert_device(scenario_device(ctx.instance(), old.clone(), None, false));
                ctx.set_availability(&old, false);
            } else {
                ctx.remove_device(&old);
            }
        }
    }
}

fn scenario_device(
    instance: &InstanceId,
    id: DeviceId,
    value: Option<&str>,
    writable: bool,
) -> Device {
    Device {
        id,
        instance: instance.clone(),
        native_name: moli_i18n::tr!("overkiz.cloud_scenario").into(),
        manufacturer: Some("Somfy".into()),
        model: None,
        description: None,
        native_room: None,
        members: vec![],
        points: vec![PointSpec {
            key: "run".into(),
            label: moli_i18n::tr!("overkiz.cloud_run").into(),
            kind: Kind::Enum {
                values: value.into_iter().map(Into::into).collect(),
            },
            access: Access {
                read: false,
                write: writable,
            },
            unit: None,
            semantic: Semantic::Control,
        }],
    }
}

#[cfg(test)]
#[path = "execution_tests.rs"]
mod tests;
