//! On-demand, bounded LAN discovery. No driver, registry write, background
//! timer, broadcast payload or equipment command is created by a search.

use std::collections::{BTreeSet, HashSet};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use moli_api::airzone_setup::{DiscoveredGateway, GatewayDiscovery, GatewayStatus, SetupError};
use moli_runtime::BoxFuture;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

const MAX_NETWORKS: usize = 2;
const MAX_TARGETS: usize = 512;
const CONNECT_CONCURRENCY: usize = 16;
const PROBE_CONCURRENCY: usize = 4;
const MAX_PROBES: usize = 32;
const CONNECT_LIMIT: Duration = Duration::from_millis(250);
const SEARCH_LIMIT: Duration = Duration::from_secs(18);
const COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
pub(super) struct Gate(Mutex<GateState>);

#[derive(Debug, Default)]
struct GateState {
    started: Option<Instant>,
    running: bool,
}

pub(super) struct Guard<'a>(&'a Gate);

impl Gate {
    pub(super) fn enter(&self) -> Result<Guard<'_>, SetupError> {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if state.running || state.started.is_some_and(|at| at.elapsed() < COOLDOWN) {
            return Err(SetupError::new("busy"));
        }
        state.started = Some(Instant::now());
        state.running = true;
        Ok(Guard(self))
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.0
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running = false;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    host: Ipv4Addr,
    port: u16,
    tls_fingerprint: Option<String>,
}

#[derive(Debug)]
struct Plan {
    targets: Vec<Target>,
    incomplete: bool,
}

pub(super) fn private_host(host: &str) -> Option<Ipv4Addr> {
    let ip: Ipv4Addr = host.parse().ok()?;
    (ip.is_private() && !matches!(ip.octets()[3], 0 | 255)).then_some(ip)
}

fn plan_targets(known: &[GatewayStatus], hints: &[Ipv4Addr], neighbours: &[Ipv4Addr]) -> Plan {
    let mut targets = Vec::new();
    let mut endpoints = HashSet::new();
    let mut known_hosts = HashSet::new();
    let mut networks = Vec::new();
    let mut incomplete = known.is_empty() && hints.is_empty();
    // Explicit endpoints win over a default HTTP port. In particular a known
    // pinned HTTPS connection must never be replaced by an unpinned probe.
    for gateway in known {
        if let Some(host) = private_host(&gateway.host) {
            add_network(host, &mut networks, &mut incomplete);
            if !networks.contains(&prefix(host)) {
                continue;
            }
            known_hosts.insert(host);
            if gateway.port != 0 && endpoints.insert((host, gateway.port)) {
                targets.push(Target {
                    host,
                    port: gateway.port,
                    tls_fingerprint: gateway.tls_fingerprint.clone(),
                });
            }
        }
    }
    for &host in hints.iter().chain(neighbours) {
        if private_host(&host.to_string()).is_some() {
            add_network(host, &mut networks, &mut incomplete);
        }
    }
    // Already observed hosts precede the rest of their subnet. Only port 3000
    // is discovered; other ports and unknown TLS pins require manual setup.
    let hosts = hints
        .iter()
        .copied()
        .chain(neighbours.iter().copied())
        .chain(
            networks
                .iter()
                .flat_map(|[a, b, c]| (1..=254).map(move |d| Ipv4Addr::new(*a, *b, *c, d))),
        );
    for host in hosts {
        if private_host(&host.to_string()).is_some()
            && networks.contains(&prefix(host))
            && !known_hosts.contains(&host)
            && endpoints.insert((host, 3000))
        {
            targets.push(Target {
                host,
                port: 3000,
                tls_fingerprint: None,
            });
        }
    }
    incomplete |= targets.len() > MAX_TARGETS || targets.is_empty();
    targets.truncate(MAX_TARGETS);
    Plan {
        targets,
        incomplete,
    }
}

fn prefix(ip: Ipv4Addr) -> [u8; 3] {
    let [a, b, c, _] = ip.octets();
    [a, b, c]
}

fn add_network(host: Ipv4Addr, networks: &mut Vec<[u8; 3]>, incomplete: &mut bool) {
    let network = prefix(host);
    if !networks.contains(&network) {
        if networks.len() < MAX_NETWORKS {
            networks.push(network);
        } else {
            *incomplete = true;
        }
    }
}

pub(super) trait Inspector: Send + Sync {
    fn connect<'a>(&'a self, target: &'a Target) -> BoxFuture<'a, bool>;
    fn probe<'a>(&'a self, target: &'a Target)
    -> BoxFuture<'a, Option<moli_airzone::GatewayProbe>>;
}

pub(super) struct Network;

impl Inspector for Network {
    fn connect<'a>(&'a self, target: &'a Target) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            tokio::time::timeout(
                CONNECT_LIMIT,
                tokio::net::TcpStream::connect((target.host, target.port)),
            )
            .await
            .is_ok_and(|connection| connection.is_ok())
        })
    }

    fn probe<'a>(
        &'a self,
        target: &'a Target,
    ) -> BoxFuture<'a, Option<moli_airzone::GatewayProbe>> {
        Box::pin(async move {
            moli_airzone::probe_read_only(moli_airzone::Config {
                host: target.host.to_string(),
                port: target.port,
                poll_secs: 30,
                mac: None,
                read_only: true,
                write: moli_airzone::WriteMode::Legacy,
                dhw_units: None,
                tls_fingerprint: target.tls_fingerprint.clone(),
            })
            .await
            .ok()
        })
    }
}

enum Checked {
    Absent,
    Skipped,
    Found(moli_airzone::GatewayProbe),
}

async fn inspect(
    target: Target,
    inspector: Arc<dyn Inspector>,
    slots: Arc<Semaphore>,
    probes: Arc<AtomicUsize>,
) -> (Target, Checked) {
    if !inspector.connect(&target).await {
        return (target, Checked::Absent);
    }
    if probes.fetch_add(1, Ordering::Relaxed) >= MAX_PROBES {
        return (target, Checked::Skipped);
    }
    let Ok(_slot) = slots.acquire().await else {
        return (target, Checked::Skipped);
    };
    let report = inspector.probe(&target).await;
    (target, report.map_or(Checked::Absent, Checked::Found))
}

pub(super) async fn search(
    known: Vec<GatewayStatus>,
    hints: Vec<Ipv4Addr>,
    cancel: CancellationToken,
    inspector: Arc<dyn Inspector>,
) -> Result<GatewayDiscovery, SetupError> {
    let neighbours = moli_net::lan::neighbours()
        .await
        .into_iter()
        .map(|(ip, _)| ip)
        .take(MAX_TARGETS)
        .collect::<Vec<_>>();
    run(
        plan_targets(&known, &hints, &neighbours),
        known,
        cancel,
        inspector,
        SEARCH_LIMIT,
    )
    .await
}

async fn run(
    plan: Plan,
    known: Vec<GatewayStatus>,
    cancel: CancellationToken,
    inspector: Arc<dyn Inspector>,
    deadline: Duration,
) -> Result<GatewayDiscovery, SetupError> {
    if cancel.is_cancelled() {
        return Err(SetupError::new("shutting_down"));
    }
    let mut result = GatewayDiscovery {
        gateways: Vec::new(),
        scanned: 0,
        target_count: plan.targets.len(),
        incomplete: plan.incomplete,
    };
    let mut targets = plan.targets.into_iter().enumerate();
    let mut tasks = JoinSet::new();
    let slots = Arc::new(Semaphore::new(PROBE_CONCURRENCY));
    let probes = Arc::new(AtomicUsize::new(0));
    let mut found = Vec::new();
    let until = tokio::time::sleep(deadline);
    tokio::pin!(until);
    loop {
        while tasks.len() < CONNECT_CONCURRENCY {
            let Some((index, target)) = targets.next() else {
                break;
            };
            let (inspector, slots, probes) = (inspector.clone(), slots.clone(), probes.clone());
            tasks.spawn(async move { (index, inspect(target, inspector, slots, probes).await) });
        }
        if tasks.is_empty() {
            break;
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(SetupError::new("shutting_down")),
            () = &mut until => { result.incomplete = true; break; }
            next = tasks.join_next() => match next {
                Some(Ok((index, (target, checked)))) => {
                    result.scanned += 1;
                    match checked {
                        Checked::Absent => {},
                        Checked::Skipped => result.incomplete = true,
                        Checked::Found(report) => {
                            result.incomplete |= report.incomplete;
                            found.push((index, discovered(target, report, &known)));
                        }
                    }
                }
                _ => result.incomplete = true,
            }
        }
    }
    // Dropping JoinSet aborts every unfinished socket/probe, also when the
    // HTTP future is cancelled. No detached scan survives the request.
    drop(tasks);
    found.sort_by_key(|(index, _)| *index);
    let mut macs = BTreeSet::new();
    result.gateways = found
        .into_iter()
        .map(|(_, gateway)| gateway)
        .filter(|gateway| macs.insert(gateway.mac.clone()))
        .collect();
    result
        .gateways
        .sort_by(|a, b| (&a.host, a.port, &a.mac).cmp(&(&b.host, b.port, &b.mac)));
    Ok(result)
}

fn discovered(
    target: Target,
    report: moli_airzone::GatewayProbe,
    known: &[GatewayStatus],
) -> DiscoveredGateway {
    let existing = known
        .iter()
        .find(|gateway| gateway.mac.as_deref() == Some(&report.mac))
        .or_else(|| {
            known.iter().find(|gateway| {
                gateway.mac.is_none()
                    && gateway.host == target.host.to_string()
                    && gateway.port == target.port
            })
        });
    DiscoveredGateway {
        host: target.host.to_string(),
        port: target.port,
        tls_fingerprint: target.tls_fingerprint,
        mac: report.mac,
        gateway_type: report.gateway_type,
        api_version: report.api_version,
        firmware: report.firmware,
        systems: report.systems,
        zones: report.zones,
        incomplete: report.incomplete,
        read_only: true,
        existing_instance: existing.map(|gateway| gateway.instance.clone()),
        existing_enabled: existing.map(|gateway| gateway.enabled),
    }
}

#[cfg(test)]
mod tests;
