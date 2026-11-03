//! The answers of the Voltalis API decoded and its orders built: pure, without
//! any I/O.

use std::collections::HashSet;

use anyhow::Context as _;
use moli_core::Value;
use serde_json::{Value as Json, json};

/// One heater, as the service knows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Heater {
    pub id: u64,
    pub name: String,
    pub on: Option<bool>,
    pub mode: Option<&'static str>,
    pub target: Option<f64>,
    /// On the service's programme, not on a manual setting.
    pub automatic: Option<bool>,
    /// The modes this heater takes, in Moli's words.
    pub modes: Vec<&'static str>,
    /// « ECOV » instead of « ECO »: the service wants it spelled that way back.
    pub ecov: bool,
}

const TEMPERATURE: &str = "temperature";

fn mode(code: &str) -> Option<&'static str> {
    match code {
        "CONFORT" => Some("confort"),
        "ECO" | "ECOV" => Some("eco"),
        "HORS_GEL" => Some("hors_gel"),
        "TEMPERATURE" => Some("temperature"),
        _ => None,
    }
}

fn list(body: &[u8]) -> anyhow::Result<Vec<Json>> {
    let answer: Json = serde_json::from_slice(body)
        .ok()
        .context(moli_i18n::tr!("pilotes.voltalis.reponse_illisible"))?;
    match answer {
        Json::Array(items) => Ok(items),
        _ => anyhow::bail!(moli_i18n::tr!("pilotes.voltalis.reponse_illisible")),
    }
}

fn decode(entry: &Json) -> Option<Heater> {
    if entry["applianceType"].as_str() != Some("HEATER") {
        return None;
    }
    let programming = &entry["programming"];
    let available = || {
        entry["availableModes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
    };
    let mut modes: Vec<&'static str> = Vec::new();
    for known in available().filter_map(mode) {
        if !modes.contains(&known) {
            modes.push(known);
        }
    }
    Some(Heater {
        id: entry["id"].as_u64()?,
        name: entry["name"].as_str()?.to_owned(),
        on: programming["isOn"].as_bool(),
        mode: programming["mode"].as_str().and_then(mode),
        target: programming["temperatureTarget"]
            .as_f64()
            .filter(|n| n.is_finite() && (0.0..=50.0).contains(n)),
        automatic: programming["progType"].as_str().map(|p| p != "MANUAL"),
        ecov: available().any(|code| code == "ECOV"),
        modes,
    })
}

/// `managed-appliance`: the site's heaters.
pub fn heaters(body: &[u8]) -> anyhow::Result<Vec<Heater>> {
    Ok(list(body)?.iter().filter_map(decode).collect())
}

/// `autodiag`: the heaters the service reports a problem on. Any status but
/// `OK` is one, so a new status is never read as good news.
pub fn faulty(body: &[u8]) -> anyhow::Result<HashSet<u64>> {
    Ok(list(body)?
        .iter()
        .filter(|entry| entry["status"].as_str() != Some("OK"))
        .filter_map(|entry| entry["csApplianceId"].as_u64())
        .collect())
}

/// `auth/login`: the session's bearer token.
pub fn token(body: &[u8]) -> anyhow::Result<String> {
    let answer: Json = serde_json::from_slice(body)
        .ok()
        .context(moli_i18n::tr!("pilotes.voltalis.reponse_illisible"))?;
    answer["token"]
        .as_str()
        .map(str::to_owned)
        .context(moli_i18n::tr!("pilotes.voltalis.reponse_illisible"))
}

/// `account/me`: the site every other request is about.
pub fn site(body: &[u8]) -> anyhow::Result<u64> {
    let answer: Json = serde_json::from_slice(body)
        .ok()
        .context(moli_i18n::tr!("pilotes.voltalis.reponse_illisible"))?;
    answer["defaultSite"]["id"]
        .as_u64()
        .context(moli_i18n::tr!("pilotes.voltalis.reponse_illisible"))
}

/// `manualsetting`: the setting that commands this heater. Every heater has one,
/// used or not, and a heater on its programme may not name it.
pub fn setting_of(body: &[u8], heater: u64) -> anyhow::Result<u64> {
    list(body)?
        .iter()
        .find(|entry| entry["idAppliance"].as_u64() == Some(heater))
        .and_then(|entry| entry["id"].as_u64())
        .context(moli_i18n::tr!("pilotes.voltalis.reponse_illisible"))
}

fn wire_mode(heater: &Heater, mode: &str) -> Option<&'static str> {
    match mode {
        "confort" => Some("CONFORT"),
        "eco" if heater.ecov => Some("ECOV"),
        "eco" => Some("ECO"),
        "hors_gel" => Some("HORS_GEL"),
        TEMPERATURE => Some("TEMPERATURE"),
        _ => None,
    }
}

/// The body of `PUT manualsetting/{id}` for a point set to `value` (already
/// checked against the point). `now` closes a manual setting: a setting that is
/// not enabled still has to say when it ended. What stops an order is said.
///
/// Manual orders hold until further notice: a heater does what it was last told,
/// until told otherwise or put back on its programme (`automatic`).
pub fn order(heater: &Heater, key: &str, value: &Value, now: &str) -> Result<Json, String> {
    let refused = || moli_i18n::tr!("pilotes.voltalis.ne_se_regle_pas", point = key);
    // An unknown mode must not become one the heater never announced.
    let current_mode = heater
        .mode
        .or_else(|| heater.modes.first().copied())
        .ok_or_else(refused)?;
    let current_target = heater.target.unwrap_or(FALLBACK_TARGET);
    let manual = |is_on: bool, mode: &str, target: f64| {
        let mode = wire_mode(heater, mode).ok_or_else(refused)?;
        Ok::<Json, String>(json!({
            "idAppliance": heater.id,
            "enabled": true,
            "untilFurtherNotice": true,
            "isOn": is_on,
            "mode": mode,
            "endDate": null,
            "temperatureTarget": target,
        }))
    };
    match (key, value) {
        ("on", Value::Bool(on)) => manual(*on, current_mode, current_target),
        ("mode", Value::Text(mode)) if heater.modes.contains(&mode.as_ref()) => {
            manual(true, mode, current_target)
        }
        ("target_temperature", wanted) if heater.modes.contains(&TEMPERATURE) => {
            let target = wanted.as_f64().ok_or_else(refused)?;
            manual(true, TEMPERATURE, target)
        }
        ("automatic", Value::Bool(false)) => {
            manual(heater.on.unwrap_or(true), current_mode, current_target)
        }
        ("automatic", Value::Bool(true)) => Ok(json!({
            "idAppliance": heater.id,
            "enabled": false,
            "untilFurtherNotice": false,
            "isOn": true,
            "mode": wire_mode(heater, current_mode).ok_or_else(refused)?,
            "endDate": now,
            "temperatureTarget": current_target,
        })),
        _ => Err(refused()),
    }
}

/// When the heater has never said its setpoint; the service's own default.
const FALLBACK_TARGET: f64 = 18.0;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const APPLIANCES: &[u8] =
        include_bytes!("../../../integrations/voltalis/fixtures/managed_appliance.json");
    const DIAG: &[u8] = include_bytes!("../../../integrations/voltalis/fixtures/autodiag.json");

    #[test]
    fn a_real_answer_gives_every_heater() {
        let found = heaters(APPLIANCES).unwrap();
        assert_eq!(found.len(), 4);
        assert_eq!(found[0].id, 1000);
        assert_eq!(found[0].mode, Some("confort"));
        assert_eq!(found[0].on, Some(false));
        assert_eq!(found[3].mode, Some("temperature"));
        assert_eq!(found[3].target, Some(17.0));
        assert_eq!(found[0].automatic, Some(false));
        assert_eq!(
            found[0].modes,
            ["eco", "confort", "temperature", "hors_gel"]
        );
    }

    fn heater() -> Heater {
        heaters(APPLIANCES).unwrap().remove(0)
    }

    const NOW: &str = "2026-11-03T12:00:00Z";

    #[test]
    fn switching_off_keeps_the_mode_for_the_next_time() {
        let body = order(&heater(), "on", &Value::Bool(false), NOW).unwrap();
        assert_eq!(body["enabled"], json!(true));
        assert_eq!(body["isOn"], json!(false));
        assert_eq!(body["mode"], json!("CONFORT"));
        assert_eq!(body["untilFurtherNotice"], json!(true));
        assert_eq!(body["endDate"], Json::Null);
    }

    #[test]
    fn choosing_a_mode_or_a_setpoint_also_switches_on() {
        let body = order(&heater(), "mode", &Value::from("hors_gel"), NOW).unwrap();
        assert_eq!(body["isOn"], json!(true));
        assert_eq!(body["mode"], json!("HORS_GEL"));
        let body = order(&heater(), "target_temperature", &Value::Float(19.5), NOW).unwrap();
        assert_eq!(body["mode"], json!("TEMPERATURE"));
        assert_eq!(body["temperatureTarget"], json!(19.5));
    }

    #[test]
    fn back_to_the_programme_closes_the_manual_setting() {
        let body = order(&heater(), "automatic", &Value::Bool(true), NOW).unwrap();
        assert_eq!(body["enabled"], json!(false));
        assert_eq!(body["untilFurtherNotice"], json!(false));
        assert_eq!(body["endDate"], json!(NOW));
    }

    #[test]
    fn eco_is_spelled_as_the_heater_spells_it() {
        let mut ecov = heater();
        ecov.ecov = true;
        ecov.modes = vec!["eco"];
        let body = order(&ecov, "mode", &Value::from("eco"), NOW).unwrap();
        assert_eq!(body["mode"], json!("ECOV"));
    }

    #[test]
    fn what_the_heater_does_not_take_is_never_sent() {
        let mut limited = heater();
        limited.modes = vec!["eco", "confort"];
        assert!(order(&limited, "mode", &Value::from("hors_gel"), NOW).is_err());
        assert!(order(&limited, "target_temperature", &Value::Float(20.0), NOW).is_err());
        assert!(order(&limited, "fault", &Value::Bool(true), NOW).is_err());
        assert!(order(&limited, "on", &Value::from("oui"), NOW).is_err());
    }

    #[test]
    fn an_unknown_mode_falls_back_on_one_the_heater_announces() {
        let mut limited = heater();
        limited.mode = None;
        limited.modes = vec!["confort"];
        let body = order(&limited, "on", &Value::Bool(false), NOW).unwrap();
        assert_eq!(body["mode"], json!("CONFORT"));
        limited.modes = Vec::new();
        assert!(order(&limited, "on", &Value::Bool(false), NOW).is_err());
    }

    #[test]
    fn eco_and_ecov_are_one_choice() {
        let body = json!([{
            "id": 1,
            "name": "Unit",
            "applianceType": "HEATER",
            "availableModes": ["ECO", "CONFORT", "ECOV"],
        }]);
        let found = &heaters(body.to_string().as_bytes()).unwrap()[0];
        assert_eq!(found.modes, ["eco", "confort"]);
    }

    #[test]
    fn the_setting_of_a_heater_is_found_or_the_order_stops() {
        let body = br#"[{"id":7,"idAppliance":1000},{"id":8,"idAppliance":2000}]"#;
        assert_eq!(setting_of(body, 1000).unwrap(), 7);
        assert!(setting_of(body, 3).is_err());
    }

    #[test]
    fn a_real_diagnosis_names_the_faulty_heater() {
        assert_eq!(faulty(DIAG).unwrap(), HashSet::from([1000]));
    }

    #[test]
    fn an_unknown_diagnosis_counts_as_a_problem() {
        let body = json!([{ "status": "SOMETHING_NEW", "csApplianceId": 7 }]);
        assert!(faulty(body.to_string().as_bytes()).unwrap().contains(&7));
    }

    #[test]
    fn a_missing_or_absurd_reading_is_unknown_and_never_invented() {
        let body = json!([{
            "id": 1,
            "name": "Unit",
            "applianceType": "HEATER",
            "programming": { "mode": "NEW_MODE", "temperatureTarget": 900.0 },
        }]);
        let found = &heaters(body.to_string().as_bytes()).unwrap()[0];
        assert_eq!(found.on, None);
        assert_eq!(found.mode, None);
        assert_eq!(found.target, None);
    }

    #[test]
    fn only_heaters_are_described() {
        let body = json!([{ "id": 1, "name": "Water", "applianceType": "WATER_HEATER" }]);
        assert!(heaters(body.to_string().as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn login_and_account_answers_are_read() {
        assert_eq!(token(br#"{"token":"abc"}"#).unwrap(), "abc");
        assert!(token(b"{}").is_err());
        assert_eq!(site(br#"{"defaultSite":{"id":12}}"#).unwrap(), 12);
        assert!(site(b"<html>").is_err());
    }
}
