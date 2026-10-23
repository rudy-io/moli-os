//! Somfy TaHoma driver, through the gateway's local Overkiz API.
//!
//! The gateway is discovered by mDNS (`_kizboxdev._tcp`) and its certificate
//! is pinned on first contact, before the token goes anywhere. The token is
//! not a password: it is generated in the Somfy app (developer mode) and
//! stored in Moli's vault, so nothing here can create a session by itself.
//!
//! Positions: Overkiz counts a closure from the open end (`0` open, `100`
//! closed) while Moli shows an opening percentage, so every position is
//! inverted on the way in and on the way out. Awnings count deployment
//! (`0` folded, `100` deployed) which is already an opening.
//!
//! Commands are the gateway's own (`open`, `stop`, `close`, `setClosure`,
//! `setDeployment`): the driver never invents one. `exec/apply` accepting the
//! action is all a command acknowledges; the position that follows arrives
//! on the next poll, as a normal state update.

mod hub;
mod model;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;

use crate::hub::{Gateway, PIN_MISMATCH, TOKEN_REFUSED};
use crate::model::{Index, Load, RawDevice};

/// How often the gateway is asked for its states. Shutters take seconds to
/// move: five seconds is enough to follow them without hammering the gateway.
const POLL: Duration = Duration::from_secs(5);

/// A command must not outlive the hub's own deadline (5 s).
const COMMAND_BUDGET: Duration = Duration::from_millis(4500);

/// After this many failed polls in a row (five minutes), the driver restarts
/// with a backoff instead of pretending the gateway is there.
const GIVE_UP_AFTER: u32 = 60;

/// The bearer token generated in the Somfy app (developer mode).
const TOKEN: &str = "token";
/// The gateway's certificate fingerprint, pinned before the token is used.
const CERT: &str = "cert_sha256";

/// `[[driver]] kind = "tahoma"` with `host` in the options.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Address of the gateway on the home network.
    pub host: String,
    /// A fingerprint given by hand wins over the one pinned at first contact.
    #[serde(default)]
    pub cert_sha256: Option<String>,
}

#[derive(Debug)]
pub struct Tahoma {
    config: Config,
}

impl Tahoma {
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Driver for Tahoma {
    fn kind(&self) -> &'static str {
        "tahoma"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let host = config.host.as_str();
    let pin = match (config.cert_sha256.clone(), ctx.secret(CERT)) {
        (Some(pin), _) | (None, Some(pin)) => pin,
        (None, None) if !ctx.can_store_secrets() => {
            let reason = moli_i18n::tr!(
                "pilotes.tahoma.epinglage_impossible",
                problem = secrets_problem(ctx)
            );
            return park(ctx, reason).await;
        }
        (None, None) => {
            let seen = Gateway::first_contact(host).await.context("tahoma")?;
            ctx.store_secret(CERT, &seen).await?;
            tracing::info!(instance = %ctx.instance(), fingerprint = %seen, "tahoma gateway certificate pinned");
            seen
        }
    };

    let Some(token) = ctx.secret(TOKEN) else {
        let reason = moli_i18n::tr!("pilotes.tahoma.jeton_absent", host = host);
        return park(ctx, reason).await;
    };

    let gateway = Arc::new(Gateway::new(host, Some(pin), &token)?);
    let raw = match gateway.devices().await {
        Ok(raw) => raw,
        Err(e) if is_pin_mismatch(&e) => {
            let reason = certificate_changed(host, ctx, &gateway);
            return park(ctx, reason).await;
        }
        Err(e) if is_token_refused(&e) => {
            let reason = moli_i18n::tr!("pilotes.tahoma.jeton_refuse", host = host);
            return park(ctx, reason).await;
        }
        Err(e) => return Err(e.context("tahoma: first load")),
    };

    let mut index = publish(ctx, &raw);
    tracing::info!(
        instance = %ctx.instance(),
        devices = index.ids().len(),
        "tahoma gateway loaded"
    );
    ctx.ready();

    let mut failures = 0u32;
    loop {
        tokio::select! {
            command = ctx.next_command() => {
                let Some(command) = command else { return Ok(()) };
                spawn_command(&gateway, &index, command);
            }
            () = tokio::time::sleep(POLL) => {
                match gateway.devices().await {
                    Ok(raw) => {
                        failures = 0;
                        let load = Load::new(ctx.instance(), &raw);
                        if !load.index.same_ids(&index) {
                            tracing::info!(instance = %ctx.instance(), "tahoma devices changed: full load");
                            for id in index.ids() {
                                if !load.index.contains(&id) {
                                    ctx.remove_device(&id);
                                }
                            }
                            for device in load.devices {
                                ctx.upsert_device(device);
                            }
                            index = load.index;
                        }
                        apply(ctx, &index, &raw);
                    }
                    Err(e) if is_pin_mismatch(&e) => {
                        let reason = certificate_changed(host, ctx, &gateway);
                        return park(ctx, reason).await;
                    }
                    Err(e) if is_token_refused(&e) => {
                        let reason = moli_i18n::tr!("pilotes.tahoma.jeton_refuse", host = host);
                        return park(ctx, reason).await;
                    }
                    Err(e) => {
                        failures += 1;
                        tracing::warn!(instance = %ctx.instance(), error = %e, "tahoma poll failed");
                        if failures == 1 {
                            mark_offline(ctx, &index);
                        }
                        if failures >= GIVE_UP_AFTER {
                            return Err(e.context("tahoma: gateway unreachable"));
                        }
                    }
                }
            }
        }
    }
}

/// Publishes the loaded devices and returns their index.
fn publish(ctx: &DriverCtx, raw: &[RawDevice]) -> Index {
    let load = Load::new(ctx.instance(), raw);
    if !load.skipped.is_empty() {
        let classes: Vec<&str> = load.skipped.iter().map(|s| &*s.class).collect();
        tracing::debug!(instance = %ctx.instance(), classes = ?classes, "tahoma devices left out");
    }
    let index = load.index.clone();
    for device in load.devices {
        ctx.upsert_device(device);
    }
    apply(ctx, &index, raw);
    index
}

fn apply(ctx: &DriverCtx, index: &Index, raw: &[RawDevice]) {
    for update in index.decode(raw) {
        for (key, value) in &update.values {
            ctx.set_state(&update.id, key, value.clone());
        }
        ctx.set_availability(&update.id, update.online);
    }
}

fn mark_offline(ctx: &DriverCtx, index: &Index) {
    for id in index.ids() {
        ctx.set_availability(&id, false);
    }
}

/// Commands run beside the polling loop: a slow gateway never delays a state.
fn spawn_command(gateway: &Arc<Gateway>, index: &Index, command: CommandRequest) {
    let Some((url, commands)) = index.encode(&command.device.id, &command.key, &command.value)
    else {
        command.reply(Err(moli_i18n::tr!("pilotes.tahoma.point_non_ecrivable")));
        return;
    };
    let actions = vec![serde_json::json!({ "deviceURL": url, "commands": commands })];
    let gateway = Arc::clone(gateway);
    let label = format!("moli {}", command.key);
    tokio::spawn(async move {
        let result = tokio::time::timeout(COMMAND_BUDGET, gateway.apply(&label, actions))
            .await
            .unwrap_or_else(|_| {
                Err(anyhow::anyhow!(moli_i18n::tr!(
                    "pilotes.tahoma.passerelle_muette"
                )))
            });
        command.reply(result.map(|_exec| ()).map_err(|e| format!("{e:#}")));
    });
}

async fn park(ctx: &DriverCtx, reason: String) -> anyhow::Result<()> {
    ctx.wait_for(reason);
    ctx.cancelled().await;
    Ok(())
}

fn secrets_problem(ctx: &DriverCtx) -> String {
    ctx.secrets_problem().map_or_else(
        || moli_i18n::tr!("pilotes.tahoma.coffre_indisponible"),
        str::to_owned,
    )
}

/// The gateway presented another certificate: say which one was expected and
/// how to pin the new one, without ever sending the token over it.
fn certificate_changed(host: &str, ctx: &DriverCtx, gateway: &Gateway) -> String {
    moli_i18n::tr!(
        "pilotes.tahoma.certificat_change",
        host = host,
        pinned = gateway.pinned_fingerprint().unwrap_or_default(),
        seen = gateway.seen_fingerprint().unwrap_or_default(),
        instance = ctx.instance(),
        key = CERT
    )
}

fn is_pin_mismatch(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains(PIN_MISMATCH)
}

fn is_token_refused(error: &anyhow::Error) -> bool {
    format!("{error:#}").contains(TOKEN_REFUSED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_refusal_is_told_apart_from_a_network_error() {
        let refused = anyhow::anyhow!("{TOKEN_REFUSED}: 401 Unauthorized");
        assert!(is_token_refused(&refused));
        assert!(!is_pin_mismatch(&refused));
        let network = anyhow::anyhow!("connect timeout");
        assert!(!is_token_refused(&network));
    }

    #[test]
    fn a_changed_certificate_is_recognised() {
        let changed = anyhow::anyhow!("{PIN_MISMATCH}: expected aa, saw bb");
        assert!(is_pin_mismatch(&changed));
    }
}
