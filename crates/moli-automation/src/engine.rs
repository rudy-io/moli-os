//! When automations start: the hub's changes, a clock that ticks every
//! second, and the moment Moli starts. Only approved, switched-on
//! automations are considered.

use std::time::{Duration, Instant};

use moli_core::{Event, Notice, PointId, Value};
use moli_i18n::tr;
use tokio::sync::broadcast::error::RecvError;

use crate::model::{Automation, Mode, Step, SunEvent, minutes_of};
use crate::run::{day_ok, execute, same};
use crate::{Automations, clock, decimal, lock, yes_no};

/// Runs per minute beyond which an automation is switched off.
const RUNAWAY: usize = 20;
/// Runs waiting their turn, per queued automation.
const MAX_QUEUE: usize = 10;
/// Let the drivers connect before « when Moli starts ».
const START_DELAY: Duration = Duration::from_secs(15);

pub(crate) async fn run(autos: Automations) {
    let mut events = autos.0.hub.subscribe();
    remember_all(&autos);
    mark_this_minute(&autos);
    let mut clock = tokio::time::interval(Duration::from_secs(1));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let started = tokio::time::sleep(START_DELAY);
    tokio::pin!(started);
    let mut start_done = false;
    let cancel = autos.0.cancel.clone();
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            event = events.recv() => match event {
                Ok(Event::State { point, value, .. }) => on_state(&autos, &point, &value),
                Ok(_) => {}
                Err(RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "automations lagged behind the hub: some changes were not seen");
                    remember_all(&autos);
                }
                Err(RecvError::Closed) => break,
            },
            _ = clock.tick() => on_clock(&autos),
            () = &mut started, if !start_done => {
                start_done = true;
                for a in live(&autos).iter() {
                    for t in a.graph.triggers().filter(|t| matches!(t.step, Step::OnStart)) {
                        fire(&autos, a, &t.id, tr!("moteur.pourquoi.demarrage"));
                    }
                }
            }
        }
    }
    // Shutdown: running automations stop here, and get a moment to record
    // themselves (« interrompu ») before the process ends: a light switched
    // on for five minutes then left on must show in the history.
    autos
        .0
        .shutting_down
        .store(true, std::sync::atomic::Ordering::Relaxed);
    for (_, all) in lock(&autos.0.stops).drain() {
        all.cancel();
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while !lock(&autos.0.active).is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Clock triggers due this very minute count as fired: after a restart
/// within the minute, they do not fire twice.
fn mark_this_minute(autos: &Automations) {
    let now = jiff::Timestamp::now().to_zoned(autos.0.tz.clone());
    let minute = u32::from(now.hour().unsigned_abs()) * 60 + u32::from(now.minute().unsigned_abs());
    let stamp = format!("{} {minute}", now.date());
    let mut fired = lock(&autos.0.fired);
    for a in live(autos).iter() {
        for t in a.graph.triggers() {
            if matches!(
                t.step,
                Step::AtTime { .. } | Step::AtSun { .. } | Step::Every { .. }
            ) {
                fired.insert((a.id.clone(), t.id.clone()), stamp.clone());
            }
        }
    }
}

fn live(autos: &Automations) -> std::sync::Arc<Vec<Automation>> {
    autos.live()
}

/// Current values, so the first change seen has a « before ».
fn remember_all(autos: &Automations) {
    let snapshot = autos.0.hub.snapshot();
    let mut last = lock(&autos.0.last);
    for d in snapshot.devices {
        for (key, sample) in d.state {
            last.insert(format!("{}/{key}", d.device.id), sample.value);
        }
    }
}

fn on_state(autos: &Automations, point: &PointId, value: &Value) {
    let key = point.to_string();
    let prev = lock(&autos.0.last).insert(key.clone(), value.clone());
    if prev.as_ref() == Some(value) {
        return;
    }
    for a in live(autos).iter() {
        for t in a.graph.triggers() {
            match &t.step {
                Step::WhenState {
                    point: p,
                    to,
                    from,
                    for_s,
                } if *p == key => {
                    let to_ok = to.as_ref().is_none_or(|to| same(value, to));
                    let from_ok = from
                        .as_ref()
                        .is_none_or(|from| prev.as_ref().is_some_and(|p| same(p, from)));
                    // A point seen for the first time is not a change.
                    if prev.is_none() {
                        continue;
                    }
                    if to_ok && from_ok {
                        let why = format!("{} → {}", label(autos, &key), words(value));
                        if *for_s == 0 {
                            fire(autos, a, &t.id, why);
                        } else {
                            if to.is_none() {
                                unhold(autos, &a.id, &t.id);
                            }
                            hold(
                                autos,
                                a,
                                &t.id,
                                *for_s,
                                why,
                                key.clone(),
                                to.clone().or(Some(value.to_json())),
                            );
                        }
                    } else {
                        unhold(autos, &a.id, &t.id);
                    }
                }
                Step::WhenThreshold {
                    point: p,
                    above,
                    below,
                    for_s,
                } if *p == key => {
                    let inside = |v: &Value| {
                        v.as_f64().is_some_and(|x| {
                            above.is_none_or(|a| x > a) && below.is_none_or(|b| x < b)
                        })
                    };
                    let now_in = inside(value);
                    let was_in = prev.as_ref().map(inside);
                    if now_in && was_in == Some(false) {
                        let why = format!("{} → {}", label(autos, &key), words(value));
                        if *for_s == 0 {
                            fire(autos, a, &t.id, why);
                        } else {
                            let (above, below) = (*above, *below);
                            hold_until(autos, a, &t.id, *for_s, why, key.clone(), move |v| {
                                v.as_f64().is_some_and(|x| {
                                    above.is_none_or(|a| x > a) && below.is_none_or(|b| x < b)
                                })
                            });
                        }
                    } else if !now_in {
                        unhold(autos, &a.id, &t.id);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Fires after `for_s` if the point still has the value by then.
#[allow(clippy::too_many_arguments)]
fn hold(
    autos: &Automations,
    a: &Automation,
    trigger: &str,
    for_s: u64,
    why: String,
    point: String,
    to: Option<serde_json::Value>,
) {
    hold_until(autos, a, trigger, for_s, why, point, move |v| {
        to.as_ref().is_none_or(|to| same(v, to))
    });
}

fn hold_until(
    autos: &Automations,
    a: &Automation,
    trigger: &str,
    for_s: u64,
    why: String,
    point: String,
    still: impl Fn(&Value) -> bool + Send + 'static,
) {
    let key = (a.id.clone(), trigger.to_owned());
    // Already counting: the value did not leave, the clock keeps running.
    if lock(&autos.0.holds)
        .get(&key)
        .is_some_and(|h| !h.is_finished())
    {
        return;
    }
    let (autos2, a_id, trigger2) = (autos.clone(), a.id.clone(), trigger.to_owned());
    let task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(for_s)).await;
        let now = autos2
            .0
            .hub
            .state(&PointId::from(point.as_str()))
            .map(|s| s.value);
        lock(&autos2.0.holds).remove(&(a_id.clone(), trigger2.clone()));
        if now.as_ref().is_some_and(&still)
            && let Some(a) = autos2.get(&a_id).filter(Automation::is_live)
        {
            fire(&autos2, &a, &trigger2, why);
        }
    });
    lock(&autos.0.holds).insert(key, task.abort_handle());
}

fn unhold(autos: &Automations, automation: &str, trigger: &str) {
    if let Some(h) = lock(&autos.0.holds).remove(&(automation.to_owned(), trigger.to_owned())) {
        h.abort();
    }
}

fn on_clock(autos: &Automations) {
    let now = jiff::Timestamp::now().to_zoned(autos.0.tz.clone());
    let minute = u32::from(now.hour().unsigned_abs()) * 60 + u32::from(now.minute().unsigned_abs());
    let stamp = format!("{} {minute}", now.date());
    for a in live(autos).iter() {
        for t in a.graph.triggers() {
            let due = match &t.step {
                Step::AtTime { at, days } => day_ok(days, &now) && minutes_of(at) == Some(minute),
                Step::AtSun {
                    event,
                    offset_min,
                    days,
                } => {
                    day_ok(days, &now)
                        && sun_minute(autos, &now, *event, *offset_min) == Some(minute)
                }
                Step::Every { minutes } => *minutes > 0 && minute % minutes == 0,
                _ => false,
            };
            if !due {
                continue;
            }
            let key = (a.id.clone(), t.id.clone());
            let first = lock(&autos.0.fired).insert(key, stamp.clone()).as_deref() != Some(&*stamp);
            if first {
                fire(
                    autos,
                    a,
                    &t.id,
                    tr!(
                        "moteur.pourquoi.heure",
                        heure = clock(now.hour(), now.minute())
                    ),
                );
            }
        }
    }
}

/// Today's sunrise or sunset (+ offset), in local minutes.
fn sun_minute(
    autos: &Automations,
    now: &jiff::Zoned,
    event: SunEvent,
    offset_min: i32,
) -> Option<u32> {
    let (lat, lon) = (autos.0.config.latitude?, autos.0.config.longitude?);
    let (rise, set) = crate::sun::times(now.date(), lat, lon)?;
    let at = if event == SunEvent::Rise { rise } else { set };
    let local = at.to_zoned(autos.0.tz.clone());
    let m = i32::from(local.hour()) * 60 + i32::from(local.minute()) + offset_min;
    u32::try_from(m.rem_euclid(24 * 60)).ok()
}

/// Starts a run, by the automation's mode, unless it is running away.
pub(crate) fn fire(autos: &Automations, a: &Automation, trigger: &str, why: String) {
    if runaway(autos, a) {
        return;
    }
    autos.start_run(a, trigger, why, false);
}

fn runaway(autos: &Automations, a: &Automation) -> bool {
    let now = Instant::now();
    let too_many = {
        let mut rate = lock(&autos.0.rate);
        let starts = rate.entry(a.id.clone()).or_default();
        while starts
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
        {
            starts.pop_front();
        }
        starts.push_back(now);
        starts.len() > RUNAWAY
    };
    if too_many {
        tracing::warn!(automation = a.id, "automation running away: switched off");
        let autos2 = autos.clone();
        let (id, name) = (a.id.clone(), a.name.clone());
        tokio::spawn(async move {
            let _ = autos2.update(&id, |a| {
                a.enabled = false;
                a.note = Some(tr!("moteur.notice.boucle_note", max = RUNAWAY));
            });
            autos2.stop(&id);
            if let Err(e) = autos2.persist().await {
                tracing::warn!(error = %e, "could not save the switched-off automation");
            }
            autos2.changed(&id);
            autos2.0.hub.notice(Notice {
                title: Some(tr!("moteur.notice.boucle_titre")),
                message: tr!("moteur.notice.boucle", nom = name),
                from: None,
                ts: moli_core::now_ms(),
            });
        });
    }
    too_many
}

impl Automations {
    /// Spawns a run by the automation's mode; returns the run as it starts
    /// (its id is final).
    pub(crate) fn start_run(
        &self,
        a: &Automation,
        trigger: &str,
        why: String,
        manual: bool,
    ) -> crate::Run {
        let skipped = |why: String| crate::Run {
            id: 0,
            automation: a.id.clone(),
            name: a.name.clone(),
            trigger: trigger.to_owned(),
            why,
            started: moli_core::now_ms(),
            ended: Some(moli_core::now_ms()),
            status: crate::RunStatus::Cancelled,
            dry: false,
            steps: Vec::new(),
        };
        // Every run's token is a child of the automation's: switching it off
        // cancels them all, the waiting ones too.
        let token = lock(&self.0.stops)
            .entry(a.id.clone())
            .or_default()
            .child_token();
        match a.mode {
            Mode::Restart => {
                if let Some((_, old)) = lock(&self.0.active).remove(&a.id) {
                    old.cancel();
                }
            }
            Mode::Single => {
                let busy = lock(&self.0.active)
                    .get(&a.id)
                    .is_some_and(|(_, t)| !t.is_cancelled());
                if busy && !manual {
                    tracing::debug!(automation = a.id, "already running: trigger ignored");
                    return skipped(why);
                }
            }
            Mode::Queued => {
                let mut waiting = lock(&self.0.waiting);
                let n = waiting.entry(a.id.clone()).or_default();
                if *n >= MAX_QUEUE {
                    tracing::warn!(automation = a.id, "queue full: trigger ignored");
                    return skipped(why);
                }
                *n += 1;
            }
        }
        let id = self.next_id();
        lock(&self.0.active).insert(a.id.clone(), (id, token.clone()));
        let queue = (a.mode == Mode::Queued).then(|| {
            lock(&self.0.queues)
                .entry(a.id.clone())
                .or_default()
                .clone()
        });
        let (this, a2, trigger2, why2) = (self.clone(), a.clone(), trigger.to_owned(), why.clone());
        tokio::spawn(async move {
            let turn = match &queue {
                Some(q) => {
                    let turn = q.lock().await;
                    if let Some(n) = lock(&this.0.waiting).get_mut(&a2.id) {
                        *n = n.saturating_sub(1);
                    }
                    Some(turn)
                }
                None => None,
            };
            // After waiting its turn, it runs only if that same version is
            // still approved and on.
            let current = this.get(&a2.id);
            let still = current.is_some_and(|c| c.is_live() && c.fingerprint() == a2.fingerprint());
            if still && !token.is_cancelled() {
                execute(&this, &a2, &trigger2, why2, false, token, id).await;
            }
            drop(turn);
            // Only clear our own entry: a newer run may have replaced it.
            let mut active = lock(&this.0.active);
            if active.get(&a2.id).is_some_and(|(rid, _)| *rid == id) {
                active.remove(&a2.id);
            }
        });
        crate::Run {
            id,
            automation: a.id.clone(),
            name: a.name.clone(),
            trigger: trigger.to_owned(),
            why,
            started: moli_core::now_ms(),
            ended: None,
            status: crate::RunStatus::Running,
            dry: false,
            steps: Vec::new(),
        }
    }
}

fn label(autos: &Automations, point: &str) -> String {
    PointId::from(point)
        .split()
        .and_then(|(d, _)| autos.0.hub.device(&d))
        .map_or_else(
            || point.to_owned(),
            |v| {
                v.label
                    .name
                    .clone()
                    .unwrap_or_else(|| v.device.native_name.to_string())
            },
        )
}

fn words(v: &Value) -> String {
    match v {
        Value::Bool(b) => yes_no(*b),
        Value::Float(f) => decimal(&format!("{}", (f * 10.0).round() / 10.0)),
        Value::Int(i) => i.to_string(),
        Value::Text(t) => t.to_string(),
        Value::Null => "—".into(),
    }
}
