//! Synology NAS (DSM): health, disks, volumes, UPS, Hyper Backup, and the
//! orders that wake, stop or restart it.
//!
//! The NAS device is published at once and stays: a NAS that sleeps is the
//! normal case, and the order that wakes it must exist then. Its failure to
//! answer is a value (`up` = false), never a driver failure: a failing driver
//! would empty the order queue and take the wake order with it.
mod dsm;
mod model;

use anyhow::{Context as _, bail, ensure};
use dsm::Client;
use model::States;
use moli_core::{DeviceId, Value};
use moli_net::{PIN_MISMATCH, PinnedHttps};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx, KnownAddress};
use serde::Deserialize;
use serde_json::Value as Json;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

const USERNAME: &str = "username";
const PASSWORD: &str = "password";
const CERT: &str = "cert";
/// DSM: this account has no right to that API (not an administrator).
const NO_RIGHT: u32 = 105;
/// How often the storage is read. It is the Storage Manager's own call: read
/// every minute, it can keep the disks from going to sleep (or wake them).
/// Disk temperature, health and free space move slowly.
const STORAGE_EVERY: Duration = Duration::from_secs(600);
/// After a refused login, the NAS is not asked again before this long: DSM
/// blocks an address that fails a few times in a few minutes, yet it also
/// refuses logins while it starts, so the refusal cannot be final.
const RETRY_AFTER_REFUSAL: Duration = Duration::from_secs(if cfg!(test) { 2 } else { 900 });
/// The longest pause between two tries (two hours, on the fourth refusal in
/// a row): past that, a NAS that keeps refusing is a fault to look at, not a
/// transient state, and a shorter pause only feeds DSM's block list.
const RETRY_AFTER_REFUSAL_MAX: Duration = Duration::from_secs(if cfg!(test) { 8 } else { 7200 });

/// The pause before asking again, from the number of refusals in a row: the
/// first waits the base (15 min, 2 s in test), then the pause doubles
/// (30 min, 60 min, 2 h) and stops at the ceiling. Saturating, so a counter
/// that kept climbing cannot overflow.
fn retry_after(refusals: u32) -> Duration {
    // Eight doublings already go past any ceiling: no need to shift further.
    let doublings = refusals.saturating_sub(1).min(8);
    RETRY_AFTER_REFUSAL
        .saturating_mul(1 << doublings)
        .min(RETRY_AFTER_REFUSAL_MAX)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: String,
    /// DSM's HTTPS port (5001 unless changed in DSM).
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_poll")]
    pub poll_secs: u64,
    /// The MAC address that wakes the NAS (Wake-on-LAN enabled in DSM). On a
    /// local network, Moli learns it when the NAS first answers.
    #[serde(default)]
    pub mac: Option<String>,
}

fn default_port() -> u16 {
    5001
}

fn default_poll() -> u64 {
    60
}

#[derive(Debug)]
pub struct Synology {
    config: Config,
}

impl Synology {
    pub fn new(mut config: Config) -> anyhow::Result<Self> {
        ensure!(
            !config.host.is_empty()
                && config.host.len() <= 253
                && config.host.split('.').all(|label| !label.is_empty()
                    && label.len() <= 63
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && !label.starts_with('-')
                    && !label.ends_with('-')),
            moli_i18n::tr!("pilotes.synology.config_invalide")
        );
        ensure!(
            config.port > 0 && (5..=3600).contains(&config.poll_secs),
            moli_i18n::tr!("pilotes.synology.config_invalide")
        );
        if let Some(mac) = config.mac {
            config.mac = Some(
                moli_net::lan::mac_hex(&mac)
                    .with_context(|| moli_i18n::tr!("pilotes.synology.config_invalide"))?,
            );
        }
        Ok(Self { config })
    }
}

impl Driver for Synology {
    fn kind(&self) -> &'static str {
        "synology"
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.run_inner(ctx))
    }
}

/// What a run keeps between two polls.
struct State {
    username: String,
    password: String,
    client: Option<Client>,
    /// `aabbccddeeff`: configured, remembered, or learned from the network.
    mac: Option<String>,
    model: Option<String>,
    /// Whether the last poll reached the NAS (`None`: not tried yet).
    up: Option<bool>,
    /// The last storage read and when: read again once `STORAGE_EVERY` has
    /// passed, or at once after the NAS stopped answering.
    storage: Option<(Instant, Json)>,
    /// When the NAS last refused the login (the cool-down runs from then).
    refused: Option<Instant>,
    /// How many refusals in a row: the pause grows with it, and a reading
    /// that goes through puts it back to zero.
    refusals: u32,
}

impl State {
    fn cooling_down(&self) -> bool {
        self.refused
            .is_some_and(|at| at.elapsed() < retry_after(self.refusals))
    }
}

/// `aabbccddeeff` → `aa:bb:cc:dd:ee:ff`.
fn colon(mac: &str) -> String {
    mac.as_bytes()
        .chunks(2)
        .map(|pair| String::from_utf8_lossy(pair).into_owned())
        .collect::<Vec<_>>()
        .join(":")
}

fn describe(error: &anyhow::Error) -> String {
    if let Some(dsm::Api(code)) = error.downcast_ref() {
        if *code == NO_RIGHT {
            moli_i18n::tr!("pilotes.synology.refuse_droit")
        } else {
            moli_i18n::tr!("pilotes.synology.refuse", code = code)
        }
    } else if error.downcast_ref::<dsm::Refused>().is_some() {
        moli_i18n::tr!("pilotes.synology.connexion_refusee")
    } else {
        moli_i18n::tr!("pilotes.synology.injoignable", error = format!("{error:#}"))
    }
}

impl Synology {
    async fn run_inner(&self, ctx: &mut DriverCtx) -> anyhow::Result<()> {
        let (Some(username), Some(password)) = (ctx.secret(USERNAME), ctx.secret(PASSWORD)) else {
            ctx.wait_for(moli_i18n::tr!(
                "pilotes.synology.identifiants_manquants",
                instance = ctx.instance(),
                username = USERNAME,
                password = PASSWORD
            ));
            ctx.cancelled().await;
            return Ok(());
        };
        if ctx.secret(CERT).is_none() && !ctx.can_store_secrets() {
            ctx.wait_for(moli_i18n::tr!("pilotes.synology.coffre_indisponible"));
            ctx.cancelled().await;
            return Ok(());
        }
        let mut state = State {
            username,
            password,
            client: None,
            mac: self
                .config
                .mac
                .clone()
                .or_else(|| ctx.known_address().and_then(|a| a.mac)),
            model: None,
            up: None,
            storage: None,
            refused: None,
            refusals: 0,
        };
        publish_nas(ctx, &state, model::nas_states_down());
        ctx.ready();
        let mut poll = tokio::time::interval(Duration::from_secs(self.config.poll_secs));
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                command = ctx.next_command() => {
                    let Some(command) = command else { return Ok(()) };
                    let result = self.execute(ctx, &mut state, &command).await;
                    let refused = result
                        .as_ref()
                        .err()
                        .filter(|e| e.downcast_ref::<dsm::Refused>().is_some())
                        .map(|e| format!("{e:#}"));
                    command.reply(result.map_err(|e| describe(&e)));
                    if let Some(why) = refused {
                        refuse(ctx, &mut state, &why);
                    }
                }
                _ = poll.tick() => {
                    if state.cooling_down() {
                        continue;
                    }
                    let Err(error) = self.poll(ctx, &mut state).await else {
                        if state.up != Some(true) {
                            tracing::info!(instance = %ctx.instance(), "synology reachable");
                        }
                        state.up = Some(true);
                        state.refusals = 0;
                        if state.refused.take().is_some() {
                            ctx.clear_warning();
                        }
                        continue;
                    };
                    unreachable(ctx, &state);
                    state.storage = None;
                    if error.downcast_ref::<dsm::Refused>().is_some() {
                        refuse(ctx, &mut state, &format!("{error:#}"));
                        continue;
                    }
                    if format!("{error:#}").contains(PIN_MISMATCH) {
                        ctx.wait_for(moli_i18n::tr!(
                            "pilotes.synology.certificat_change",
                            instance = ctx.instance(),
                            key = CERT
                        ));
                        ctx.cancelled().await;
                        return Ok(());
                    }
                    if state.up != Some(false) {
                        tracing::warn!(instance = %ctx.instance(), error = %format!("{error:#}"), "synology unreachable");
                    }
                    // Not a refusal (asleep, out of reach): the « login
                    // refused » warning no longer holds, and nothing is
                    // cooled down.
                    state.refused = None;
                    state.refusals = 0;
                    ctx.clear_warning();
                    state.up = Some(false);
                }
            }
        }
    }

    /// The session, opened on first need: the certificate is pinned before
    /// the password leaves.
    async fn client<'a>(
        &self,
        ctx: &DriverCtx,
        state: &'a mut State,
    ) -> anyhow::Result<&'a mut Client> {
        if state.client.is_none() {
            let pin = if let Some(pin) = ctx.secret(CERT) {
                pin
            } else {
                self.first_pin(ctx).await?
            };
            let http = PinnedHttps::new(&self.config.host, self.config.port, Some(pin))?;
            state.client = Some(Client::new(
                http,
                state.username.clone(),
                state.password.clone(),
            ));
        }
        state.client.as_mut().context("no session")
    }

    async fn first_pin(&self, ctx: &DriverCtx) -> anyhow::Result<String> {
        ensure!(
            ctx.can_store_secrets(),
            "cannot pin the NAS's certificate: the secret store is unavailable"
        );
        let probe = PinnedHttps::new(&self.config.host, self.config.port, None)?;
        probe.handshake().await.context("first contact")?;
        let seen = probe.seen_fingerprint().context("no certificate seen")?;
        ctx.store_secret(CERT, &seen).await?;
        tracing::info!(instance = %ctx.instance(), "synology certificate pinned");
        Ok(seen)
    }

    async fn poll(&self, ctx: &DriverCtx, state: &mut State) -> anyhow::Result<()> {
        let storage_due = state
            .storage
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() >= STORAGE_EVERY);
        let client = self.client(ctx, state).await?;
        let info = client
            .read("SYNO.DSM.Info", "2", "getinfo", &[])
            .await
            .context("system information")?;
        // The rest is best effort: an account without the right to a part
        // loses that part only.
        let usage = optional(
            client,
            "SYNO.Core.System.Utilization",
            "1",
            "get",
            &[("type", "current")],
        )
        .await?;
        let storage_read = if storage_due {
            Some(optional(client, "SYNO.Storage.CGI.Storage", "1", "load_info", &[]).await?)
        } else {
            None
        };
        let ups = optional(client, "SYNO.Core.ExternalDevice.UPS", "1", "get", &[]).await?;
        let tasks = optional(
            client,
            "SYNO.Backup.Task",
            "1",
            "list",
            &[("additional", r#"["last_bkp_time","last_bkp_result"]"#)],
        )
        .await?;

        // A storage that could not be read is asked again at the next poll.
        match storage_read {
            Some(Json::Null) => state.storage = None,
            Some(read) => state.storage = Some((Instant::now(), read)),
            None => {}
        }
        let storage = state
            .storage
            .as_ref()
            .map_or(Json::Null, |(_, s)| s.clone());

        let model = model::model(&info).map(str::to_owned);
        if model != state.model {
            state.model = model;
            publish_nas_device(ctx, state);
        }
        self.learn_mac(ctx, state).await;
        publish_nas(ctx, state, model::nas_states(&info, &usage));

        let instance = ctx.instance();
        let mut entities = model::volumes(instance, &storage);
        entities.extend(model::disks(instance, &storage));
        entities.extend(model::ups(instance, &ups));
        entities.extend(model::backups(instance, &tasks));
        let mut present = vec![ctx.device_id(model::NAS)];
        for entity in entities {
            let id = entity.device.id.clone();
            present.push(id.clone());
            ctx.upsert_device(entity.device);
            for (key, value) in entity.states {
                ctx.set_state(&id, key, value);
            }
            ctx.set_availability(&id, true);
        }
        // Gone from the NAS (a task deleted, a disk pulled): offline, not
        // removed, so its history stays readable.
        for id in ctx.devices() {
            if !present.contains(&id) {
                forget(ctx, &id);
                ctx.set_availability(&id, false);
            }
        }
        Ok(())
    }

    /// On a local network the NAS's MAC address is in the neighbour table:
    /// with it, Moli can wake the NAS even after a restart.
    async fn learn_mac(&self, ctx: &DriverCtx, state: &mut State) {
        if state.mac.is_some() {
            return;
        }
        let Ok(ip) = self.config.host.parse::<Ipv4Addr>() else {
            return;
        };
        let Some(mac) = moli_net::lan::mac_of(ip).await else {
            return;
        };
        ctx.remember_address(KnownAddress {
            configured: self.config.host.clone(),
            host: self.config.host.clone(),
            mac: Some(mac.clone()),
            identity: None,
        })
        .await;
        state.mac = Some(mac);
        publish_nas_device(ctx, state);
    }

    async fn execute(
        &self,
        ctx: &DriverCtx,
        state: &mut State,
        command: &CommandRequest,
    ) -> anyhow::Result<()> {
        ensure!(
            !command.abandoned(),
            moli_i18n::tr!("pilotes.synology.expire")
        );
        ensure!(
            &*command.key == model::CONTROL,
            moli_i18n::tr!("pilotes.synology.commande_inconnue")
        );
        let Value::Text(order) = &command.value else {
            bail!(moli_i18n::tr!("pilotes.synology.commande_inconnue"));
        };
        match &**order {
            model::WAKE => {
                let mac = state
                    .mac
                    .as_deref()
                    .context(moli_i18n::tr!("pilotes.synology.pas_de_mac"))?;
                moli_net::wol::wake(&colon(mac), Some(&self.config.host)).await
            }
            order @ (model::SHUTDOWN | model::REBOOT) => {
                ensure!(
                    !state.cooling_down(),
                    moli_i18n::tr!("pilotes.synology.refus_en_cours")
                );
                let client = self.client(ctx, state).await?;
                // Last check before the one request: it is never repeated.
                ensure!(
                    !command.abandoned(),
                    moli_i18n::tr!("pilotes.synology.expire")
                );
                client
                    .order(
                        "SYNO.Core.System",
                        "1",
                        order,
                        &[("force", "false"), ("local", "true")],
                    )
                    .await
                    .map(|_| ())
            }
            _ => bail!(moli_i18n::tr!("pilotes.synology.commande_inconnue")),
        }
    }
}

/// The NAS refused the login, from the poll or from an order: nothing is
/// known of it any more, and it is not asked again for a while.
fn refuse(ctx: &DriverCtx, state: &mut State, why: &str) {
    unreachable(ctx, state);
    state.storage = None;
    state.up = Some(false);
    state.refused = Some(Instant::now());
    state.refusals = state.refusals.saturating_add(1);
    let wait = retry_after(state.refusals);
    tracing::warn!(instance = %ctx.instance(), why, wait = ?wait, "synology refused the login, next try later");
    ctx.warn(moli_i18n::tr!(
        "pilotes.synology.identifiants_refuses",
        instance = ctx.instance(),
        username = USERNAME,
        password = PASSWORD
    ));
}

/// A part that cannot be read (no right, too slow, a dropped connection)
/// reads as empty: only that part is lost, and only `SYNO.DSM.Info` says
/// whether the NAS answers. A refused login or a changed certificate still
/// stops the poll.
async fn optional(
    client: &mut Client,
    api: &str,
    version: &str,
    method: &str,
    params: &[(&str, &str)],
) -> anyhow::Result<Json> {
    match client.read(api, version, method, params).await {
        Err(e)
            if e.downcast_ref::<dsm::Refused>().is_some()
                || format!("{e:#}").contains(PIN_MISMATCH) =>
        {
            Err(e)
        }
        Err(e) => {
            tracing::debug!(api, error = %format!("{e:#}"), "synology part unavailable");
            Ok(Json::Null)
        }
        ok => ok,
    }
}

fn publish_nas_device(ctx: &DriverCtx, state: &State) {
    ctx.upsert_device(model::nas_device(
        ctx.instance(),
        state.model.as_deref(),
        state.mac.is_some(),
    ));
}

/// The NAS device with its values. Always available: it is the driver's own.
fn publish_nas(ctx: &DriverCtx, state: &State, states: States) {
    publish_nas_device(ctx, state);
    let id = ctx.device_id(model::NAS);
    for (key, value) in states {
        ctx.set_state(&id, key, value);
    }
    ctx.set_availability(&id, true);
}

/// Nothing is known any more: no stale temperature, no stale « all fine ».
fn unreachable(ctx: &DriverCtx, state: &State) {
    let nas = ctx.device_id(model::NAS);
    for id in ctx.devices() {
        if id != nas {
            forget(ctx, &id);
            ctx.set_availability(&id, false);
        }
    }
    publish_nas(ctx, state, model::nas_states_down());
}

/// A part that is no longer read keeps no verdict: its `problem` and its
/// words go back to `Null`, or the dashboard would show a fault that nothing
/// reports any more.
fn forget(ctx: &DriverCtx, id: &DeviceId) {
    ctx.set_state(id, "problem", Value::Null);
    ctx.set_state(id, "detail", Value::Null);
}

#[cfg(test)]
mod tests;
