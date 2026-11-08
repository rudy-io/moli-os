//! What the Tuya cloud says of the home's devices, as `moli-os tuya import`
//! takes it: the same file `tools/home-assistant/tuya-export.py` writes,
//! read here straight from the cloud project (its linked app accounts), so
//! Home Assistant is no longer needed to add a device.
//!
//! The cloud's data model names some data points its own way: for the same
//! number, a bulb's `bright_value_v2` is `bright_value` there, a scene
//! switch's `switch_mode1` is `switch_type_1`. The device speaks the first
//! names (Home Assistant reads those): the known ones are given back here,
//! and [`keep_known`] leaves a device already filed as it is.

use serde_json::{Map, Value as Json, json};

use crate::{DeviceFile, ImportDevice, ImportFile};

/// Devices that carry sub-devices: Zigbee/BLE gateways, infrared hubs.
fn is_hub(category: &str) -> bool {
    category.starts_with("wg") || matches!(category, "wfcon" | "wnykq")
}

/// The import from the cloud's device list (`raws`) and each device's data
/// model (`models`, in the same order; `Null` when the cloud gave none).
#[must_use]
pub fn from_cloud(raws: &[Json], models: &[Json]) -> ImportFile {
    let mut devices: Vec<Map<String, Json>> = raws
        .iter()
        .zip(models.iter().chain(std::iter::repeat(&Json::Null)))
        .filter_map(|(raw, model)| device(raw, model))
        .collect();
    find_parents(&mut devices);
    ImportFile {
        devices: devices
            .into_iter()
            .filter_map(|d| serde_json::from_value::<ImportDevice>(Json::Object(d)).ok())
            .collect(),
    }
}

fn text<'a>(raw: &'a Json, key: &str) -> Option<&'a str> {
    raw[key].as_str().map(str::trim).filter(|s| !s.is_empty())
}

/// One device as the import takes it (`tuya-export.py`'s shape).
fn device(raw: &Json, model: &Json) -> Option<Map<String, Json>> {
    let id = text(raw, "id")?;
    let category = text(raw, "category").unwrap_or("");
    let mut out = Map::new();
    out.insert("id".into(), json!(id));
    out.insert("name".into(), json!(text(raw, "name").unwrap_or(id)));
    out.insert("category".into(), json!(category));
    if let Some(product) = text(raw, "product_name") {
        out.insert("product".into(), json!(product));
    }
    out.insert("dps".into(), Json::Array(dps(model, category)));
    out.insert("local_key".into(), json!(text(raw, "local_key")));
    // A gateway has a node_id too: only what is not a hub is a sub-device.
    let node_id = text(raw, "node_id");
    let sub = raw["sub"].as_bool().unwrap_or(false) || node_id.is_some();
    if sub && !is_hub(category) {
        out.insert("gateway".into(), json!("tuya"));
        if let Some(node_id) = node_id {
            out.insert("node_id".into(), json!(node_id));
        }
        if let Some(parent) = text(raw, "gateway_id").or_else(|| text(raw, "parent_id")) {
            out.insert("parent".into(), json!(parent));
        }
    }
    Some(out)
}

/// The data points of a thing model (`/v2.0/cloud/thing/{id}/model`):
/// `abilityId` is the DP's number, `typeSpec` its type and range.
fn dps(model: &Json, category: &str) -> Vec<Json> {
    let mut out: Vec<Json> = model["services"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|s| s["properties"].as_array().into_iter().flatten())
        .filter_map(|p| dp(p, category))
        .collect();
    out.sort_by_key(|d| d["id"].as_u64());
    out.dedup_by_key(|d| d["id"].as_u64());
    out
}

/// The name the device itself gives this data point, when the cloud's
/// model says another (`None`: the cloud's own).
fn device_code(category: &str, id: u64, code: &str) -> Option<String> {
    match category {
        // A bulb's « v2 » points, by their number.
        "dj" | "dd" | "xdd" | "fwd" | "dc" | "tgq" | "tgkg" => match id {
            22 => Some("bright_value_v2".into()),
            23 => Some("temp_value_v2".into()),
            24 => Some("colour_data_v2".into()),
            25 => Some("scene_data_v2".into()),
            26 => Some("countdown_1".into()),
            _ => None,
        }
        .filter(|name| !code.ends_with("_v2") && name != code),
        // A scene switch's buttons: `switch_type_1` is `switch_mode1`.
        "wxkg" => code
            .strip_prefix("switch_type_")
            .map(|n| format!("switch_mode{n}")),
        _ => None,
    }
}

/// Data points only the factory uses: never shown.
const FACTORY: &[&str] = &["test_bit"];

fn dp(property: &Json, category: &str) -> Option<Json> {
    let id = property["abilityId"].as_u64()?;
    let code = text(property, "code")?;
    if FACTORY.contains(&code) {
        return None;
    }
    let code = device_code(category, id, code).unwrap_or_else(|| code.to_owned());
    let spec = &property["typeSpec"];
    let kind = match spec["type"].as_str()? {
        "bool" => "Boolean",
        "value" => "Integer",
        "enum" => "Enum",
        "string" => "String",
        "bitmap" => "Bitmap",
        "raw" => "Raw",
        _ => "Json",
    };
    let writable = matches!(property["accessMode"].as_str(), Some("rw" | "wr"));
    let mut out = json!({ "id": id, "code": code, "type": kind, "writable": writable });
    if let Some(unit) = text(spec, "unit") {
        out["unit"] = json!(unit);
    }
    if let Some(scale) = spec["scale"].as_u64() {
        out["scale"] = json!(scale);
    }
    for key in ["min", "max", "step"] {
        if let Some(n) = spec[key].as_f64() {
            out[key] = json!(n);
        }
    }
    if let Some(range) = spec["range"].as_array().filter(|r| !r.is_empty()) {
        let range: Vec<String> = range
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        out["range"] = json!(range);
    }
    Some(out)
}

/// A sub-device's gateway, when the cloud does not name it: the hub sharing
/// its local key, else the home's only hub of its kind (infrared for
/// `infrared_*`, Zigbee/BLE for the rest). A hub the cloud gives no key
/// takes its sub-devices' one: they share their gateway's key. As
/// `tuya-export.py` does.
fn find_parents(devices: &mut [Map<String, Json>]) {
    let key = |d: &Map<String, Json>| d.get("local_key").and_then(Json::as_str).map(str::to_owned);
    let category = |d: &Map<String, Json>| {
        d.get("category")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let hubs: Vec<(String, String, Option<String>)> = devices
        .iter()
        .filter(|d| !d.contains_key("gateway") && is_hub(&category(d)))
        .map(|d| {
            let id = d.get("id").and_then(Json::as_str).unwrap_or("").to_owned();
            (id, category(d), key(d))
        })
        .collect();
    for d in devices.iter_mut() {
        if !d.contains_key("gateway") || d.contains_key("parent") {
            continue;
        }
        let own = key(d);
        let same_key: Vec<&String> = hubs
            .iter()
            .filter(|(_, _, k)| own.is_some() && *k == own)
            .map(|(id, _, _)| id)
            .collect();
        let infrared = category(d).starts_with("infrared");
        let kind: Vec<&String> = hubs
            .iter()
            .filter(|(_, c, _)| (c == "wnykq") == infrared)
            .map(|(id, _, _)| id)
            .collect();
        if let [one] = same_key[..] {
            d.insert("parent".into(), json!(one));
        } else if let [one] = kind[..] {
            d.insert("parent".into(), json!(one));
        }
    }
    for (hub, _, hub_key) in &hubs {
        if hub_key.is_some() {
            continue;
        }
        let keys: std::collections::BTreeSet<String> = devices
            .iter()
            .filter(|d| d.get("parent").and_then(Json::as_str) == Some(hub.as_str()))
            .filter_map(key)
            .collect();
        if keys.len() == 1
            && let Some(d) = devices
                .iter_mut()
                .find(|d| d.get("id").and_then(Json::as_str) == Some(hub.as_str()))
        {
            d.insert("local_key".into(), json!(keys.first()));
        }
    }
}

/// A device already filed keeps its data points exactly as they are, none
/// included (they work, automations name them; a camera's or an infrared
/// remote's would bring `sd_format`, a siren, raw codes): the cloud only
/// brings its local key. Only a new device takes the cloud's points.
pub fn keep_known(import: &mut ImportFile, existing: Option<&DeviceFile>) {
    let Some(existing) = existing else {
        return;
    };
    for d in &mut import.devices {
        if let Some(old) = existing.devices.iter().find(|o| o.id == d.device.id) {
            d.device.dps.clone_from(&old.dps);
        }
    }
}

/// One line per device, for a person to check the import: never a key,
/// only whether there is one.
#[must_use]
pub fn summary(file: &ImportFile) -> Vec<String> {
    file.devices
        .iter()
        .map(|d| {
            let sub = match (&d.device.gateway, &d.device.parent) {
                (Some(_), Some(parent)) => format!(
                    " · sous-appareil de …{}",
                    &parent[parent.len().saturating_sub(6)..]
                ),
                (Some(_), None) => " · sous-appareil, passerelle inconnue".into(),
                _ => String::new(),
            };
            format!(
                "{} [{}] · {} DP · clé {}{sub}",
                d.device.name,
                d.device.category,
                d.device.dps.len(),
                if d.local_key.is_some() { "oui" } else { "non" },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cloud's names for a bulb's and a scene switch's points become
    /// the device's own; a device already filed keeps its points.
    #[test]
    fn the_device_keeps_the_names_it_speaks() {
        let raws = [
            json!({ "id": "bu0000000000000000dddd", "name": "Ampoule", "category": "dj", "local_key": "0123456789abcdef" }),
            json!({ "id": "sw0000000000000000eeee", "name": "Bouton", "category": "wxkg", "sub": true,
                    "node_id": "ee00000000000000", "local_key": "0123456789abcdef" }),
            json!({ "id": "pl0000000000000000cccc", "name": "Prise", "category": "cz", "local_key": "fedcba9876543210" }),
        ];
        let bulb = json!({ "services": [{ "properties": [
            { "abilityId": 20, "code": "switch_led", "accessMode": "rw", "typeSpec": { "type": "bool" } },
            { "abilityId": 22, "code": "bright_value", "accessMode": "rw",
              "typeSpec": { "type": "value", "min": 10, "max": 1000, "scale": 0, "step": 1 } },
            { "abilityId": 24, "code": "colour_data", "accessMode": "rw", "typeSpec": { "type": "string" } },
        ]}]});
        let button = json!({ "services": [{ "properties": [
            { "abilityId": 1, "code": "switch_type_1", "accessMode": "ro",
              "typeSpec": { "type": "enum", "range": ["click", "double_click", "press"] } },
        ]}]});
        let plug = json!({ "services": [{ "properties": [
            { "abilityId": 1, "code": "switch_1", "accessMode": "rw", "typeSpec": { "type": "bool" } },
            { "abilityId": 51, "code": "test_bit", "accessMode": "ro", "typeSpec": { "type": "value" } },
        ]}]});
        let mut file = from_cloud(&raws, &[bulb, button, plug]);
        let codes = |d: &ImportDevice| {
            d.device
                .dps
                .iter()
                .map(|p| p.code.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            codes(&file.devices[0]),
            ["switch_led", "bright_value_v2", "colour_data_v2"]
        );
        assert_eq!(codes(&file.devices[1]), ["switch_mode1"]);
        assert_eq!(codes(&file.devices[2]), ["switch_1"], "no factory point");
        // Already filed (by the export through HA): its points stay.
        let filed: DeviceFile = serde_json::from_value(json!({ "devices": [
            { "id": "pl0000000000000000cccc", "name": "Prise", "category": "cz",
              "dps": [{ "id": 1, "code": "switch_1", "type": "Boolean", "writable": true },
                      { "id": 9, "code": "countdown_1", "type": "Integer", "writable": true }] },
            // Filed without points (a camera, a remote): it stays without.
            { "id": "sw0000000000000000eeee", "name": "Bouton", "category": "wxkg", "dps": [] },
        ]}))
        .unwrap();
        keep_known(&mut file, Some(&filed));
        assert_eq!(codes(&file.devices[2]), ["switch_1", "countdown_1"]);
        assert!(codes(&file.devices[1]).is_empty());
        assert_eq!(
            codes(&file.devices[0]).len(),
            3,
            "a new device keeps the cloud's"
        );
    }

    /// A gateway without a key of its own, a probe behind it (named by its
    /// node), a plug: as the cloud lists them, with fake ids and keys.
    #[test]
    fn the_cloud_list_becomes_the_import_file() {
        let raws = [
            json!({ "id": "gw0000000000000000aaaa", "name": "Passerelle", "category": "wg2",
                    "product_name": "Zigbee Gateway", "node_id": "aa00000000000000", "local_key": "" }),
            json!({ "id": "pr0000000000000000bbbb", "name": " Sonde ", "category": "wsdcg",
                    "sub": true, "node_id": "bb00000000000000", "local_key": "0123456789abcdef" }),
            json!({ "id": "pl0000000000000000cccc", "name": "", "category": "cz",
                    "local_key": "fedcba9876543210" }),
        ];
        let probe = json!({ "services": [{ "properties": [
            { "abilityId": 2, "code": "va_humidity", "accessMode": "ro",
              "typeSpec": { "type": "value", "min": 0, "max": 100, "scale": 0, "step": 1, "unit": "%" } },
            { "abilityId": 1, "code": "va_temperature", "accessMode": "ro",
              "typeSpec": { "type": "value", "min": -200, "max": 600, "scale": 1, "step": 1, "unit": "℃" } },
        ]}]});
        let plug = json!({ "services": [{ "properties": [
            { "abilityId": 1, "code": "switch_1", "accessMode": "rw", "typeSpec": { "type": "bool" } },
            { "abilityId": 38, "code": "relay_status", "accessMode": "rw",
              "typeSpec": { "type": "enum", "range": ["power_off", "power_on", "last"] } },
        ]}]});
        let file = from_cloud(&raws, &[Json::Null, probe, plug]);
        let [gateway, probe, plug] = &file.devices[..] else {
            panic!("three devices");
        };
        // The hub takes its sub-device's key, and is no sub-device itself.
        assert_eq!(gateway.device.gateway, None);
        assert_eq!(gateway.local_key.as_deref(), Some("0123456789abcdef"));
        assert_eq!(probe.device.name, "Sonde");
        assert_eq!(probe.device.gateway.as_deref(), Some("tuya"));
        assert_eq!(probe.device.node_id.as_deref(), Some("bb00000000000000"));
        assert_eq!(
            probe.device.parent.as_deref(),
            Some("gw0000000000000000aaaa")
        );
        let temperature = &probe.device.dps[0];
        assert_eq!((temperature.id, temperature.scale), (1, 1));
        assert!(!temperature.writable);
        assert_eq!(temperature.unit.as_deref(), Some("℃"));
        // No name: its id.
        assert_eq!(plug.device.name, "pl0000000000000000cccc");
        assert!(plug.device.dps[0].writable);
        assert_eq!(plug.device.dps[1].range, ["power_off", "power_on", "last"]);
        // A person checks it without seeing a key.
        let lines = summary(&file).join("\n");
        assert!(!lines.contains("0123456789abcdef") && !lines.contains("fedcba9876543210"));
        assert!(lines.contains("Sonde [wsdcg] · 2 DP · clé oui · sous-appareil de …00aaaa"));
    }
}
