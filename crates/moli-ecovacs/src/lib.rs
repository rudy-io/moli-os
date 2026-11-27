//! Deebot X8 PRO OMNI over the Ecovacs Italy cloud.

mod cloud;
mod settings;

use std::collections::BTreeMap;
use std::io::{BufRead, Read as _, Write as _};
use std::time::{Duration, Instant};

use anyhow::ensure;
use moli_core::{Access, Device, Kind, PointSpec, Semantic, Unit, Value};
use moli_i18n::tr;
use moli_runtime::{BoxFuture, CommandRequest, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use zeroize::Zeroizing;

use cloud::{Account, CommandResult, Login, Robot, Session};

pub const SESSION_SECRET: &str = "session";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    writable: bool,
    #[serde(default = "default_poll")]
    poll_seconds: u64,
}

const fn default_poll() -> u64 {
    30
}

#[derive(Debug)]
pub struct Ecovacs(Config);

impl Ecovacs {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        ensure!(
            (15..=300).contains(&config.poll_seconds),
            "{}",
            tr!("ecovacs.invalid_poll")
        );
        Ok(Self(config))
    }

    async fn drive(&self, ctx: &mut DriverCtx) -> anyhow::Result<()> {
        let Some(secret) = ctx.secret(SESSION_SECRET).map(Zeroizing::new) else {
            ctx.wait_for(tr!("ecovacs.pair_required"));
            ctx.cancelled().await;
            return Ok(());
        };
        let account: Account = serde_json::from_str(&secret)
            .map_err(|_| anyhow::anyhow!(tr!("ecovacs.pair_required")))?;
        ensure!(
            account.resource.len() == 32 && account.resource.bytes().all(|b| b.is_ascii_hexdigit()),
            "{}",
            tr!("ecovacs.pair_required")
        );

        let mut session = account.session().await?;
        let mut robots = scan(ctx, &account, &session, self.0.writable).await?;
        let mut next_scan = Instant::now() + Duration::from_secs(900);
        let mut tick = tokio::time::interval(Duration::from_secs(self.0.poll_seconds));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                order = ctx.next_command() => {
                    let Some(order) = order else { return Ok(()) };
                    dispatch(order, &session, &account, &robots, self.0.writable).await;
                }
                _ = tick.tick() => {
                    if Instant::now() >= session.renew_at {
                        session = account.session().await?;
                    }
                    if Instant::now() >= next_scan {
                        robots = scan(ctx, &account, &session, self.0.writable).await?;
                        next_scan = Instant::now() + Duration::from_secs(900);
                    }
                    for (id, robot) in &mut robots {
                        Box::pin(poll(ctx, &account, &session, id, robot)).await;
                    }
                }
            }
        }
    }
}

impl Driver for Ecovacs {
    fn kind(&self) -> &'static str {
        "ecovacs"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.drive(ctx))
    }
}

fn input_line(input: &mut impl BufRead, limit: usize) -> anyhow::Result<Zeroizing<String>> {
    let mut line = Zeroizing::new(String::new());
    input.take((limit + 2) as u64).read_line(&mut line)?;
    let trimmed = line.trim_end_matches(['\n', '\r']).len();
    line.truncate(trimmed);
    ensure!(
        !line.is_empty() && line.len() <= limit,
        "{}",
        tr!("ecovacs.invalid_input")
    );
    Ok(line)
}

/// Pair a cloud account from two lines on stdin and, if required, a third
/// line with the emailed code. Only the resulting session goes in the vault.
pub async fn pair(input: &mut impl BufRead) -> anyhow::Result<Zeroizing<String>> {
    let email = input_line(input, 254)?;
    ensure!(email.contains('@'), "{}", tr!("ecovacs.invalid_input"));
    let password = input_line(input, 254)?;
    let login = cloud::login(email.trim(), &password).await?;
    drop(password);
    let account = match login {
        Login::Complete(account) => account,
        Login::Verify(verification) => {
            println!("verification_required");
            std::io::stdout().flush()?;
            eprintln!("{}", tr!("ecovacs.enter_code"));
            let code = input_line(input, 16)?;
            ensure!(
                code.bytes().all(|b| b.is_ascii_alphanumeric()),
                "{}",
                tr!("ecovacs.invalid_input")
            );
            verification.confirm(&code).await?
        }
    };
    // Confirm the account can obtain portal credentials before replacing an
    // existing vault entry.
    account.session().await?;
    Ok(Zeroizing::new(serde_json::to_string(&account)?))
}

struct DeviceState {
    robot: Robot,
    online: bool,
}

type Robots = BTreeMap<moli_core::DeviceId, DeviceState>;

fn supports_model(class: &str) -> bool {
    matches!(class, "eom321" | "gcu5tt" | "n0vyif" | "uke6j4" | "y72606")
}

async fn scan(
    ctx: &DriverCtx,
    account: &Account,
    session: &Session,
    writable: bool,
) -> anyhow::Result<Robots> {
    let mut robots = BTreeMap::new();
    for robot in session.robots(&account.resource).await? {
        if !supports_model(&robot.class) {
            continue;
        }
        let id = ctx.device_id(&robot.did);
        ctx.upsert_device(Device {
            id: id.clone(),
            instance: ctx.instance().clone(),
            native_name: "DEEBOT X8 PRO OMNI".into(),
            manufacturer: Some("Ecovacs".into()),
            model: Some("DEEBOT X8 PRO OMNI".into()),
            native_room: None,
            description: None,
            members: Vec::new(),
            points: points(writable),
        });
        ctx.set_availability(&id, false);
        robots.insert(
            id,
            DeviceState {
                robot,
                online: false,
            },
        );
    }
    for id in ctx.devices() {
        if !robots.contains_key(&id) {
            ctx.set_availability(&id, false);
        }
    }
    if robots.is_empty() {
        ctx.wait_for(tr!("ecovacs.no_robot"));
    } else {
        ctx.ready();
    }
    Ok(robots)
}

fn points(writable: bool) -> Vec<PointSpec> {
    let read = Access {
        read: true,
        write: false,
    };
    let control = |key: &'static str, label: String, readable| PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Binary,
        access: Access {
            read: readable,
            write: writable,
        },
        unit: None,
        semantic: Semantic::Control,
    };
    let mut points = vec![
        PointSpec {
            key: "battery".into(),
            label: tr!("ecovacs.battery").into(),
            kind: Kind::Numeric {
                min: Some(0.0),
                max: Some(100.0),
                step: None,
            },
            access: read,
            unit: Some(Unit::Percent),
            semantic: Semantic::Battery,
        },
        PointSpec {
            key: "state".into(),
            label: tr!("ecovacs.state").into(),
            kind: Kind::Text,
            access: read,
            unit: None,
            semantic: Semantic::Other,
        },
        control("clean", tr!("ecovacs.clean"), true),
        control("pause", tr!("ecovacs.pause"), true),
    ];
    if writable {
        points.push(control("dock", tr!("ecovacs.dock"), false));
    }
    points.extend(settings::points(writable));
    points
}

async fn poll(
    ctx: &DriverCtx,
    account: &Account,
    session: &Session,
    id: &moli_core::DeviceId,
    state: &mut DeviceState,
) {
    let request = |name| {
        session.command(
            &account.resource,
            &state.robot,
            name,
            None,
            Duration::from_secs(4),
        )
    };
    let (battery, work, charge, speed, mode, water, errors) = tokio::join!(
        request("getBattery"),
        request("getWorkState"),
        request("getChargeState"),
        request("getSpeed"),
        request("getWorkMode"),
        request("getWaterInfo"),
        request("getError")
    );
    let battery = body(battery);
    let work = body(work);
    let charge = body(charge);
    state.online = battery.is_some() || work.is_some() || charge.is_some();
    ctx.set_availability(id, state.online);
    if let Some(value) = battery
        .and_then(|body| body["data"]["value"].as_f64())
        .filter(|value| (0.0..=100.0).contains(value))
    {
        ctx.set_state(id, "battery", Value::Float(value));
    }
    if let Some(value) = status(work.as_ref(), charge.as_ref()) {
        ctx.set_state(id, "state", Value::Text(value.into()));
        ctx.set_state(
            id,
            "clean",
            Value::Bool(matches!(value, "cleaning" | "paused")),
        );
        ctx.set_state(id, "pause", Value::Bool(value == "paused"));
    }
    settings::publish(
        ctx,
        id,
        body(speed).as_ref(),
        body(mode).as_ref(),
        body(water).as_ref(),
        body(errors).as_ref(),
    );
}

fn body(result: anyhow::Result<CommandResult>) -> Option<Json> {
    match result {
        Ok(CommandResult::Body(body)) if body.is_object() => Some(body),
        _ => None,
    }
}

fn status(work: Option<&Json>, charge: Option<&Json>) -> Option<&'static str> {
    if charge.is_some_and(|body| body["data"]["isCharging"] == 1 || code(body) == Some(30007)) {
        return Some("charging");
    }
    let data = &work?["data"];
    if data["paused"] == 1 {
        return Some("paused");
    }
    match data["stationState"]["state"].as_str()? {
        "goCharging" | "goEmptying" => Some("tocharge"),
        "emptying" | "washing" | "drying" => Some("charging"),
        "idle" => match data["robotState"]["state"].as_str()? {
            "cleaning" => Some("cleaning"),
            "idle" => Some("idle"),
            _ => None,
        },
        _ => None,
    }
}

fn code(body: &Json) -> Option<i64> {
    body["code"]
        .as_i64()
        .or_else(|| body["code"].as_str()?.parse().ok())
}

fn order_payload(key: &str, value: &Value) -> Option<(&'static str, Json)> {
    let (action, content) = match (key, value) {
        ("clean", Value::Bool(true)) => ("start", json!({"type":"auto"})),
        ("clean", Value::Bool(false)) => ("stop", json!({"type":""})),
        ("pause", Value::Bool(true)) => ("pause", json!({"type":""})),
        ("pause", Value::Bool(false)) => ("resume", json!({})),
        ("dock", Value::Bool(true)) => return Some(("charge", json!({"act":"go"}))),
        _ => return settings::order(key, value),
    };
    Some(("clean_V2", json!({"act":action, "content":content})))
}

async fn dispatch(
    order: CommandRequest,
    session: &Session,
    account: &Account,
    robots: &Robots,
    writable: bool,
) {
    let Some(state) = robots
        .get(&order.device.id)
        .filter(|state| state.online && writable)
    else {
        order.reply_refused(tr!("ecovacs.unavailable"));
        return;
    };
    let Some((name, body)) = order_payload(&order.key, &order.value) else {
        order.reply_refused(tr!("ecovacs.command_refused"));
        return;
    };
    if order.abandoned() {
        order.reply_refused(tr!("ecovacs.command_refused"));
        return;
    }
    let time = order.deadline.saturating_duration_since(Instant::now());
    match session
        .command(&account.resource, &state.robot, name, Some(body), time)
        .await
    {
        Ok(CommandResult::Body(body))
            if code(&body) == Some(0) || (name == "charge" && code(&body) == Some(30007)) =>
        {
            order.reply(Ok(()));
        }
        Ok(CommandResult::Body(body)) if code(&body).is_some() => {
            order.reply_refused(tr!("ecovacs.command_refused"));
        }
        Ok(CommandResult::Refused) => order.reply_refused(tr!("ecovacs.command_refused")),
        _ => order.reply_outcome_unknown(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x8_work_and_dock_responses() {
        let paused = json!({"data":{"paused":1,"robotState":{"state":"cleaning"},"stationState":{"state":"idle"}}});
        let charging = json!({"data":{"isCharging":1}});
        assert_eq!(status(Some(&paused), None), Some("paused"));
        assert_eq!(status(Some(&paused), Some(&charging)), Some("charging"));
        assert_eq!(status(None, Some(&json!({"data":{"isCharging":0}}))), None);
    }

    #[test]
    fn x8_resume_preserves_the_current_job() {
        assert_eq!(
            order_payload("pause", &Value::Bool(false)),
            Some(("clean_V2", json!({"act":"resume","content":{}})))
        );
        assert!(order_payload("dock", &Value::Bool(false)).is_none());
        assert!(points(false).iter().all(|point| !point.access.write));
        assert!(
            points(true)
                .iter()
                .filter(|point| point.access.write)
                .all(|point| point.semantic == Semantic::Control)
        );
    }
}
