//! The machine Moli runs on (a mini-PC, a Pi…): how busy its
//! processor is, its memory, its temperatures, its network, since when it
//! runs. Linux only, by reading `/proc` and `/sys` (no privilege, nothing to
//! install). Disks and containers are a host script's (`data/infra.json`): Moli
//! has no business holding Docker's socket. The house's other computers:
//! awake or not, woken by Wake-on-LAN, and with the Moli agent ([`agents`])
//! everything they do, and orders back.

pub mod agents;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agents::{Agents, Report};
use moli_core::{Access, Device, DeviceId, Kind, PointSpec, Semantic, Unit, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::Value as Json;
use tokio::sync::mpsc;

const POLL: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The machine's name (default: its host name).
    #[serde(default)]
    pub name: Option<String>,
    /// The network interface to follow (default: the busiest one).
    #[serde(default)]
    pub interface: Option<String>,
    /// Where `/proc` and `/sys` are (tests read fixtures).
    #[serde(default)]
    pub root: Option<PathBuf>,
    /// The house's other computers: seen awake on the network, woken from it.
    #[serde(default)]
    pub machines: Vec<Machine>,
}

/// A computer of the house (a PC, a console).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Machine {
    pub id: String,
    pub name: String,
    /// Its address on the network.
    pub host: String,
    /// For Wake-on-LAN (none: it cannot be switched on from here).
    #[serde(default)]
    pub mac: Option<String>,
    /// A port it answers on while awake (Windows: 135).
    #[serde(default = "default_probe")]
    pub port: u16,
    /// The SHA-256 of its Moli agent's token (the token stays on the PC).
    #[serde(default)]
    pub agent_sha256: Option<String>,
}

fn default_probe() -> u16 {
    135
}

#[derive(Debug)]
pub struct Host {
    config: Config,
    agents: Agents,
    reports: tokio::sync::Mutex<mpsc::Receiver<Report>>,
}

impl Host {
    #[must_use]
    pub fn new(config: Config) -> Self {
        open(config).1
    }
}

/// The driver and what the API shares with it (the agents' reports and
/// orders).
#[must_use]
pub fn open(config: Config) -> (Agents, Host) {
    let (agents, reports) = Agents::new(
        config
            .machines
            .iter()
            .filter_map(|m| Some((m.id.clone(), m.agent_sha256.clone()?))),
    );
    (
        agents.clone(),
        Host {
            config,
            agents,
            reports: tokio::sync::Mutex::new(reports),
        },
    )
}

impl Driver for Host {
    fn kind(&self) -> &'static str {
        "host"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let mut reports = self.reports.lock().await;
            run(&self.config, &self.agents, &mut reports, ctx).await
        })
    }
}

// ---- reading the kernel's files (pure, tested) ----------------------------------------------

/// `/proc/stat`'s first line: (busy, total) jiffies.
fn cpu_times(stat: &str) -> Option<(u64, u64)> {
    let line = stat.lines().find(|l| l.starts_with("cpu "))?;
    let n: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse().ok())
        .collect();
    if n.len() < 4 {
        return None;
    }
    let total: u64 = n.iter().take(8).sum();
    // Idle and waiting on disks.
    let idle = n[3] + n.get(4).copied().unwrap_or(0);
    Some((total.saturating_sub(idle), total))
}

/// `/proc/meminfo`: (total, available, swap total, swap free), in kB.
fn memory(meminfo: &str) -> Option<(u64, u64, u64, u64)> {
    let field = |name: &str| {
        meminfo
            .lines()
            .find(|l| l.starts_with(name) && l[name.len()..].starts_with(':'))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
    };
    Some((
        field("MemTotal")?,
        field("MemAvailable")?,
        field("SwapTotal").unwrap_or(0),
        field("SwapFree").unwrap_or(0),
    ))
}

/// `/proc/net/dev`: bytes (received, sent) of `interface`, or of the busiest
/// real interface (not the loopback, not Docker's bridges).
fn network(dev: &str, interface: Option<&str>) -> Option<(String, u64, u64)> {
    dev.lines()
        .skip(2)
        .filter_map(|l| {
            let (name, rest) = l.split_once(':')?;
            let name = name.trim();
            let n: Vec<u64> = rest
                .split_whitespace()
                .filter_map(|v| v.parse().ok())
                .collect();
            Some((name.to_owned(), *n.first()?, *n.get(8)?))
        })
        .filter(|(name, _, _)| match interface {
            Some(wanted) => name == wanted,
            None => {
                name != "lo"
                    && !name.starts_with("docker")
                    && !name.starts_with("br-")
                    && !name.starts_with("veth")
            }
        })
        .max_by_key(|(_, rx, tx)| rx + tx)
}

fn percent(part: f64, whole: f64) -> Option<f64> {
    (whole > 0.0).then(|| (part / whole * 1000.0).round() / 10.0)
}

/// The temperatures under `/sys/class/hwmon`: (processor, disk), in °C. The
/// processor's package (`coretemp`, `k10temp`, a Pi's `cpu_thermal`), the
/// NVMe's composite.
fn temperatures(sys: &Path) -> (Option<f64>, Option<f64>) {
    let mut cpu = None;
    let mut disk = None;
    let Ok(dirs) = std::fs::read_dir(sys.join("class/hwmon")) else {
        return (None, None);
    };
    for dir in dirs.flatten() {
        let dir = dir.path();
        let name = std::fs::read_to_string(dir.join("name")).unwrap_or_default();
        let read = |n: u32| {
            std::fs::read_to_string(dir.join(format!("temp{n}_input")))
                .ok()
                .and_then(|v| v.trim().parse::<f64>().ok())
                .map(|milli| (milli / 100.0).round() / 10.0)
        };
        let label = |n: u32| {
            std::fs::read_to_string(dir.join(format!("temp{n}_label"))).unwrap_or_default()
        };
        match name.trim() {
            "coretemp" | "k10temp" | "zenpower" | "cpu_thermal" => {
                // The package (or Tctl) when labelled, else the first.
                let n = (1..=16)
                    .find(|&n| {
                        let l = label(n);
                        l.starts_with("Package") || l.starts_with("Tctl") || l.starts_with("Tdie")
                    })
                    .unwrap_or(1);
                cpu = cpu.or_else(|| read(n));
            }
            "nvme" | "drivetemp" => disk = disk.or_else(|| read(1)),
            _ => {}
        }
    }
    (cpu, disk)
}

// ---- the device ---------------------------------------------------------------------------------

fn spec(key: &str, label: &str, unit: Option<Unit>, max: Option<f64>) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        // Not a room's temperature, not a power to count: the machine's own.
        semantic: Semantic::Other,
        kind: Kind::Numeric {
            min: Some(0.0),
            max,
            step: None,
        },
        access: Access {
            read: true,
            write: false,
        },
        unit,
    }
}

fn device(ctx: &DriverCtx, id: &DeviceId, name: &str) -> Device {
    let mbps = Unit::parse("Mbit/s");
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: name.into(),
        manufacturer: None,
        // An identifier the dashboard recognizes the server by: never translated.
        model: Some("Serveur de la maison".into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            spec(
                "cpu",
                &moli_i18n::tr!("pilotes.host.processeur"),
                Some(Unit::Percent),
                Some(100.0),
            ),
            spec("load", &moli_i18n::tr!("pilotes.host.charge"), None, None),
            spec(
                "ram",
                &moli_i18n::tr!("pilotes.host.memoire"),
                Some(Unit::Percent),
                Some(100.0),
            ),
            spec(
                "ram_used",
                &moli_i18n::tr!("pilotes.host.memoire_utilisee"),
                Unit::parse("Go"),
                None,
            ),
            spec(
                "ram_total",
                &moli_i18n::tr!("pilotes.host.memoire_totale"),
                Unit::parse("Go"),
                None,
            ),
            spec(
                "swap",
                &moli_i18n::tr!("pilotes.host.memoire_echange"),
                Some(Unit::Percent),
                Some(100.0),
            ),
            spec(
                "cpu_temperature",
                &moli_i18n::tr!("pilotes.host.temperature_processeur"),
                Some(Unit::Celsius),
                None,
            ),
            spec(
                "disk_temperature",
                &moli_i18n::tr!("pilotes.host.temperature_disque"),
                Some(Unit::Celsius),
                None,
            ),
            spec(
                "net_down",
                &moli_i18n::tr!("pilotes.host.reseau_reception"),
                mbps.clone(),
                None,
            ),
            spec(
                "net_up",
                &moli_i18n::tr!("pilotes.host.reseau_emission"),
                mbps,
                None,
            ),
            spec(
                "uptime",
                &moli_i18n::tr!("pilotes.host.allume_depuis"),
                Some(Unit::Second),
                None,
            ),
            spec("cores", &moli_i18n::tr!("pilotes.host.coeurs"), None, None),
        ],
    }
}

/// How a machine's figure is shown.
#[derive(Clone, Copy)]
enum Shape {
    Percent,
    Number,
    Flag,
}

/// What the agent reports, as points: key, label (its key in the catalogue),
/// shape, unit.
const AGENT_POINTS: &[(&str, &str, Shape, &str)] = &[
    ("agent", "pilotes.host.agent_connecte", Shape::Flag, ""),
    ("cpu", "pilotes.host.processeur", Shape::Percent, "%"),
    (
        "cpu_temperature",
        "pilotes.host.temperature_processeur",
        Shape::Number,
        "°C",
    ),
    ("ram", "pilotes.host.memoire", Shape::Percent, "%"),
    (
        "ram_used",
        "pilotes.host.memoire_utilisee",
        Shape::Number,
        "Go",
    ),
    (
        "ram_total",
        "pilotes.host.memoire_totale",
        Shape::Number,
        "Go",
    ),
    (
        "gpu_load",
        "pilotes.host.carte_graphique",
        Shape::Percent,
        "%",
    ),
    (
        "gpu_temperature",
        "pilotes.host.temperature_carte_graphique",
        Shape::Number,
        "°C",
    ),
    (
        "gpu_memory",
        "pilotes.host.memoire_graphique_utilisee",
        Shape::Number,
        "Go",
    ),
    (
        "gpu_memory_total",
        "pilotes.host.memoire_graphique",
        Shape::Number,
        "Go",
    ),
    (
        "gpu_power",
        "pilotes.host.consommation_carte_graphique",
        Shape::Number,
        "W",
    ),
    ("disk", "pilotes.host.disque_systeme", Shape::Percent, "%"),
    (
        "net_down",
        "pilotes.host.reseau_reception",
        Shape::Number,
        "Mbit/s",
    ),
    (
        "net_up",
        "pilotes.host.reseau_emission",
        Shape::Number,
        "Mbit/s",
    ),
    ("uptime", "pilotes.host.allume_depuis", Shape::Number, "s"),
    ("claude_app", "pilotes.host.claude_ouvert", Shape::Flag, ""),
    ("codex_app", "pilotes.host.codex_ouvert", Shape::Flag, ""),
];

/// Orders for the agent: an agent (Moli, a script) asking gets a human's
/// approval first (Control). Key, label (its key in the catalogue), shape.
const AGENT_ORDERS: &[(&str, &str, Shape)] = &[
    ("restart", "pilotes.host.redemarrer", Shape::Flag),
    ("sleep", "pilotes.host.mettre_en_veille", Shape::Flag),
];

fn machine_point(
    key: &str,
    label: &str,
    shape: Shape,
    unit: &str,
    write: bool,
    semantic: Semantic,
) -> PointSpec {
    let kind = match shape {
        Shape::Percent => Kind::Numeric {
            min: Some(0.0),
            max: Some(100.0),
            step: None,
        },
        Shape::Number => Kind::Numeric {
            min: Some(0.0),
            max: None,
            step: None,
        },
        Shape::Flag => Kind::Binary,
    };
    PointSpec {
        key: key.into(),
        label: label.into(),
        semantic,
        kind,
        access: Access { read: true, write },
        unit: Unit::parse(unit),
    }
}

fn machine_device(ctx: &DriverCtx, id: &DeviceId, machine: &Machine) -> Device {
    let agent = machine.agent_sha256.is_some();
    // Not a switch of the room; switching off asks the PC's agent, and an
    // agent (Moli, a script) asking gets a human's approval first.
    let mut points = vec![machine_point(
        "power",
        &moli_i18n::tr!("pilotes.host.allume"),
        Shape::Flag,
        "",
        machine.mac.is_some() || agent,
        Semantic::Control,
    )];
    if agent {
        points.extend(AGENT_POINTS.iter().map(|&(key, label, shape, unit)| {
            machine_point(
                key,
                &moli_i18n::tr(label),
                shape,
                unit,
                false,
                Semantic::Other,
            )
        }));
        points.extend(AGENT_ORDERS.iter().map(|&(key, label, shape)| {
            machine_point(
                key,
                &moli_i18n::tr(label),
                shape,
                "",
                true,
                Semantic::Control,
            )
        }));
    }
    Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: machine.name.as_str().into(),
        manufacturer: None,
        // An identifier the dashboard recognizes the computers by: never translated.
        model: Some("Ordinateur".into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

/// The address the network last saw `mac` at (`/proc/net/arp`).
fn arp_address(arp: &str, mac: &str) -> Option<String> {
    let wanted = mac.replace('-', ":").to_lowercase();
    arp.lines()
        .skip(1)
        .map(str::split_whitespace)
        .filter_map(|mut f| Some((f.next()?, f.nth(2)?)))
        .find(|(_, m)| m.to_lowercase() == wanted)
        .map(|(ip, _)| ip.to_owned())
}

async fn answers(host: &str, port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_millis(1500),
            tokio::net::TcpStream::connect((host, port)),
        )
        .await,
        Ok(Ok(_))
    )
}

/// Whether the machine answers on its port: at its configured address, or
/// where the network last saw its MAC (the box may have given it another).
async fn awake(machine: &Machine, arp: &str) -> bool {
    if answers(&machine.host, machine.port).await {
        return true;
    }
    match machine.mac.as_deref().and_then(|mac| arp_address(arp, mac)) {
        Some(ip) if ip != machine.host => answers(&ip, machine.port).await,
        _ => false,
    }
}

/// Wake-on-LAN.
async fn wake(machine: &Machine) -> Result<(), String> {
    let mac = machine
        .mac
        .as_deref()
        .ok_or_else(|| moli_i18n::tr!("pilotes.host.sans_mac", name = machine.name))?;
    moli_net::wol::wake(mac, Some(&machine.host))
        .await
        .map_err(|e| moli_i18n::tr!("pilotes.host.reveil", error = format!("{e:#}")))?;
    tracing::info!(machine = %machine.name, "wake-on-lan sent");
    Ok(())
}

/// An order to a machine: on is Wake-on-LAN; off, restart and sleep go to
/// its agent (at its next report). The remote mode has its own route (PIN).
async fn command_machine(
    machine: &Machine,
    agents: &Agents,
    key: &str,
    value: &Value,
) -> Result<(), String> {
    let ask = |order: Json| -> Result<(), String> {
        if !agents.connected(&machine.id) {
            return Err(moli_i18n::tr!(
                "pilotes.host.agent_muet",
                name = machine.name
            ));
        }
        agents
            .order(&machine.id, order)
            .map(|_| ())
            .map_err(|e| format!("{e:#}"))
    };
    match (key, value) {
        ("power", Value::Bool(true)) => wake(machine).await,
        ("power", Value::Bool(false)) if agents.has(&machine.id) => {
            ask(serde_json::json!({ "kind": "shutdown" }))
        }
        ("power", Value::Bool(false)) => Err(moli_i18n::tr!(
            "pilotes.host.eteindre_demande_agent",
            name = machine.name
        )),
        ("restart", Value::Bool(true)) => ask(serde_json::json!({ "kind": "restart" })),
        ("sleep", Value::Bool(true)) => ask(serde_json::json!({ "kind": "sleep" })),
        // Buttons: releasing them does nothing.
        ("restart" | "sleep", Value::Bool(false)) => Ok(()),
        _ => Err(moli_i18n::tr!("pilotes.host.lecture_seule")),
    }
}

/// What the agent says, as points (the details stay in its last report,
/// served by the API: points keep a history, a process list need not).
fn apply_report(ctx: &DriverCtx, id: &DeviceId, body: &Json) {
    let num = |v: &Json| v.as_f64().map(|n| Value::Float((n * 10.0).round() / 10.0));
    let whole = |v: &Json| v.as_f64().map(|n| Value::Float(n.round()));
    ctx.set_state(id, "agent", Value::Bool(true));
    ctx.set_state(id, "power", Value::Bool(true));
    for (key, value) in [
        ("cpu", whole(&body["cpu"])),
        ("cpu_temperature", whole(&body["cpu_temperature"])),
        ("ram", whole(&body["ram"])),
        ("ram_used", num(&body["ram_used"])),
        ("ram_total", num(&body["ram_total"])),
        ("gpu_load", whole(&body["gpu"]["load"])),
        ("gpu_temperature", whole(&body["gpu"]["temperature"])),
        ("gpu_memory", num(&body["gpu"]["memory_used"])),
        ("gpu_memory_total", num(&body["gpu"]["memory_total"])),
        ("gpu_power", whole(&body["gpu"]["power"])),
        ("net_down", num(&body["net_down"])),
        ("net_up", num(&body["net_up"])),
    ] {
        if let Some(value) = value {
            ctx.set_state(id, key, value);
        }
    }
    if let Some(up) = body["uptime"].as_f64() {
        // To the minute: the history keeps changes only.
        #[allow(clippy::cast_possible_truncation)]
        let minutes = (up / 60.0).floor() as i64;
        ctx.set_state(id, "uptime", Value::Int(minutes * 60));
    }
    // The system disk (C: on Windows, else the first).
    let disks = body["disks"].as_array();
    if let Some(d) = disks.and_then(|d| {
        d.iter()
            .find(|d| d["name"].as_str() == Some("C:"))
            .or_else(|| d.first())
    }) && let (Some(used), Some(total)) = (d["used"].as_f64(), d["total"].as_f64())
        && total > 0.0
    {
        ctx.set_state(id, "disk", Value::Float((used / total * 100.0).round()));
    }
    // The apps the phone reaches the PC through.
    for (key, app) in [("claude_app", "claude"), ("codex_app", "codex")] {
        if let Some(open) = body["apps"][app].as_bool() {
            ctx.set_state(id, key, Value::Bool(open));
        }
    }
}

// ---- the run loop -------------------------------------------------------------------------------

fn kb_to_gb(kb: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let gb = kb as f64 / 1_048_576.0;
    (gb * 10.0).round() / 10.0
}

/// Reads the machine's figures, keeping what a rate needs between reads.
struct Sampler {
    proc_: PathBuf,
    sys: PathBuf,
    interface: Option<String>,
    last_cpu: Option<(u64, u64)>,
    last_net: Option<(Instant, u64, u64)>,
}

impl Sampler {
    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.proc_.join(file)).unwrap_or_default()
    }

    #[allow(clippy::cast_precision_loss)]
    fn sample(&mut self, ctx: &DriverCtx, id: &DeviceId) {
        if let Some(now) = cpu_times(&self.read("stat")) {
            if let Some(before) = self.last_cpu
                && let Some(p) = percent(
                    now.0.saturating_sub(before.0) as f64,
                    now.1.saturating_sub(before.1) as f64,
                )
            {
                ctx.set_state(id, "cpu", Value::Float(p.round()));
            }
            self.last_cpu = Some(now);
        }
        if let Some(load) = self
            .read("loadavg")
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<f64>().ok())
        {
            ctx.set_state(id, "load", Value::Float(load));
        }
        if let Some((total, available, swap_total, swap_free)) = memory(&self.read("meminfo")) {
            let used = total.saturating_sub(available);
            if let Some(p) = percent(used as f64, total as f64) {
                ctx.set_state(id, "ram", Value::Float(p.round()));
            }
            ctx.set_state(id, "ram_used", Value::Float(kb_to_gb(used)));
            ctx.set_state(id, "ram_total", Value::Float(kb_to_gb(total)));
            let swap = percent(
                swap_total.saturating_sub(swap_free) as f64,
                swap_total as f64,
            )
            .unwrap_or(0.0);
            ctx.set_state(id, "swap", Value::Float(swap.round()));
        }
        let (cpu_t, disk_t) = temperatures(&self.sys);
        if let Some(t) = cpu_t {
            ctx.set_state(id, "cpu_temperature", Value::Float(t.round()));
        }
        if let Some(t) = disk_t {
            ctx.set_state(id, "disk_temperature", Value::Float(t.round()));
        }
        if let Some((_, rx, tx)) = network(&self.read("net/dev"), self.interface.as_deref()) {
            let now = Instant::now();
            if let Some((at, rx0, tx0)) = self.last_net {
                let secs = now.duration_since(at).as_secs_f64();
                if secs > 0.0 {
                    let mbps = |a: u64, b: u64| {
                        ((a.saturating_sub(b) as f64 * 8.0 / secs / 1e6) * 10.0).round() / 10.0
                    };
                    ctx.set_state(id, "net_down", Value::Float(mbps(rx, rx0)));
                    ctx.set_state(id, "net_up", Value::Float(mbps(tx, tx0)));
                }
            }
            self.last_net = Some((now, rx, tx));
        }
        if let Some(up) = self
            .read("uptime")
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<f64>().ok())
        {
            // To the minute: the history keeps changes only.
            #[allow(clippy::cast_possible_truncation)]
            let minutes = (up / 60.0).floor() as i64;
            ctx.set_state(id, "uptime", Value::Int(minutes * 60));
        }
    }
}

/// The server's device and the machines' devices, published.
fn publish(config: &Config, sampler: &Sampler, ctx: &DriverCtx) -> DeviceId {
    let hostname = sampler.read("sys/kernel/hostname").trim().to_owned();
    let name = config.name.clone().unwrap_or_else(|| {
        if hostname.is_empty() {
            moli_i18n::tr!("pilotes.host.serveur")
        } else {
            hostname.clone()
        }
    });
    let id = ctx.device_id(if hostname.is_empty() {
        "local"
    } else {
        &hostname
    });
    ctx.upsert_device(device(ctx, &id, &name));
    ctx.set_availability(&id, true);
    let cores = sampler
        .read("cpuinfo")
        .lines()
        .filter(|l| l.starts_with("processor"))
        .count();
    if cores > 0 {
        ctx.set_state(&id, "cores", Value::Int(i64::try_from(cores).unwrap_or(0)));
    }
    for machine in &config.machines {
        let mid = ctx.device_id(&machine.id);
        ctx.upsert_device(machine_device(ctx, &mid, machine));
        // Always reachable from here: asleep is « power off », not « broken »
        // (an offline device refuses the order that wakes it).
        ctx.set_availability(&mid, true);
    }
    id
}

async fn run(
    config: &Config,
    agents: &Agents,
    reports: &mut mpsc::Receiver<Report>,
    ctx: &mut DriverCtx,
) -> anyhow::Result<()> {
    let root = config.root.clone().unwrap_or_else(|| PathBuf::from("/"));
    let mut sampler = Sampler {
        proc_: root.join("proc"),
        sys: root.join("sys"),
        interface: config.interface.clone(),
        last_cpu: None,
        last_net: None,
    };
    let id = publish(config, &sampler, ctx);
    let machines: Vec<(DeviceId, &Machine)> = config
        .machines
        .iter()
        .map(|m| (ctx.device_id(&m.id), m))
        .collect();
    ctx.ready();

    sampler.last_cpu = cpu_times(&sampler.read("stat"));
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                let machine = machines.iter().find(|(mid, _)| *mid == command.device.id).map(|(_, m)| *m);
                let result = match machine {
                    Some(machine) => command_machine(machine, agents, &command.key, &command.value).await,
                    None => Err(moli_i18n::tr!("pilotes.host.lecture_seule")),
                };
                command.reply(result);
            }
            Some(report) = reports.recv() => {
                if let Some((mid, _)) = machines.iter().find(|(_, m)| m.id == report.machine) {
                    apply_report(ctx, mid, &report.body);
                }
            }
            _ = tick.tick() => {
                let arp = sampler.read("net/arp");
                for (mid, machine) in &machines {
                    // The agent speaking is proof enough; else its port.
                    let connected = agents.connected(&machine.id);
                    if agents.has(&machine.id) && !connected {
                        ctx.set_state(mid, "agent", Value::Bool(false));
                    }
                    let on = connected || awake(machine, &arp).await;
                    ctx.set_state(mid, "power", Value::Bool(on));
                }
                sampler.sample(ctx, &id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processor_time_busy_and_total() {
        let stat = "cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 1 2 3 4\n";
        assert_eq!(cpu_times(stat), Some((150, 1000)));
        assert_eq!(cpu_times("nonsense"), None);
    }

    #[test]
    fn memory_and_swap() {
        let meminfo = "MemTotal:       16149128 kB\nMemFree:  100 kB\nMemAvailable:    8074564 kB\nSwapTotal: 2000 kB\nSwapFree: 1500 kB\n";
        assert_eq!(memory(meminfo), Some((16_149_128, 8_074_564, 2000, 1500)));
        assert_eq!(kb_to_gb(16_149_128), 15.4);
    }

    #[test]
    fn a_machine_found_by_its_mac() {
        let arp = "IP address       HW type     Flags       HW address            Mask     Device\n192.168.0.29     0x1         0x2         02:00:00:00:00:01     *        enp2s0\n192.168.0.34     0x1         0x2         02:00:00:00:00:02     *        enp2s0\n";
        assert_eq!(
            arp_address(arp, "02-00-00-00-00-01").as_deref(),
            Some("192.168.0.29")
        );
        assert_eq!(arp_address(arp, "aa:bb:cc:dd:ee:ff"), None);
    }

    #[test]
    fn the_busiest_real_interface() {
        let dev = "Inter-|   Receive\n face |bytes\n    lo: 900 1 0 0 0 0 0 0 900 1 0 0 0 0 0 0\nenp2s0: 500 1 0 0 0 0 0 0 300 1 0 0 0 0 0 0\ndocker0: 9000 1 0 0 0 0 0 0 9000 1 0 0 0 0 0 0\n";
        assert_eq!(network(dev, None), Some(("enp2s0".into(), 500, 300)));
        assert_eq!(network(dev, Some("lo")).map(|n| n.1), Some(900));
    }

    #[test]
    fn temperatures_from_hwmon() {
        let dir = std::env::temp_dir().join(format!("moli-host-test-{}", std::process::id()));
        let core = dir.join("class/hwmon/hwmon1");
        let nvme = dir.join("class/hwmon/hwmon0");
        std::fs::create_dir_all(&core).unwrap();
        std::fs::create_dir_all(&nvme).unwrap();
        std::fs::write(core.join("name"), "coretemp\n").unwrap();
        std::fs::write(core.join("temp1_label"), "Core 0\n").unwrap();
        std::fs::write(core.join("temp1_input"), "41000\n").unwrap();
        std::fs::write(core.join("temp2_label"), "Package id 0\n").unwrap();
        std::fs::write(core.join("temp2_input"), "47500\n").unwrap();
        std::fs::write(nvme.join("name"), "nvme\n").unwrap();
        std::fs::write(nvme.join("temp1_input"), "38850\n").unwrap();
        assert_eq!(temperatures(&dir), (Some(47.5), Some(38.9)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
