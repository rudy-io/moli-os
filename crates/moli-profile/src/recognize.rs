//! Which built-in profile a device of the home network is, from its address
//! alone: what lets a person add it from the dashboard.
//!
//! Each profile that lists the models it is for (`recognize`) is asked the
//! request its `name_from` comes from, once, as a read: never a write, never
//! a request the profile does not declare. A profile with a key (Meross) is
//! asked with an empty key first (a plug set up through Matter or HomeKit,
//! never bound to an account), then with each key the house already holds
//! for that kind of device; a key is never sent in clear, only used to sign.
//!
//! A signed read is not proof of the key: some plugs answer one whatever
//! signs it, then drop any order not encrypted with their real key. A plug
//! that lists `Appliance.Encrypt.ECDHE` is therefore asked once more, the
//! same read encrypted with the key tried: only the key it answers counts.

use std::net::Ipv4Addr;

use crate::profile::{Answer, Call, Compiled, CompiledRequest, Format, Profile, Source};
use crate::{MAX_BODY, Target, exchange, meross, meross_ask, meross_message, unreachable};

/// The key a recognized device took.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyUse {
    /// Its profile has no key.
    NotNeeded,
    /// It answered a message signed with an empty key (`empty_secret`).
    Empty,
    /// It answered a message signed with this one of the keys given.
    Known(usize),
}

/// A device that said who it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recognized {
    /// The built-in profile it runs with.
    pub profile: &'static str,
    /// The profile's name for this kind of device.
    pub name: String,
    pub manufacturer: Option<String>,
    /// The model, as the device names itself (`mss315`).
    pub model: String,
    /// What identifies it for good (the device id is `<instance>:<identity>`).
    pub identity: String,
    /// Its MAC, as the device declares it (12 lowercase hex digits).
    pub mac: Option<String>,
    pub key: KeyUse,
}

/// What the address answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recognition {
    Recognized(Recognized),
    /// A device of a kind Moli knows answered, but refused every key tried:
    /// it is bound to an account whose key the house does not hold yet.
    NeedsKey {
        manufacturer: Option<String>,
        /// The secret's name in the vault (`key`).
        secret: String,
    },
    /// It answered nothing a profile recognizes.
    Unknown,
    /// Nobody answered there.
    Silent,
}

/// The built-in profiles that can recognize their devices.
fn candidates() -> Vec<(&'static str, Compiled)> {
    crate::BUILTIN
        .iter()
        .filter_map(|(id, text)| {
            let compiled = Profile::parse(text).ok()?.compile(Source::Builtin).ok()?;
            (!compiled.meta.recognize.is_empty()).then_some((*id, compiled))
        })
        .collect()
}

/// `(request, path)` of a `request.path` reference.
fn split(reference: Option<&str>) -> Option<(&str, Vec<String>)> {
    let (request, path) = reference?.split_once('.')?;
    Some((request, path.split('.').map(str::to_owned).collect()))
}

/// What one ask came back with.
#[derive(Clone)]
enum Asked {
    Answer(Answer),
    /// The device answered, refusing the signature (wrong key).
    Refused,
    /// It answered something else (another kind of device).
    Other,
    Silent,
}

/// One read of `request`, signed with `key` when it is a Meross message.
async fn ask(target: &Target, request: &CompiledRequest, key: Option<&str>) -> Asked {
    let (body, meross) = match &request.call {
        Call::Get => (None, false),
        Call::Meross { namespace, payload } => {
            let Some(key) = key else { return Asked::Other };
            let message = meross_message("GET", namespace, payload, key);
            (Some(message.to_string().into_bytes()), true)
        }
    };
    // A request that needs a placeholder filled cannot be asked from an
    // address alone.
    if request.path.contains('{') || !request.headers.is_empty() {
        return Asked::Other;
    }
    match exchange(target, &request.path, &[], &request.path, body, MAX_BODY).await {
        Err(e) if unreachable(&e) => Asked::Silent,
        Err(_) => Asked::Other,
        Ok(bytes) => {
            let Some(answer) = Answer::parse(request.format, &bytes) else {
                return Asked::Other;
            };
            if meross {
                let Answer::Json(json) = &answer else {
                    return Asked::Other;
                };
                match json["header"]["method"].as_str() {
                    Some("GETACK") => {}
                    Some("ERROR") => return Asked::Refused,
                    _ => return Asked::Other,
                }
            }
            Asked::Answer(answer)
        }
    }
}

/// The request a profile's model comes from, as it goes on the wire (two
/// profiles that ask the same thing are asked once).
fn signature(profile: &Compiled, request: &CompiledRequest) -> String {
    format!(
        "{:?}|{}|{}|{:?}|{}",
        profile.meta.port,
        profile.meta.https,
        request.path,
        request.call,
        matches!(request.format, Format::Json)
    )
}

/// Who answers at `host` (`port`: tests only, else the profile's own),
/// trying an empty key, then `keys` in order, on the profiles that sign.
pub async fn recognize(host: Ipv4Addr, port: Option<u16>, keys: &[String]) -> Recognition {
    let profiles = candidates();
    // (signature, key index: None = empty key or none) → what came back.
    let mut asked: Vec<(String, Option<usize>, Asked)> = Vec::new();
    let mut needs_key: Option<(Option<String>, String)> = None;
    let mut anyone = false;
    for (id, profile) in &profiles {
        let Some((request_id, model_path)) = split(profile.meta.name_from.as_deref()) else {
            continue;
        };
        let Some(request) = profile.requests.iter().find(|r| r.id == request_id) else {
            continue;
        };
        let target = Target {
            host: host.to_string(),
            port: port
                .or(profile.meta.port)
                .unwrap_or(if profile.meta.https { 443 } else { 80 }),
            tls: profile.meta.https,
            vars: std::collections::BTreeMap::new(),
        };
        let wire = signature(profile, request);
        // `None`: an empty key (or none for a profile without one).
        let tries: Vec<Option<usize>> = if profile.meta.secret.is_some() {
            std::iter::once(None)
                .chain((0..keys.len()).map(Some))
                .collect()
        } else {
            vec![None]
        };
        for attempt in tries {
            let cached = asked
                .iter()
                .find(|(s, k, _)| *s == wire && *k == attempt)
                .map(|(_, _, a)| a.clone());
            let result = if let Some(result) = cached {
                result
            } else {
                let key = profile
                    .meta
                    .secret
                    .as_ref()
                    .map(|_| attempt.map_or("", |i| keys[i].as_str()));
                let result = ask(&target, request, key).await;
                asked.push((wire.clone(), attempt, result.clone()));
                result
            };
            match result {
                Asked::Silent => break,
                Asked::Other => {
                    anyone = true;
                    break;
                }
                Asked::Refused => {
                    anyone = true;
                    if let Some(secret) = &profile.meta.secret {
                        needs_key.get_or_insert_with(|| {
                            (profile.meta.manufacturer.clone(), secret.clone())
                        });
                    }
                }
                Asked::Answer(answer) => {
                    anyone = true;
                    if let (Some(key), Call::Meross { .. }) = (
                        profile
                            .meta
                            .secret
                            .as_ref()
                            .map(|_| attempt.map_or("", |i| keys[i].as_str())),
                        &request.call,
                    ) && !takes_orders_with(&target, &request.path, key, &answer).await
                    {
                        // It reads with any key: this one is not its own.
                        if let Some(secret) = &profile.meta.secret {
                            needs_key.get_or_insert_with(|| {
                                (profile.meta.manufacturer.clone(), secret.clone())
                            });
                        }
                        continue;
                    }
                    let found = read_answer(id, profile, request_id, &model_path, &answer, attempt);
                    match found {
                        Some(found) => return Recognition::Recognized(found),
                        None => break,
                    }
                }
            }
        }
    }
    match needs_key {
        Some((manufacturer, secret)) => Recognition::NeedsKey {
            manufacturer,
            secret,
        },
        None if anyone => Recognition::Unknown,
        None => Recognition::Silent,
    }
}

/// Whether a Meross device would take orders signed with `key`: one that
/// encrypts them must answer the same read encrypted with that key (a read:
/// `Appliance.System.Ability`, nothing changes). `all` is its answer to
/// `Appliance.System.All`, for its uuid and MAC.
async fn takes_orders_with(target: &Target, path: &str, key: &str, all: &Answer) -> bool {
    let Ok(ability) = meross_ask(target, path, "Appliance.System.Ability", key).await else {
        return false;
    };
    if ability["payload"]["ability"]
        .get(meross::ENCRYPTED)
        .is_none()
    {
        return true;
    }
    let Answer::Json(all) = all else { return false };
    let hardware = &all["payload"]["all"]["system"]["hardware"];
    let (Some(uuid), Some(mac)) = (hardware["uuid"].as_str(), hardware["macAddress"].as_str())
    else {
        return false;
    };
    let cipher = meross::Cipher::new(uuid, key, mac);
    let message = meross_message(
        "GET",
        "Appliance.System.Ability",
        &serde_json::json!({}),
        key,
    );
    let sealed = cipher.seal(message.to_string().as_bytes());
    let Ok(body) = exchange(target, path, &[], path, Some(sealed), MAX_BODY).await else {
        return false; // a wrong key: the connection dropped, nothing said
    };
    cipher
        .open(&body)
        .and_then(|clear| serde_json::from_slice::<serde_json::Value>(&clear).ok())
        .is_some_and(|answer| answer["header"]["method"] == "GETACK")
}

/// The device in `answer`, when its model is one `profile` recognizes.
fn read_answer(
    id: &'static str,
    profile: &Compiled,
    request_id: &str,
    model_path: &[String],
    answer: &Answer,
    attempt: Option<usize>,
) -> Option<Recognized> {
    let model = answer.text(model_path)?.to_lowercase();
    if !profile.meta.recognize.contains(&model) {
        return None;
    }
    // Read in the same answer, or not at all: one read per device.
    let field = |reference: Option<&str>| {
        split(reference)
            .filter(|(r, _)| *r == request_id)
            .and_then(|(_, path)| answer.text(&path))
    };
    let identity = field(profile.meta.identity.as_deref())?;
    let mac = field(profile.meta.mac.as_deref()).and_then(|m| moli_net::lan::mac_hex(&m));
    let key = match (&profile.meta.secret, attempt) {
        (None, _) => KeyUse::NotNeeded,
        (Some(_), None) => KeyUse::Empty,
        (Some(_), Some(i)) => KeyUse::Known(i),
    };
    Some(Recognized {
        profile: id,
        name: profile.meta.name.clone(),
        manufacturer: profile.meta.manufacturer.clone(),
        model,
        identity,
        mac,
        key,
    })
}

/// The built-in profiles a person may add from the dashboard.
#[must_use]
pub fn addable(profile: &str) -> bool {
    crate::builtin(profile)
        .and_then(|text| Profile::parse(text).ok())
        .is_some_and(|p| !p.profile.recognize.is_empty())
}
