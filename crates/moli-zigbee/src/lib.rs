//! Native Zigbee driver: talks EZSP to a Silicon Labs coordinator dongle
//! (serial or TCP), no Zigbee2MQTT and no broker.
//!
//! Dormant: nothing runs unless `[[driver]] kind = "zigbee"` is in `moli.toml`.

use std::path::{Path, PathBuf};

use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;

/// Driver configuration (`[driver.options]` in `moli.toml`).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Serial device (`/dev/ttyUSB0`) or a network adapter (`tcp://host:6638`).
    pub port: String,
    #[serde(default = "default_baud")]
    pub baud: u32,
    #[serde(default)]
    pub flow: Flow,
    /// Zigbee channel (11 to 26) of a new network; chosen by the driver if unset.
    #[serde(default)]
    pub channel: Option<u8>,
}

fn default_baud() -> u32 {
    115_200
}

/// Serial flow control.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Flow {
    /// XON/XOFF: what the Sonoff ZBDongle-E ships with.
    #[default]
    Software,
    Hardware,
    None,
}

#[derive(Debug)]
pub struct Zigbee {
    config: Config,
    /// Where the network state (devices, counters) is kept; never the key.
    data_dir: PathBuf,
}

impl Zigbee {
    #[must_use]
    pub fn new(config: Config, data_dir: &Path) -> Self {
        Self {
            config,
            data_dir: data_dir.to_path_buf(),
        }
    }
}

impl Driver for Zigbee {
    fn kind(&self) -> &'static str {
        "zigbee"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, &self.data_dir, ctx))
    }
}

/// Provisional: the ASH/EZSP layers arrive in the next steps.
async fn run(config: &Config, data_dir: &Path, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    tracing::debug!(
        port = %config.port,
        baud = config.baud,
        flow = ?config.flow,
        channel = ?config.channel,
        data_dir = %data_dir.display(),
        "zigbee driver started"
    );
    ctx.wait_for(moli_i18n::tr!("pilotes.zigbee.demarrage"));
    ctx.cancelled().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(json: &str) -> Result<Config, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn config_defaults() {
        let c = config(r#"{"port":"/dev/ttyUSB0"}"#).unwrap();
        assert_eq!(c.port, "/dev/ttyUSB0");
        assert_eq!(c.baud, 115_200);
        assert_eq!(c.flow, Flow::Software);
        assert_eq!(c.channel, None);
    }

    #[test]
    fn config_rejects_unknown_fields() {
        assert!(config(r#"{"port":"x","bauds":1}"#).is_err());
    }

    #[test]
    fn tcp_port_is_accepted() {
        let c = config(r#"{"port":"tcp://192.168.1.x:6638"}"#).unwrap();
        assert_eq!(c.port, "tcp://192.168.1.x:6638");
    }

    #[test]
    fn flow_and_channel_are_read() {
        let c = config(r#"{"port":"x","flow":"hardware","channel":15,"baud":460800}"#).unwrap();
        assert_eq!(c.flow, Flow::Hardware);
        assert_eq!(c.channel, Some(15));
        assert_eq!(c.baud, 460_800);
        assert_eq!(
            config(r#"{"port":"x","flow":"none"}"#).unwrap().flow,
            Flow::None
        );
        assert!(config(r#"{"port":"x","flow":"rts"}"#).is_err());
    }

    #[test]
    fn kind_is_zigbee() {
        let z = Zigbee::new(config(r#"{"port":"x"}"#).unwrap(), Path::new("/tmp"));
        assert_eq!(z.kind(), "zigbee");
    }
}
