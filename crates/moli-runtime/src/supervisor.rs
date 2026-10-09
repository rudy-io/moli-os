//! Driver supervision: a failing or panicking driver never takes the core
//! down; it is restarted with exponential backoff and its status is public.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::FutureExt;
use moli_core::{DriverStatus, InstanceId};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::{Driver, DriverCtx, Hub};

const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
/// A run lasting this long is considered healthy and resets the backoff.
const HEALTHY_RUN: Duration = Duration::from_secs(60);

/// Registers `driver` under `instance` and runs it until `cancel` fires.
pub fn spawn_driver(
    hub: &Hub,
    instance: InstanceId,
    driver: Arc<dyn Driver>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    let commands = hub.register_driver(&instance, driver.kind(), driver.integration());
    let mut ctx = DriverCtx::new(hub.clone(), instance.clone(), commands, cancel.clone());
    let hub = hub.clone();
    tokio::spawn(async move {
        let mut backoff = BACKOFF_MIN;
        loop {
            hub.set_driver_status(&instance, DriverStatus::Starting);
            let started = Instant::now();
            let outcome = tokio::select! {
                () = cancel.cancelled() => break,
                outcome = AssertUnwindSafe(driver.run(&mut ctx)).catch_unwind() => outcome,
            };
            let error = match outcome {
                Ok(Ok(())) => "driver stopped unexpectedly".to_owned(),
                Ok(Err(e)) => format!("{e:#}"),
                Err(panic) => format!("panic: {}", panic_message(&*panic)),
            };
            if started.elapsed() >= HEALTHY_RUN {
                backoff = BACKOFF_MIN;
            }
            // Nothing queued now may run after the restart, and its devices
            // stop pretending to be reachable.
            ctx.drain("driver restarting");
            hub.mark_instance_offline(&instance);
            tracing::warn!(%instance, %error, retry_in = ?backoff, "driver failed");
            hub.set_driver_status(
                &instance,
                DriverStatus::Backoff {
                    error,
                    retry_in_ms: u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
                },
            );
            tokio::select! {
                () = cancel.cancelled() => break,
                () = tokio::time::sleep(backoff) => {}
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
        hub.set_driver_status(&instance, DriverStatus::Stopped);
    })
}

fn panic_message(panic: &(dyn Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown")
}
