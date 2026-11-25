//! Proxmox VE: the node, its containers and virtual machines, storages,
//! backups and disks, read with an API token, never changed.
//!
//! The node device is published at once and stays: a node that does not
//! answer is a value (`up` = false, a problem), never a driver failure.
mod api;
mod model;

use anyhow::{Context as _, ensure};
use api::Client;
use model::{Limits, States};
use moli_core::{DeviceId, Value};
use moli_net::{PIN_MISMATCH, PinnedHttps};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::Value as Json;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SECRET: &str = "token_secret";
const CERT: &str = "cert";
/// Disks are read this often: their health and wear move slowly, and the
/// SMART read is not free.
const DISKS_EVERY: Duration = Duration::from_secs(600);
/// Pending updates are read this often (the list is Proxmox's own cache).
const UPDATES_EVERY: Duration = Duration::from_secs(3600);
/// The most stopped guests whose configuration is read in one poll (one
/// request each): beyond, their `onboot` is not known and not a problem.
const MAX_CONFIGS: usize = 32;
/// After a refused token, Proxmox is not asked again before this long.
const RETRY_AFTER_REFUSAL: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 900 });

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    /// Proxmox's HTTPS port (8006 unless changed).
    #[serde(default = "default_port")]
    pub port: u16,
    /// The node to read (by default the only one).
    #[serde(default)]
    pub node: Option<String>,
    /// The API token's id, `user@realm!name` (its secret is in the vault).
    pub token_id: String,
    #[serde(default = "default_poll")]
    pub poll_secs: u64,
    /// Memory, disk or storage use above this, in percent: a problem.
    #[serde(default = "default_full")]
    pub full_percent: f64,
    /// A virtual machine's memory above this, in percent: a problem. None by
    /// default: Proxmox counts the guest's disk cache as used, so a Linux VM
    /// always looks nearly full.
    #[serde(default)]
    pub vm_full_percent: Option<f64>,
    /// No successful backup for this long, in hours: a problem.
    #[serde(default = "default_backup_hours")]
    pub backup_hours: f64,
}

fn default_port() -> u16 {
    8006
}

fn default_poll() -> u64 {
    60
}

fn default_full() -> f64 {
    90.0
}

fn default_backup_hours() -> f64 {
    26.0
}

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

fn valid_node(node: &str) -> bool {
    !node.is_empty()
        && node.len() <= 63
        && node.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[derive(Debug)]
pub struct Proxmox {
    config: Config,
}

impl Proxmox {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        let invalid = || moli_i18n::tr!("pilotes.proxmox.config_invalide");
        ensure!(valid_host(&config.host), invalid());
        ensure!(api::valid_token_id(&config.token_id), invalid());
        ensure!(config.node.as_deref().is_none_or(valid_node), invalid());
        ensure!(
            config.port > 0
                && (5..=3600).contains(&config.poll_secs)
                && (1.0..=100.0).contains(&config.full_percent)
                && config
                    .vm_full_percent
                    .is_none_or(|p| (1.0..=100.0).contains(&p))
                && (1.0..=24.0 * 31.0).contains(&config.backup_hours),
            invalid()
        );
        Ok(Self { config })
    }

    fn limits(&self) -> Limits {
        Limits {
            full: self.config.full_percent,
            vm_full: self.config.vm_full_percent,
            backup_hours: self.config.backup_hours,
        }
    }
}

impl Driver for Proxmox {
    fn kind(&self) -> &'static str {
        "proxmox"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.run_inner(ctx))
    }
}

/// What a run keeps between two polls.
struct State {
    secret: String,
    client: Option<Client>,
    /// The node read: configured, or the only one Proxmox lists.
    node: Option<String>,
    version: Option<String>,
    /// Whether the last poll reached Proxmox (`None`: not tried yet).
    up: Option<bool>,
    disks: Option<(Instant, Json)>,
    updates: Option<(Instant, usize)>,
    /// When Proxmox last refused the token (the cool-down runs from then).
    refused: Option<Instant>,
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

impl Proxmox {
    async fn run_inner(&self, ctx: &mut DriverCtx) -> anyhow::Result<()> {
        let Some(secret) = ctx.secret(SECRET) else {
            ctx.wait_for(moli_i18n::tr!(
                "pilotes.proxmox.jeton_manquant",
                instance = ctx.instance(),
                key = SECRET
            ));
            ctx.cancelled().await;
            return Ok(());
        };
        if ctx.secret(CERT).is_none() && !ctx.can_store_secrets() {
            ctx.wait_for(moli_i18n::tr!("pilotes.proxmox.coffre_indisponible"));
            ctx.cancelled().await;
            return Ok(());
        }
        let mut state = State {
            secret,
            client: None,
            node: self.config.node.clone(),
            version: None,
            up: None,
            disks: None,
            updates: None,
            refused: None,
        };
        publish_node(ctx, &state, model::node_states_down());
        ctx.ready();
        let mut poll = tokio::time::interval(Duration::from_secs(self.config.poll_secs));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                command = ctx.next_command() => {
                    let Some(command) = command else { return Ok(()) };
                    command.reply(Err(moli_i18n::tr!("pilotes.proxmox.lecture_seule")));
                }
                _ = poll.tick() => {
                    if state.refused.is_some_and(|at| at.elapsed() < RETRY_AFTER_REFUSAL) {
                        continue;
                    }
                    let Err(error) = self.poll(ctx, &mut state).await else {
                        if state.up != Some(true) {
                            tracing::info!(instance = %ctx.instance(), "proxmox reachable");
                        }
                        state.up = Some(true);
                        if state.refused.take().is_some() {
                            ctx.clear_warning();
                        }
                        continue;
                    };
                    unreachable(ctx, &state);
                    if error.downcast_ref::<api::Refused>().is_some() {
                        state.refused = Some(Instant::now());
                        state.up = Some(false);
                        tracing::warn!(instance = %ctx.instance(), error = %format!("{error:#}"), "proxmox refused the token, next try later");
                        ctx.warn(moli_i18n::tr!(
                            "pilotes.proxmox.jeton_refuse",
                            instance = ctx.instance(),
                            key = SECRET
                        ));
                        continue;
                    }
                    if format!("{error:#}").contains(PIN_MISMATCH) {
                        ctx.wait_for(moli_i18n::tr!(
                            "pilotes.proxmox.certificat_change",
                            instance = ctx.instance(),
                            key = CERT
                        ));
                        ctx.cancelled().await;
                        return Ok(());
                    }
                    if state.up != Some(false) {
                        tracing::warn!(instance = %ctx.instance(), error = %format!("{error:#}"), "proxmox unreachable");
                    }
                    state.refused = None;
                    ctx.clear_warning();
                    state.up = Some(false);
                }
            }
        }
    }

    /// The client, made on first need: the certificate is pinned before the
    /// token leaves.
    async fn client<'a>(
        &self,
        ctx: &DriverCtx,
        state: &'a mut State,
    ) -> anyhow::Result<&'a Client> {
        if state.client.is_none() {
            let pin = if let Some(pin) = ctx.secret(CERT) {
                pin
            } else {
                self.first_pin(ctx).await?
            };
            let http = PinnedHttps::new(&self.config.host, self.config.port, Some(pin))?;
            state.client = Some(Client::new(http, &self.config.token_id, &state.secret));
        }
        state.client.as_ref().context("no client")
    }

    async fn first_pin(&self, ctx: &DriverCtx) -> anyhow::Result<String> {
        ensure!(
            ctx.can_store_secrets(),
            "cannot pin Proxmox's certificate: the secret store is unavailable"
        );
        let probe = PinnedHttps::new(&self.config.host, self.config.port, None)?;
        probe.handshake().await.context("first contact")?;
        let seen = probe.seen_fingerprint().context("no certificate seen")?;
        ctx.store_secret(CERT, &seen).await?;
        tracing::info!(instance = %ctx.instance(), "proxmox certificate pinned");
        Ok(seen)
    }

    async fn poll(&self, ctx: &DriverCtx, state: &mut State) -> anyhow::Result<()> {
        let limits = self.limits();
        let disks_due = state
            .disks
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() >= DISKS_EVERY);
        let updates_due = state
            .updates
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() >= UPDATES_EVERY);
        let known_node = state.node.clone();
        let client = self.client(ctx, state).await?;
        let node = match known_node {
            Some(node) => node,
            None => only_node(&client.get("/nodes").await.context("nodes")?)
                .context(moli_i18n::tr!("pilotes.proxmox.noeud_ambigu"))?,
        };
        let status = client
            .get(&format!("/nodes/{node}/status"))
            .await
            .context("node status")?;
        // The rest is best effort: a part that cannot be read is left out.
        let lxc = optional(client, &format!("/nodes/{node}/lxc")).await?;
        let qemu = optional(client, &format!("/nodes/{node}/qemu")).await?;
        let storage = optional(client, &format!("/nodes/{node}/storage")).await?;
        let tasks = optional(
            client,
            // `source=all`: the running one too (by default, only those ended).
            &format!("/nodes/{node}/tasks?typefilter=vzdump&source=all&limit=20"),
        )
        .await?;
        let disks_read = if disks_due {
            Some(optional(client, &format!("/nodes/{node}/disks/list")).await?)
        } else {
            None
        };
        let updates_read = if updates_due {
            Some(optional(client, &format!("/nodes/{node}/apt/update")).await?)
        } else {
            None
        };
        // A stopped guest: its configuration says whether it should run.
        let mut boots = Vec::new();
        let stopped = [("lxc", &lxc), ("qemu", &qemu)]
            .into_iter()
            .flat_map(|(kind, list)| {
                model::stopped(list)
                    .into_iter()
                    .map(move |vmid| (kind, vmid))
            });
        for (kind, vmid) in stopped.take(MAX_CONFIGS) {
            let config = optional(client, &format!("/nodes/{node}/{kind}/{vmid}/config")).await?;
            if !config.is_null() {
                boots.push((kind, vmid, model::starts_at_boot(&config)));
            }
        }

        match disks_read {
            Some(Json::Null) => state.disks = None,
            Some(read) => state.disks = Some((Instant::now(), read)),
            None => {}
        }
        match updates_read {
            Some(Json::Array(list)) => state.updates = Some((Instant::now(), list.len())),
            Some(_) => state.updates = None,
            None => {}
        }
        let version = model::version(&status).map(str::to_owned);
        if state.node.as_deref() != Some(&node) || version != state.version {
            state.node = Some(node);
            state.version = version;
        }
        publish_node(
            ctx,
            state,
            model::node_states(&status, state.updates.as_ref().map(|(_, n)| *n), limits),
        );

        let mut entities = Vec::new();
        let instance = ctx.instance();
        for (kind, list) in [("lxc", &lxc), ("qemu", &qemu)] {
            for guest in model::entries(list) {
                let vmid = guest["vmid"].as_u64();
                let boot = boots
                    .iter()
                    .find(|(k, v, _)| *k == kind && Some(*v) == vmid)
                    .map(|(_, _, b)| *b);
                entities.extend(model::guest(instance, kind, guest, boot, limits));
            }
        }
        entities.extend(model::storages(instance, &storage, limits));
        entities.push(model::backup(instance, &tasks, now_secs(), limits));
        if let Some((_, disks)) = &state.disks {
            entities.extend(model::disks(instance, disks));
        }
        publish_all(ctx, entities);
        Ok(())
    }
}

/// Every part read, published; a part gone from Proxmox (a container
/// deleted, a storage removed) goes offline, not removed, so its history
/// stays readable.
fn publish_all(ctx: &DriverCtx, entities: Vec<model::Entity>) {
    let mut present = vec![ctx.device_id(model::NODE)];
    for entity in entities {
        let id = entity.device.id.clone();
        present.push(id.clone());
        ctx.upsert_device(entity.device);
        for (key, value) in entity.states {
            ctx.set_state(&id, key, value);
        }
        ctx.set_availability(&id, true);
    }
    for id in ctx.devices() {
        if !present.contains(&id) {
            forget(ctx, &id);
            ctx.set_availability(&id, false);
        }
    }
}

/// The only node of the cluster, else none (which one to read is then the
/// house's choice: `node` in its configuration).
fn only_node(nodes: &Json) -> Option<String> {
    let list = nodes.as_array()?;
    let [one] = list.as_slice() else {
        return None;
    };
    one["node"]
        .as_str()
        .filter(|n| valid_node(n))
        .map(str::to_owned)
}

/// A part that cannot be read (no right, too slow) reads as empty: only that
/// part is lost. A refused token or a changed certificate still stops the poll.
async fn optional(client: &Client, path: &str) -> anyhow::Result<Json> {
    match client.get(path).await {
        Err(e)
            if e.downcast_ref::<api::Refused>().is_some()
                && !path.ends_with("/apt/update")
                && !path.ends_with("/disks/list") =>
        {
            Err(e)
        }
        Err(e) if format!("{e:#}").contains(PIN_MISMATCH) => Err(e),
        Err(e) => {
            tracing::debug!(path, error = %format!("{e:#}"), "proxmox part unavailable");
            Ok(Json::Null)
        }
        ok => ok,
    }
}

fn publish_node(ctx: &DriverCtx, state: &State, states: States) {
    ctx.upsert_device(model::node_device(
        ctx.instance(),
        state.node.as_deref().unwrap_or("Proxmox"),
        state.version.as_deref(),
    ));
    let id = ctx.device_id(model::NODE);
    for (key, value) in states {
        ctx.set_state(&id, key, value);
    }
    ctx.set_availability(&id, true);
}

/// Nothing is known any more: no stale « all fine ».
fn unreachable(ctx: &DriverCtx, state: &State) {
    let node = ctx.device_id(model::NODE);
    for id in ctx.devices() {
        if id != node {
            forget(ctx, &id);
            ctx.set_availability(&id, false);
        }
    }
    publish_node(ctx, state, model::node_states_down());
}

/// A part that is no longer read keeps no verdict.
fn forget(ctx: &DriverCtx, id: &DeviceId) {
    ctx.set_state(id, "problem", Value::Null);
    ctx.set_state(id, "detail", Value::Null);
}

#[cfg(test)]
mod tests;
