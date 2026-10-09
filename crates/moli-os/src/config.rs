//! `moli.toml`. Contains no secret: credentials are referenced by the name of
//! the environment variable that holds them.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use moli_runtime::guard::{GuardPolicy, QuietHours};
use serde::Deserialize;

/// The configuration written at a new house's first start.
const STARTER: &str = r#"# Moli OS : la configuration de cette maison, créée au premier démarrage.
# Aucun secret ici, jamais : ils vont dans le coffre chiffré (`moli-os secrets set`).
# Exemple complet et commenté : moli.example.toml ; guide : docs/components/installation.md.

[server]
data_dir = {data_dir}
# Les noms par lesquels on joint Moli (garde contre le rebinding DNS) ; une
# adresse IP tapée telle quelle marche toujours.
# allowed_hosts = ["moli.local"]

# Les appareils : un bloc [[driver]] par intégration (integrations/<nom>/onboarding.md),
# puis redémarrer Moli. Par exemple, Zigbee par un Zigbee2MQTT existant :
# [[driver]]
# id = "z2m"
# kind = "z2m"
#
# [driver.options]
# host = "192.168.1.x"
# port = 1883
# base_topic = "zigbee2mqtt"
"#;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: Server,
    #[serde(default, rename = "driver")]
    pub drivers: Vec<DriverConfig>,
    #[serde(default)]
    pub guard: GuardConfig,
    /// Energy meters and tariff (absent: no energy tracking).
    #[serde(default)]
    pub energy: Option<moli_energy::Config>,
    /// Light fixtures: bulbs and the relay that feeds them, as one light
    /// (D12). Moli runs them as instance `lumieres`.
    #[serde(default, rename = "fixture")]
    pub fixtures: Vec<moli_lights::Fixture>,
    /// The home's own assistant (absent: no conversation in the dashboard).
    /// Its API key: `moli-os secrets set assistant api_key`.
    #[serde(default)]
    pub assistant: Option<moli_assistant::Config>,
    /// Automations: the house's position (sunrise, sunset) and time zone.
    #[serde(default)]
    pub automations: moli_automation::Config,
}

/// Where and when agents need a human's approval. Edited by humans only:
/// no surface can change it.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardConfig {
    #[serde(default)]
    pub protected_rooms: Vec<String>,
    #[serde(default)]
    pub quiet_hours: Option<QuietHoursConfig>,
    /// Devices with no known room need approval too.
    #[serde(default)]
    pub protect_unassigned: bool,
    /// `"trusted"` (default): the household commands from the dashboard
    /// without a code; `"pin"`: only a PIN session does.
    #[serde(default)]
    pub dashboard: moli_runtime::guard::Dashboard,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuietHoursConfig {
    /// `"21:00"`
    pub from: String,
    /// `"07:30"` (may be earlier than `from`: the window crosses midnight)
    pub to: String,
    /// IANA name, e.g. `"Europe/Paris"` (daylight saving handled).
    pub timezone: String,
    /// Rooms concerned; empty or absent = the whole home.
    #[serde(default)]
    pub rooms: Vec<String>,
}

impl GuardConfig {
    pub fn policy(&self) -> anyhow::Result<GuardPolicy> {
        let quiet_hours = match &self.quiet_hours {
            Some(q) => Some(
                QuietHours::parse(&q.from, &q.to, &q.timezone, q.rooms.clone())
                    .map_err(|e| anyhow::anyhow!("[guard.quiet_hours] {e}"))?,
            ),
            None => None,
        };
        Ok(GuardPolicy {
            protected_rooms: self.protected_rooms.clone(),
            quiet_hours,
            protect_unassigned: self.protect_unassigned,
            dashboard: self.dashboard,
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    #[serde(default = "default_listen")]
    pub listen: SocketAddr,
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    #[serde(default = "default_workers")]
    pub worker_threads: usize,
    /// Names/IPs this instance is reached by, besides localhost
    /// (DNS-rebinding guard on every surface).
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Days of history kept (0 = no history).
    #[serde(default = "default_history_days")]
    pub history_days: u32,
    /// The Cloudflare Access application in front of the tunnel (absent:
    /// nobody coming through Cloudflare is recognized).
    #[serde(default)]
    pub access: Option<moli_api::AccessConfig>,
    /// The house's language (`locales/<language>/`): its words, its
    /// assistant, its dashboard. The one chosen at the installation wins.
    #[serde(default = "default_language")]
    pub language: String,
    /// A demo house, with demo devices only (`kind = "demo"`): anyone who
    /// reaches it is let in. Refused with any real driver.
    #[serde(default)]
    pub demo: bool,
}

fn default_language() -> String {
    moli_i18n::FALLBACK.to_owned()
}

fn default_history_days() -> u32 {
    30
}

impl Default for Server {
    fn default() -> Self {
        Self {
            listen: default_listen(),
            data_dir: default_data_dir(),
            worker_threads: default_workers(),
            allowed_hosts: Vec::new(),
            history_days: default_history_days(),
            access: None,
            language: default_language(),
            demo: false,
        }
    }
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 8790))
}
fn default_data_dir() -> PathBuf {
    PathBuf::from("data")
}
fn default_workers() -> usize {
    2
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriverConfig {
    /// Instance id: prefix of every device id it publishes. Never rename it.
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default = "empty_table")]
    pub options: toml::Value,
}

fn empty_table() -> toml::Value {
    toml::Value::Table(toml::Table::new())
}

impl Config {
    /// A new house has no configuration yet: a minimal one (no driver, data
    /// next to it) is written there and used. When it cannot be written, the
    /// same settings are used anyway, in memory.
    pub fn starter(path: &Path) -> anyhow::Result<Self> {
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let data_dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
        let text = STARTER.replace(
            "{data_dir}",
            &toml::Value::String(data_dir.display().to_string()).to_string(),
        );
        let written = std::fs::create_dir_all(dir).and_then(|()| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()))
        });
        match written {
            Ok(()) => {
                tracing::info!(
                    path = %path.display(),
                    "{}",
                    moli_i18n::tr!("serveur.config.depart_ecrite")
                );
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "{}",
                    moli_i18n::tr!("serveur.config.depart_non_ecrite")
                );
            }
        }
        let config: Self = toml::from_str(&text).context("starter configuration")?;
        config.check()?;
        Ok(config)
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let config: Self =
            toml::from_str(&text).with_context(|| format!("invalid {}", path.display()))?;
        config.check()?;
        Ok(config)
    }

    fn check(&self) -> anyhow::Result<()> {
        if !moli_i18n::languages().contains(&self.server.language.as_str()) {
            bail!(
                "[server] language = {:?}: Moli speaks {}",
                self.server.language,
                moli_i18n::languages().join(", ")
            );
        }
        // A demo is opened to anyone: nothing real may sit behind it.
        if self.server.demo
            && let Some(real) = self
                .drivers
                .iter()
                .find(|d| !["demo", "helpers"].contains(&d.kind.as_str()))
        {
            bail!(
                "[server] demo = true opens the house to anyone: driver {:?} (kind {:?}) is real, only demo and helpers drivers are allowed",
                real.id,
                real.kind
            );
        }
        let mut seen = std::collections::HashSet::new();
        for driver in &self.drivers {
            if driver.id.is_empty() || driver.id.contains([':', '/']) {
                bail!(
                    "driver id {:?} must be non-empty and contain neither ':' nor '/'",
                    driver.id
                );
            }
            // `assistant` holds the assistant's key in the secret store;
            // `energie` is the « Énergie » device Moli runs itself.
            if [
                moli_runtime::CORE_NAMESPACE,
                "assistant",
                "automations",
                moli_energy::points::INSTANCE,
                moli_lights::INSTANCE,
            ]
            .contains(&driver.id.as_str())
            {
                bail!("driver id {:?} is reserved", driver.id);
            }
            if !seen.insert(&driver.id) {
                bail!("duplicate driver id {:?}", driver.id);
            }
        }
        if self.server.worker_threads == 0 {
            bail!("worker_threads must be ≥ 1");
        }
        self.guard.policy()?;
        if let Some(energy) = &self.energy {
            energy
                .check()
                .map_err(|e| anyhow::anyhow!("[energy] {e}"))?;
        }
        moli_lights::check(&self.fixtures).map_err(|e| anyhow::anyhow!("[[fixture]] {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_is_valid() {
        let config: Config = toml::from_str(include_str!("../../../moli.example.toml")).unwrap();
        config.check().unwrap();
        assert_eq!(config.drivers[0].kind, "z2m");
    }

    #[test]
    fn a_new_house_starts_with_a_minimal_config() {
        let dir = std::env::temp_dir().join(format!("moli-starter-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("moli.toml");
        let config = Config::starter(&path).unwrap();
        assert!(config.drivers.is_empty());
        assert_eq!(config.server.data_dir, std::path::absolute(&dir).unwrap());
        // Written: the next start reads it like any configuration.
        let again = Config::load(&path).unwrap();
        assert_eq!(again.server.data_dir, config.server.data_dir);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_bad_ids() {
        let config: Config = toml::from_str("[[driver]]\nid = \"a:b\"\nkind = \"z2m\"").unwrap();
        assert!(config.check().is_err());
        let config: Config = toml::from_str(
            "[[driver]]\nid = \"a\"\nkind = \"z2m\"\n[[driver]]\nid = \"a\"\nkind = \"z2m\"",
        )
        .unwrap();
        assert!(config.check().is_err());
    }
}
