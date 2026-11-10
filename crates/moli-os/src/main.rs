//! Moli OS binary: one process, every surface.

mod airzone;
mod backup;
mod catalogue;
mod config;
mod managed_airzone;
mod timezone;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, bail};
use clap::{Parser, Subcommand};
use moli_core::InstanceId;
use moli_runtime::{Driver, Hub, HubOptions, MasterKey, spawn_driver};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use crate::config::{Config, DriverConfig};

#[derive(Parser)]
#[command(
    name = "moli-os",
    version,
    about = "Agent-native, ultra-light home automation"
)]
struct Cli {
    /// Configuration file.
    #[arg(
        short,
        long,
        env = "MOLI_CONFIG",
        default_value = "moli.toml",
        global = true
    )]
    config: PathBuf,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the hub, its drivers and every surface (default).
    Serve,
    /// Validate the configuration and exit.
    CheckConfig,
    /// Read-only Airzone diagnostics; demo data is never imported as devices.
    AirzoneInspect(airzone::Inspect),
    /// Operator tools for the encrypted secret store (values are never shown).
    #[command(subcommand)]
    Secrets(SecretsCommand),
    /// Device profiles (declarative drivers).
    #[command(subcommand)]
    Profile(ProfileCommand),
    /// The integration catalogue: one package per integration.
    #[command(subcommand)]
    Catalogue(CatalogueCommand),
    /// Energy history.
    #[command(subcommand)]
    Energy(EnergyCommand),
    /// Tuya devices.
    #[command(subcommand)]
    Tuya(TuyaCommand),
    /// Native Zigbee (EZSP coordinator).
    #[command(subcommand)]
    Zigbee(ZigbeeCommand),
    /// The house model's contract and training prompts (for the data generator).
    #[command(subcommand, hide = true)]
    Model(ModelCommand),
    /// Copy everything Moli knows into a new folder (safe while it runs).
    Backup { dir: PathBuf },
    /// Put a backup back into the data directory (moli-os stopped).
    Restore {
        dir: PathBuf,
        /// Overwrite the files already there.
        #[arg(long)]
        force: bool,
    },
    /// Exit 0 if the local instance answers `/api/health` (container
    /// healthcheck: the image has no curl).
    Health,
}

#[derive(Subcommand)]
enum TuyaCommand {
    /// File what the Tuya cloud knows: local keys into the encrypted store,
    /// the rest into the devices file. Keys are never printed. Restart
    /// moli-os afterwards. Read on standard input (the JSON of
    /// tools/home-assistant/tuya-export.py), or with `--cloud` straight from
    /// the cloud project whose keys Moli holds (no Home Assistant).
    Import {
        /// The tuya driver instance the keys belong to.
        #[arg(long, default_value = "tuya")]
        instance: String,
        /// Default: `tuya.json` in the data directory.
        #[arg(long)]
        devices_file: Option<std::path::PathBuf>,
        /// Ask the Tuya cloud itself, with the driver's cloud keys (given in
        /// the dashboard, or its `cloud_keys` file) and `cloud_region`.
        #[arg(long)]
        cloud: bool,
        /// Only list what would be filed (no key shown, nothing written).
        #[arg(long)]
        summary: bool,
    },
}

#[derive(Subcommand)]
enum ZigbeeCommand {
    /// Take over a Zigbee2MQTT network: read its coordinator backup
    /// (`coordinator_backup.json`, Open Coordinator Backup), put its keys
    /// into the encrypted store and its network into the driver's state;
    /// the driver restores it on the dongle at its next start, without
    /// re-pairing. Keys are never printed. Stop moli-os first (a running
    /// Moli keeps its vault and the driver's state in memory and would
    /// overwrite the import at its next save), start it again afterwards.
    Import {
        /// The backup file Zigbee2MQTT writes in its data folder.
        file: PathBuf,
        /// The zigbee driver instance that takes the network over.
        #[arg(long, default_value = "zigbee")]
        instance: String,
        /// Replace the network this instance already records.
        #[arg(long)]
        force: bool,
    },
    /// Write the network Moli runs as an Open Coordinator Backup, to go
    /// back to Zigbee2MQTT: its parameters and devices from the driver's
    /// state, its keys from the encrypted store, its frame counter the last
    /// one recorded plus a margin. The file holds the network key in clear:
    /// private (0600), delete it once Zigbee2MQTT has it. Stop moli-os
    /// first, and keep it stopped (one program per dongle and network).
    Export {
        /// Where to write it (Zigbee2MQTT reads `coordinator_backup.json`
        /// in its data folder).
        path: PathBuf,
        /// The zigbee driver instance whose network is written.
        #[arg(long, default_value = "zigbee")]
        instance: String,
        /// Replace a file already there.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum EnergyCommand {
    /// Import hourly history (JSON lines `{"meter", "hour" (ms), "kwh"}` on
    /// standard input), e.g. from Home Assistant statistics. Only hours
    /// before Moli's own recording are taken; safe to run twice, and while
    /// moli-os runs.
    Import,
    /// A demo house only (`[server] demo = true`): file a made-up past for
    /// its meters (a family villa's habits), up to now. Run it before the
    /// first start.
    Demo {
        #[arg(long, default_value_t = 400)]
        days: u32,
    },
}

#[derive(Subcommand)]
enum CatalogueCommand {
    /// Check every package (manifest against this binary, profile compiled,
    /// fixtures replayed on the points) and print a table. Fails on any
    /// error; warnings say what was never held against a real answer.
    Check {
        /// The catalogue: one folder per integration.
        #[arg(default_value = "integrations")]
        dir: PathBuf,
    },
}

#[derive(Subcommand)]
enum ProfileCommand {
    /// Validate a profile file (or a built-in id) and print what it declares.
    /// The gate every agent-written profile must pass before use.
    Check { profile: String },
    /// Approve a profile file: print every path it reads and writes, then
    /// record its SHA-256 in the data directory (trusted-profiles.toml).
    /// Only approved files run; any edit needs a new approval. Restart
    /// moli-os afterwards.
    Trust { profile: String },
    /// List the built-in profiles.
    List,
}

#[derive(Subcommand)]
enum SecretsCommand {
    /// Forget one secret of a driver instance (revoked key, replaced bridge…).
    /// Restart moli-os afterwards.
    Forget { instance: String, name: String },
    /// Store one secret of a driver instance, read from standard input
    /// (never from the command line, never printed). Safe while moli-os runs;
    /// restart it afterwards (drivers read their secrets at start).
    Set { instance: String, name: String },
}

#[derive(Subcommand)]
enum ModelCommand {
    /// Print the frozen tools (JSON).
    Tools,
    /// Read a house (JSON) on standard input and print the model's system prompt.
    Prompt,
}

/// `moli-os model …`: no configuration needed.
fn model_command(command: &ModelCommand) -> anyhow::Result<()> {
    match command {
        ModelCommand::Tools => print!("{}", moli_assistant::MODEL_TOOLS_JSON),
        ModelCommand::Prompt => {
            let mut house = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut house)?;
            print!("{}", moli_assistant::render_model_prompt(&house)?.to_json());
        }
    }
    Ok(())
}

fn load_config(cli: &Cli) -> anyhow::Result<Config> {
    // A new house starts without a configuration: `serve` writes a minimal
    // one; recovery commands must remain usable with a damaged registry.
    let serving = matches!(cli.command, None | Some(Command::Serve));
    if serving && !cli.config.exists() && fresh(&cli.config) {
        Config::starter(&cli.config)
    } else if matches!(
        cli.command,
        Some(Command::Backup { .. } | Command::Restore { .. })
    ) {
        Config::load_for_recovery(&cli.config)
    } else {
        Config::load(&cli.config)
    }
}

fn main() -> anyhow::Result<()> {
    let boot = Instant::now();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,rumqttc=warn,rmcp=warn")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();
    if let Some(Command::Health) = &cli.command {
        return health();
    }
    // Profile tools need no configuration (any agent can check a profile
    // anywhere), except approving one: that is for this installation.
    if let Some(Command::Profile(command)) = &cli.command {
        return profile_command(command, &cli.config);
    }
    if let Some(Command::Catalogue(CatalogueCommand::Check { dir })) = &cli.command {
        return catalogue::run(dir, KINDS);
    }
    if let Some(Command::Model(command)) = &cli.command {
        return model_command(command);
    }
    let config = load_config(&cli)?;
    match cli.command.unwrap_or(Command::Serve) {
        Command::Profile(_) | Command::Catalogue(_) | Command::Health | Command::Model(_) => {
            unreachable!("handled above")
        }
        Command::Backup { dir } => backup::backup(&config.server.data_dir, &dir),
        Command::Restore { dir, force } => backup::restore(&config.server.data_dir, &dir, force),
        Command::Tuya(TuyaCommand::Import {
            instance,
            devices_file,
            cloud,
            summary,
        }) => tuya_import(&config, &instance, devices_file, cloud, summary),
        Command::Zigbee(command) => zigbee_command(&config, command),
        Command::Energy(EnergyCommand::Import) => energy_import(&config),
        Command::Energy(EnergyCommand::Demo { days }) => energy_demo(&config, days),
        Command::CheckConfig => check_config(&config),
        Command::AirzoneInspect(args) => airzone::run(&config, &args),
        Command::Secrets(SecretsCommand::Forget { instance, name }) => {
            let key = master_key(&config.server.data_dir, false)?.context(
                "no master key (MOLI_MASTER_KEY[_FILE], or master.key in the data directory)",
            )?;
            let path = config.server.data_dir.join("secrets.enc");
            let existed = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(moli_runtime::forget_secret(
                    &path,
                    &key,
                    &format!("{instance}/{name}"),
                ))?;
            println!(
                "{instance}/{name}: {}",
                if existed { "forgotten" } else { "not found" }
            );
            Ok(())
        }
        Command::Secrets(SecretsCommand::Set { instance, name }) => {
            // Typed at a terminal, the value would echo on screen (and end
            // up in scrollback, recordings…): demand a pipe.
            if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                bail!(
                    "pipe the value, never type it: read -rs V; printf %s \"$V\" | moli-os secrets set {instance} {name}"
                );
            }
            let key = master_key(&config.server.data_dir, false)?.context(
                "no master key (MOLI_MASTER_KEY[_FILE], or master.key in the data directory)",
            )?;
            let mut value = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut value)?;
            let value = value.trim();
            if value.is_empty() {
                bail!("empty value on standard input");
            }
            let path = config.server.data_dir.join("secrets.enc");
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(moli_runtime::set_secret(
                    &path,
                    &key,
                    &format!("{instance}/{name}"),
                    value,
                ))?;
            println!(
                "{instance}/{name}: stored ({} characters)",
                value.chars().count()
            );
            Ok(())
        }
        Command::Serve => tokio::runtime::Builder::new_multi_thread()
            .worker_threads(config.server.worker_threads)
            .enable_all()
            .build()?
            .block_on(serve(config, boot)),
    }
}

/// The file that says a data directory is a demo's: only such a directory
/// (or an empty one) is ever emptied.
const DEMO_MARKER: &str = ".moli-demo";

/// A demo house starts as its seed says, every time (`[server] demo_seed`):
/// what visitors changed is gone, the energy past reaches today.
fn demo_fresh(config: &Config) -> anyhow::Result<()> {
    let (true, Some(seed)) = (config.server.demo, &config.server.demo_seed) else {
        return Ok(());
    };
    let data = &config.server.data_dir;
    std::fs::create_dir_all(data).with_context(|| format!("cannot create {}", data.display()))?;
    let empty = std::fs::read_dir(data)?.next().is_none();
    if !empty && !data.join(DEMO_MARKER).exists() {
        bail!(
            "{} holds data that is not a demo's (no {DEMO_MARKER}): it is never emptied",
            data.display()
        );
    }
    for entry in std::fs::read_dir(data)? {
        let path = entry?.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    std::fs::write(
        data.join(DEMO_MARKER),
        "a demo house: emptied at every start
",
    )?;
    for entry in
        std::fs::read_dir(seed).with_context(|| format!("cannot read {}", seed.display()))?
    {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            std::fs::copy(entry.path(), data.join(entry.file_name()))?;
        }
    }
    if config.energy.is_some() {
        energy_demo(config, 400)?;
    }
    tracing::info!(seed = %seed.display(), "demo house reset");
    Ok(())
}

/// The house's language (the one chosen at the installation, else the
/// configuration's), then its time zone, said in that language: where it
/// came from, and what should be written instead of guessed.
fn speak_and_tell_the_time(config: &Config) {
    let language = moli_api::settings::language(&config.server.data_dir)
        .unwrap_or_else(|| config.server.language.clone());
    if !moli_i18n::set_language(&language) {
        tracing::warn!(%language, "unknown language: Moli speaks {}", moli_i18n::FALLBACK);
    }
    tracing::info!(timezone = %config.zone.name, from = %config.zone.origin(), "the house's time zone");
    for warning in config.zone.warnings() {
        tracing::warn!("{warning}");
    }
}

/// The hub over the data directory: labels, state, journal, addresses, the
/// vault when a master key is given, the guard and the mirror switch.
fn open_hub(config: &Config) -> anyhow::Result<Hub> {
    // The master key comes from the environment (a secrets manager), or a
    // new house's key file.
    let secrets = master_key(&config.server.data_dir, true)?
        .map(|key| (config.server.data_dir.join("secrets.enc"), key));
    if secrets.is_none() {
        tracing::warn!("no master key: drivers cannot persist pairing credentials");
    }
    Hub::new(HubOptions {
        labels_path: Some(config.server.data_dir.join("labels.toml")),
        state_path: Some(config.server.data_dir.join("state.json")),
        journal_path: Some(config.server.data_dir.join("journal.jsonl")),
        addresses_path: Some(config.server.data_dir.join("addresses.json")),
        secrets,
        guard: config.guard.policy()?,
        mirror: config.server.mirror,
        ..HubOptions::default()
    })
    .context("cannot open the data directory (labels, journal, secrets)")
}

async fn serve(config: Config, boot: Instant) -> anyhow::Result<()> {
    // Before anything opens the data directory.
    demo_fresh(&config)?;
    speak_and_tell_the_time(&config);
    let hub = open_hub(&config)?;
    let cancel = CancellationToken::new();
    let airzone_setup = managed_airzone::Manager::new(hub.clone(), &config, cancel.clone())?;

    // History first: it must be listening before drivers publish anything.
    let (history, recorder) = start_history(&config, &hub, &cancel)?.unzip();
    // Energy too: a reading missed is energy filed in the wrong hour.
    let (energy, metering) = match &config.energy {
        Some(energy) => {
            let (energy, task) = moli_energy::Energy::start(
                &hub,
                &config.server.data_dir.join("energy.db"),
                energy.clone(),
                cancel.clone(),
            )
            .context("cannot open the energy database")?;
            (Some(energy), Some(task))
        }
        None => (None, None),
    };

    let (mut tasks, gates) = spawn_drivers(&config, &hub, &cancel, &airzone_setup)?;
    tasks.extend(spawn_house_drivers(&config, &hub, energy.as_ref(), &cancel));
    let (household, whereabouts) = spawn_household(&config, &hub, gates.phones.clone(), &cancel);
    tasks.push(whereabouts);

    let listener = tokio::net::TcpListener::bind(config.server.listen)
        .await
        .with_context(|| format!("cannot listen on {}", config.server.listen))?;
    tracing::info!(
        listen = %config.server.listen,
        drivers = hub.snapshot().drivers.len(),
        boot_ms = boot.elapsed().as_millis(),
        "moli-os ready"
    );

    let saver = housekeeping(&hub, &cancel);
    let shutdown = cancel.clone();
    // Given, even empty: the house is set up (its vault is wrong); absent:
    // a new house, which may ask for its installation.
    let ui_pin = from_env_or_file("MOLI_UI_PIN")?
        .as_deref()
        .map(moli_api::UiPin::new);
    let (assistant, automations, automating) =
        start_brains(&config, &hub, energy.as_ref(), history.as_ref(), &cancel).await?;
    let surfaces = moli_api::Options {
        airzone_setup: Some(Arc::new(airzone_setup.clone())),
        allowed_hosts: config.server.allowed_hosts.clone(),
        ui_pin,
        history,
        bench_path: Some(config.server.data_dir.join("bench.json")),
        energy,
        home_path: Some(config.server.data_dir.join("home.json")),
        assistant,
        automations: Some(automations),
        access: config.server.access.clone(),
        demo: config.server.demo,
        household: Some(household),
        phones: gates.phones,
        machines: gates.machines,
        weather: weather(&config),
        timezone: Some(config.zone.name.clone()),
    };
    // Client addresses matter: human sessions are bound to them.
    axum::serve(
        listener,
        moli_api::router(hub.clone(), cancel.clone(), &surfaces)
            .into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        wait_for_signal().await;
        tracing::info!("shutting down");
        shutdown.cancel();
    })
    .await?;
    airzone_setup.shutdown().await;
    for task in tasks {
        let _ = task.await;
    }
    let _ = saver.await;
    let _ = automating.await;
    if let Some(recorder) = recorder {
        let _ = recorder.await; // flushes its last batch
    }
    if let Some(metering) = metering {
        let _ = metering.await; // files its queued readings
    }
    hub.persist_state()
        .await
        .context("final state cache write")?;
    Ok(())
}

/// The sky over the house (the dashboard's forecast and weather map):
/// where the assistant places it, else the automations (for the sun). One for
/// the dashboard and for « actualise la météo » (what the one keeps, the other
/// refreshes).
fn weather(config: &Config) -> Option<moli_weather::Weather> {
    static SKY: std::sync::OnceLock<Option<moli_weather::Weather>> = std::sync::OnceLock::new();
    SKY.get_or_init(|| new_weather(config)).clone()
}

fn new_weather(config: &Config) -> Option<moli_weather::Weather> {
    let assistant = config.assistant.as_ref();
    let latitude = assistant
        .and_then(|a| a.latitude)
        .or(config.automations.latitude);
    let longitude = assistant
        .and_then(|a| a.longitude)
        .or(config.automations.longitude);
    // The position a person chose from the conversation wins (`home.json`).
    let chosen = moli_assistant::saved_place(&config.server.data_dir.join("home.json"));
    let (latitude, longitude) = match chosen {
        Some(p) => (Some(p.latitude), Some(p.longitude)),
        None => (latitude, longitude),
    };
    // The demo house sits nowhere: no sky from the outside world.
    if config.server.demo {
        return None;
    }
    // Unplaced, it is still there: a position chosen from the conversation
    // places it for the dashboard and the assistant alike, without a restart.
    let data_dir = Some(config.server.data_dir.clone());
    Some(match latitude.zip(longitude) {
        Some((lat, lon)) => moli_weather::Weather::new(lat, lon, data_dir),
        None => moli_weather::Weather::unplaced(data_dir),
    })
}

/// The automations engine, and Moli (who drafts them and writes their
/// messages).
async fn start_brains(
    config: &Config,
    hub: &Hub,
    energy: Option<&moli_energy::Energy>,
    history: Option<&moli_history::History>,
    cancel: &CancellationToken,
) -> anyhow::Result<(
    Option<moli_assistant::Assistant>,
    moli_automation::Automations,
    tokio::task::JoinHandle<()>,
)> {
    // The sun rises where the house is: the position a person chose from the
    // conversation (`home.json`) wins over `moli.toml`'s.
    let mut sun = config.automations.clone();
    if let Some(p) = moli_assistant::saved_place(&config.server.data_dir.join("home.json")) {
        (sun.latitude, sun.longitude) = (Some(p.latitude), Some(p.longitude));
    }
    let (automations, task) = moli_automation::Automations::start(
        hub,
        sun,
        Some(&config.server.data_dir),
        cancel.clone(),
    )
    .await
    .context("cannot load the automations")?;
    let assistant = start_assistant(config, hub, energy, history)?;
    if let Some(moli) = &assistant {
        moli.set_automations(automations.clone());
        automations.set_writer(Arc::new(moli.clone()));
        // The voice satellites (already running) find Moli from now on.
        hub.set_voice_brain(Arc::new(moli.clone()));
        moli.start_recap();
    }
    Ok((assistant, automations, task))
}

/// `moli-os check-config`: every driver builds, the assistant's providers hold together.
fn check_config(config: &Config) -> anyhow::Result<()> {
    for driver in &config.drivers {
        build_driver(driver, &config.server.data_dir)?;
    }
    if let Some(assistant) = &config.assistant {
        assistant.check().context("[assistant]")?;
    }
    println!("time zone: {} ({})", config.zone.name, config.zone.origin());
    for warning in config.zone.warnings() {
        println!("  ! {warning}");
    }
    println!("configuration OK ({} driver(s))", config.drivers.len());
    Ok(())
}

/// The home's own assistant: always there (without `[assistant]`, with its
/// defaults), waiting for a key given from the dashboard if it has none.
fn start_assistant(
    config: &Config,
    hub: &Hub,
    energy: Option<&moli_energy::Energy>,
    history: Option<&moli_history::History>,
) -> anyhow::Result<Option<moli_assistant::Assistant>> {
    let mut settings = config.assistant.clone().unwrap_or_default();
    // Without [assistant], its defaults: but the house's clock, always.
    settings.timezone.clone_from(&config.zone.name);
    // The forecast's location: the one the automations already know.
    if settings.latitude.zip(settings.longitude).is_none() {
        settings.latitude = config.automations.latitude;
        settings.longitude = config.automations.longitude;
    }
    let moli = moli_assistant::Assistant::new(
        hub.clone(),
        energy.cloned(),
        history.cloned(),
        Some(config.server.data_dir.join("home.json")),
        settings,
    )
    .context("[assistant]")?;
    moli.load_voice_prefs(config.server.data_dir.join("assistant.json"));
    moli.open_exchanges(config.server.data_dir.join("assistant-exchanges.jsonl"));
    moli.open_recordings(config.server.data_dir.join("recordings"));
    if let Some(sky) = weather(config) {
        moli.set_weather(sky);
    }
    // The house model reads its prompt now, not at the first question.
    let warming = moli.clone();
    tokio::spawn(async move { warming.warm_up().await });
    Ok(Some(moli))
}

/// `moli-os tuya import`: keys into the encrypted store, the rest into the
/// devices file.
fn tuya_import(
    config: &Config,
    instance: &str,
    devices_file: Option<std::path::PathBuf>,
    cloud: bool,
    summary: bool,
) -> anyhow::Result<()> {
    if !cloud && std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        bail!(
            "pipe the export in (it holds local keys, never type or paste them), or ask the cloud: --cloud"
        );
    }
    let key = master_key(&config.server.data_dir, false)?
        .context("no master key (MOLI_MASTER_KEY[_FILE], or master.key in the data directory)")?;
    // The file the driver reads (its `devices_file`), else the data directory's.
    let devices_file = devices_file
        .or_else(|| tuya_driver(config, instance).ok().map(|t| t.devices_file))
        .unwrap_or_else(|| config.server.data_dir.join("tuya.json"));
    let path = config.server.data_dir.join("secrets.enc");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut import: moli_tuya::ImportFile = if cloud {
        let (keys, region) = tuya_cloud_access(config, instance, &path, &key)?;
        runtime
            .block_on(moli_tuya::cloud::import(keys, region))
            .context("the Tuya cloud did not give the devices")?
    } else {
        serde_json::from_reader(std::io::stdin().lock())
            .context("invalid import (expected the JSON of tuya-export.py)")?
    };
    let existing: Option<moli_tuya::DeviceFile> = match std::fs::read(&devices_file) {
        Ok(bytes) => Some(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if cloud {
        // The cloud names some points its own way: what works stays.
        moli_tuya::import::keep_known(&mut import, existing.as_ref());
    }
    if summary {
        for line in moli_tuya::import::summary(&import) {
            println!("{line}");
        }
        println!(
            "{} devices, nothing written (--summary); the import would write {}",
            import.devices.len(),
            devices_file.display()
        );
        return Ok(());
    }
    let (file, keys) = moli_tuya::merge_import(import, existing);
    for (name, value) in &keys {
        runtime.block_on(moli_runtime::set_secret(
            &path,
            &key,
            &format!("{instance}/{name}"),
            value,
        ))?;
    }
    let text = serde_json::to_string_pretty(&file)?;
    std::fs::write(&devices_file, text + "\n")
        .with_context(|| format!("cannot write {}", devices_file.display()))?;
    keep_owner(&config.server.data_dir, &[&devices_file, &path]);
    println!(
        "{} devices filed in {}, {} local keys stored (encrypted)",
        file.devices.len(),
        devices_file.display(),
        keys.len()
    );
    Ok(())
}

fn zigbee_command(config: &Config, command: ZigbeeCommand) -> anyhow::Result<()> {
    match command {
        ZigbeeCommand::Import {
            file,
            instance,
            force,
        } => {
            // Keys filed under a mistyped instance would never be read.
            zigbee_instance(config, &instance)?;
            let key = master_key(&config.server.data_dir, false)?.context(
                "no master key (MOLI_MASTER_KEY[_FILE], or master.key in the data directory)",
            )?;
            let summary = zigbee_import(&config.server.data_dir, &key, &instance, &file, force)?;
            println!("{summary}");
            Ok(())
        }
        ZigbeeCommand::Export {
            path,
            instance,
            force,
        } => {
            zigbee_instance(config, &instance)?;
            let key = master_key(&config.server.data_dir, false)?.context(
                "no master key (MOLI_MASTER_KEY[_FILE], or master.key in the data directory)",
            )?;
            let summary = zigbee_export(&config.server.data_dir, &key, &instance, &path, force)?;
            println!("{summary}");
            Ok(())
        }
    }
}

/// Files written by a tool run as root (the import) go back to whoever
/// owns the data directory: moli-os runs as that user and must still read
/// its vault and its devices file.
#[cfg(unix)]
fn keep_owner(data_dir: &std::path::Path, files: &[&std::path::Path]) {
    use std::os::unix::fs::MetadataExt as _;
    let Ok(dir) = std::fs::metadata(data_dir) else {
        return;
    };
    for file in files {
        let Ok(meta) = std::fs::metadata(file) else {
            continue;
        };
        if (meta.uid(), meta.gid()) != (dir.uid(), dir.gid())
            && let Err(e) = std::os::unix::fs::chown(file, Some(dir.uid()), Some(dir.gid()))
        {
            eprintln!(
                "{} keeps its owner ({e}): give it to the data directory's",
                file.display()
            );
        }
    }
}

/// `moli-os zigbee export`: the network the driver runs, as an Open
/// Coordinator Backup Zigbee2MQTT restores (to go back to it), written to
/// `path` (private, never over a file without `force`). Its keys are read
/// from the vault at that moment, never printed; nothing is written without
/// both of them. Returns what to print.
fn zigbee_export(
    data_dir: &std::path::Path,
    master: &MasterKey,
    instance: &str,
    path: &std::path::Path,
    force: bool,
) -> anyhow::Result<String> {
    use moli_zigbee::{backup, network, store};
    let store_path = store::path(data_dir, instance);
    let state = store::Store::load(&store_path)?;
    if state.import_pending {
        bail!(
            "an interrupted `moli-os zigbee import`: the vault and {} may not match; run that import again first",
            store_path.display()
        );
    }
    let Some(record) = state.network.as_ref() else {
        bail!(
            "{instance} records no network yet ({}): nothing to export",
            store_path.display()
        );
    };
    let secrets = data_dir.join("secrets.enc");
    let key = |name: &str| -> anyhow::Result<network::Key> {
        let entry = format!("{instance}/{name}");
        let held = moli_runtime::get_secret(&secrets, master, &entry)
            .with_context(|| format!("the vault ({}) cannot be read", secrets.display()))?
            .with_context(|| format!("no {entry} in the vault: nothing is exported without it"))?;
        network::Key::from_hex(&held)
            .with_context(|| format!("the vault's {entry} is not a 128-bit key (32 hex digits)"))
    };
    let network_key = key(network::NETWORK_KEY)?;
    let tc_link_key = key(network::TC_LINK_KEY)?;
    let exported = backup::export(
        &state,
        &network_key,
        &tc_link_key,
        &format!("moli-os@{}", env!("CARGO_PKG_VERSION")),
        &jiff::Timestamp::now().to_string(),
        backup::unix_now(),
    )?;
    exported.write_private(path, force)?;
    Ok(format!(
        "{instance}: network PAN {:#06x}, channel {}, {} device(s) exported to {}\n\
         frame counter {} (the last one recorded, {}, plus {}: Zigbee2MQTT restores from it as written)\n\
         {}",
        record.pan_id,
        record.channel,
        exported.devices.len(),
        path.display(),
        exported.network_key.frame_counter,
        record.frame_counter.unwrap_or_default(),
        backup::FRAME_COUNTER_MARGIN,
        moli_i18n::tr!("pilotes.zigbee.export_avertissement", path = path.display()),
    ))
}

/// `instance` is a zigbee driver of the configuration.
fn zigbee_instance(config: &Config, instance: &str) -> anyhow::Result<()> {
    if !config
        .drivers
        .iter()
        .any(|d| d.id == instance && d.kind == "zigbee")
    {
        bail!(
            "no [[driver]] with id = {instance:?} and kind = \"zigbee\" in the configuration: declare it first (integrations/zigbee/onboarding.md)"
        );
    }
    Ok(())
}

/// `moli-os zigbee import`: a Zigbee2MQTT coordinator backup, its keys into
/// the vault (`<instance>/network_key`, `<instance>/tc_link_key`), its
/// network into the driver's store. Returns what to print (never a key).
fn zigbee_import(
    data_dir: &std::path::Path,
    master: &MasterKey,
    instance: &str,
    file: &std::path::Path,
    force: bool,
) -> anyhow::Result<String> {
    use moli_zigbee::{backup::OpenCoordinatorBackup, network, store};
    let bytes = std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
    let import = OpenCoordinatorBackup::parse(&bytes)
        .and_then(|backup| backup.import())
        .with_context(|| format!("{} cannot be imported", file.display()))?;
    let store_path = store::path(data_dir, instance);
    let mut state = store::Store::load(&store_path)?;
    // Refused over another network before anything is written; its counter
    // recorded now (a restore refuses it a day later).
    import.apply(&mut state, force, moli_zigbee::backup::unix_now())?;
    let (tc_link_key, tc_link_origin) = match &import.tc_link_key {
        Some(key) => (key.clone(), "from the backup"),
        None => (
            network::Key::generate()?,
            "a new random one: the backup has no stack_specific.ezsp.hashed_tclk",
        ),
    };
    // Store, vault, store: the vault and the store cannot be written at
    // once, so the store first says an import is under way. Interrupted
    // anywhere, the driver waits (`import_pending`) instead of misreading
    // a store and a vault that do not match, until this same import runs
    // again (same network: no --force needed).
    state.import_pending = true;
    state.save(&store_path)?;
    let secrets = data_dir.join("secrets.enc");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    for (name, key) in [
        (network::NETWORK_KEY, &import.network_key),
        (network::TC_LINK_KEY, &tc_link_key),
    ] {
        runtime.block_on(moli_runtime::set_secret(
            &secrets,
            master,
            &format!("{instance}/{name}"),
            &key.to_hex(),
        ))
        .with_context(|| {
            format!(
                "the import was interrupted while storing {instance}/{name}: run the same command again"
            )
        })?;
    }
    state.import_pending = false;
    state.save(&store_path)?;
    let record = &import.record;
    Ok(format!(
        "{instance}: network PAN {:#06x}, channel {}, {} imported into {}\n\
         network key stored (encrypted) as {instance}/{}\n\
         trust center link key stored (encrypted) as {instance}/{} ({tc_link_origin})\n\
         frame counter {} (a restore starts {} above it)\n\
         moli-os must stay stopped during an import (a running Moli would overwrite it); start it now: \
         the driver takes this network over, restoring it on its own dongle if the dongle has none.\n\
         {}",
        record.pan_id,
        record.channel,
        import.devices_text(),
        store_path.display(),
        network::NETWORK_KEY,
        network::TC_LINK_KEY,
        record.frame_counter.unwrap_or_default(),
        moli_zigbee::backup::FRAME_COUNTER_MARGIN,
        moli_i18n::tr!("pilotes.zigbee.import_avertissement", path = file.display()),
    ))
}

/// Elsewhere, files keep the owner the system gives them.
#[cfg(not(unix))]
fn keep_owner(_data_dir: &std::path::Path, _files: &[&std::path::Path]) {}

/// The options of the tuya driver `instance`.
fn tuya_driver(config: &Config, instance: &str) -> anyhow::Result<moli_tuya::Config> {
    let driver = config
        .drivers
        .iter()
        .find(|d| d.id == instance && d.kind == "tuya")
        .with_context(|| format!("no tuya driver `{instance}` in the configuration"))?;
    driver
        .options
        .clone()
        .try_into()
        .context("the tuya driver's options")
}

/// The cloud project's keys and region of the tuya driver `instance`: its
/// `cloud_keys` file, else what a person gave in the dashboard (the vault).
fn tuya_cloud_access(
    config: &Config,
    instance: &str,
    vault: &std::path::Path,
    key: &MasterKey,
) -> anyhow::Result<(moli_tuya::cloud::Keys, moli_tuya::cloud::Region)> {
    let tuya = tuya_driver(config, instance)?;
    let region = moli_tuya::cloud::Region::named(&tuya.cloud_region).with_context(|| {
        format!(
            "unknown cloud_region `{}` (known: {})",
            tuya.cloud_region,
            moli_tuya::cloud::Region::names()
        )
    })?;
    let from_file = std::fs::read_to_string(&tuya.cloud_keys)
        .ok()
        .and_then(|text| moli_tuya::cloud::Keys::parse(&text));
    let stored = |name: &str| {
        moli_runtime::get_secret(vault, key, &format!("{instance}/{name}"))
            .ok()
            .flatten()
    };
    let keys = from_file
        .or_else(|| {
            stored(moli_tuya::cloud::STORED_ID)
                .zip(stored(moli_tuya::cloud::STORED_SECRET))
                .and_then(|(id, secret)| moli_tuya::cloud::Keys::new(&id, &secret))
        })
        .context(
            "no Tuya cloud keys: give them in the dashboard (Système), or in the driver's cloud_keys file",
        )?;
    Ok((keys, region))
}

/// `moli-os energy import`: hourly history from elsewhere (HA statistics).
fn energy_import(config: &Config) -> anyhow::Result<()> {
    let energy = config
        .energy
        .as_ref()
        .context("no [energy] section in the configuration")?;
    let mut rows = Vec::new();
    for (n, line) in std::io::stdin().lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        rows.push(
            serde_json::from_str::<moli_energy::ImportRow>(&line)
                .with_context(|| format!("line {}", n + 1))?,
        );
    }
    let report = moli_energy::import(&config.server.data_dir.join("energy.db"), energy, &rows)?;
    println!(
        "{} hours imported, {} priced, {} skipped (known or recorded live), {} invalid",
        report.inserted, report.repriced, report.skipped, report.invalid
    );
    Ok(())
}

fn energy_demo(config: &Config, days: u32) -> anyhow::Result<()> {
    if !config.server.demo {
        bail!("a made-up past is for a demo house only ([server] demo = true)");
    }
    let energy = config
        .energy
        .as_ref()
        .context("no [energy] section in the configuration")?;
    let tz = jiff::tz::TimeZone::get(&energy.timezone)?;
    let meters: Vec<String> = energy.meters.iter().map(|m| m.id.clone()).collect();
    let rows: Vec<moli_energy::ImportRow> =
        moli_demo::history::history(&meters, &tz, jiff::Timestamp::now(), days)
            .into_iter()
            .map(|(meter, hour, kwh)| moli_energy::ImportRow { meter, hour, kwh })
            .collect();
    let report = moli_energy::import(&config.server.data_dir.join("energy.db"), energy, &rows)?;
    println!(
        "{} hours imported, {} skipped (known or recorded live)",
        report.inserted, report.skipped
    );
    Ok(())
}

const HOUSEKEEPING: std::time::Duration = std::time::Duration::from_secs(15);
/// State cache flush every 4 housekeeping rounds (one minute).
const STATE_FLUSH_ROUNDS: u32 = 4;
/// The master key, from `MOLI_MASTER_KEY_FILE` (preferred: a tmpfs file,
/// never on disk) or `MOLI_MASTER_KEY`; else a new house's key file,
/// `master.key` in the data directory. `serve` creates that file when there
/// is no key at all and no secret yet (a store encrypted with an unknown key
/// never gets a new one). It never goes into a backup: keep a copy elsewhere.
fn master_key(data_dir: &std::path::Path, create: bool) -> anyhow::Result<Option<MasterKey>> {
    if let Some(key) = from_env_or_file("MOLI_MASTER_KEY")? {
        return Ok(Some(MasterKey::new(key)));
    }
    let path = data_dir.join("master.key");
    match std::fs::read_to_string(&path) {
        Ok(key) if key.trim().is_empty() => {
            bail!(
                "{} is empty: put the house's key back (a new one would not open its secrets)",
                path.display()
            )
        }
        Ok(key) => return Ok(Some(MasterKey::new(key.trim().to_owned()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
    if !create {
        return Ok(None);
    }
    if data_dir.join("secrets.enc").exists() {
        tracing::warn!(
            "secrets.enc exists but no master key: give MOLI_MASTER_KEY[_FILE] (a new key would not open it)"
        );
        return Ok(None);
    }
    let key = MasterKey::generate()?;
    if let Err(e) =
        std::fs::create_dir_all(data_dir).and_then(|()| write_private(&path, key.expose_for_file()))
    {
        // Moli still starts, without a store (as before a key existed).
        tracing::warn!(path = %path.display(), error = %e, "no master key written: secrets cannot be kept");
        return Ok(None);
    }
    tracing::warn!(
        path = %path.display(),
        "{}",
        moli_i18n::tr!("serveur.coffre.nouvelle_cle")
    );
    Ok(Some(key))
}

/// A new file only its owner can read, whole or absent (a power cut never
/// leaves an empty key): written aside, synced, then linked into place,
/// never over an existing one.
fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    let aside = path.with_extension("new");
    let _ = std::fs::remove_file(&aside);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let written = options.open(&aside).and_then(|mut file| {
        std::io::Write::write_all(&mut file, text.as_bytes())?;
        file.sync_all()
    });
    let linked = written.and_then(|()| {
        std::fs::hard_link(&aside, path).or_else(|e| {
            // Some shared folders have no hard links: a rename, still never
            // over an existing key.
            if path.exists() {
                Err(e)
            } else {
                std::fs::rename(&aside, path)
            }
        })
    });
    let _ = std::fs::remove_file(&aside);
    linked
}

/// Whether the configuration's folder holds no house yet (a wrong path next
/// to a house's data must fail, never start an empty house beside it).
fn fresh(config: &std::path::Path) -> bool {
    let dir = config
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    ![
        "secrets.enc",
        "state.json",
        "history.db",
        "journal.jsonl",
        "master.key",
    ]
    .iter()
    .any(|f| dir.join(f).exists())
}

/// A secret from `{NAME}_FILE` (preferred: a tmpfs file) or `{NAME}`.
fn from_env_or_file(name: &str) -> anyhow::Result<Option<String>> {
    if let Ok(path) = std::env::var(format!("{name}_FILE")) {
        let value = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {name} file {path}"))?;
        return Ok(Some(value.trim().to_owned()));
    }
    Ok(std::env::var(name).ok())
}

fn profile_command(command: &ProfileCommand, config_path: &std::path::Path) -> anyhow::Result<()> {
    match command {
        ProfileCommand::List => {
            for (id, _) in moli_profile::BUILTIN {
                println!("{id}");
            }
        }
        ProfileCommand::Check { profile } => {
            let (compiled, file) = moli_profile::open(profile)?;
            print_profile(&compiled, file.as_ref());
        }
        ProfileCommand::Trust { profile } => {
            let (compiled, file) = moli_profile::open(profile)?;
            let Some(file) = file else {
                bail!(
                    "{profile} is built in: it ships reviewed with the binary, nothing to approve"
                );
            };
            let config = Config::load(config_path)?;
            print_profile(&compiled, Some(&file));
            let trust_file = config.server.data_dir.join(moli_profile::TRUST_FILE);
            let recorded = moli_profile::trust(&trust_file, &file).with_context(|| {
                format!("cannot record the approval in {}", trust_file.display())
            })?;
            let what = match recorded {
                moli_profile::Recorded::Added => "approved",
                moli_profile::Recorded::Replaced => {
                    "approved (the previous version of this file no longer is)"
                }
                moli_profile::Recorded::AlreadyTrusted => "already approved",
            };
            println!(
                "{what}: sha256 {} in {}. Restart moli-os.",
                file.sha256,
                trust_file.display()
            );
        }
    }
    Ok(())
}

/// What a profile reads and writes, every path in full: what a human
/// approves.
fn print_profile(compiled: &moli_profile::Compiled, file: Option<&moli_profile::ProfileFile>) {
    println!("✓ {} — {}", compiled.meta.id, compiled.meta.name);
    if let Some(file) = file {
        println!("  file    {}", file.path);
        println!("  sha256  {}", file.sha256);
    }
    let scheme = if compiled.meta.https { "https" } else { "http" };
    match &compiled.meta.host {
        Some(host) => println!("  host    {scheme}://{host}"),
        None => println!("  host    {scheme}, the instance's `host`"),
    }
    if let Some(secret) = &compiled.meta.secret {
        println!("  secret  {secret} (from the encrypted store, sent to that host only)");
    }
    if !compiled.vars.is_empty() {
        let vars: Vec<&str> = compiled.vars.iter().map(String::as_str).collect();
        println!("  needs vars: {}", vars.join(", "));
    }
    for r in &compiled.requests {
        let method = match &r.call {
            moli_profile::Call::Get => "GET".to_owned(),
            moli_profile::Call::Meross { namespace, .. } => {
                format!("POST (meross GET {namespace})")
            }
        };
        println!(
            "  request {:<10} every {:>5}s  {:?}  {method} {}",
            r.id,
            r.every.as_secs(),
            r.format,
            r.path
        );
        for (name, value) in &r.headers {
            println!("          header {name}: {value}");
        }
    }
    for p in &compiled.points {
        let unit = p
            .spec
            .unit
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        println!(
            "  point   {:<20} {:<12} {:<6} {:?}",
            p.spec.key,
            format!("{:?}", p.spec.semantic),
            unit,
            p.spec.kind
        );
    }
    if let Some(path) = &compiled.meta.snapshot {
        println!("  image   on demand           GET {path}");
    }
    if compiled.writes.is_empty() {
        println!("  writes: none (read-only)");
        return;
    }
    println!("  writes (each one is a command: guard and journal apply):");
    for w in &compiled.writes {
        println!("  write   {:<20} GET {}", w.key, w.path);
        if let Some(i) = w.refresh {
            println!("          then reads {}", compiled.requests[i].id);
        }
        if let Some(expect) = &w.expect {
            println!("          done only if the answer contains {expect:?}");
        }
    }
}

/// Every 15 s: held orders nobody decided on expire; last known values are
/// flushed (only when changed) every minute.
fn housekeeping(hub: &Hub, cancel: &CancellationToken) -> tokio::task::JoinHandle<()> {
    let (hub, cancel) = (hub.clone(), cancel.clone());
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(HOUSEKEEPING);
        let mut rounds = 0u32;
        let mut unmatched: Vec<String> = Vec::new();
        let mut unmatched_devices: Vec<String> = Vec::new();
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = tick.tick() => {}
            }
            hub.expire_approvals();
            rounds = rounds.wrapping_add(1);
            // Logged once per change, not every round.
            let lost = hub.unmatched_protected_rooms();
            // (Skip the first round: drivers are still loading.)
            if rounds > 1 && lost != unmatched {
                if !lost.is_empty() {
                    tracing::warn!(rooms = ?lost, "protected rooms match no device: protection not applying");
                }
                unmatched = lost;
            }
            let lost = hub.unmatched_protected_devices();
            if rounds > 1 && lost != unmatched_devices {
                if !lost.is_empty() {
                    tracing::warn!(devices = ?lost, "protected devices unknown: protection not applying");
                }
                unmatched_devices = lost;
            }
            if !rounds.is_multiple_of(STATE_FLUSH_ROUNDS) {
                continue;
            }
            if let Err(e) = hub.persist_state().await {
                tracing::warn!(error = %e, "state cache not written");
            }
        }
    })
}

/// History: every change, kept `history_days` days (0 disables it).
fn start_history(
    config: &Config,
    hub: &Hub,
    cancel: &CancellationToken,
) -> anyhow::Result<Option<(moli_history::History, tokio::task::JoinHandle<()>)>> {
    if config.server.history_days == 0 {
        return Ok(None);
    }
    let options = moli_history::Options {
        path: config.server.data_dir.join("history.db"),
        retention_days: config.server.history_days,
    };
    let started = moli_history::History::start(hub, &options, cancel.clone())
        .context("cannot open the history database")?;
    Ok(Some(started))
}

/// Every driver kind this binary builds (`[[driver]] kind`): what the
/// catalogue checks packages against, without building anything. A kind
/// missing here is unknown, whatever `build_driver`'s match says.
const KINDS: &[&str] = &[
    "z2m",
    "zigbee",
    "zwave",
    "hue",
    "overkiz",
    "dirigera",
    "enedis",
    "homeassistant",
    "igd",
    "melcloud",
    "tapo",
    "lifx",
    "voltalis",
    "shelly",
    "airzone",
    "izypower",
    "solarman",
    "philips",
    "frigate",
    "bambu",
    "esphome",
    "alexa",
    "sonos",
    "airplay",
    "reolink",
    "tuya",
    "helpers",
    "presence",
    "telegram",
    "phones",
    "moonraker",
    "ipp",
    "host",
    "profile",
    "demo",
];

/// Every enabled driver, running. The phones driver and the API share its
/// gateway (pairing, reports): returned along.
/// The devices Moli runs for the house itself: the light fixtures (D12)
/// and the « Énergie » figures as points, for automations.
fn spawn_house_drivers(
    config: &Config,
    hub: &Hub,
    energy: Option<&moli_energy::Energy>,
    cancel: &CancellationToken,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut tasks = Vec::new();
    if !config.fixtures.is_empty() {
        tasks.push(spawn_driver(
            hub,
            InstanceId::from(moli_lights::INSTANCE),
            Arc::new(moli_lights::Fixtures::new(
                hub.clone(),
                config.fixtures.clone(),
            )),
            cancel.clone(),
        ));
    }
    if let Some(energy) = energy {
        tasks.push(spawn_driver(
            hub,
            InstanceId::from(moli_energy::points::INSTANCE),
            Arc::new(moli_energy::points::Points::new(energy.clone())),
            cancel.clone(),
        ));
        let energy_config = energy.config();
        if !energy_config.loads.is_empty()
            && let Ok(tz) = jiff::tz::TimeZone::get(&energy_config.timezone)
        {
            tasks.push(spawn_driver(
                hub,
                InstanceId::from(moli_energy::load::INSTANCE),
                Arc::new(moli_energy::load::Loads::new(
                    hub.clone(),
                    energy_config.loads.clone(),
                    tz,
                    energy_config.tariff.as_ref().map(|t| t.period.clone()),
                )),
                cancel.clone(),
            ));
        }
    }
    tasks
}

/// The household's people (`data/people.json`), and the driver that says
/// where each one is (`personnes:*`).
fn spawn_household(
    config: &Config,
    hub: &Hub,
    phones: Option<moli_phones::Gateway>,
    cancel: &CancellationToken,
) -> (moli_api::Household, tokio::task::JoinHandle<()>) {
    let owners = config
        .server
        .access
        .as_ref()
        .map(|a| a.owners.clone())
        .unwrap_or_default();
    let household = moli_api::Household::open(hub, Some(&config.server.data_dir), &owners);
    let task = spawn_driver(
        hub,
        InstanceId::from(moli_api::PEOPLE_INSTANCE),
        household.whereabouts(hub, phones),
        cancel.clone(),
    );
    (household, task)
}

/// What some drivers share with the API: the phones' pairing and reports,
/// the computers' agents.
#[derive(Default)]
struct Gateways {
    phones: Option<moli_phones::Gateway>,
    machines: Option<moli_host::agents::Agents>,
}

fn spawn_drivers(
    config: &Config,
    hub: &Hub,
    cancel: &CancellationToken,
    airzone: &managed_airzone::Manager,
) -> anyhow::Result<(Vec<tokio::task::JoinHandle<()>>, Gateways)> {
    let mut tasks = Vec::new();
    let mut gates = Gateways::default();
    for driver in config.drivers.iter().filter(|d| d.enabled != Some(false)) {
        if driver.kind == "airzone" {
            continue;
        }
        let context = || format!("driver {:?} options", driver.id);
        let built: Arc<dyn Driver> = match driver.kind.as_str() {
            "phones" => {
                let options = driver.options.clone().try_into().with_context(context)?;
                let (gateway, phones_driver) = moli_phones::open(options, &config.server.data_dir)?;
                gates.phones = Some(gateway);
                Arc::new(phones_driver)
            }
            "host" => {
                let options = driver.options.clone().try_into().with_context(context)?;
                let (agents, host) = moli_host::open(options);
                gates.machines = Some(agents);
                Arc::new(host)
            }
            _ => build_driver(driver, &config.server.data_dir)?,
        };
        tasks.push(spawn_driver(
            hub,
            InstanceId::from(driver.id.as_str()),
            built,
            cancel.clone(),
        ));
    }
    airzone.start_configured()?;
    Ok((tasks, gates))
}

#[allow(clippy::too_many_lines)] // one short arm per kind of driver
fn build_driver(
    config: &DriverConfig,
    data_dir: &std::path::Path,
) -> anyhow::Result<Arc<dyn Driver>> {
    let context = || format!("driver {:?} options", config.id);
    if !KINDS.contains(&config.kind.as_str()) {
        bail!(
            "unknown driver kind {:?} (driver {:?}; known: {})",
            config.kind,
            config.id,
            KINDS.join(", ")
        );
    }
    Ok(match config.kind.as_str() {
        "z2m" => Arc::new(moli_z2m::Z2m::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "zigbee" => Arc::new(moli_zigbee::Zigbee::new(
            config.options.clone().try_into().with_context(context)?,
            data_dir,
        )),
        "zwave" => Arc::new(moli_zwave::Zwave::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "hue" => Arc::new(moli_hue::Hue::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "homeassistant" => configured_driver(config, moli_homeassistant::HomeAssistant::new)?,
        "overkiz" => configured_driver(config, moli_overkiz::Overkiz::new)?,
        "dirigera" => Arc::new(moli_dirigera::Dirigera::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "enedis" => Arc::new(moli_enedis::Enedis::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "igd" => Arc::new(moli_igd::Igd::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "melcloud" => Arc::new(moli_melcloud::Melcloud::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "tapo" => Arc::new(moli_tapo::Tapo::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "lifx" => Arc::new(moli_lifx::Lifx::new(
            config.options.clone().try_into().with_context(context)?,
        )?),
        "voltalis" => Arc::new(moli_voltalis::Voltalis::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "shelly" => Arc::new(moli_shelly::Shelly::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "airzone" => configured_driver(config, moli_airzone::Airzone::new)?,
        "izypower" => Arc::new(moli_izypower::Izypower::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "solarman" => Arc::new(moli_solarman::Solarman::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "philips" => Arc::new(moli_philips::Philips::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "frigate" => Arc::new(moli_frigate::Frigate::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "bambu" => Arc::new(moli_bambu::Bambu::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "esphome" => Arc::new(moli_esphome::Esphome::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "alexa" => Arc::new(moli_alexa::Alexa::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "moonraker" => Arc::new(moli_moonraker::Moonraker::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "ipp" => Arc::new(moli_ipp::Ipp::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "host" => Arc::new(moli_host::Host::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "sonos" => Arc::new(moli_sonos::Sonos::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "airplay" => Arc::new(moli_airplay::Airplay::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "reolink" => Arc::new(moli_reolink::Reolink::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "tuya" => Arc::new(moli_tuya::Tuya::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "presence" => configured_driver(config, moli_presence::Presence::new)?,
        "phones" => Arc::new(
            moli_phones::open(
                config.options.clone().try_into().with_context(context)?,
                data_dir,
            )?
            .1,
        ),
        "telegram" => configured_driver(config, moli_telegram::Telegram::new)?,
        "helpers" => configured_driver(config, moli_helpers::Helpers::new)?,
        "demo" => configured_driver(config, moli_demo::Demo::new)?,
        "profile" => Arc::new(moli_profile::ProfileDriver::new(
            config.options.clone().try_into().with_context(context)?,
            data_dir.join(moli_profile::TRUST_FILE),
        )?),
        other => bail!("driver kind {other:?} is in KINDS but never built"),
    })
}

/// Drivers with validated constructors share option decoding and error context.
fn configured_driver<C, D>(
    config: &DriverConfig,
    create: fn(C) -> anyhow::Result<D>,
) -> anyhow::Result<Arc<dyn Driver>>
where
    C: serde::de::DeserializeOwned,
    D: Driver,
{
    let context = || format!("driver {:?} options", config.id);
    let options = config.options.clone().try_into().with_context(context)?;
    Ok(Arc::new(create(options).with_context(context)?))
}

/// `moli-os health`: the local instance answers, quickly.
fn health() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let request = moli_net::empty(
        http::Request::builder()
            .method(http::Method::GET)
            .uri("/api/health")
            .header(http::header::HOST, "127.0.0.1"),
    )?;
    let (status, _) = runtime.block_on(moli_net::plain("127.0.0.1", 8790, request))?;
    if !status.is_success() {
        bail!("health: {status}");
    }
    Ok(())
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn driver(kind: &str) -> DriverConfig {
        DriverConfig {
            id: "t".into(),
            kind: kind.into(),
            enabled: None,
            options: toml::Value::Table(toml::Table::new()),
        }
    }

    #[test]
    fn kinds_are_what_build_driver_builds() {
        let data = std::env::temp_dir();
        for kind in KINDS {
            // Empty options: built, or refused for its options, never for its kind.
            if let Err(e) = build_driver(&driver(kind), &data) {
                let e = format!("{e:#}");
                assert!(!e.contains("driver kind"), "{kind}: {e}");
            }
        }
        let Err(e) = build_driver(&driver("x10"), &data) else {
            panic!("an unknown kind is built");
        };
        assert!(e.to_string().contains("unknown driver kind"), "{e}");
    }

    /// A data directory of its own, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("moli-os-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn zigbee_fixture() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../integrations/zigbee/fixtures/coordinator_backup.json")
    }

    /// What the vault of `dir` holds for `zigbee/<name>`.
    fn vault_secret(dir: &std::path::Path, master: &MasterKey, name: &str) -> Option<String> {
        let hub = Hub::new(HubOptions {
            secrets: Some((dir.join("secrets.enc"), master.clone())),
            ..HubOptions::default()
        })
        .unwrap();
        hub.secret(&InstanceId::from("zigbee"), name)
    }

    #[test]
    fn import_stores_key_in_vault_only() {
        let data = Scratch::new("zigbee-import");
        let master = MasterKey::generate().unwrap();
        let started = moli_zigbee::backup::unix_now();
        let summary = zigbee_import(&data.0, &master, "zigbee", &zigbee_fixture(), false).unwrap();
        let ended = moli_zigbee::backup::unix_now();
        // The keys: in the vault, never in the store nor in what is printed.
        let key = "01030507090b0d0f00020406080a0c0d";
        let tclk = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";
        assert_eq!(
            vault_secret(&data.0, &master, "network_key").as_deref(),
            Some(key)
        );
        assert_eq!(
            vault_secret(&data.0, &master, "tc_link_key").as_deref(),
            Some(tclk)
        );
        let store_path = moli_zigbee::store::path(&data.0, "zigbee");
        let file = std::fs::read_to_string(&store_path).unwrap().to_lowercase();
        for secret in [key, tclk, "0103", "5a5a"] {
            assert!(!file.contains(secret), "{file}");
            assert!(!summary.to_lowercase().contains(secret), "{summary}");
        }
        let store = moli_zigbee::store::Store::load(&store_path).unwrap();
        let network = store.network.unwrap();
        assert_eq!(network.pan_id, 0x1a62);
        assert_eq!(network.frame_counter, Some(123_456));
        assert!(network.frame_counter_fresh);
        // Recorded at the time of the import, whatever the backup's date.
        let at = network.frame_counter_recorded_at.unwrap();
        assert!((started..=ended).contains(&at), "{at}");
        assert_eq!(store.devices.len(), 2);
        assert!(summary.contains("0x1a62"), "{summary}");
        assert!(summary.contains("channel 15"), "{summary}");
        assert!(summary.contains("2 devices"), "{summary}");
        assert!(summary.contains("zigbee/network_key"), "{summary}");
        assert!(summary.contains("stay stopped"), "{summary}");
        // The copy made for the import holds the key: deleted afterwards.
        assert!(
            summary.contains(&moli_i18n::tr!(
                "pilotes.zigbee.import_avertissement",
                path = zigbee_fixture().display()
            )),
            "{summary}"
        );
        assert!(!store.import_pending);
    }

    #[test]
    fn import_without_trust_center_key_makes_one() {
        let data = Scratch::new("zigbee-import-tclk");
        let master = MasterKey::generate().unwrap();
        let mut backup: serde_json::Value =
            serde_json::from_slice(&std::fs::read(zigbee_fixture()).unwrap()).unwrap();
        backup["stack_specific"]["ezsp"] = serde_json::json!({});
        let file = data.0.join("backup.json");
        std::fs::write(&file, serde_json::to_vec(&backup).unwrap()).unwrap();
        let summary = zigbee_import(&data.0, &master, "zigbee", &file, false).unwrap();
        let tclk = vault_secret(&data.0, &master, "tc_link_key").expect("a new key");
        assert_eq!(tclk.len(), 32);
        assert_ne!(tclk, "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a");
        assert!(summary.contains("new random"), "{summary}");
    }

    #[test]
    fn an_interrupted_import_waits() {
        let data = Scratch::new("zigbee-import-interrupted");
        let master = MasterKey::generate().unwrap();
        // The vault cannot be written: the import stops between its steps.
        std::fs::create_dir_all(data.0.join("secrets.enc")).unwrap();
        let error =
            zigbee_import(&data.0, &master, "zigbee", &zigbee_fixture(), false).unwrap_err();
        let shown = format!("{error:#}");
        assert!(shown.contains("run the same command again"), "{shown}");
        // The store says so (the driver waits on it), the key went nowhere.
        let store_path = moli_zigbee::store::path(&data.0, "zigbee");
        let pending = moli_zigbee::store::Store::load(&store_path).unwrap();
        assert!(pending.import_pending);
        assert_eq!(pending.network.as_ref().unwrap().pan_id, 0x1a62);
        // Run again (the vault writable now): completed, without --force.
        std::fs::remove_dir(data.0.join("secrets.enc")).unwrap();
        zigbee_import(&data.0, &master, "zigbee", &zigbee_fixture(), false).unwrap();
        let done = moli_zigbee::store::Store::load(&store_path).unwrap();
        assert!(!done.import_pending);
        let network = done.network.unwrap();
        assert_eq!(
            network,
            moli_zigbee::store::NetworkRecord {
                frame_counter_recorded_at: network.frame_counter_recorded_at,
                ..pending.network.unwrap()
            }
        );
        assert_eq!(
            vault_secret(&data.0, &master, "network_key").as_deref(),
            Some("01030507090b0d0f00020406080a0c0d")
        );
        let text = std::fs::read_to_string(&store_path).unwrap();
        assert!(!text.contains("import_pending"), "{text}");
    }

    #[test]
    fn import_over_another_network_needs_force() {
        let data = Scratch::new("zigbee-import-force");
        let master = MasterKey::generate().unwrap();
        let store_path = moli_zigbee::store::path(&data.0, "zigbee");
        let other = moli_zigbee::store::Store {
            network: Some(moli_zigbee::store::NetworkRecord {
                pan_id: 0x1234,
                extended_pan_id: [0xEE; 8],
                channel: 11,
                coordinator_ieee: "0x00124b0000000001".into(),
                nwk_update_id: 0,
                frame_counter: None,
                frame_counter_fresh: false,
                frame_counter_recorded_at: None,
                key_sequence_number: None,
            }),
            ..moli_zigbee::store::Store::default()
        };
        other.save(&store_path).unwrap();
        let error = zigbee_import(&data.0, &master, "zigbee", &zigbee_fixture(), false)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("0x1234") && error.contains("--force"),
            "{error}"
        );
        // Nothing touched: no vault written, the store as it was.
        assert!(!data.0.join("secrets.enc").exists());
        assert_eq!(moli_zigbee::store::Store::load(&store_path).unwrap(), other);
        zigbee_import(&data.0, &master, "zigbee", &zigbee_fixture(), true).unwrap();
        let store = moli_zigbee::store::Store::load(&store_path).unwrap();
        assert_eq!(store.network.unwrap().pan_id, 0x1a62);
        assert!(vault_secret(&data.0, &master, "network_key").is_some());
    }

    #[test]
    fn export_round_trips() {
        let data = Scratch::new("zigbee-export");
        let master = MasterKey::generate().unwrap();
        zigbee_import(&data.0, &master, "zigbee", &zigbee_fixture(), false).unwrap();
        let out = data.0.join("coordinator_backup.json");
        let summary = zigbee_export(&data.0, &master, "zigbee", &out, false).unwrap();
        let key = vault_secret(&data.0, &master, "network_key").unwrap();
        let tclk = vault_secret(&data.0, &master, "tc_link_key").unwrap();
        // Never printed; the person is told what the file holds.
        for secret in [key.as_str(), tclk.as_str(), "0103", "5a5a"] {
            assert!(!summary.to_lowercase().contains(secret), "{summary}");
        }
        assert!(summary.contains("188992"), "{summary}");
        assert!(
            summary.contains(&moli_i18n::tr!(
                "pilotes.zigbee.export_avertissement",
                path = out.display()
            )),
            "{summary}"
        );
        // Read back: the store's network, the vault's keys, the counter
        // plus the margin.
        let bytes = std::fs::read(&out).unwrap();
        let backup = moli_zigbee::backup::OpenCoordinatorBackup::parse(&bytes).unwrap();
        assert_eq!(
            moli_zigbee::backup::OpenCoordinatorBackup::parse(backup.to_json().as_bytes()).unwrap(),
            backup
        );
        let written: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let fixture: serde_json::Value =
            serde_json::from_slice(&std::fs::read(zigbee_fixture()).unwrap()).unwrap();
        for field in [
            "coordinator_ieee",
            "pan_id",
            "extended_pan_id",
            "nwk_update_id",
            "channel",
            "channel_mask",
            "stack_specific",
        ] {
            assert_eq!(written[field], fixture[field], "{field}");
        }
        assert_eq!(written["network_key"]["key"], key.as_str());
        assert_eq!(
            written["stack_specific"]["ezsp"]["hashed_tclk"],
            tclk.as_str()
        );
        assert_eq!(written["network_key"]["frame_counter"], 123_456 + 65_536);
        assert!(
            written["metadata"]["source"]
                .as_str()
                .unwrap()
                .starts_with("moli-os@")
        );
        assert_eq!(written["devices"].as_array().unwrap().len(), 2);
        // Imported into an empty instance: the same network and keys, its
        // counter the exported one.
        let other = Scratch::new("zigbee-export-import");
        let other_master = MasterKey::generate().unwrap();
        zigbee_import(&other.0, &other_master, "zigbee", &out, false).unwrap();
        let load = |dir: &std::path::Path| {
            moli_zigbee::store::Store::load(&moli_zigbee::store::path(dir, "zigbee")).unwrap()
        };
        let (before, after) = (load(&data.0), load(&other.0));
        let network = after.network.unwrap();
        assert_eq!(
            network,
            moli_zigbee::store::NetworkRecord {
                frame_counter: Some(188_992),
                frame_counter_recorded_at: network.frame_counter_recorded_at,
                ..before.network.unwrap()
            }
        );
        assert_eq!(after.devices, before.devices);
        assert_eq!(
            vault_secret(&other.0, &other_master, "network_key"),
            Some(key)
        );
        assert_eq!(
            vault_secret(&other.0, &other_master, "tc_link_key"),
            Some(tclk)
        );
    }

    #[test]
    fn an_old_counter_is_not_exported() {
        let data = Scratch::new("zigbee-export-old");
        let master = MasterKey::generate().unwrap();
        zigbee_import(&data.0, &master, "zigbee", &zigbee_fixture(), false).unwrap();
        // Moli stopped for more than a day after its last record.
        let store_path = moli_zigbee::store::path(&data.0, "zigbee");
        let mut store = moli_zigbee::store::Store::load(&store_path).unwrap();
        let day = moli_zigbee::backup::COUNTER_MAX_AGE.as_secs();
        store.network.as_mut().unwrap().frame_counter_recorded_at =
            Some(moli_zigbee::backup::unix_now() - day - 60);
        store.save(&store_path).unwrap();
        let out = data.0.join("coordinator_backup.json");
        let error = zigbee_export(&data.0, &master, "zigbee", &out, false)
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            moli_i18n::tr!("pilotes.zigbee.export_compteur_ancien")
        );
        assert!(!out.exists(), "nothing written");
    }

    #[test]
    fn export_needs_the_vault_keys() {
        let data = Scratch::new("zigbee-export-no-key");
        let master = MasterKey::generate().unwrap();
        zigbee_import(&data.0, &master, "zigbee", &zigbee_fixture(), false).unwrap();
        let out = data.0.join("coordinator_backup.json");
        let secrets = data.0.join("secrets.enc");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for name in ["network_key", "tc_link_key"] {
            let held = vault_secret(&data.0, &master, name).unwrap();
            let entry = format!("zigbee/{name}");
            runtime
                .block_on(moli_runtime::forget_secret(&secrets, &master, &entry))
                .unwrap();
            let error = zigbee_export(&data.0, &master, "zigbee", &out, false)
                .unwrap_err()
                .to_string();
            assert!(error.contains(&entry), "{error}");
            assert!(!out.exists(), "never written without its key");
            runtime
                .block_on(moli_runtime::set_secret(&secrets, &master, &entry, &held))
                .unwrap();
        }
        // Another master key: the vault cannot be read, nothing written.
        let error = zigbee_export(
            &data.0,
            &MasterKey::generate().unwrap(),
            "zigbee",
            &out,
            false,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("vault"), "{error:#}");
        assert!(!out.exists());
        // No network recorded for that instance: nothing written either.
        let error = zigbee_export(&data.0, &master, "other", &out, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no network"), "{error}");
        assert!(!out.exists());
        let names: Vec<_> = std::fs::read_dir(&data.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.iter().all(|n| !n.contains(".tmp")), "{names:?}");
    }

    #[test]
    fn check_config_validates_the_assistant_providers() {
        let bad: moli_assistant::Config = toml::from_str("[routes]\nchat = \"local\"\n").unwrap();
        assert!(bad.check().is_err());
        let good: moli_assistant::Config = toml::from_str("").unwrap();
        assert!(good.check().is_ok());
    }

    #[test]
    fn the_catalogue_describes_every_driver_and_built_in_profile() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations");
        let rows = catalogue::check(&dir, KINDS).unwrap();
        for row in &rows {
            assert!(row.errors.is_empty(), "{}: {:?}", row.id, row.errors);
        }
        let natives: std::collections::BTreeSet<&str> = rows
            .iter()
            .filter(|r| r.driver != "profile")
            .map(|r| r.driver.as_str())
            .collect();
        let kinds: std::collections::BTreeSet<&str> =
            KINDS.iter().copied().filter(|k| *k != "profile").collect();
        assert_eq!(natives, kinds, "one package per native kind");
        let profiles: std::collections::BTreeSet<&str> = rows
            .iter()
            .filter(|r| r.driver == "profile")
            .map(|r| r.id.as_str())
            .collect();
        let builtin: std::collections::BTreeSet<&str> =
            moli_profile::BUILTIN.iter().map(|(id, _)| *id).collect();
        assert_eq!(profiles, builtin, "one package per built-in profile");
    }
}
