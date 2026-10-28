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
# Le fuseau de la maison (automatismes, énergie, heures calmes, Moli). Sans lui,
# celui du système, souvent UTC dans un conteneur.
# timezone = "Europe/Paris"

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
    /// Automations: the house's position (sunrise, sunset).
    #[serde(default)]
    pub automations: moli_automation::Config,
    /// The pantry, « Le Cellier » (absent: no shopping list). `[pantry]`,
    /// even empty, switches it on; never in a demo house.
    #[serde(default)]
    pub pantry: Option<moli_pantry::Config>,
    /// The house's time zone, resolved once and given to every section.
    #[serde(skip)]
    pub zone: crate::timezone::Zone,
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
    /// The house's (`[server] timezone`). Written here, it must agree.
    #[serde(default)]
    pub timezone: Option<String>,
    /// Rooms concerned; empty or absent = the whole home.
    #[serde(default)]
    pub rooms: Vec<String>,
}

impl GuardConfig {
    pub fn policy(&self) -> anyhow::Result<GuardPolicy> {
        let quiet_hours = match &self.quiet_hours {
            Some(q) => {
                // Given the house's when the configuration is read: never guessed here.
                let timezone = q
                    .timezone
                    .as_deref()
                    .context("[guard.quiet_hours] no time zone: write [server] timezone")?;
                Some(
                    QuietHours::parse(&q.from, &q.to, timezone, q.rooms.clone())
                        .map_err(|e| anyhow::anyhow!("[guard.quiet_hours] {e}"))?,
                )
            }
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
    /// The house's time zone (IANA): when automations fire, where energy
    /// days are cut, the assistant's clock, the quiet hours. Absent: the
    /// system's, named at start (in a container, usually UTC).
    #[serde(default)]
    pub timezone: Option<String>,
    /// The house's language (`locales/<language>/`): its words, its
    /// assistant, its dashboard. The one chosen at the installation wins.
    #[serde(default = "default_language")]
    pub language: String,
    /// A demo house, with demo devices only (`kind = "demo"`): anyone who
    /// reaches it is let in. Refused with any real driver.
    #[serde(default)]
    pub demo: bool,
    /// A demo house's starting point (home, plan, automations…): at every
    /// start, its data directory is emptied and filled from here, and a
    /// made-up energy past is filed. Only with `demo = true`.
    #[serde(default)]
    pub demo_seed: Option<PathBuf>,
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
            timezone: None,
            language: default_language(),
            demo: false,
            demo_seed: None,
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
        let config = Self::parse(&text).context("starter configuration")?;
        config.check()?;
        Ok(config)
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let mut config =
            Self::parse(&text).with_context(|| format!("invalid {}", path.display()))?;
        config.beside(path.parent().unwrap_or(Path::new(".")));
        config.check()?;
        Ok(config)
    }

    /// The configuration, its time zone resolved and given to every
    /// section that reads one (see `timezone`).
    fn parse(text: &str) -> anyhow::Result<Self> {
        let sections = crate::timezone::written(&toml::from_str(text)?)?;
        let mut config: Self = toml::from_str(text)?;
        config.zone = crate::timezone::settle(
            config.server.timezone.as_deref(),
            &sections,
            crate::timezone::system,
        )?;
        let zone = config.zone.name.clone();
        config.automations.timezone.clone_from(&zone);
        if let Some(energy) = &mut config.energy {
            energy.timezone.clone_from(&zone);
        }
        if let Some(assistant) = &mut config.assistant {
            assistant.timezone.clone_from(&zone);
        }
        if let Some(quiet) = &mut config.guard.quiet_hours {
            quiet.timezone = Some(zone);
        }
        Ok(config)
    }

    /// A demo's files (its devices, its seed) are found next to its
    /// configuration, wherever Moli is started from.
    fn beside(&mut self, dir: &Path) {
        let near = |p: &Path| {
            if p.is_relative() {
                dir.join(p)
            } else {
                p.to_path_buf()
            }
        };
        if let Some(seed) = &self.server.demo_seed {
            self.server.demo_seed = Some(near(seed));
        }
        for driver in self.drivers.iter_mut().filter(|d| d.kind == "demo") {
            if let Some(toml::Value::String(f)) = driver.options.get_mut("fixture") {
                *f = near(Path::new(f.as_str())).to_string_lossy().into_owned();
            }
        }
    }

    fn check(&self) -> anyhow::Result<()> {
        if !moli_i18n::languages().contains(&self.server.language.as_str()) {
            bail!(
                "[server] language = {:?}: Moli speaks {}",
                self.server.language,
                moli_i18n::languages().join(", ")
            );
        }
        if self.server.demo_seed.is_some() && !self.server.demo {
            bail!("[server] demo_seed is for a demo house only (demo = true)");
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
                moli_energy::load::INSTANCE,
                moli_lights::INSTANCE,
                moli_api::PEOPLE_INSTANCE,
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
    fn an_empty_pantry_section_switches_the_pantry_on() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.pantry.is_none(), "absent: no pantry");
        let config: Config = toml::from_str("[pantry]\n").unwrap();
        assert!(config.pantry.is_some());
        assert!(
            toml::from_str::<Config>("[pantry]\nx = 1\n").is_err(),
            "an unknown pantry option is refused"
        );
    }

    #[test]
    fn every_section_inherits_the_house_zone() {
        let config = Config::parse(
            "[server]\ntimezone = \"Indian/Reunion\"\n\
             [assistant]\n\
             [energy]\n\
             [guard.quiet_hours]\nfrom = \"21:00\"\nto = \"07:30\"",
        )
        .unwrap();
        assert_eq!(config.zone.source, crate::timezone::Source::Server);
        assert_eq!(config.automations.timezone, "Indian/Reunion");
        assert_eq!(config.energy.unwrap().timezone, "Indian/Reunion");
        assert_eq!(config.assistant.unwrap().timezone, "Indian/Reunion");
        let quiet = config.guard.policy().unwrap().quiet_hours.unwrap();
        assert_eq!(quiet.tz_name, "Indian/Reunion");
    }

    #[test]
    fn a_section_contradicting_the_house_zone_is_refused() {
        let err = Config::parse(
            "[server]\ntimezone = \"Indian/Reunion\"\n[automations]\ntimezone = \"Europe/Paris\"",
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("[server]") && err.contains("[automations]"),
            "both are named: {err}"
        );
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
