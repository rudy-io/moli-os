//! Moli OS binary: one process, every surface.

mod backup;
mod catalogue;
mod config;

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
    /// File what the Tuya cloud knows (JSON on standard input, see
    /// tools/home-assistant/tuya-export.py): local keys into the encrypted store,
    /// the rest into the devices file. Keys are never printed. Restart
    /// moli-os afterwards.
    Import {
        /// The tuya driver instance the keys belong to.
        #[arg(long, default_value = "tuya")]
        instance: String,
        /// Default: `tuya.json` in the data directory.
        #[arg(long)]
        devices_file: Option<std::path::PathBuf>,
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
    // A new house starts without a configuration: `serve` writes a minimal
    // one; every other command needs the real thing.
    let serving = matches!(cli.command, None | Some(Command::Serve));
    let config = if serving && !cli.config.exists() && fresh(&cli.config) {
        Config::starter(&cli.config)?
    } else {
        Config::load(&cli.config)?
    };
    match cli.command.unwrap_or(Command::Serve) {
        Command::Profile(_) | Command::Catalogue(_) | Command::Health => {
            unreachable!("handled above")
        }
        Command::Backup { dir } => backup::backup(&config.server.data_dir, &dir),
        Command::Restore { dir, force } => backup::restore(&config.server.data_dir, &dir, force),
        Command::Tuya(TuyaCommand::Import {
            instance,
            devices_file,
        }) => tuya_import(&config, &instance, devices_file),
        Command::Energy(EnergyCommand::Import) => energy_import(&config),
        Command::Energy(EnergyCommand::Demo { days }) => energy_demo(&config, days),
        Command::CheckConfig => {
            for driver in &config.drivers {
                build_driver(driver, &config.server.data_dir)?;
            }
            println!("configuration OK ({} driver(s))", config.drivers.len());
            Ok(())
        }
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

async fn serve(config: Config, boot: Instant) -> anyhow::Result<()> {
    // Before anything opens the data directory.
    demo_fresh(&config)?;
    // The house's language: the one chosen at the installation, else the
    // configuration's.
    let language = moli_api::settings::language(&config.server.data_dir)
        .unwrap_or_else(|| config.server.language.clone());
    if !moli_i18n::set_language(&language) {
        tracing::warn!(%language, "unknown language: Moli speaks {}", moli_i18n::FALLBACK);
    }
    // The master key comes from the environment (a secrets manager), or a
    // new house's key file.
    let secrets = master_key(&config.server.data_dir, true)?
        .map(|key| (config.server.data_dir.join("secrets.enc"), key));
    if secrets.is_none() {
        tracing::warn!("no master key: drivers cannot persist pairing credentials");
    }
    let hub = Hub::new(HubOptions {
        labels_path: Some(config.server.data_dir.join("labels.toml")),
        state_path: Some(config.server.data_dir.join("state.json")),
        journal_path: Some(config.server.data_dir.join("journal.jsonl")),
        secrets,
        guard: config.guard.policy()?,
        ..HubOptions::default()
    })
    .context("cannot open the data directory (labels, journal, secrets)")?;
    let cancel = CancellationToken::new();

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

    let (mut tasks, gates) = spawn_drivers(&config, &hub, &cancel)?;
    tasks.extend(spawn_house_drivers(&config, &hub, energy.as_ref(), &cancel));
    let (household, whereabouts) = spawn_household(&config, &hub, gates.phones.clone(), &cancel);
    tasks.push(whereabouts);

    let listener = tokio::net::TcpListener::bind(config.server.listen)
        .await
        .with_context(|| format!("cannot listen on {}", config.server.listen))?;
    tracing::info!(
        listen = %config.server.listen,
        drivers = tasks.len(),
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
/// where the assistant places it, else the automations (for the sun).
fn weather(config: &Config) -> Option<moli_weather::Weather> {
    let assistant = config.assistant.as_ref();
    let latitude = assistant
        .and_then(|a| a.latitude)
        .or(config.automations.latitude);
    let longitude = assistant
        .and_then(|a| a.longitude)
        .or(config.automations.longitude);
    // The demo house sits nowhere: no sky from the outside world.
    if config.server.demo {
        return None;
    }
    latitude.zip(longitude).map(|(lat, lon)| {
        moli_weather::Weather::new(lat, lon, Some(config.server.data_dir.clone()))
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
    let (automations, task) = moli_automation::Automations::start(
        hub,
        config.automations.clone(),
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

/// The home's own assistant: always there (without `[assistant]`, with its
/// defaults), waiting for a key given from the dashboard if it has none.
fn start_assistant(
    config: &Config,
    hub: &Hub,
    energy: Option<&moli_energy::Energy>,
    history: Option<&moli_history::History>,
) -> anyhow::Result<Option<moli_assistant::Assistant>> {
    let mut settings = config.assistant.clone().unwrap_or_default();
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
    Ok(Some(moli))
}

/// `moli-os tuya import`: keys into the encrypted store, the rest into the
/// devices file.
fn tuya_import(
    config: &Config,
    instance: &str,
    devices_file: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        bail!("pipe the export in: it holds local keys, never type or paste them");
    }
    let key = master_key(&config.server.data_dir, false)?
        .context("no master key (MOLI_MASTER_KEY[_FILE], or master.key in the data directory)")?;
    let devices_file = devices_file.unwrap_or_else(|| config.server.data_dir.join("tuya.json"));
    let import: moli_tuya::ImportFile = serde_json::from_reader(std::io::stdin().lock())
        .context("invalid import (expected the JSON of tuya-export.py)")?;
    let existing = match std::fs::read(&devices_file) {
        Ok(bytes) => Some(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let (file, keys) = moli_tuya::merge_import(import, existing);
    let path = config.server.data_dir.join("secrets.enc");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
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
    println!(
        "{} devices filed in {}, {} local keys stored (encrypted)",
        file.devices.len(),
        devices_file.display(),
        keys.len()
    );
    Ok(())
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
    "hue",
    "igd",
    "tapo",
    "philips",
    "frigate",
    "bambu",
    "esphome",
    "alexa",
    "sonos",
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
) -> anyhow::Result<(Vec<tokio::task::JoinHandle<()>>, Gateways)> {
    let mut tasks = Vec::new();
    let mut gates = Gateways::default();
    for driver in config.drivers.iter().filter(|d| d.enabled != Some(false)) {
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
    Ok((tasks, gates))
}

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
        "hue" => Arc::new(moli_hue::Hue::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "igd" => Arc::new(moli_igd::Igd::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "tapo" => Arc::new(moli_tapo::Tapo::new(
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
        "reolink" => Arc::new(moli_reolink::Reolink::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "tuya" => Arc::new(moli_tuya::Tuya::new(
            config.options.clone().try_into().with_context(context)?,
        )),
        "presence" => Arc::new(moli_presence::Presence::new(
            config.options.clone().try_into().with_context(context)?,
        )?),
        "phones" => Arc::new(
            moli_phones::open(
                config.options.clone().try_into().with_context(context)?,
                data_dir,
            )?
            .1,
        ),
        "telegram" => Arc::new(moli_telegram::Telegram::new(
            config.options.clone().try_into().with_context(context)?,
        )?),
        "helpers" => Arc::new(moli_helpers::Helpers::new(
            config.options.clone().try_into().with_context(context)?,
        )?),
        "demo" => Arc::new(moli_demo::Demo::new(
            config.options.clone().try_into().with_context(context)?,
        )?),
        "profile" => Arc::new(moli_profile::ProfileDriver::new(
            config.options.clone().try_into().with_context(context)?,
            data_dir.join(moli_profile::TRUST_FILE),
        )?),
        other => bail!("driver kind {other:?} is in KINDS but never built"),
    })
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
