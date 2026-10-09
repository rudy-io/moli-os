//! The contract between the core and a driver.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use moli_core::{Device, DeviceId, DriverStatus, InstanceId, PointId, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::Hub;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A driver connects one external system to the hub.
///
/// `run` is expected to run forever. Returning — with `Ok` or `Err` — or
/// panicking makes the supervisor restart it with backoff, reusing the same
/// [`DriverCtx`] so queued commands survive restarts.
pub trait Driver: Send + Sync + 'static {
    fn kind(&self) -> &'static str;
    /// The catalogue package it runs (`integrations/<id>/`): a native
    /// driver's own kind; the generic profile driver says which profile.
    fn integration(&self) -> Arc<str> {
        Arc::from(self.kind())
    }
    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>>;
}

/// A validated write addressed to a driver. The driver must answer through
/// [`CommandRequest::reply`] once the order has been handed to the device
/// (state confirmation arrives later as a regular state update).
#[derive(Debug)]
pub struct CommandRequest {
    pub point: PointId,
    pub device: Arc<Device>,
    pub key: Arc<str>,
    /// Canonical value, already validated against the point spec.
    pub value: Value,
    /// When the hub stopped waiting: an order past it must never run (a
    /// lamp switched on hours after « failed » is worse than a refusal).
    pub deadline: Instant,
    pub(crate) reply: oneshot::Sender<Result<(), String>>,
}

impl CommandRequest {
    /// Nobody waits for the answer any more: the hub timed out or gave up.
    #[must_use]
    pub fn abandoned(&self) -> bool {
        self.reply.is_closed() || Instant::now() >= self.deadline
    }

    /// Reports the outcome. Dropping the request without replying is
    /// reported to the caller as a driver failure.
    pub fn reply(self, result: Result<(), String>) {
        let _ = self.reply.send(result);
    }
}

/// A driver's view of the hub, scoped to its own instance: a driver can only
/// publish and modify the devices it owns.
#[derive(Debug)]
pub struct DriverCtx {
    hub: Hub,
    instance: InstanceId,
    commands: mpsc::Receiver<CommandRequest>,
    cancel: CancellationToken,
}

impl DriverCtx {
    pub(crate) fn new(
        hub: Hub,
        instance: InstanceId,
        commands: mpsc::Receiver<CommandRequest>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            hub,
            instance,
            commands,
            cancel,
        }
    }

    #[must_use]
    pub fn instance(&self) -> &InstanceId {
        &self.instance
    }

    /// Builds a device id owned by this instance.
    #[must_use]
    pub fn device_id(&self, native: &str) -> DeviceId {
        DeviceId::new(&self.instance, native)
    }

    /// Signals that the driver is connected and operational.
    pub fn ready(&self) {
        self.hub
            .set_driver_status(&self.instance, DriverStatus::Running);
    }

    /// Signals that the driver is blocked on a human action (shown on every
    /// surface, so an agent can relay it). `ready()` clears it.
    pub fn wait_for(&self, reason: impl Into<String>) {
        self.hub.set_driver_status(
            &self.instance,
            DriverStatus::Waiting {
                reason: reason.into(),
            },
        );
    }

    /// Whether credentials obtained at runtime can be persisted.
    #[must_use]
    pub fn can_store_secrets(&self) -> bool {
        self.hub.has_secret_store()
    }

    /// A secret previously stored by this instance.
    #[must_use]
    pub fn secret(&self, name: &str) -> Option<String> {
        self.hub.secret(&self.instance, name)
    }

    /// Persists a secret for this instance (encrypted at rest).
    pub async fn store_secret(&self, name: &str, value: &str) -> std::io::Result<()> {
        self.hub.store_secret(&self.instance, name, value).await
    }

    /// Deletes a secret of this instance (revoked key…).
    pub async fn forget_secret(&self, name: &str) -> std::io::Result<bool> {
        self.hub.forget_secret(&self.instance, name).await
    }

    /// Why secrets cannot be stored, if they cannot (human-readable).
    #[must_use]
    pub fn secrets_problem(&self) -> Option<&str> {
        self.hub.secrets_problem()
    }

    /// Records an occurrence (button press…): broadcast even when the value
    /// equals the previous one, because each one is a new event.
    pub fn pulse_state(&self, device: &DeviceId, key: &str, value: Value) {
        if self.owns(device) {
            self.hub.pulse_state(device, key, value);
        }
    }

    /// Publishes or replaces a device. Devices of other instances are refused.
    pub fn upsert_device(&self, device: Device) {
        if self.owns(&device.id) && device.instance == self.instance {
            self.hub.upsert_device(device);
        } else {
            tracing::warn!(instance = %self.instance, device = %device.id, "refused foreign device");
        }
    }

    /// Something that publishes media for devices to fetch, usable from a
    /// task the driver spawns (the context itself stays in its loop).
    #[must_use]
    pub fn media_publisher(&self) -> MediaPublisher {
        MediaPublisher(self.hub.clone())
    }

    /// Declares that `device` (one of ours) can produce images on demand.
    pub fn provide_snapshots(
        &self,
        device: &DeviceId,
        source: Arc<dyn crate::media::SnapshotSource>,
    ) {
        if self.owns(device) {
            self.hub.provide_snapshots(device, source);
        }
    }

    /// Declares that `device` (one of ours) prints documents.
    pub fn provide_printing(&self, device: &DeviceId, sink: Arc<dyn crate::media::PrintSink>) {
        if self.owns(device) {
            self.hub.provide_printing(device, sink);
        }
    }

    pub fn remove_device(&self, id: &DeviceId) {
        if self.owns(id) {
            self.hub.remove_device(id);
        }
    }

    /// Ids of the devices currently published by this instance.
    #[must_use]
    pub fn devices(&self) -> Vec<DeviceId> {
        self.hub.device_ids_of(&self.instance)
    }

    /// The current value of one of this instance's points (restored from
    /// the state cache after a restart), if any.
    #[must_use]
    pub fn current(&self, device: &DeviceId, key: &str) -> Option<Value> {
        if !self.owns(device) {
            return None;
        }
        self.hub
            .state(&PointId::new(device, key))
            .map(|sample| sample.value)
    }

    /// Records a value for a point of one of this instance's devices.
    pub fn set_state(&self, device: &DeviceId, key: &str, value: Value) {
        if self.owns(device) {
            self.hub.set_state(device, key, value);
        }
    }

    pub fn set_availability(&self, device: &DeviceId, online: bool) {
        if self.owns(device) {
            self.hub.set_availability(device, online);
        }
    }

    /// Next command for this driver. Cancel-safe; `None` once shut down.
    /// Orders nobody waits for any more (past their deadline) are refused
    /// here and never reach the driver.
    pub async fn next_command(&mut self) -> Option<CommandRequest> {
        loop {
            let command = tokio::select! {
                () = self.cancel.cancelled() => None,
                cmd = self.commands.recv() => cmd,
            }?;
            if !command.abandoned() {
                return Some(command);
            }
            tracing::warn!(instance = %self.instance, point = %command.point, "stale order dropped");
            command.reply(Err("expired".into()));
        }
    }

    /// Refuses every queued order: the driver is restarting, and nothing
    /// queued now may run later by surprise.
    pub(crate) fn drain(&mut self, why: &str) {
        while let Ok(command) = self.commands.try_recv() {
            command.reply(Err(why.to_owned()));
        }
    }

    /// Resolves when the system is shutting down.
    pub async fn cancelled(&self) {
        self.cancel.cancelled().await;
    }

    fn owns(&self, id: &DeviceId) -> bool {
        id.instance() == self.instance
    }
}

/// Publishes short-lived media (see [`Hub::publish_media`]).
#[derive(Clone, Debug)]
pub struct MediaPublisher(Hub);

impl MediaPublisher {
    /// The name under which `/api/media/<name>` serves it for a few minutes.
    #[must_use]
    /// `None` only if the system could not draw a random name.
    pub fn publish(&self, extension: &str, image: crate::media::Image) -> Option<String> {
        self.0.publish_media(extension, image)
    }

    /// Tells the people at home that something the driver did in the
    /// background did not happen (the command itself already answered).
    pub fn tell(&self, from: &str, message: String) {
        self.0.notice(moli_core::Notice {
            title: None,
            message,
            from: Some(from.to_owned()),
            ts: moli_core::now_ms(),
        });
    }
}
