//! Temporary Home Assistant migration bridge.

mod model;
mod net;
mod runtime;

use anyhow::{bail, ensure};
use serde::Deserialize;
use std::collections::HashSet;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub transport: Transport,
    pub cert_sha256: Option<String>,
    #[serde(default = "poll_default")]
    pub poll_seconds: u64,
    pub entities: Vec<Entity>,
}

const fn poll_default() -> u64 {
    15
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    Http,
    PinnedHttps,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entity {
    pub native_id: String,
    pub entity_id: String,
    pub domain: String,
    pub room: String,
    pub safety: Safety,
    #[serde(default)]
    pub writable: Vec<String>,
    pub turn_on_mode: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Safety {
    Unknown,
    Ordinary,
    Machine,
}

#[derive(Debug)]
pub struct HomeAssistant {
    config: Config,
}

impl HomeAssistant {
    /// Validates configuration without opening a socket.
    pub fn new(config: Config) -> anyhow::Result<Self> {
        config
            .validate()
            .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("homeassistant.invalid_config")))?;
        Ok(Self { config })
    }
}

impl Config {
    fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            !self.host.is_empty()
                && self.host.len() <= 253
                && self
                    .host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-'),
            "invalid bridge host"
        );
        ensure!(self.port != 0, "invalid bridge port");
        ensure!(
            (5..=300).contains(&self.poll_seconds),
            "invalid poll interval"
        );
        match (self.transport, &self.cert_sha256) {
            (Transport::Http, None) => {}
            (Transport::PinnedHttps, Some(pin))
                if pin.len() == 64 && pin.bytes().all(|b| b.is_ascii_hexdigit()) => {}
            _ => bail!("HTTPS requires a SHA-256 pin; HTTP must not have one"),
        }
        ensure!(
            !self.entities.is_empty() && self.entities.len() <= 128,
            "invalid entity count"
        );
        let mut natives = HashSet::new();
        let mut locators = HashSet::new();
        for e in &self.entities {
            ensure!(identifier(&e.native_id, 64, true), "invalid native alias");
            ensure!(
                natives.insert(&e.native_id) && locators.insert(&e.entity_id),
                "duplicate bridge identity"
            );
            let Some((domain, object)) = e.entity_id.split_once('.') else {
                bail!("invalid entity locator")
            };
            ensure!(
                domain == e.domain && identifier(object, 128, false) && object != "all",
                "invalid entity locator"
            );
            ensure!(
                !e.room.trim().is_empty()
                    && e.room.len() <= 128
                    && !e.room.chars().any(char::is_control),
                "invalid reviewed room"
            );
            let allowed: &[&str] = match e.domain.as_str() {
                "sensor" | "binary_sensor" => &[],
                "climate" => &["on", "mode", "target_temperature"],
                "light" => &["on", "brightness", "color", "color_temp"],
                "switch" => &["on"],
                "cover" => &["control", "percent_control"],
                "vacuum" => &["control"],
                "media_player" => &["playing", "volume", "mute"],
                _ => bail!("unsupported bridge domain"),
            };
            ensure!(
                e.writable.len() <= allowed.len()
                    && e.writable.iter().all(|p| allowed.contains(&p.as_str())),
                "unsupported writable point"
            );
            ensure!(
                e.writable.iter().collect::<HashSet<_>>().len() == e.writable.len(),
                "duplicate writable point"
            );
            ensure!(
                e.safety != Safety::Unknown || e.writable.is_empty(),
                "unknown effects must stay read-only"
            );
            ensure!(
                e.domain != "cover" || e.writable.is_empty() || e.safety == Safety::Ordinary,
                "only reviewed ordinary covers can be written"
            );
            if let Some(mode) = &e.turn_on_mode {
                ensure!(
                    e.domain == "climate"
                        && matches!(mode.as_str(), "heat" | "cool" | "dry" | "fan_only" | "auto"),
                    "invalid turn-on mode"
                );
            }
        }
        Ok(())
    }

    /// Bind a vault token to this endpoint and, for HTTPS, this certificate.
    #[must_use]
    pub fn token_endpoint(&self) -> String {
        let scheme = match self.transport {
            Transport::Http => "http",
            Transport::PinnedHttps => "https",
        };
        let pin = self
            .cert_sha256
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase();
        format!(
            "{scheme}://{}:{}/#{pin}",
            self.host.to_ascii_lowercase(),
            self.port
        )
    }
}

fn identifier(s: &str, max: usize, dash: bool) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || (dash && b == b'-')
        })
}
