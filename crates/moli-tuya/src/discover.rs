//! New devices of the Tuya account, found by Moli itself with the cloud
//! keys it already holds: what `tools/tuya-iot/tuya-iot-export.py` does for
//! `moli-os tuya import`, for the devices the device file does not know yet.
//!
//! Known devices are left as they are (their data points, address, room),
//! except that their local key is read again (a device paired anew gets a new
//! one). A device the account no longer lists stays in the file.

use std::collections::{HashMap, HashSet};

use anyhow::Context as _;
use serde_json::Value as Json;

use crate::cloud::{Api, Keys, Region};
use crate::model::{DeviceFile, Dp, DpKind, TuyaDevice};
use crate::{key_name, parse_key};

/// What a discovery found: the file to write and the keys to store.
#[derive(Debug)]
pub struct Found {
    pub file: DeviceFile,
    /// Names of the devices that were not in the file.
    pub added: Vec<String>,
    /// `(secret name, local key)` of the new devices and of known ones whose
    /// key changed.
    pub keys: Vec<(String, String)>,
}

/// Asks the account for its devices. `has_key` tells whether a device's local
/// key is already stored with this value (then it is not returned again).
///
/// # Errors
/// The cloud refuses (keys, region, project not linked to the account) or
/// does not answer; nothing is partly returned.
pub async fn discover(
    keys: Keys,
    region: Region,
    existing: DeviceFile,
    stored_key: impl Fn(&str) -> Option<String>,
) -> anyhow::Result<Found> {
    let mut api = Api::new(keys, region);
    let listed = list(&mut api).await?;
    let known: HashSet<&str> = existing.devices.iter().map(|d| d.id.as_str()).collect();
    let gateways = if listed
        .iter()
        .any(|d| d.sub && !known.contains(d.id.as_str()))
    {
        gateways_of(&mut api, &listed).await
    } else {
        HashMap::new()
    };
    let mut file = existing.clone();
    let mut added = Vec::new();
    let mut keys = Vec::new();
    for device in listed {
        let secret = key_name(&device.id);
        let key = device.local_key.clone().filter(|k| parse_key(k).is_some());
        if let Some(key) = key
            && stored_key(&secret).as_deref() != Some(key.as_str())
        {
            keys.push((secret, key));
        }
        if known.contains(device.id.as_str()) {
            continue;
        }
        let dps = dps_of(&mut api, &device.id)
            .await
            .with_context(|| device.name.clone())?;
        let parent = gateways.get(&device.id);
        file.devices.push(TuyaDevice {
            id: device.id.clone(),
            name: device.name.clone(),
            category: device.category,
            product: device.product,
            ip: None,
            version: None,
            room: None,
            gateway: device.sub.then(|| parent.cloned().unwrap_or("tuya".into())),
            cid: parent.and(device.node_id),
            dps,
        });
        added.push(device.name);
    }
    Ok(Found { file, added, keys })
}

/// Finds the account's new devices with the keys Moli holds (the vault's file
/// first, else those given in the dashboard), files their local keys in the
/// encrypted store and the devices in the device file. The driver, watching
/// that file, restarts with them. Returns the names of the new devices.
///
/// # Errors
/// No cloud keys, the cloud refuses, or a file cannot be written.
pub async fn file_new(
    config: &crate::Config,
    hub: &moli_runtime::Hub,
    instance: &moli_core::InstanceId,
) -> anyhow::Result<Vec<String>> {
    let keys = match tokio::fs::read_to_string(&config.cloud_keys).await {
        Ok(text) => Keys::parse(&text),
        Err(_) => None,
    }
    .or_else(|| {
        Keys::new(
            &hub.secret(instance, crate::cloud::STORED_ID)?,
            &hub.secret(instance, crate::cloud::STORED_SECRET)?,
        )
    })
    .context(moli_i18n::tr!("pilotes.tuya.sans_cles_cloud"))?;
    let region = Region::named(&config.cloud_region).context("unknown cloud_region")?;
    let existing: DeviceFile = match tokio::fs::read(&config.devices_file).await {
        Ok(bytes) => serde_json::from_slice(&bytes)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DeviceFile::default(),
        Err(e) => return Err(e.into()),
    };
    let found = discover(keys, region, existing, |name| hub.secret(instance, name)).await?;
    for (name, key) in &found.keys {
        hub.store_secret(instance, name, key).await?;
    }
    if !found.added.is_empty() || !found.keys.is_empty() {
        let aside = config.devices_file.with_extension("json.new");
        let text = serde_json::to_string_pretty(&found.file)?;
        tokio::fs::write(&aside, text + "\n").await?;
        tokio::fs::rename(&aside, &config.devices_file).await?;
    }
    tracing::info!(%instance, added = found.added.len(), keys = found.keys.len(), "tuya cloud: devices looked for");
    Ok(found.added)
}

struct Listed {
    id: String,
    name: String,
    category: String,
    product: Option<String>,
    local_key: Option<String>,
    sub: bool,
    node_id: Option<String>,
}

fn text(value: &Json) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

async fn list(api: &mut Api) -> anyhow::Result<Vec<Listed>> {
    let mut found = Vec::new();
    let mut last = String::new();
    loop {
        let mut path = "/v1.0/iot-01/associated-users/devices?size=100".to_owned();
        if !last.is_empty() {
            path = format!("{path}&last_row_key={}", escaped(&last));
        }
        let page = api.call(&path).await?;
        for d in page["devices"].as_array().into_iter().flatten() {
            let Some(id) = text(&d["id"]) else { continue };
            found.push(Listed {
                name: text(&d["name"]).unwrap_or_else(|| id.clone()),
                category: text(&d["category"]).unwrap_or_default(),
                product: text(&d["product_name"]),
                local_key: text(&d["local_key"]),
                sub: d["sub"].as_bool().unwrap_or(false),
                node_id: text(&d["node_id"]),
                id,
            });
        }
        last = text(&page["last_row_key"]).unwrap_or_default();
        if page["has_more"] != Json::Bool(true) || last.is_empty() {
            return Ok(found);
        }
    }
}

/// A query value as Tuya signs it: everything but unreserved characters
/// percent-encoded (`last_row_key` is the only free-form one).
fn escaped(value: &str) -> String {
    use std::fmt::Write as _;
    value.bytes().fold(String::new(), |mut out, b| {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
        out
    })
}

/// Each sub-device's gateway, asked of every device that is not one itself.
/// A gateway that does not answer leaves its sub-devices to the `tuya` default.
async fn gateways_of(api: &mut Api, listed: &[Listed]) -> HashMap<String, String> {
    let mut parent = HashMap::new();
    for gateway in listed.iter().filter(|d| !d.sub) {
        let Ok(subs) = api
            .call(&format!("/v1.0/devices/{}/sub-devices", gateway.id))
            .await
        else {
            continue;
        };
        let list = subs.as_array().or_else(|| subs["list"].as_array());
        for sub in list.into_iter().flatten() {
            if let Some(id) = text(&sub["id"]) {
                parent.insert(id, gateway.id.clone());
            }
        }
    }
    parent
}

fn kind_of(model: &str) -> DpKind {
    match model {
        "bool" | "Boolean" => DpKind::Boolean,
        "value" | "Integer" => DpKind::Integer,
        "enum" | "Enum" => DpKind::Enum,
        "string" | "String" => DpKind::String,
        "bitmap" | "Bitmap" => DpKind::Bitmap,
        "struct" | "array" | "Json" => DpKind::Json,
        _ => DpKind::Raw,
    }
}

/// Unit, scale, bounds and choices, from a thing model or a standard set.
fn described(mut dp: Dp, spec: &Json) -> Dp {
    if let Some(unit) = spec["unit"].as_str().filter(|u| !u.is_empty()) {
        dp.unit = Some(unit.to_owned());
    }
    if let Some(scale) = spec["scale"].as_u64().and_then(|s| u32::try_from(s).ok()) {
        dp.scale = scale;
    }
    dp.min = spec["min"].as_f64().or(dp.min);
    dp.max = spec["max"].as_f64().or(dp.max);
    dp.step = spec["step"].as_f64().or(dp.step);
    if let Some(range) = spec["range"].as_array().filter(|r| !r.is_empty()) {
        dp.range = range
            .iter()
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
            .collect();
    }
    dp
}

fn blank(id: u32, code: &str, kind: DpKind, writable: bool) -> Dp {
    Dp {
        id,
        code: code.to_owned(),
        kind,
        writable,
        unit: None,
        scale: 0,
        min: None,
        max: None,
        step: None,
        range: Vec::new(),
    }
}

/// The thing model: the DPs with their local ids.
async fn model_dps(api: &mut Api, id: &str) -> anyhow::Result<Vec<Dp>> {
    let answer = api.call(&format!("/v2.0/cloud/thing/{id}/model")).await?;
    let model: Json = serde_json::from_str(answer["model"].as_str().unwrap_or("{}"))
        .context("Tuya: the model is not JSON")?;
    let mut dps = Vec::new();
    for prop in model["services"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| s["properties"].as_array().into_iter().flatten())
    {
        let (Some(ability), Some(code)) = (prop["abilityId"].as_u64(), prop["code"].as_str())
        else {
            continue;
        };
        let spec = &prop["typeSpec"];
        let writable = prop["accessMode"].as_str().is_some_and(|m| m.contains('w'));
        dps.push(described(
            blank(
                u32::try_from(ability)?,
                code,
                kind_of(spec["type"].as_str().unwrap_or("")),
                writable,
            ),
            spec,
        ));
    }
    dps.sort_by_key(|d| d.id);
    Ok(dps)
}

fn celsius(unit: Option<&str>) -> &str {
    match unit.unwrap_or("") {
        "°C" | "C" => "℃",
        other => other,
    }
}

fn same_shape(dp: &Dp, standard: &Dp) -> bool {
    dp.kind == standard.kind
        && celsius(dp.unit.as_deref()) == celsius(standard.unit.as_deref())
        && dp.min == standard.min
        && dp.max == standard.max
        && dp.scale == standard.scale
}

/// The thing model's DPs, named with the standard codes the cloud reports
/// states with. The standard set gives no DP id: a standard code missing from
/// the model takes the one model DP of the same type, unit and range, or none
/// (then the model's code stays).
async fn dps_of(api: &mut Api, id: &str) -> anyhow::Result<Vec<Dp>> {
    let mut dps = model_dps(api, id).await?;
    let Ok(spec) = api
        .call(&format!("/v1.0/iot-03/devices/{id}/specification"))
        .await
    else {
        return Ok(dps);
    };
    let standard: Vec<Dp> = spec["status"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let values: Json =
                serde_json::from_str(item["values"].as_str().unwrap_or("{}")).unwrap_or_default();
            let kind = kind_of(item["type"].as_str().unwrap_or(""));
            Some(described(
                blank(0, item["code"].as_str()?, kind, false),
                &values,
            ))
        })
        .collect();
    let codes: HashSet<String> = standard.iter().map(|s| s.code.clone()).collect();
    for s in &standard {
        if dps.iter().any(|d| d.code == s.code) {
            continue;
        }
        let twins: Vec<usize> = (0..dps.len())
            .filter(|&i| !codes.contains(&dps[i].code) && same_shape(&dps[i], s))
            .collect();
        if let [only] = twins[..] {
            dps[only].code.clone_from(&s.code);
        }
    }
    Ok(dps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_key_is_escaped_for_the_signature() {
        assert_eq!(escaped("az09-._~"), "az09-._~");
        assert_eq!(escaped("a b/c="), "a%20b%2Fc%3D");
    }

    #[test]
    fn a_standard_code_goes_to_the_one_model_dp_of_its_shape() {
        let mut model = blank(4, "temp_set", DpKind::Integer, true);
        model = described(
            model,
            &serde_json::json!({"unit": "°C", "min": 50, "max": 400, "scale": 1}),
        );
        let standard = described(
            blank(0, "temp_set_std", DpKind::Integer, false),
            &serde_json::json!({"unit": "℃", "min": 50, "max": 400, "scale": 1}),
        );
        assert!(same_shape(&model, &standard), "°C and ℃ are one unit");
        let other = described(
            blank(0, "x", DpKind::Integer, false),
            &serde_json::json!({"unit": "℃", "min": 0, "max": 400, "scale": 1}),
        );
        assert!(!same_shape(&model, &other), "other bounds, other DP");
    }

    #[test]
    fn model_types_become_dp_kinds() {
        assert_eq!(kind_of("bool"), DpKind::Boolean);
        assert_eq!(kind_of("value"), DpKind::Integer);
        assert_eq!(kind_of("struct"), DpKind::Json);
        assert_eq!(kind_of("whatever"), DpKind::Raw);
    }
}
