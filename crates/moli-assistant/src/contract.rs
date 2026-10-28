//! The contract between Moli and its house model: the tools it calls and the
//! system prompt it reads. See `tools/model/` for the data built against it.

use anyhow::Context as _;
use serde_json::Value as Json;

/// The frozen tools, shared with the data generator (`tools/model/`).
pub const TOOLS_JSON: &str = include_str!("../../../tools/model/contract/tools.json");

fn document() -> anyhow::Result<Json> {
    serde_json::from_str(TOOLS_JSON).context("tools/model/contract/tools.json")
}

/// The contract's version: bumped whenever a tool or the prompt changes.
pub fn version() -> anyhow::Result<u64> {
    document()?["version"]
        .as_u64()
        .context("tools.json has no numeric `version`")
}

/// The contract's name as a provider declares it (`moli-v4`).
pub fn name() -> anyhow::Result<String> {
    Ok(format!("moli-v{}", version()?))
}

/// The tools the house model is trained to call, as JSON values (tests; the
/// `model tools` command prints `TOOLS_JSON` as it is).
#[cfg(test)]
pub fn tools() -> anyhow::Result<Json> {
    Ok(document()?["tools"].clone())
}

use std::collections::BTreeMap;
use std::sync::Arc;

use moli_core::{Device, Label, Sample};
use moli_runtime::DeviceView;
use serde::Deserialize;

use crate::house::{self, Layout};

/// A device as a house file writes it: what `DeviceView` serialises to.
#[derive(Deserialize)]
struct DeviceIn {
    #[serde(flatten)]
    device: Device,
    #[serde(default)]
    label: Label,
    #[serde(default)]
    online: Option<bool>,
    #[serde(default)]
    state: BTreeMap<Arc<str>, Sample>,
    #[serde(default)]
    camera: bool,
    #[serde(default)]
    printer: bool,
}

/// A house for the data generator: its rooms, its protected rooms, its devices.
#[derive(Deserialize)]
struct HouseIn {
    #[serde(default)]
    layout: Layout,
    #[serde(default)]
    protected: Vec<String>,
    devices: Vec<DeviceIn>,
}

/// The system prompt of a house, and which device each number in it stands for.
#[derive(Debug)]
pub struct HousePrompt {
    pub prompt: String,
    /// `d1` → `z2m:plafonnier_salon_1`: the model writes numbers, Moli reads ids.
    pub aliases: BTreeMap<String, String>,
}

impl HousePrompt {
    /// `{"prompt": "…", "aliases": {"d1": "…"}}`, what `moli-os model prompt` prints.
    pub fn to_json(&self) -> String {
        serde_json::json!({ "prompt": self.prompt, "aliases": self.aliases }).to_string()
    }
}

/// The system prompt the house model reads: the contract's version, the rooms
/// and the house's **static** inventory (devices numbered, no value, no date:
/// the prompt only moves when the house does, so the provider can keep it in
/// cache). French, frozen: it does not follow the interface language.
pub fn render_prompt(house_json: &str) -> anyhow::Result<HousePrompt> {
    ensure_french()?;
    let house: HouseIn = serde_json::from_str(house_json).context("house")?;
    let devices: Vec<DeviceView> = house
        .devices
        .into_iter()
        .map(|d| DeviceView {
            device: Arc::new(d.device),
            label: d.label,
            online: d.online,
            state: d.state,
            camera: d.camera,
            printer: d.printer,
        })
        .collect();
    let (inventory, aliases) = house::inventory_static(&devices, &house.layout, &house.protected);
    let rooms: Vec<&str> = house.layout.rooms.iter().map(|r| r.name.as_str()).collect();
    Ok(HousePrompt {
        prompt: prompt(&rooms, &house.protected, &inventory)?,
        aliases: aliases
            .into_iter()
            .map(|(alias, id)| (alias, id.as_str().to_owned()))
            .collect(),
    })
}

/// The inventory speaks the process's language (`tr!`): the contract is French.
pub(crate) fn ensure_french() -> anyhow::Result<()> {
    anyhow::ensure!(
        moli_i18n::language() == moli_i18n::FALLBACK,
        "the house model prompt is French: the process language must be {}",
        moli_i18n::FALLBACK
    );
    Ok(())
}

/// The contract's prompt around an inventory already written.
pub(crate) fn prompt(
    rooms: &[&str],
    protected: &[String],
    inventory: &str,
) -> anyhow::Result<String> {
    let one_line = crate::house::one_line;
    let protected = if protected.is_empty() {
        "aucune".to_owned()
    } else {
        protected
            .iter()
            .map(|p| one_line(p))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let rooms: Vec<String> = rooms.iter().map(|r| one_line(r)).collect();
    Ok(format!(
        "Contrat Moli v{}. Tu pilotes la maison par appels d'outils.\nPièces : {}.\nPièces protégées : {}.\nAppareils :\n{}",
        version()?,
        rooms.join(", "),
        protected,
        inventory
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value as Json, json};

    fn by_name(tools: &Json, name: &str) -> Json {
        tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["function"]["name"] == name)
            .unwrap_or_else(|| panic!("no tool {name}"))["function"]["parameters"]
            .clone()
    }

    /// The shape of a parameter list without its prose: property names and
    /// types (and `enum`, `anyOf`), and `required`.
    fn shape(parameters: &Json) -> Json {
        fn strip(v: &Json) -> Json {
            match v {
                Json::Object(map) => Json::Object(
                    map.iter()
                        .filter(|(k, _)| k.as_str() != "description")
                        .map(|(k, v)| (k.clone(), strip(v)))
                        .collect(),
                ),
                Json::Array(items) => Json::Array(items.iter().map(strip).collect()),
                other => other.clone(),
            }
        }
        let mut copy = strip(parameters);
        if copy.get("required").is_none() {
            copy["required"] = Json::Array(Vec::new());
        }
        copy
    }

    #[test]
    fn the_contract_has_a_version_and_valid_tools() {
        assert_eq!(version().unwrap(), 4);
        let tools = tools().unwrap();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "set",
                "set_all",
                "adjust",
                "get_device",
                "find",
                "problems",
                "energy",
                "history",
                "last_change",
                "now",
                "weather",
                "refresh_weather",
                "ambiance",
                "schedule",
                "remind",
                "rename_device",
                "move_device",
                "set_device_enabled",
                "rename_room",
                "add_person",
                "set_home_location",
                "undo",
                "draft_automation",
                "request_driver",
                "delegate",
            ]
        );
    }

    #[test]
    fn the_frozen_tools_match_the_tools_moli_really_offers() {
        let live = crate::tools();
        let frozen = tools().unwrap();
        for name in ["get_device", "energy", "history"] {
            assert_eq!(
                shape(&by_name(&frozen, name)),
                shape(&by_name(&live, name)),
                "tool {name} diverges from the one Moli offers: update tools/model/contract/tools.json and bump the version"
            );
        }
    }

    /// Moli words an order itself: the model does not write what to say.
    #[test]
    fn an_order_is_a_point_and_a_value_nothing_to_say() {
        let frozen = shape(&by_name(&tools().unwrap(), "set"));
        let live = shape(&by_name(&crate::tools(), "set"));
        assert_eq!(frozen["required"], json!(["point", "value"]));
        let names = |p: &Json| -> Vec<String> {
            p["properties"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect()
        };
        assert_eq!(names(&frozen), ["point", "value"]);
        assert!(
            names(&live).contains(&"say".to_owned()),
            "the general chat still asks for it"
        );
    }

    const FIXTURE_HOUSE: &str = include_str!("../../../tools/model/contract/fixture_house.json");
    const FIXTURE_PROMPT: &str = include_str!("../../../tools/model/contract/fixture_prompt.txt");

    #[test]
    fn the_prompt_is_the_static_inventory_with_numbered_devices() {
        let house = render_prompt(FIXTURE_HOUSE).unwrap();
        let prompt = &house.prompt;
        assert!(prompt.starts_with("Contrat Moli v4."), "{prompt}");
        assert!(prompt.contains("Pièces : Salon, Cuisine, Chambre 1.\n"));
        assert!(prompt.contains("Pièces protégées : Chambre 1.\n"));
        assert!(prompt.contains("Appareils :\n## Salon\n"), "{prompt}");
        // nothing that moves: no date, no value, no word about reachability
        assert!(!prompt.contains("Date"), "{prompt}");
        assert!(
            !prompt.contains("=false") && !prompt.contains("22.5"),
            "{prompt}"
        );
        assert!(!prompt.contains("injoignable"), "{prompt}");
        // writable points first with their choices or range, readings with their unit
        assert!(
            prompt.contains("· Plafonnier · state✎, brightness✎ (0–100 %)\n"),
            "{prompt}"
        );
        assert!(
            prompt.contains("· Climatisation · mode✎ [auto|froid|chaud], temperature °C\n"),
            "{prompt}"
        );
        // a protected room is marked, a device without a room is gathered apart
        assert!(prompt.contains("## Chambre 1 (protégée : "), "{prompt}");
        assert!(prompt.contains("## Sans pièce\n"), "{prompt}");
        // every number stands for a real device
        assert_eq!(
            house.aliases.get("d1").map(String::as_str),
            Some("z2m:camera_salon_1")
        );
        assert!(house.aliases.values().all(|id| id.starts_with("z2m:")));
        assert_eq!(house.aliases.len(), prompt.matches("\n- d").count());
    }

    #[test]
    fn the_golden_prompt_is_stable() {
        // Regenerate with MOLI_BLESS=1 after a deliberate change (and bump the version).
        let prompt = render_prompt(FIXTURE_HOUSE).unwrap().prompt;
        if std::env::var_os("MOLI_BLESS").is_some() {
            std::fs::write(
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tools/model/contract/fixture_prompt.txt"
                ),
                &prompt,
            )
            .unwrap();
            return;
        }
        assert_eq!(
            prompt, FIXTURE_PROMPT,
            "the prompt changed: bless it and bump the contract version"
        );
    }

    const SNAPSHOTS: &str = include_str!("../../../tools/model/contract/snapshots.txt");

    /// What `moli-os model tools` and `moli-os model prompt` (on the fixture
    /// house) print, as one SHA-256: the whole contract a model is trained on.
    fn contract_digest() -> String {
        let prompt = render_prompt(FIXTURE_HOUSE).unwrap().to_json();
        let mut both = TOOLS_JSON.as_bytes().to_vec();
        both.push(0);
        both.extend_from_slice(prompt.as_bytes());
        ring::digest::digest(&ring::digest::SHA256, &both)
            .as_ref()
            .iter()
            .fold(String::new(), |mut hex, b| {
                use std::fmt::Write as _;
                let _ = write!(hex, "{b:02x}");
                hex
            })
    }

    #[test]
    fn the_contract_cannot_change_without_a_new_version() {
        // One line per version: `v<N> <sha256>`. A new version adds its line
        // (MOLI_BLESS=1); the line of a version is never rewritten, since
        // models were trained on it.
        let version = format!("v{}", version().unwrap());
        let digest = contract_digest();
        let known = SNAPSHOTS
            .lines()
            .filter_map(|l| l.split_once(' '))
            .find(|(v, _)| *v == version)
            .map(|(_, d)| d.trim().to_owned());
        match known {
            Some(frozen) => assert_eq!(
                digest, frozen,
                "the tools or the prompt changed but the contract is still {version}: bump `version` in tools.json"
            ),
            None if std::env::var_os("MOLI_BLESS").is_some() => {
                let path = concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../tools/model/contract/snapshots.txt"
                );
                std::fs::write(path, format!("{SNAPSHOTS}{version} {digest}\n")).unwrap();
            }
            None => panic!("no snapshot for contract {version}: add it with MOLI_BLESS=1"),
        }
    }

    #[test]
    fn a_bad_house_is_a_clear_error() {
        let e = format!("{:#}", render_prompt("{\"devices\": 3}").unwrap_err());
        assert!(e.contains("house"), "{e}");
    }
}
