//! BLE central role for Matter commissioning (feature `central`).
//!
//! Scans for a commissionable device by discriminator, connects, opens the
//! Matter BTP GATT service (C1 write / C2 indication), performs the BTP
//! handshake, and drives the resulting `BtpSession` from an async pump task,
//! exposing a `BtpChannel` that sends and receives whole Matter messages.
//!
//! **macOS Bluetooth permission (TCC):** `BleCentral::new` instantiates a
//! `CoreBluetooth` `CBCentralManager`, which triggers the one-time Bluetooth
//! permission prompt attributed to the terminal application. Construct it only
//! from a user-initiated commissioning flow — never at library init — and see
//! `docs/runbooks/ble-commissioning.md`.
//!
//! btleplug specifics relied on here are pinned against 0.12.0:
//! service-data advertisements arrive on the `events()` stream (never the stale
//! `PeripheralProperties`), a service-UUID `ScanFilter` is unusable because it
//! blinds the `BlueZ` backend (so scans are unfiltered — see
//! `BleCentral::find_device`), `notifications()` is a capacity-16 broadcast that
//! drops on lag (so the pump drains it continuously, started before the
//! handshake write), and `mtu()` is unreadable on macOS (so the handshake
//! requests MTU 0 and adopts the peripheral's advertised fragment size).

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use btleplug::api::{
    Central, CentralEvent, CentralState, Characteristic, Manager as _, Peripheral as _, ScanFilter,
    WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral, PeripheralId};
use futures::{Stream, StreamExt};
use thiserror::Error;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot, Mutex as TokioMutex, MutexGuard, OnceCell};
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::advert::CommissionableAdvert;
use crate::handshake::{HandshakeRequest, HandshakeResponse, WINDOW_SIZE};
use crate::session::{BtpSession, Role};
use crate::BtpError;

/// Diagnostic pump tracing, enabled by setting `MATTER_BLE_PUMP_TRACE=1`.
///
/// macOS has no `btmon`, and the pump runs in a spawned task that prints
/// nothing between "commissioning…" and the result — so a hang (the known
/// macOS `CoreBluetooth` BTP stall) is invisible. When enabled, the pump emits
/// a timestamped line at each `open_btp` boundary (peripheral, connect, service
/// discovery, notifications, C1 write, subscribe, handshake response) and each
/// C1-write / C2-indication in the pump. This is how the macOS hang was
/// root-caused: the trace stops at `discover_services START` (or, past a patched
/// btleplug, at `handshake C1 write START`) — see `TODO-1.0.md`, "BLE central
/// on macOS". Confirmed cause: btleplug 0.12.0 drops errored
/// `CoreBluetooth` delegate events, and `CoreBluetooth` rejects the `CHIPoBLE`
/// GATT ops with `CBError.uuidNotAllowed`.
///
/// Off by default and side-effect-free (a single relaxed env read, cached), so
/// it cannot affect the proven Linux path.
fn pump_trace_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var("MATTER_BLE_PUMP_TRACE")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    })
}

/// Emit one diagnostic pump-trace line to stderr when `MATTER_BLE_PUMP_TRACE`
/// is set (see [`pump_trace_enabled`]).
fn pt(msg: &str) {
    if pump_trace_enabled() {
        eprintln!("[btp-pump] {msg}");
    }
}

/// Matter BTP GATT service UUID (`0000fff6-0000-1000-8000-00805f9b34fb`).
pub const MATTER_SERVICE_UUID: Uuid = Uuid::from_u128(0x0000_fff6_0000_1000_8000_0080_5f9b_34fb);
/// C1 characteristic — central writes BTP fragments here
/// (`18ee2ef5-263d-4559-959f-4f9c429f9d11`).
pub const C1_UUID: Uuid = Uuid::from_u128(0x18ee_2ef5_263d_4559_959f_4f9c_429f_9d11);
/// C2 characteristic — the device indicates BTP fragments here
/// (`18ee2ef5-263d-4559-959f-4f9c429f9d12`).
pub const C2_UUID: Uuid = Uuid::from_u128(0x18ee_2ef5_263d_4559_959f_4f9c_429f_9d12);

/// How long to wait for a GATT connection before giving up (plain `connect`
/// never times out on `CoreBluetooth`).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long to wait for the BTP handshake response indication
/// (chip `BTP_CONN_RSP_TIMEOUT_MS`).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Per-attempt timeout for GATT service discovery. On `CoreBluetooth`,
/// `discover_services()` can never return on its own: `btleplug` 0.12.0 only
/// completes the discovery future once every characteristic's *descriptor*
/// discovery reports success, but it silently drops the completion event when a
/// descriptor probe returns an error (`central_delegate.rs`, the
/// `didDiscoverDescriptorsForCharacteristic` handler gates on `error.is_none()`).
/// `CHIPoBLE`'s C1/C2 return `CBError.uuidNotAllowed` for that probe, so the
/// future stalls forever. A per-attempt bound converts the stall into a clean,
/// fast failure instead of hanging past every commissioning deadline. See
/// `TODO-1.0.md`, "BLE central on macOS".
const SERVICE_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(12);
/// How many times to (re-)issue service discovery before giving up. A retry
/// cannot recover the `btleplug` event-drop above (re-discovery hits the same
/// descriptor error), but it is cheap insurance against a genuinely transient
/// stall on other peripherals.
const SERVICE_DISCOVERY_ATTEMPTS: u32 = 2;
/// Timeout for a single C1 GATT write (`WithResponse`). Same `btleplug` 0.12.0
/// failure mode as discovery: the `didWriteValueForCharacteristic` handler only
/// emits its completion event on `error.is_none()`, so a write that
/// `CoreBluetooth` rejects (`CHIPoBLE` writes currently draw
/// `CBError.uuidNotAllowed` on macOS)
/// leaves `write().await` pending forever. Bound it so the failure surfaces.
const C1_WRITE_TIMEOUT: Duration = Duration::from_secs(12);
/// Bound for the best-effort pre-connect disconnect. On a not-connected
/// peripheral `CoreBluetooth`'s `disconnect()` never returns, so this is short:
/// if there is nothing to tear down we move on to `connect()` promptly.
const PRE_CONNECT_DISCONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Bound for each best-effort step of [`teardown_link`]. Both `unsubscribe` and
/// `disconnect` can hang indefinitely on a link that is already gone (same
/// `CoreBluetooth` shape as [`PRE_CONNECT_DISCONNECT_TIMEOUT`]), and teardown
/// runs on paths that are already failing — it must never be what blocks the
/// caller's error from surfacing.
const TEARDOWN_STEP_TIMEOUT: Duration = Duration::from_secs(2);

/// Refcount for the one radio scan on the shared adapter.
///
/// Neither backend refcounts discovery: a second `StartDiscovery` on `BlueZ`
/// draws `org.bluez.Error.InProgress`, and `CoreBluetooth`'s `stop_scan` stops
/// the radio globally — so a live [`CommissionableScan`] and a concurrent
/// [`BleCentral::find_device`] (an in-flight commission) would break each
/// other. All starts/stops route through [`acquire_scan`]/[`release_scan`],
/// which hold one async mutex across the btleplug call: the radio scan starts
/// on the 0→1 transition and stops on 1→0.
#[derive(Default)]
struct ScanRefCount {
    active: u32,
}

impl ScanRefCount {
    /// Register a scan user. `true` ⇒ this was 0→1: the caller must
    /// `start_scan` (and must call [`Self::release`] on start failure).
    fn acquire(&mut self) -> bool {
        self.active += 1;
        self.active == 1
    }

    /// Deregister a scan user. `true` ⇒ this was 1→0: the caller must
    /// `stop_scan`. An underflowing release (defensive double-drop) is a
    /// no-op that never requests a stop.
    fn release(&mut self) -> bool {
        if self.active == 0 {
            return false;
        }
        self.active -= 1;
        self.active == 0
    }
}

/// Drop-guard around the scan-refcount `MutexGuard`, so a cancelled
/// [`acquire_scan`] (the caller's future dropped mid-`start_scan`, e.g. a
/// timeout wrapped around [`BleCentral::scan_commissionables`] or
/// [`BleCentral::find_device`]) releases the slot it took instead of
/// stranding the count above zero with no scan user.
///
/// Race-free: `Drop::drop` runs before the wrapped `MutexGuard` field is
/// itself dropped, so [`ScanRefCount::release`] executes while the lock is
/// still held — a concurrent `acquire`/`release` can never interleave with
/// it. Call [`Self::defuse`] once `start_scan` has returned `Ok`; past that
/// point the increment is owned by the caller (released later via
/// `release_scan`) and the guard's drop must not touch it.
struct ScanGuard<'a> {
    guard: MutexGuard<'a, ScanRefCount>,
    defused: bool,
}

impl ScanGuard<'_> {
    /// Hand ownership of the increment to the caller: the guard's `Drop`
    /// becomes a no-op.
    fn defuse(mut self) {
        self.defused = true;
    }
}

impl Drop for ScanGuard<'_> {
    fn drop(&mut self) {
        if !self.defused {
            self.guard.release();
        }
    }
}

/// Take a scan slot, starting the radio scan if this is the first holder.
/// The lock is held across `start_scan` so a concurrent release cannot
/// interleave between the count change and the radio call.
///
/// Cancel-safe: if this future is dropped before it resolves, the
/// [`ScanGuard`] releases the slot it took rather than leaking a phantom
/// holder — without it, every later scan would see `count > 0` and skip
/// `start_scan` forever. Residual, best-effort case (same tier as the drop
/// caveat on [`CommissionableScan`]): if the btleplug backend's
/// `start_scan` actually completed at the same instant this future was
/// dropped, the radio may keep scanning with `count == 0` until a later
/// acquire/release cycle notices and corrects it.
///
/// # Errors
/// [`CentralError::Scan`] if the radio scan fails to start (the slot is
/// released).
async fn acquire_scan(
    scan: &Arc<TokioMutex<ScanRefCount>>,
    adapter: &Adapter,
) -> Result<(), CentralError> {
    let mut guard = ScanGuard {
        guard: scan.lock().await,
        defused: false,
    };
    if guard.guard.acquire() {
        // Unfiltered on purpose — a service-UUID ScanFilter blinds BlueZ. See
        // the doc comment on `find_device`. If this await is where the
        // caller's future gets dropped (see the doc comment above), `guard`
        // drops un-defused and releases the slot it took.
        adapter
            .start_scan(ScanFilter::default())
            .await
            .map_err(|e| CentralError::Scan(e.to_string()))?;
    }
    // Past this point the increment is owned by the caller (released via
    // `release_scan` when their scan ends) — defuse so the guard's drop
    // doesn't undo it.
    guard.defuse();
    Ok(())
}

/// Return a scan slot, stopping the radio scan (best-effort) if this was the
/// last holder. Held-lock stop serializes against any new `acquire_scan`, so
/// a delayed release can never kill a scan started after it.
async fn release_scan(scan: &Arc<TokioMutex<ScanRefCount>>, adapter: &Adapter) {
    let mut guard = scan.lock().await;
    if guard.release() {
        // Best-effort; we are done scanning either way.
        let _ = adapter.stop_scan().await;
    }
}

/// Errors from the BLE central and its BTP pump.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CentralError {
    /// No Bluetooth adapter, or the adapter is not powered on. The message
    /// points at the runbook; on macOS this is also how a denied Bluetooth
    /// permission surfaces (the adapter never reaches `PoweredOn`).
    #[error("bluetooth adapter unavailable ({0}); see docs/runbooks/ble-commissioning.md")]
    AdapterUnavailable(String),
    /// A scan operation failed.
    #[error("BLE scan failed: {0}")]
    Scan(String),
    /// No commissionable device matching the discriminator appeared before the
    /// scan timeout.
    #[error("no matching commissionable device found before timeout")]
    ScanTimeout,
    /// Connecting to the peripheral failed.
    #[error("BLE connect failed: {0}")]
    Connect(String),
    /// GATT service discovery did not complete (all attempts failed or timed
    /// out). Kept distinct from [`Self::Connect`]: a connect abort is often
    /// transient and worth one retry, while discovery exhaustion on macOS is
    /// the documented `CoreBluetooth` `uuidNotAllowed` stall, which a retry
    /// cannot recover (see `docs/runbooks/ble-commissioning.md`).
    #[error("GATT service discovery failed: {0}")]
    ServiceDiscovery(String),
    /// A required GATT characteristic (C1 or C2) was not found on the device.
    #[error("Matter BTP characteristic not found: {0}")]
    GattNotFound(&'static str),
    /// The BTP handshake failed (bad response, or the codec rejected it).
    #[error("BTP handshake failed: {0}")]
    Handshake(#[from] BtpError),
    /// No handshake response arrived before the timeout.
    #[error("BTP handshake response timed out")]
    HandshakeTimeout,
    /// The BLE link dropped.
    #[error("BLE peripheral disconnected")]
    Disconnected,
    /// A GATT write or subscription failed.
    #[error("BLE GATT operation failed: {0}")]
    Gatt(String),
}

/// A commissionable device located by [`BleCentral::find_device`] or yielded
/// by [`CommissionableScan`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FoundDevice {
    /// btleplug identifier for the peripheral (a `CoreBluetooth` UUID on macOS —
    /// never a hardware MAC).
    pub peripheral_id: PeripheralId,
    /// The parsed advertisement that matched.
    pub advert: CommissionableAdvert,
    /// BLE local name from the peripheral's cached advertisement properties,
    /// when one was observed. **Best-effort:** names usually arrive in a scan
    /// response *after* the first service-data advertisement, so early sightings
    /// of a device may carry `None` while later ones carry the name.
    pub local_name: Option<String>,
}

/// Does `advert` match the requested `discriminator`?
///
/// With `short == false` the full 12-bit discriminator must match exactly. With
/// `short == true` only the upper 4 bits are compared (the short discriminator
/// carried by manual pairing codes): the request supplies those 4 bits in its
/// low nibble.
#[must_use]
pub fn advert_matches(advert: &CommissionableAdvert, discriminator: u16, short: bool) -> bool {
    if short {
        (advert.discriminator >> 8) == (discriminator & 0x0f)
    } else {
        advert.discriminator == discriminator
    }
}

/// The one process-wide btleplug handle: the first adapter plus the scan
/// refcount every [`BleCentral`] shares.
///
/// **Why a singleton:** on Linux, every `btleplug::platform::Manager::new()`
/// opens a fresh D-Bus system-bus connection (`bluez-async`'s
/// `BluetoothSession::new`), whose connection-owning future is detached with
/// `tokio::spawn` — and btleplug discards the join handle, so nothing can ever
/// abort it. Dropping the manager/adapter/central does NOT close the
/// connection: each construction leaks one fd for the life of the process.
/// Constructing a central per operation therefore climbs monotonically toward
/// dbus-daemon's 256-connections-per-user cap and then breaks Bluetooth
/// **system-wide** — every process's BLE fails with "No buffer space
/// available" until this process exits (observed live on a Pi hub after ~45
/// minutes of periodic scans). `bluez-async` documents the intended model:
/// one session per process. Caching the adapter here gives exactly that —
/// every [`BleCentral::new`] after the first reuses the single session and
/// the fd count stays flat regardless of how consumers construct centrals.
///
/// The scan refcount lives here for the same reason: with one shared radio,
/// the start/stop-scan refcount must span every `BleCentral` in the process,
/// or two instances would each believe they own the radio scan.
///
/// Known trade-off: the adapter identity is cached for the process lifetime,
/// so hot-replacing the only Bluetooth adapter (e.g. re-plugging a USB
/// dongle) requires a process restart — the same guidance `bluez-async`
/// gives for a lost D-Bus connection.
struct SharedCore {
    adapter: Adapter,
    scan: Arc<TokioMutex<ScanRefCount>>,
}

/// See [`SharedCore`]. Success is cached for the process lifetime; a failed
/// init is retried on the next [`BleCentral::new`] (floor-tested in
/// `shared_core_once_cell_retries_after_error_and_caches_success`).
static SHARED_CORE: OnceCell<SharedCore> = OnceCell::const_new();

/// Initialize the process-wide [`SharedCore`]: one btleplug manager, its
/// first adapter. Runs at most once per process on success. The `PoweredOn`
/// check deliberately does NOT happen here — it lives in
/// [`BleCentral::new`] so it re-runs on every construction.
///
/// # Errors
/// [`CentralError::AdapterUnavailable`] when the manager cannot be created or
/// no adapter exists (including a denied macOS Bluetooth permission). The
/// error is not cached.
async fn init_shared_core() -> Result<SharedCore, CentralError> {
    let manager = Manager::new()
        .await
        .map_err(|e| CentralError::AdapterUnavailable(e.to_string()))?;
    let adapters = manager
        .adapters()
        .await
        .map_err(|e| CentralError::AdapterUnavailable(e.to_string()))?;
    let adapter = adapters
        .into_iter()
        .next()
        .ok_or_else(|| CentralError::AdapterUnavailable("no adapters".to_string()))?;
    Ok(SharedCore {
        adapter,
        scan: Arc::new(TokioMutex::new(ScanRefCount::default())),
    })
}

/// A BLE central bound to a specific adapter.
///
/// Construction is the macOS TCC trigger — see the module docs. All centrals
/// in a process share one underlying btleplug session and one scan refcount
/// (see the private `SharedCore` doc for why: btleplug's Linux backend leaks
/// one D-Bus connection per manager, so per-operation construction of
/// independent sessions eventually breaks BLE system-wide), so constructing
/// centrals per operation is cheap and leak-free.
pub struct BleCentral {
    adapter: Adapter,
    /// Scan refcount shared with every [`CommissionableScan`] AND every other
    /// [`BleCentral`] in the process (see [`SharedCore`]).
    scan: Arc<TokioMutex<ScanRefCount>>,
}

impl BleCentral {
    /// Acquire the process-wide shared adapter (first call creates it) and
    /// verify it is powered on.
    ///
    /// **The first call in a process instantiates `CoreBluetooth` and may
    /// raise the macOS Bluetooth permission prompt.** Call only from a
    /// user-initiated commissioning flow. Subsequent calls reuse the shared
    /// session — no new D-Bus connection on Linux, no new
    /// `CBCentralManager` on macOS — so per-operation construction stays
    /// cheap and the process's fd count stays flat.
    ///
    /// # Errors
    /// [`CentralError::AdapterUnavailable`] when no adapter exists or it is not
    /// `PoweredOn` (including a denied macOS Bluetooth permission). A failed
    /// first initialization is retried on the next call, not cached.
    pub async fn new() -> Result<Self, CentralError> {
        let core = SHARED_CORE.get_or_try_init(init_shared_core).await?;
        // Power state is re-checked on EVERY call — the cached adapter
        // outlives Bluetooth being toggled off or the permission being
        // revoked, and callers must still see those as `AdapterUnavailable`.
        match core.adapter.adapter_state().await {
            Ok(CentralState::PoweredOn) => Ok(Self {
                adapter: core.adapter.clone(),
                scan: Arc::clone(&core.scan),
            }),
            Ok(other) => Err(CentralError::AdapterUnavailable(format!("state {other:?}"))),
            Err(e) => Err(CentralError::AdapterUnavailable(e.to_string())),
        }
    }

    /// Scan for a commissionable Matter device whose advertisement matches
    /// `discriminator`, giving up after `timeout`.
    ///
    /// Service-data advertisements are consumed from the adapter event stream
    /// (never the stale per-peripheral properties). `short` selects short- vs
    /// long-discriminator matching (see [`advert_matches`]).
    ///
    /// The scan is deliberately **unfiltered**, with [`MATTER_SERVICE_UUID`]
    /// matched in-crate instead of being handed to btleplug as a
    /// [`ScanFilter`]. Passing that filter is not portable: `CoreBluetooth`
    /// honours it, but on `BlueZ` it suppresses the backend entirely — no
    /// service-data events and an empty `peripherals()` — so every scan found
    /// nothing on Linux while macOS worked. Filtering here costs only the
    /// discarded non-Matter adverts and behaves identically on both backends.
    ///
    /// This is now a thin, discriminator-filtered wrapper over
    /// [`Self::scan_commissionables`] — see that method to enumerate every
    /// nearby commissionable device instead of matching one. Name resolution
    /// now happens inside the scan stream itself, so each distinct nearby
    /// commissionable peripheral costs one best-effort properties lookup
    /// (cached per peripheral) before discriminator filtering, rather than
    /// only the one that ultimately matches.
    ///
    /// # Errors
    /// [`CentralError::Scan`] on a scan failure, [`CentralError::ScanTimeout`]
    /// when nothing matches in time.
    pub async fn find_device(
        &self,
        discriminator: u16,
        short: bool,
        timeout: Duration,
    ) -> Result<FoundDevice, CentralError> {
        let mut scan = self.scan_commissionables().await?;
        let found = tokio::time::timeout(timeout, async {
            while let Some(dev) = scan.next().await {
                if advert_matches(&dev.advert, discriminator, short) {
                    return Some(dev);
                }
            }
            None // adapter event stream ended
        })
        .await;
        scan.stop().await;
        match found {
            Ok(Some(dev)) => Ok(dev),
            Ok(None) | Err(_) => Err(CentralError::ScanTimeout),
        }
    }

    /// Start a continuous, **unfiltered** scan for commissionable Matter
    /// devices (see [`CommissionableScan`] for yield semantics and lifecycle).
    ///
    /// Unlike [`Self::find_device`] this surfaces *every* commissionable
    /// advert — no discriminator needed — so consumers can enumerate nearby
    /// devices without sweeping short-discriminator values, and slow
    /// advertisers (post-reset intervals up to ~1.2 s) appear whenever they
    /// advertise. Safe to hold while a commission runs: the radio scan is
    /// refcounted across all scan users on this central.
    ///
    /// The scan is deliberately unfiltered at the btleplug layer — a
    /// service-UUID `ScanFilter` blinds the `BlueZ` backend (see
    /// [`Self::find_device`]); [`MATTER_SERVICE_UUID`] is matched in-crate.
    ///
    /// # Errors
    /// [`CentralError::Scan`] if the adapter event stream or radio scan could
    /// not be started.
    pub async fn scan_commissionables(&self) -> Result<CommissionableScan, CentralError> {
        // Events BEFORE the scan starts, so no advert can slip between.
        let events = self
            .adapter
            .events()
            .await
            .map_err(|e| CentralError::Scan(e.to_string()))?;
        acquire_scan(&self.scan, &self.adapter).await?;
        Ok(CommissionableScan {
            adapter: self.adapter.clone(),
            events,
            scan: Arc::clone(&self.scan),
            names: NameCache::new(),
            released: false,
        })
    }

    /// Connect to `device`, open the Matter BTP GATT service, run the BTP
    /// handshake, and return a live [`BtpChannel`].
    ///
    /// **Ordering is protocol-critical: the C1 handshake request must be
    /// written before subscribing to C2.** chip's peripheral stashes its
    /// capabilities response and only indicates it upon receiving the
    /// subscription (`BLEEndPoint::HandleSubscribeReceived`), which additionally
    /// requires the endpoint to already be in `kState_Connecting` with a
    /// non-empty send queue — the state the capabilities request establishes.
    /// Subscribing first therefore makes the peripheral reject the subscribe as
    /// `CHIP_ERROR_INCORRECT_STATE` and leaves the queued response with no
    /// trigger: the device goes silent and the handshake times out. Real
    /// hardware is the only thing that catches this; a loopback peer that
    /// answers a request immediately accepts either order.
    ///
    /// The local notification stream is still opened *before* the write (it
    /// writes no CCCD and the peripheral cannot observe it), so the response
    /// cannot be lost to btleplug's bounded notification buffer.
    ///
    /// # Errors
    /// [`CentralError`] variants for connect, GATT discovery, subscription, or
    /// handshake failures.
    pub async fn open_btp(&self, device: &FoundDevice) -> Result<BtpChannel, CentralError> {
        pt("open_btp: peripheral()");
        let peripheral = self
            .adapter
            .peripheral(&device.peripheral_id)
            .await
            .map_err(|e| CentralError::Connect(e.to_string()))?;
        // Clear any stale system-level connection to this peripheral before
        // connecting. On macOS `CoreBluetooth` a connection left over from a
        // prior (crashed/killed) attempt survives at the daemon and keeps the
        // peripheral from re-advertising, so a best-effort disconnect first
        // forces a clean link. Bounded by a short timeout: on a not-connected
        // peripheral `disconnect()` itself never returns (it awaits a
        // `didDisconnect` that never fires), so we must not block on it.
        let _ = tokio::time::timeout(PRE_CONNECT_DISCONNECT_TIMEOUT, peripheral.disconnect()).await;
        pt("open_btp: connect START");
        peripheral
            .connect_with_timeout(CONNECT_TIMEOUT)
            .await
            .map_err(|e| CentralError::Connect(e.to_string()))?;
        pt("open_btp: connect DONE; discover_services START");

        // The link is UP from here, so every failure below must tear it down
        // before returning — otherwise the next attempt to this same BLE
        // address inherits a half-configured GATT state and cannot complete the
        // handshake (see [`teardown_link`] for the traced mechanism). `c2` is
        // threaded back out because teardown needs that handle to unsubscribe
        // and it only comes into existence partway through the body.
        let mut c2: Option<Characteristic> = None;
        match self.open_btp_connected(&peripheral, device, &mut c2).await {
            Ok(channel) => Ok(channel),
            Err(e) => {
                pt(&format!("open_btp: failed ({e}); tearing down link"));
                teardown_link(&peripheral, c2.as_ref()).await;
                Err(e)
            }
        }
    }

    /// The post-connect body of [`Self::open_btp`]: GATT discovery, the BTP
    /// handshake, and the pump spawn, over an already-connected `peripheral`.
    ///
    /// Split out purely so [`Self::open_btp`] can apply [`teardown_link`] to
    /// **every** failure with a single `match`, rather than eight hand-written
    /// cleanup arms that a later edit could forget to extend. `c2_out` is an
    /// out-parameter for the same reason: teardown must unsubscribe C2, and
    /// that handle does not exist until service discovery has run.
    async fn open_btp_connected(
        &self,
        peripheral: &Peripheral,
        device: &FoundDevice,
        c2_out: &mut Option<Characteristic>,
    ) -> Result<BtpChannel, CentralError> {
        discover_services_with_retry(peripheral).await?;
        pt("open_btp: discover_services DONE");

        let c1 = find_char(peripheral, C1_UUID).ok_or(CentralError::GattNotFound("C1"))?;
        let c2 = find_char(peripheral, C2_UUID).ok_or(CentralError::GattNotFound("C2"))?;
        *c2_out = Some(c2.clone());

        // Clear any C2 subscription `bluetoothd` restored during THIS reconnect,
        // before the handshake write goes out.
        //
        // This is not redundant with [`teardown_link`]. A previous attempt whose
        // link dropped before it could unsubscribe left the notify session
        // recorded and had no live ATT channel over which to clear it, so no
        // amount of teardown discipline on the losing side can help; the only
        // place the restored CCCD can be cleared is here, on the connection that
        // inherited it. On a genuinely fresh connection there is no notify
        // session and `BlueZ` puts nothing on the wire, so this costs one D-Bus
        // round trip and changes nothing the peripheral can observe.
        //
        // Ordering is the whole point: this must precede the C1 write so that
        // the device never sees a subscribe ahead of the handshake request (see
        // [`Self::open_btp`]'s doc comment for why chip rejects that).
        let _ = tokio::time::timeout(TEARDOWN_STEP_TIMEOUT, peripheral.unsubscribe(&c2)).await;
        pt("open_btp: cleared any restored C2 subscription");

        // Open the local notification stream before anything can be indicated,
        // so the response cannot be lost to btleplug's bounded buffer. This is
        // local only — it writes no CCCD and is invisible to the peripheral.
        let mut indications = peripheral
            .notifications()
            .await
            .map_err(|e| CentralError::Gatt(e.to_string()))?;
        pt("open_btp: notifications opened");

        // Handshake: request MTU 0 (unknown on macOS) + our receive window; the
        // device's response fixes the negotiated fragment size and window.
        let request = HandshakeRequest {
            mtu: 0,
            window: WINDOW_SIZE,
        };
        pt("open_btp: handshake C1 write START");
        tokio::time::timeout(
            C1_WRITE_TIMEOUT,
            peripheral.write(&c1, &request.encode(), WriteType::WithResponse),
        )
        .await
        .map_err(|_| CentralError::Gatt("C1 handshake write timed out".into()))?
        .map_err(|e| CentralError::Gatt(e.to_string()))?;
        pt("open_btp: handshake C1 write DONE");

        // Subscribe AFTER the request: the CCCD write is what makes the
        // peripheral emit the response (see [`Self::open_btp`]'s doc comment).
        peripheral
            .subscribe(&c2)
            .await
            .map_err(|e| CentralError::Gatt(e.to_string()))?;
        pt("open_btp: subscribed C2; awaiting handshake response");

        let resp_bytes = tokio::time::timeout(HANDSHAKE_TIMEOUT, next_c2(&mut indications))
            .await
            .map_err(|_| CentralError::HandshakeTimeout)?
            .ok_or(CentralError::Disconnected)?;
        pt("open_btp: handshake response received");
        let response = HandshakeResponse::parse(&resp_bytes)?;

        let now = Instant::now();
        let session = BtpSession::new(Role::Central, response.segment_size, response.window, now);

        // Adapter event stream, used by the pump to observe DeviceDisconnected
        // for this peripheral (the notification stream alone does not reliably
        // close on link loss).
        let events = self
            .adapter
            .events()
            .await
            .map_err(|e| CentralError::Gatt(e.to_string()))?;

        Ok(spawn_pump(
            peripheral.clone(),
            c1,
            c2,
            device.peripheral_id.clone(),
            indications,
            events,
            session,
        ))
    }
}

/// How long a nameless lookup result is trusted before the `BlueZ` properties
/// round trip is retried. Names usually arrive in a scan response shortly
/// after the first advertisement, so negatives must expire quickly — but a
/// nameless device advertising 2-10×/s must not cost a D-Bus round trip per
/// advertisement.
const NAMELESS_RETRY_TTL: Duration = Duration::from_secs(10);

/// One name-cache slot: a name, once seen, is kept for the scan's lifetime;
/// an empty lookup is suppressed only until its TTL passes.
enum NameSlot {
    Known(String),
    UnknownUntil(Instant),
}

/// What a scan should do about one advertisement's local name — the result of
/// [`NameCache::decide`].
enum NameLookup<'a> {
    /// Answered from the cache; no D-Bus traffic. `None` is a *nameless*
    /// result whose TTL has not expired yet, not "unknown".
    Cached(Option<&'a str>),
    /// Cache miss, or a negative entry that has expired: pay one `BlueZ`
    /// round trip and feed the result back via [`NameCache::record`].
    Query,
}

/// Local-name cache policy, sans-IO (`now` injected) and key-generic so the
/// TTL logic is unit-testable without a real `PeripheralId`.
struct NameCache<K> {
    slots: HashMap<K, NameSlot>,
}

impl<K: std::hash::Hash + Eq> NameCache<K> {
    fn new() -> Self {
        Self {
            slots: HashMap::new(),
        }
    }

    /// Whether this advertisement needs a `BlueZ` round trip, or can be
    /// answered from the cache. This is the whole decision the scan makes per
    /// advertisement — kept pure and separate from the I/O so the
    /// query-per-advertisement behaviour is testable without an adapter (see
    /// `nameless_device_queries_are_bounded_by_ttl_not_advert_rate`).
    fn decide(&self, id: &K, now: Instant) -> NameLookup<'_> {
        match self.slots.get(id) {
            Some(NameSlot::Known(name)) => NameLookup::Cached(Some(name.as_str())),
            Some(NameSlot::UnknownUntil(until)) if now < *until => NameLookup::Cached(None),
            _ => NameLookup::Query,
        }
    }

    fn record(&mut self, id: K, name: Option<String>, now: Instant) {
        let slot = match name {
            Some(n) => NameSlot::Known(n),
            None => NameSlot::UnknownUntil(now + NAMELESS_RETRY_TTL),
        };
        self.slots.insert(id, slot);
    }
}

/// Diagnostic scan tracing, enabled by setting `MATTER_BLE_SCAN_TRACE=1` —
/// one line per `BlueZ` D-Bus name query, for verifying the negative cache
/// bounds queries by TTL rather than advertisement rate. Same off-by-default,
/// side-effect-free shape as [`pump_trace_enabled`].
fn scan_trace(msg: &str) {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    let enabled = *ENABLED.get_or_init(|| {
        std::env::var("MATTER_BLE_SCAN_TRACE")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    });
    if enabled {
        eprintln!("[ble-scan] {msg}");
    }
}

/// A live, unfiltered scan for commissionable Matter devices, from
/// [`BleCentral::scan_commissionables`].
///
/// Yields a [`FoundDevice`] for **every** observed commissionable
/// advertisement — **no dedup and no discriminator filter**: the same device
/// yields again on every advertising interval (typically every 100–500 ms;
/// btleplug requests duplicate reports on both backends). Consumers own
/// windowing and dedup (key on [`FoundDevice::peripheral_id`]).
///
/// Owns its own adapter handle, so it is `'static` — hand it to a UI task.
/// Scanning stops when the last scan user goes away: on [`Self::stop`]
/// (preferred), or best-effort on drop (requires an ambient tokio runtime;
/// without one the radio scan persists until another scan user finishes).
/// The underlying adapter event stream is a bounded broadcast that drops on
/// lag; that is benign here because advertisements repeat.
pub struct CommissionableScan {
    adapter: Adapter,
    events: Pin<Box<dyn Stream<Item = CentralEvent> + Send>>,
    scan: Arc<TokioMutex<ScanRefCount>>,
    /// Cache of observed local names, so repeated adverts from the same
    /// device don't re-pay the `BlueZ` D-Bus properties round trip. Only
    /// names are kept permanently; empty results are retried after
    /// [`NAMELESS_RETRY_TTL`].
    names: NameCache<PeripheralId>,
    /// Set by [`Self::stop`] so `Drop` doesn't release the scan slot twice.
    released: bool,
}

impl CommissionableScan {
    /// Next observed commissionable advertisement, or `None` if the adapter
    /// event stream ends (adapter gone).
    ///
    /// `local_name` is filled best-effort from cached peripheral properties —
    /// see the field docs on [`FoundDevice::local_name`].
    ///
    /// Cancel-safe in effect: a dropped `next()` may lose one advertisement,
    /// which the device re-sends on its next advertising interval.
    pub async fn next(&mut self) -> Option<FoundDevice> {
        while let Some(event) = self.events.next().await {
            let CentralEvent::ServiceDataAdvertisement { id, service_data } = event else {
                continue;
            };
            let Some(advert) = commissionable_from_service_data(&service_data) else {
                continue;
            };
            let now = Instant::now();
            let local_name = match self.names.decide(&id, now) {
                NameLookup::Cached(cached) => cached.map(str::to_owned),
                NameLookup::Query => {
                    scan_trace("name query");
                    let name = local_name_for(&self.adapter, &id).await;
                    self.names.record(id.clone(), name.clone(), now);
                    name
                }
            };
            return Some(FoundDevice {
                peripheral_id: id,
                advert,
                local_name,
            });
        }
        None
    }

    /// Stop this scan user, stopping the radio scan if it was the last one.
    /// Preferred over drop: deterministic, and needs no ambient runtime.
    ///
    /// Not cancel-safe — dropping this future after it is first polled can
    /// leak the scan slot (the drop-release is already defused); poll it to
    /// completion, or drop the scan itself instead to use the best-effort
    /// drop path.
    pub async fn stop(mut self) {
        self.released = true;
        release_scan(&self.scan, &self.adapter).await;
    }
}

impl Drop for CommissionableScan {
    /// Best-effort release: spawns the (async) scan-slot release when a tokio
    /// runtime is ambient. The release re-checks the refcount under the lock,
    /// so a delayed drop can never kill a scan started after it.
    fn drop(&mut self) {
        if self.released {
            return;
        }
        if let Ok(handle) = Handle::try_current() {
            let scan = Arc::clone(&self.scan);
            let adapter = self.adapter.clone();
            handle.spawn(async move {
                release_scan(&scan, &adapter).await;
            });
        }
        // No runtime handle: the slot leaks until another scan user's release
        // drives the count to zero. Documented on the type; `stop()` is the
        // clean path.
    }
}

/// Parse a service-data map's Matter-UUID entry into a commissionable
/// advertisement. Pure over its inputs; no discriminator filtering.
fn commissionable_from_service_data(
    service_data: &HashMap<Uuid, Vec<u8>>,
) -> Option<CommissionableAdvert> {
    let raw = service_data.get(&MATTER_SERVICE_UUID)?;
    CommissionableAdvert::parse(raw).ok()
}

/// Find a characteristic by UUID among the peripheral's discovered set.
fn find_char(peripheral: &Peripheral, uuid: Uuid) -> Option<Characteristic> {
    peripheral
        .characteristics()
        .into_iter()
        .find(|c| c.uuid == uuid)
}

/// Best-effort BLE local name for `id` from btleplug's cached advertisement
/// properties. Cheap: a map lookup on `CoreBluetooth`, one bounded D-Bus
/// round trip on `BlueZ`. Any error (e.g. the device was evicted from the
/// cache) is swallowed to `None` — the name is cosmetic.
async fn local_name_for(adapter: &Adapter, id: &PeripheralId) -> Option<String> {
    let peripheral = adapter.peripheral(id).await.ok()?;
    peripheral.properties().await.ok().flatten()?.local_name
}

/// Await the next C2 indication payload from the notification stream, skipping
/// notifications for other characteristics. `None` when the stream ends.
async fn next_c2<S>(indications: &mut S) -> Option<Vec<u8>>
where
    S: Stream<Item = btleplug::api::ValueNotification> + Unpin,
{
    while let Some(note) = indications.next().await {
        if note.uuid == C2_UUID {
            return Some(note.value);
        }
    }
    None
}

/// A command sent to the pump task.
///
/// Exposed `#[doc(hidden)]` so out-of-crate adapters (e.g. matter-controller's
/// `BtpDatagram`) can unit-test against a [`BtpChannel::from_channels`] seam
/// without a live peripheral. Not part of the stable public API.
#[doc(hidden)]
pub enum PumpCommand {
    /// Queue a whole Matter message for segmentation; the pump acks once it is
    /// accepted by the BTP session.
    Send(Vec<u8>, oneshot::Sender<Result<(), CentralError>>),
    /// Close the channel and stop the pump.
    Close,
}

/// A live BTP session over BLE: send and receive whole Matter messages.
///
/// `recv` is single-consumer. Both [`Self::close`] and dropping the channel
/// stop the pump and tear the GATT link down with it — dropping the C2
/// subscription is not optional cleanup, it is what lets the *next* connection
/// to this device succeed: `BlueZ` restores a leftover `StartNotify` session on
/// reconnect, ahead of service discovery, which breaks the handshake ordering
/// chip requires. `close` awaits that teardown; `Drop` can only set it in
/// motion.
pub struct BtpChannel {
    commands: mpsc::Sender<PumpCommand>,
    inbound: mpsc::Receiver<Result<Vec<u8>, CentralError>>,
    pump: Option<JoinHandle<()>>,
}

impl BtpChannel {
    /// Send one whole Matter message. It is segmented into BTP fragments by the
    /// pump.
    ///
    /// # Errors
    /// [`CentralError::Disconnected`] if the pump has stopped; the underlying
    /// [`CentralError`] if the session rejected the message.
    pub async fn send(&self, message: &[u8]) -> Result<(), CentralError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.commands
            .send(PumpCommand::Send(message.to_vec(), ack_tx))
            .await
            .map_err(|_| CentralError::Disconnected)?;
        ack_rx.await.map_err(|_| CentralError::Disconnected)?
    }

    /// Await the next whole Matter message reassembled from the device.
    ///
    /// # Errors
    /// [`CentralError::Disconnected`] when the link drops or the pump stops.
    pub async fn recv(&mut self) -> Result<Vec<u8>, CentralError> {
        match self.inbound.recv().await {
            Some(result) => result,
            None => Err(CentralError::Disconnected),
        }
    }

    /// Build a `BtpChannel` directly from raw channel halves, with **no pump
    /// task** (`pump: None`).
    ///
    /// This is a `#[doc(hidden)]` test seam: it lets code in other crates
    /// unit-test adapters layered over a `BtpChannel` (matter-controller's
    /// `BtpDatagram`) by driving the command and inbound sides by hand — no
    /// live BLE peripheral, no `BleCentral`/`Manager`/`Adapter`. Not part of
    /// the stable public API.
    ///
    /// The caller owns the peer halves: the receiver of `commands` observes
    /// each [`PumpCommand`] produced by [`Self::send`] (and must reply on the
    /// embedded ack channel for `send` to resolve), and the sender of `inbound`
    /// feeds [`Self::recv`].
    #[doc(hidden)]
    #[must_use]
    pub fn from_channels(
        commands: mpsc::Sender<PumpCommand>,
        inbound: mpsc::Receiver<Result<Vec<u8>, CentralError>>,
    ) -> Self {
        Self {
            commands,
            inbound,
            pump: None,
        }
    }

    /// Stop the pump, dropping the C2 subscription and the GATT link with it.
    /// Idempotent, and awaits the pump so the teardown has actually run by the
    /// time this returns — which [`Drop`] cannot promise. Prefer it wherever
    /// there is an `async` context to call it from.
    pub async fn close(mut self) {
        let _ = self.commands.send(PumpCommand::Close).await;
        if let Some(handle) = self.pump.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for BtpChannel {
    /// Ask the pump to close and **detach** it, rather than aborting it.
    ///
    /// `abort()` cancels the pump wherever it is parked in its `select!`, so
    /// the teardown in its close arm never runs and the C2 subscription is left
    /// recorded in `BlueZ` — which is precisely what breaks the next connection
    /// to this address, because `BlueZ` restores that subscription on reconnect
    /// ahead of service discovery. Sending `Close` instead lets the pump
    /// unsubscribe and disconnect on its own.
    ///
    /// `try_send` because `Drop` cannot await: the command channel has capacity
    /// 8 and the pump drains it, so a full queue here means a pump that is
    /// already wedged and would not have honoured an abort-free close either.
    /// Dropping the `JoinHandle` detaches the task; its own teardown steps are
    /// individually timeout-bounded, so it cannot linger indefinitely.
    fn drop(&mut self) {
        let _ = self.commands.try_send(PumpCommand::Close);
        drop(self.pump.take());
    }
}

/// Spawn the pump task that owns the [`BtpSession`] and bridges it to the GATT
/// characteristics. Returns the [`BtpChannel`] wired to it.
#[allow(clippy::too_many_arguments)] // Pump wiring: all inputs are load-bearing.
fn spawn_pump(
    peripheral: Peripheral,
    c1: Characteristic,
    c2: Characteristic,
    peripheral_id: PeripheralId,
    indications: impl Stream<Item = btleplug::api::ValueNotification> + Unpin + Send + 'static,
    events: impl Stream<Item = CentralEvent> + Unpin + Send + 'static,
    session: BtpSession,
) -> BtpChannel {
    let (cmd_tx, cmd_rx) = mpsc::channel::<PumpCommand>(8);
    let (sdu_tx, sdu_rx) = mpsc::channel::<Result<Vec<u8>, CentralError>>(8);
    let pump = tokio::spawn(pump_loop(
        peripheral,
        c1,
        c2,
        peripheral_id,
        Box::pin(indications),
        Box::pin(events),
        session,
        cmd_rx,
        sdu_tx,
    ));
    BtpChannel {
        commands: cmd_tx,
        inbound: sdu_rx,
        pump: Some(pump),
    }
}

/// Drop the C2 subscription and the GATT link, best effort.
///
/// **The `unsubscribe` is the load-bearing half, and it must happen while the
/// link is still up.** `BlueZ` remembers a characteristic's `StartNotify`
/// session per bonded/known address and *re-enables it during reconnect*, ahead
/// of service discovery — so a subscription left behind by one attempt is
/// restored on the next connection to the same BLE address, and `btleplug`'s
/// later explicit `subscribe` becomes a no-op (no second CCCD write). That
/// produces exactly the subscribe-before-write ordering
/// [`BleCentral::open_btp`]'s doc comment explains chip rejects, and the
/// handshake then times out with both sides waiting on the other. Traced on
/// hardware with `btmon` (Pi 5 → ESP32-C6, 2026-09-04): on the second
/// connection the CCCD write landed at 17.014 s, *before* service discovery at
/// 17.209 s, and the device logged `CHIPoBLE subscribe received` ahead of
/// `Write request received for CHIPoBLE RX`.
///
/// `disconnect` afterwards is hygiene: it stops a failed attempt from holding a
/// system-level connection that keeps the peripheral from re-advertising.
///
/// Both steps are bounded by [`TEARDOWN_STEP_TIMEOUT`] and their results
/// discarded — this runs on paths that are already failing, and a peripheral
/// whose link has *already* dropped cannot be cleaned up at all (there is no
/// live ATT channel to write the CCCD over). That residual case is why
/// [`BleCentral::open_btp`] also clears the CCCD defensively before its
/// handshake write rather than trusting teardown to have run.
async fn teardown_link(peripheral: &Peripheral, c2: Option<&Characteristic>) {
    if let Some(c2) = c2 {
        let _ = tokio::time::timeout(TEARDOWN_STEP_TIMEOUT, peripheral.unsubscribe(c2)).await;
    }
    let _ = tokio::time::timeout(TEARDOWN_STEP_TIMEOUT, peripheral.disconnect()).await;
}

/// Discover the peripheral's GATT services, bounding the macOS
/// `CoreBluetooth` hang where `discover_services()` never returns.
///
/// Root cause (verified against a real ESP32-C6 on macOS 26): `btleplug` 0.12.0
/// only fulfills the discovery future after every characteristic's *descriptor*
/// discovery reports back, but its delegate drops that event when the probe
/// errors. `CHIPoBLE`'s C1/C2 answer descriptor discovery with
/// `CBError.uuidNotAllowed`, so the future never completes. A per-attempt bound
/// plus a small retry budget ([`SERVICE_DISCOVERY_ATTEMPTS`]) turns the infinite
/// hang into a fast, diagnosable failure. Full macOS BLE commissioning is still
/// blocked upstream (the same `uuidNotAllowed` also rejects the C1 write); this
/// only ensures we fail cleanly rather than hang.
async fn discover_services_with_retry(peripheral: &Peripheral) -> Result<(), CentralError> {
    let mut last: Option<String> = None;
    for attempt in 1..=SERVICE_DISCOVERY_ATTEMPTS {
        pt(&format!("discover_services attempt {attempt}"));
        match tokio::time::timeout(SERVICE_DISCOVERY_TIMEOUT, peripheral.discover_services()).await
        {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(e)) => last = Some(e.to_string()),
            Err(_) => last = Some("timed out (no reply from CoreBluetooth)".into()),
        }
        // Let a still-in-progress service modification settle before re-issuing;
        // the next `discover_services` replaces the stale in-flight discovery.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    Err(CentralError::ServiceDiscovery(format!(
        "GATT service discovery did not complete after {SERVICE_DISCOVERY_ATTEMPTS} attempts: {}",
        last.unwrap_or_else(|| "unknown".into())
    )))
}

/// Write one fragment to C1 and release the one-op-in-flight slot.
///
/// Every C1 write — data fragment OR standalone ack — latches the session's
/// GATT-in-flight flag; `gatt_write_completed` MUST be called after the write
/// resolves or the session can never send again (a real deadlock caught by the
/// PASE-over-BTP floor test).
async fn write_fragment(
    peripheral: &Peripheral,
    c1: &Characteristic,
    session: &mut BtpSession,
    fragment: &[u8],
) -> Result<(), CentralError> {
    // The tell-tale for the macOS half-duplex-deadlock hypothesis: a `START`
    // with no matching `DONE` means the pump is blocked in this `WithResponse`
    // write `await` (which runs outside the `select!`), so it has stopped
    // reading C2 indications. See [`pump_trace_enabled`].
    pt(&format!("C1 write START ({} bytes)", fragment.len()));
    peripheral
        .write(c1, fragment, WriteType::WithResponse)
        .await
        .map_err(|e| CentralError::Gatt(e.to_string()))?;
    pt("C1 write DONE");
    session.gatt_write_completed(Instant::now());
    Ok(())
}

/// What the pump should wait on after draining outbound work.
enum Wake {
    /// A future timer deadline (deferred send-ack, or the 15 s ack-timeout).
    At(Instant),
    /// A due action is blocked on a closed window / in-flight GATT slot. Park on
    /// IO but re-check after a bounded delay so the blocked ack is retried and
    /// the 15 s ack-timeout still fires — without hot-spinning on a past-due
    /// deadline (`sleep_until(past)` returns instantly).
    Retry,
    /// Nothing is pending; block on IO only.
    Idle,
}

/// Bounded fallback re-check delay for a blocked send-ack (see [`Wake::Retry`]).
/// Normal operation unblocks far sooner via an inbound indication crediting the
/// window; this only bounds the pathological silent-window case.
const BLOCKED_ACK_RETRY: Duration = Duration::from_millis(250);

/// Flush every fragment the session currently permits, then emit any
/// immediately-due standalone ack. Returns what the pump should wait on next.
async fn drive_outbound(
    peripheral: &Peripheral,
    c1: &Characteristic,
    session: &mut BtpSession,
) -> Result<Wake, CentralError> {
    while let Some(fragment) = session.next_gatt_write(Instant::now()) {
        write_fragment(peripheral, c1, session, &fragment).await?;
    }
    loop {
        match session.poll_timeout() {
            None => return Ok(Wake::Idle),
            Some(due) if due > Instant::now() => return Ok(Wake::At(due)),
            Some(_) => match session.handle_timeout(Instant::now())? {
                Some(ack) => write_fragment(peripheral, c1, session, &ack).await?,
                // Emission is deferred (blocked on window / GATT slot): do not
                // spin on the past-due deadline — bounded-retry and park on IO.
                None => return Ok(Wake::Retry),
            },
        }
    }
}

/// The pump: owns the [`BtpSession`], services outbound fragments/acks, feeds
/// inbound C2 indications, surfaces completed messages, and closes on a
/// `DeviceDisconnected` event for this peripheral.
#[allow(clippy::too_many_arguments)] // Pump wiring: all inputs are load-bearing.
async fn pump_loop(
    peripheral: Peripheral,
    c1: Characteristic,
    c2: Characteristic,
    peripheral_id: PeripheralId,
    mut indications: std::pin::Pin<Box<dyn Stream<Item = btleplug::api::ValueNotification> + Send>>,
    mut events: std::pin::Pin<Box<dyn Stream<Item = CentralEvent> + Send>>,
    mut session: BtpSession,
    mut commands: mpsc::Receiver<PumpCommand>,
    sdu_tx: mpsc::Sender<Result<Vec<u8>, CentralError>>,
) {
    loop {
        let wake = match drive_outbound(&peripheral, &c1, &mut session).await {
            Ok(wake) => wake,
            Err(e) => {
                let _ = sdu_tx.send(Err(e)).await;
                teardown_link(&peripheral, Some(&c2)).await;
                return;
            }
        };

        let timer = async {
            match wake {
                Wake::At(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                Wake::Retry => tokio::time::sleep(BLOCKED_ACK_RETRY).await,
                Wake::Idle => std::future::pending::<()>().await,
            }
        };

        tokio::select! {
            command = commands.recv() => match command {
                Some(PumpCommand::Send(message, ack)) => {
                    let _ = ack.send(session.queue_message(&message).map_err(CentralError::from));
                }
                Some(PumpCommand::Close) | None => {
                    // Graceful close: drop the C2 subscription and the link
                    // (best-effort — the caller is done regardless).
                    teardown_link(&peripheral, Some(&c2)).await;
                    return;
                }
            },
            note = indications.next() => match note {
                Some(note) if note.uuid == C2_UUID => {
                    pt(&format!("C2 indication ({} bytes)", note.value.len()));
                    match session.on_indication(&note.value, Instant::now()) {
                        Ok(Some(sdu)) => {
                            pt(&format!("SDU surfaced ({} bytes)", sdu.len()));
                            if sdu_tx.send(Ok(sdu)).await.is_err() {
                                teardown_link(&peripheral, Some(&c2)).await;
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            let _ = sdu_tx.send(Err(CentralError::from(e))).await;
                            teardown_link(&peripheral, Some(&c2)).await;
                            return;
                        }
                    }
                }
                Some(_) => {}
                None => {
                    let _ = sdu_tx.send(Err(CentralError::Disconnected)).await;
                    teardown_link(&peripheral, Some(&c2)).await;
                    return;
                }
            },
            event = events.next() => match event {
                Some(CentralEvent::DeviceDisconnected(id)) if id == peripheral_id => {
                    let _ = sdu_tx.send(Err(CentralError::Disconnected)).await;
                    teardown_link(&peripheral, Some(&c2)).await;
                    return;
                }
                // Other events (and other peripherals) are irrelevant here; a
                // closed event stream is not itself fatal (the notification
                // stream / ack-timeout still detect a dead link).
                Some(_) | None => {}
            },
            () = timer => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;
    use crate::advert::CommissionableAdvert;

    fn advert(discriminator: u16) -> CommissionableAdvert {
        CommissionableAdvert {
            discriminator,
            adv_version: 0,
            vendor_id: 0xFFF1,
            product_id: 0x8000,
            has_additional_data: false,
            extended_announcement: false,
        }
    }

    #[test]
    fn long_discriminator_matches_exactly() {
        // Vector test-vectors/btp/advert.json "pi_default_disc": disc 0xF00.
        assert!(advert_matches(&advert(0xF00), 0xF00, false));
        assert!(!advert_matches(&advert(0xF00), 0xF01, false));
        assert!(!advert_matches(&advert(0xABC), 0xF00, false));
    }

    #[test]
    fn short_discriminator_matches_upper_nibble() {
        // Long 0xF00 has upper nibble 0xF; a short request of 0xF matches.
        assert!(advert_matches(&advert(0xF00), 0x0F, true));
        assert!(advert_matches(&advert(0xABC), 0x0A, true));
        assert!(!advert_matches(&advert(0xABC), 0x0B, true));
    }

    #[test]
    fn service_uuid_stringifies_to_spec_value() {
        assert_eq!(
            MATTER_SERVICE_UUID.to_string(),
            "0000fff6-0000-1000-8000-00805f9b34fb"
        );
    }

    #[test]
    fn characteristic_uuids_stringify_to_spec_values() {
        assert_eq!(C1_UUID.to_string(), "18ee2ef5-263d-4559-959f-4f9c429f9d11");
        assert_eq!(C2_UUID.to_string(), "18ee2ef5-263d-4559-959f-4f9c429f9d12");
    }

    #[test]
    fn scan_refcount_first_acquire_starts_last_release_stops() {
        let mut rc = ScanRefCount::default();
        assert!(rc.acquire(), "0->1 must start the radio scan");
        assert!(!rc.acquire(), "1->2 must not restart");
        assert!(!rc.release(), "2->1 must not stop");
        assert!(rc.release(), "1->0 must stop the radio scan");
    }

    /// Dropping a `BtpChannel` must ASK the pump to close rather than abort it.
    ///
    /// Regression guard: `Drop` used to call `JoinHandle::abort()`, which
    /// cancels the pump wherever it is parked and so skips the `unsubscribe` +
    /// `disconnect` in its close arm. That left the C2 subscription recorded in
    /// `BlueZ`, which restores it on the next connection to the same address —
    /// ahead of service discovery — producing the subscribe-before-write
    /// ordering chip rejects, and every retry then fails at the BTP handshake.
    #[tokio::test]
    async fn dropping_the_channel_asks_the_pump_to_close() {
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<PumpCommand>(8);
        let (_sdu_tx, sdu_rx) = mpsc::channel::<Result<Vec<u8>, CentralError>>(8);

        drop(BtpChannel::from_channels(cmd_tx, sdu_rx));

        match cmd_rx.try_recv() {
            Ok(PumpCommand::Close) => {}
            other => panic!(
                "drop must enqueue PumpCommand::Close, got {:?}",
                other.is_ok()
            ),
        }
    }

    #[test]
    fn scan_refcount_release_without_acquire_never_stops() {
        // A defensive underflow (double drop) must not issue a spurious stop_scan
        // that would kill an unrelated live scan.
        let mut rc = ScanRefCount::default();
        assert!(!rc.release());
        assert!(rc.acquire(), "recovers to normal operation");
        assert!(rc.release());
    }

    /// Floor test for the `tokio::sync::OnceCell` semantics
    /// [`BleCentral::new`]'s process-wide shared core relies on:
    /// `get_or_try_init` must cache success (the initializer runs exactly once
    /// across all later calls) and must NOT cache failure — a transient
    /// "no adapter" / denied-permission error on the first `BleCentral::new`
    /// must not poison every future one.
    #[tokio::test]
    async fn shared_core_once_cell_retries_after_error_and_caches_success() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let cell: OnceCell<u32> = OnceCell::const_new();
        let attempts = AtomicU32::new(0);

        let failed = cell
            .get_or_try_init(|| async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err::<u32, &str>("no adapter yet")
            })
            .await;
        assert!(failed.is_err(), "first init fails");

        let ok = cell
            .get_or_try_init(|| async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok::<u32, &str>(7)
            })
            .await;
        assert_eq!(ok, Ok(&7), "an errored init must be retried, not cached");

        let cached = cell
            .get_or_try_init(|| async {
                attempts.fetch_add(1, Ordering::SeqCst);
                Ok::<u32, &str>(9)
            })
            .await;
        assert_eq!(cached, Ok(&7), "success must be cached");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "initializer must not run again after a success"
        );
    }

    #[tokio::test]
    async fn scan_guard_releases_on_drop_unless_defused() {
        // This is the acquire-side cancellation fix: an un-defused ScanGuard
        // must release the slot it took (the "caller's future got dropped
        // mid-start_scan" case), while a defused one must leave the count
        // untouched (the normal "start_scan succeeded" case). Exercised here
        // directly against ScanRefCount/TokioMutex — no Adapter needed, since
        // ScanGuard's Drop only ever touches the MutexGuard it wraps.
        let scan: Arc<TokioMutex<ScanRefCount>> =
            Arc::new(TokioMutex::new(ScanRefCount::default()));

        // Cancellation case: acquire, then drop the guard without defusing —
        // the slot must come back to 0.
        {
            let mut inner = scan.lock().await;
            assert!(inner.acquire(), "0->1");
            let guard = ScanGuard {
                guard: inner,
                defused: false,
            };
            drop(guard);
        }
        assert_eq!(
            scan.lock().await.active,
            0,
            "un-defused drop must release the slot (the cancellation-hole fix)"
        );

        // Success case: acquire, defuse, drop — the slot must stay held for
        // the caller to release later via release_scan.
        {
            let mut inner = scan.lock().await;
            assert!(inner.acquire(), "0->1");
            let guard = ScanGuard {
                guard: inner,
                defused: false,
            };
            guard.defuse();
        }
        assert_eq!(
            scan.lock().await.active,
            1,
            "defused drop must not release the slot"
        );
    }

    #[test]
    fn commissionable_from_service_data_parses_matter_uuid_only() {
        // Vector test-vectors/btp/advert.json "pi_default_disc": disc 0xF00.
        let payload = vec![0x00, 0x00, 0x0F, 0xF1, 0xFF, 0x00, 0x80, 0x00];
        let mut data = HashMap::new();
        data.insert(MATTER_SERVICE_UUID, payload.clone());
        let advert = commissionable_from_service_data(&data).unwrap();
        assert_eq!(advert.discriminator, 0xF00);

        // A non-Matter UUID must not match.
        let mut other = HashMap::new();
        other.insert(C1_UUID, payload);
        assert!(commissionable_from_service_data(&other).is_none());

        // Garbage service data for the Matter UUID must not match.
        let mut garbled = HashMap::new();
        garbled.insert(MATTER_SERVICE_UUID, vec![0xFF]);
        assert!(commissionable_from_service_data(&garbled).is_none());
    }

    #[test]
    fn found_device_has_optional_local_name() {
        // Compile-shape test: `PeripheralId` has no portable constructor, so we
        // assert the new field's existence and type via a never-called closure.
        // Population is best-effort at yield time (service-data events carry no
        // name); construction sites must default it to `None`.
        let shape = |peripheral_id: PeripheralId, advert: CommissionableAdvert| FoundDevice {
            peripheral_id,
            advert,
            local_name: None::<String>,
        };
        let _ = shape;
    }

    #[test]
    fn service_discovery_error_is_distinct_from_connect() {
        // The controller's single connect retry keys on `Connect`; discovery exhaustion
        // (the macOS uuidNotAllowed stall) must NOT be that variant, or the retry
        // doubles a ~25 s known-hopeless failure.
        let e = CentralError::ServiceDiscovery("2 attempts".into());
        assert!(matches!(e, CentralError::ServiceDiscovery(_)));
        assert!(e.to_string().contains("service discovery"));
    }

    #[test]
    fn name_cache_negative_ttl_policy() {
        // Sans-IO: `now` injected, key generic (u32) — no Adapter needed.
        let t0 = Instant::now();
        let mut cache: NameCache<u32> = NameCache::new();

        // Unknown key: caller must query.
        assert!(matches!(cache.decide(&7, t0), NameLookup::Query));

        // Negative result: suppressed until the TTL passes…
        cache.record(7, None, t0);
        assert!(matches!(cache.decide(&7, t0), NameLookup::Cached(None)));
        assert!(matches!(
            cache.decide(
                &7,
                (t0 + NAMELESS_RETRY_TTL)
                    .checked_sub(Duration::from_millis(1))
                    .unwrap()
            ),
            NameLookup::Cached(None)
        ));
        // …then re-queried (names often arrive in a later scan response).
        assert!(matches!(
            cache.decide(&7, t0 + NAMELESS_RETRY_TTL),
            NameLookup::Query
        ));

        // A late name upgrades the entry permanently.
        cache.record(7, Some("Onvis".into()), t0 + NAMELESS_RETRY_TTL);
        assert!(matches!(
            cache.decide(&7, t0 + Duration::from_secs(3600)),
            NameLookup::Cached(Some("Onvis"))
        ));
    }

    /// Drive `decide`/`record` exactly as `CommissionableScan::next` drives
    /// them, over a simulated advertisement stream, and count the D-Bus
    /// queries that would result. This is the property the negative cache
    /// exists for, and the reason it is worth its complexity: query cost is
    /// bounded by [`NAMELESS_RETRY_TTL`], not by how fast the device
    /// advertises. Without the negative entry every one of these 600
    /// advertisements pays a `BlueZ` round trip.
    #[test]
    fn nameless_device_queries_are_bounded_by_ttl_not_advert_rate() {
        let t0 = Instant::now();
        let mut cache: NameCache<u32> = NameCache::new();
        let mut queries = 0u32;
        let mut adverts = 0u32;

        // A nameless device advertising 10×/s for 60 s.
        for tick in 0..600u32 {
            let now = t0 + Duration::from_millis(u64::from(tick) * 100);
            adverts += 1;
            match cache.decide(&7, now) {
                NameLookup::Cached(_) => {}
                NameLookup::Query => {
                    queries += 1;
                    // The device stays nameless, so the query comes back empty.
                    cache.record(7, None, now);
                }
            }
        }

        assert_eq!(adverts, 600);
        // One at t=0, then one at each 10 s expiry (t=10,20,30,40,50). The
        // final tick is t=59.9 s, still inside the window opened at t=50 s.
        assert_eq!(
            queries, 6,
            "one query per {NAMELESS_RETRY_TTL:?} window, not one per advert"
        );
    }

    /// The counterpart, and a caveat worth pinning: a device that *does*
    /// advertise a name is cached positively on its first query and never
    /// re-queried, so it never exercises the negative path at all. A hardware
    /// check aimed at the negative cache therefore needs a NAMELESS
    /// advertiser to be meaningful.
    #[test]
    fn named_device_is_queried_once_for_the_whole_scan() {
        let t0 = Instant::now();
        let mut cache: NameCache<u32> = NameCache::new();
        let mut queries = 0u32;

        for tick in 0..600u32 {
            let now = t0 + Duration::from_millis(u64::from(tick) * 100);
            if matches!(cache.decide(&7, now), NameLookup::Query) {
                queries += 1;
                cache.record(7, Some("MATTER-3840".into()), now);
            }
        }

        assert_eq!(queries, 1, "a named device is queried once, then cached");
    }
}
