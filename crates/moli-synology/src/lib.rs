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
use moli_core::Value;
use moli_net::{PIN_MISMATCH, PinnedHttps};
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx, KnownAddress};
use serde::Deserialize;
use serde_json::Value as Json;
use std::net::Ipv4Addr;
use std::time::Duration;

const USERNAME: &str = "username";
const PASSWORD: &str = "password";
const CERT: &str = "cert";
/// DSM: this account has no right to that API (not an administrator).
const NO_RIGHT: u32 = 105;

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
                    command.reply(result.map_err(|e| describe(&e)));
                }
                _ = poll.tick() => {
                    let Err(error) = self.poll(ctx, &mut state).await else {
                        if state.up != Some(true) {
                            tracing::info!(instance = %ctx.instance(), "synology reachable");
                        }
                        state.up = Some(true);
                        continue;
                    };
                    unreachable(ctx, &state);
                    if error.downcast_ref::<dsm::Refused>().is_some() {
                        // Asking again would get the hub's address blocked.
                        ctx.wait_for(moli_i18n::tr!(
                            "pilotes.synology.identifiants_refuses",
                            instance = ctx.instance(),
                            username = USERNAME,
                            password = PASSWORD
                        ));
                        ctx.cancelled().await;
                        return Ok(());
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
        let storage = optional(client, "SYNO.Storage.CGI.Storage", "1", "load_info", &[]).await?;
        let ups = optional(client, "SYNO.Core.ExternalDevice.UPS", "1", "get", &[]).await?;
        let tasks = optional(
            client,
            "SYNO.Backup.Task",
            "1",
            "list",
            &[("additional", r#"["last_bkp_time","last_bkp_result"]"#)],
        )
        .await?;

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

/// A part DSM refuses to this account (code 105 and the like) reads as
/// empty; a refused login or a transport failure is still an error.
async fn optional(
    client: &mut Client,
    api: &str,
    version: &str,
    method: &str,
    params: &[(&str, &str)],
) -> anyhow::Result<Json> {
    match client.read(api, version, method, params).await {
        Err(e) if e.downcast_ref::<dsm::Api>().is_some() => {
            tracing::debug!(api, error = %e, "synology part unavailable");
            Ok(Json::Null)
        }
        other => other,
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
            ctx.set_availability(&id, false);
        }
    }
    publish_nas(ctx, state, model::nas_states_down());
}

#[cfg(test)]
mod tests;
