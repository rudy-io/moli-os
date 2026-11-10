//! Operator-only Airzone reads; the host comes from the configured instance.
use anyhow::Context as _;
use clap::{Args, ValueEnum};
use moli_airzone::DiagnosticRequest;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Operation {
    Hvac,
    Iaq,
    Version,
    Webserver,
    Integration,
    Demo,
    Thermostat,
}

#[derive(Args)]
pub struct Inspect {
    /// Existing Airzone driver instance in the configuration.
    #[arg(long)]
    instance: String,
    #[arg(value_enum)]
    operation: Operation,
    /// 0: all zones or DHW (without --zone); 127: all systems.
    #[arg(long, default_value_t = 0)]
    system: u8,
    /// Zone 0 reads all zones. Omit for system/DHW information.
    #[arg(long)]
    zone: Option<u8>,
    /// IAQ sensor 0 reads all sensors of the given system.
    #[arg(long, default_value_t = 0)]
    sensor: u8,
    /// Use the documented GET alternative for HVAC or IAQ.
    #[arg(long)]
    get: bool,
}

impl Inspect {
    fn request(&self) -> DiagnosticRequest {
        match self.operation {
            Operation::Hvac => DiagnosticRequest::Hvac {
                system: self.system,
                zone: self.zone,
                get: self.get,
            },
            Operation::Iaq => DiagnosticRequest::Iaq {
                system: self.system,
                sensor: self.sensor,
                get: self.get,
            },
            Operation::Version => DiagnosticRequest::Version,
            Operation::Webserver => DiagnosticRequest::Webserver,
            Operation::Integration => DiagnosticRequest::Integration,
            Operation::Demo => DiagnosticRequest::Demo,
            Operation::Thermostat => DiagnosticRequest::Thermostat,
        }
    }
}

pub fn run(config: &crate::config::Config, args: &Inspect) -> anyhow::Result<()> {
    let instance = config
        .drivers
        .iter()
        .find(|d| d.id == args.instance && d.kind == "airzone")
        .context(moli_i18n::tr!("airzone.instance_missing"))?;
    let options = instance.options.clone().try_into()?;
    let report = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(moli_airzone::inspect(options, args.request()))?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
