//! TP-Link Kasa plugs on their legacy local protocol: JSON on TCP 9999, each
//! message « encrypted » by an XOR autokey (171) and prefixed with its length
//! (4 bytes, big-endian). No account, no cloud. First used with an HS110
//! (hardware 1.0), the plug with an energy meter.
//!
//! Points: `state` (the relay, writable), `led` (its light, writable), and
//! for a plug that measures: `power` (W), `voltage` (V), `current` (A),
//! `energy` (kWh, its lifetime counter). Hardware 1 says them in W, V, A and
//! kWh; hardware 2 in mW, mV, mA and Wh: both are read.
//!
//! Newer firmware that speaks only KLAP (port 80) is the `tapo` driver's.

use std::time::Duration;

use anyhow::{Context as _, bail};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

const LIMIT: Duration = Duration::from_secs(5);
/// An answer longer than this is not a plug's.
const MAX_ANSWER: usize = 64 * 1024;
const RETRY_MAX: Duration = Duration::from_secs(60);

fn default_port() -> u16 {
    9999
}

fn default_poll() -> u64 {
    10
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Seconds between two readings (2 to 3600).
    #[serde(default = "default_poll")]
    pub poll_seconds: u64,
}

#[derive(Debug)]
pub struct Kasa {
    config: Config,
}

impl Kasa {
    /// # Errors
    /// A polling period out of 2..=3600 s, or no host.
    pub fn new(config: Config) -> anyhow::Result<Self> {
        if !(2..=3600).contains(&config.poll_seconds) {
            bail!("poll_seconds {}: from 2 to 3600", config.poll_seconds);
        }
        if config.host.trim().is_empty() {
            bail!("host is needed");
        }
        Ok(Self { config })
    }
}

impl Driver for Kasa {
    fn kind(&self) -> &'static str {
        "kasa"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// A message as the plug takes it: length, then each byte XORed with the
/// previous encrypted one (171 for the first).
#[must_use]
pub fn encrypt(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(bytes.len() + 4);
    out.extend_from_slice(&len.to_be_bytes());
    let mut key = 171u8;
    for &b in bytes {
        key ^= b;
        out.push(key);
    }
    out
}

/// The plug's answer (without its length prefix) in clear.
#[must_use]
pub fn decrypt(bytes: &[u8]) -> String {
    let mut key = 171u8;
    let clear: Vec<u8> = bytes
        .iter()
        .map(|&c| {
            let b = key ^ c;
            key = c;
            b
        })
        .collect();
    String::from_utf8_lossy(&clear).into_owned()
}

/// One request on its own connection (the plug closes it after answering).
async fn ask(config: &Config, request: &Json) -> anyhow::Result<Json> {
    let exchange = async {
        let mut tcp = TcpStream::connect((config.host.as_str(), config.port))
            .await
            .with_context(|| format!("cannot reach {}:{}", config.host, config.port))?;
        tcp.write_all(&encrypt(&request.to_string())).await?;
        let mut len = [0u8; 4];
        tcp.read_exact(&mut len).await?;
        let len = usize::try_from(u32::from_be_bytes(len))?;
        if len > MAX_ANSWER {
            bail!("an answer of {len} bytes: not a Kasa plug");
        }
        let mut body = vec![0u8; len];
        tcp.read_exact(&mut body).await?;
        Ok(serde_json::from_str(&decrypt(&body))?)
    };
    tokio::time::timeout(LIMIT, exchange)
        .await
        .context("the plug did not answer in time")?
}

/// What `get_sysinfo` says.
#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub mac: String,
    pub alias: String,
    pub model: String,
    pub on: bool,
    pub led: bool,
    /// It has an energy meter (`feature` holds `ENE`).
    pub meter: bool,
}

#[must_use]
pub fn info(json: &Json) -> Option<Info> {
    let s = &json["system"]["get_sysinfo"];
    let text = |k: &str| s[k].as_str().unwrap_or_default().to_owned();
    let mac = text("mac");
    if mac.is_empty() {
        return None;
    }
    Some(Info {
        mac,
        alias: text("alias"),
        model: text("model"),
        on: s["relay_state"].as_u64() == Some(1),
        led: s["led_off"].as_u64() != Some(1),
        meter: text("feature").contains("ENE"),
    })
}

/// The meter now: power (W), voltage (V), current (A), energy (kWh).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reading {
    pub power: Option<f64>,
    pub voltage: Option<f64>,
    pub current: Option<f64>,
    pub energy: Option<f64>,
}

#[must_use]
pub fn reading(json: &Json) -> Option<Reading> {
    let r = &json["emeter"]["get_realtime"];
    if !r.is_object() || r["err_code"].as_i64().is_some_and(|e| e != 0) {
        return None;
    }
    // Hardware 1 in units, hardware 2 in thousandths.
    let either = |unit: &str, milli: &str| {
        r[unit]
            .as_f64()
            .or_else(|| r[milli].as_f64().map(|v| v / 1000.0))
            .filter(|v| v.is_finite())
    };
    let round = |v: Option<f64>, f: f64| v.map(|v| (v * f).round() / f);
    Some(Reading {
        power: round(either("power", "power_mw"), 10.0),
        voltage: round(either("voltage", "voltage_mv"), 10.0),
        current: round(either("current", "current_ma"), 1000.0),
        energy: round(either("total", "total_wh"), 1000.0),
    })
}

fn point(key: &str, label: &str, kind: Kind, write: bool, unit: Option<Unit>) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        semantic: Semantic::infer(key, unit.as_ref()),
        kind,
        access: Access { read: true, write },
        unit,
    }
}

fn number() -> Kind {
    Kind::Numeric {
        min: None,
        max: None,
        step: None,
    }
}

fn device(ctx: &DriverCtx, id: &DeviceId, info: &Info) -> Device {
    let mut points = vec![
        point(
            "state",
            &moli_i18n::tr!("pilotes.kasa.prise"),
            Kind::Binary,
            true,
            None,
        ),
        point(
            "led",
            &moli_i18n::tr!("pilotes.kasa.voyant"),
            Kind::Binary,
            true,
            None,
        ),
    ];
    if info.meter {
        points.extend([
            point(
                "power",
                &moli_i18n::tr!("pilotes.kasa.puissance"),
                number(),
                false,
                Some(Unit::Watt),
            ),
            point(
                "voltage",
                &moli_i18n::tr!("pilotes.kasa.tension"),
                number(),
                false,
                Some(Unit::Volt),
            ),
            point(
                "current",
                &moli_i18n::tr!("pilotes.kasa.courant"),
                number(),
                false,
                Some(Unit::Ampere),
            ),
            point(
                "energy",
                &moli_i18n::tr!("pilotes.kasa.energie"),
                number(),
                false,
                Some(Unit::KiloWattHour),
            ),
        ]);
    }
    // The LED is the plug's own light, never the thing it powers.
    if let Some(led) = points.iter_mut().find(|p| &*p.key == "led") {
        led.semantic = Semantic::Other;
    }
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: info.alias.clone().into(),
        manufacturer: Some("TP-Link".into()),
        model: Some(info.model.clone().into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    ctx.ready();
    let poll = Duration::from_secs(config.poll_seconds);
    let mut known: Option<(DeviceId, Info)> = None;
    let mut failures: u32 = 0;
    let mut last_error: Option<String> = None;
    loop {
        match poll_once(config, ctx, &mut known).await {
            Ok(()) => {
                failures = 0;
                last_error = None;
            }
            Err(e) => {
                failures = failures.saturating_add(1);
                let e = format!("{e:#}");
                if last_error.as_deref() != Some(e.as_str()) {
                    tracing::warn!(instance = %ctx.instance(), error = %e, "kasa plug unreachable");
                    last_error = Some(e);
                }
                if let Some((id, _)) = &known
                    && failures >= 3
                {
                    ctx.set_availability(id, false);
                }
            }
        }
        let pause = if failures == 0 {
            poll
        } else {
            (poll * failures.min(6)).min(RETRY_MAX)
        };
        let deadline = tokio::time::Instant::now() + pause;
        loop {
            tokio::select! {
                () = tokio::time::sleep_until(deadline) => break,
                command = ctx.next_command() => {
                    let Some(command) = command else { return Ok(()) };
                    let result = order(config, &command).await;
                    let changed = result.is_ok();
                    command.reply(result);
                    if changed {
                        break; // read back what the plug did
                    }
                }
            }
        }
    }
}

async fn poll_once(
    config: &Config,
    ctx: &DriverCtx,
    known: &mut Option<(DeviceId, Info)>,
) -> anyhow::Result<()> {
    let answer = ask(config, &json!({"system": {"get_sysinfo": {}}})).await?;
    let found = info(&answer).context("no sysinfo in the answer")?;
    let id = match known {
        Some((id, old)) if *old == found => id.clone(),
        _ => {
            let id = ctx.device_id(&found.mac.replace(':', "").to_lowercase());
            ctx.upsert_device(device(ctx, &id, &found));
            if known.is_none() {
                tracing::info!(instance = %ctx.instance(), model = %found.model, "kasa plug connected");
            }
            *known = Some((id.clone(), found.clone()));
            id
        }
    };
    ctx.set_availability(&id, true);
    ctx.set_state(&id, "state", Value::Bool(found.on));
    ctx.set_state(&id, "led", Value::Bool(found.led));
    if found.meter {
        let answer = ask(config, &json!({"emeter": {"get_realtime": {}}})).await?;
        if let Some(r) = reading(&answer) {
            for (key, v) in [
                ("power", r.power),
                ("voltage", r.voltage),
                ("current", r.current),
                ("energy", r.energy),
            ] {
                if let Some(v) = v {
                    ctx.set_state(&id, key, Value::Float(v));
                }
            }
        }
    }
    Ok(())
}

/// The relay or the LED; the plug's `err_code` says whether it obeyed.
async fn order(config: &Config, command: &CommandRequest) -> Result<(), String> {
    let request = match (&*command.key, &command.value) {
        ("state", Value::Bool(on)) => {
            json!({"system": {"set_relay_state": {"state": u8::from(*on)}}})
        }
        ("led", Value::Bool(on)) => json!({"system": {"set_led_off": {"off": u8::from(!*on)}}}),
        (key, _) => return Err(moli_i18n::tr!("pilotes.kasa.lecture_seule", point = key)),
    };
    let answer = ask(config, &request).await.map_err(|e| format!("{e:#}"))?;
    let code = answer["system"]
        .as_object()
        .and_then(|o| o.values().next())
        .and_then(|v| v["err_code"].as_i64());
    match code {
        Some(0) => Ok(()),
        other => Err(moli_i18n::tr!(
            "pilotes.kasa.refus",
            code = other.map_or_else(|| "?".to_owned(), |c| c.to_string())
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Json {
        let path = format!(
            "{}/../../integrations/kasa/fixtures/{name}.json",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn the_xor_autokey_goes_both_ways() {
        let text = r#"{"system":{"get_sysinfo":{}}}"#;
        let bytes = encrypt(text);
        assert_eq!(&bytes[..4], &[0, 0, 0, 29]);
        // The protocol's well-known first byte: '{' (0x7b) ^ 171 = 0xd0.
        assert_eq!(bytes[4], 0xd0);
        assert_eq!(decrypt(&bytes[4..]), text);
    }

    #[test]
    fn sysinfo_is_read() {
        let i = info(&fixture("sysinfo")).unwrap();
        assert_eq!(i.mac, "AA:BB:CC:DD:EE:FF");
        assert_eq!(i.model, "HS110(EU)");
        assert!(!i.on);
        assert!(!i.led, "led_off = 1: the light is off");
        assert!(i.meter);
        assert_eq!(info(&json!({})), None);
    }

    #[test]
    fn both_meter_generations_are_read() {
        let v1 = reading(&fixture("realtime-v1")).unwrap();
        assert_eq!(
            (v1.power, v1.voltage, v1.current, v1.energy),
            (Some(0.0), Some(240.1), Some(0.014), Some(1.302))
        );
        let v2 = reading(&fixture("realtime-v2")).unwrap();
        assert_eq!(
            (v2.power, v2.voltage, v2.current, v2.energy),
            (Some(2001.2), Some(231.5), Some(8.712), Some(15.873))
        );
        assert_eq!(
            reading(&json!({"emeter": {"get_realtime": {"err_code": -1}}})),
            None
        );
    }

    #[test]
    fn a_wrong_polling_period_is_refused() {
        let config = |poll_seconds| Config {
            host: "192.168.1.2".into(),
            port: 9999,
            poll_seconds,
        };
        assert!(Kasa::new(config(10)).is_ok());
        assert!(Kasa::new(config(1)).is_err());
    }
}
