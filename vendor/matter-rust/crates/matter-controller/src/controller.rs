//! `MatterController` — the public entry point. A cheap, cloneable handle
//! over the owning actor task (a crate-internal `tokio` task).

use std::sync::Arc;

use matter_commissioning::driver::AsyncDatagram;
use matter_commissioning::{NocRng, SystemNocRng};
use matter_transport::Discovery;
use tokio::sync::{mpsc, oneshot};

use crate::actor::{Actor, Command};
use crate::builder::MatterControllerBuilder;
use crate::error::Error;
use crate::fabric::FabricConfig;
use crate::node::Node;
use crate::node_info::NodeInfo;
use crate::snapshot;
use crate::state::ControllerState;
use crate::store::ControllerStore;
use crate::trust::AttestationTrust;

/// `BasicInformation` cluster id (Matter §11.1) — read post-commission for the
/// device's `VendorID`/`ProductID`. Sourced from the generated cluster
/// definitions so it stays tied to the codegen source of truth.
const BASIC_INFORMATION_CLUSTER: u32 = matter_clusters::gen::basic_information::CLUSTER_ID;
/// `BasicInformation.VendorID` attribute id.
const BASIC_INFO_ATTR_VENDOR_ID: u32 =
    matter_clusters::gen::basic_information::attribute_id::VENDOR_ID;
/// `BasicInformation.ProductID` attribute id.
const BASIC_INFO_ATTR_PRODUCT_ID: u32 =
    matter_clusters::gen::basic_information::attribute_id::PRODUCT_ID;

const COMMAND_CHANNEL_DEPTH: usize = 32;

/// Addresses to advertise for a self-hosted operational service (OTA provider,
/// ICD check-in listener). A wildcard bind (`[::]`) reports an unspecified
/// `local_addr` that a peer cannot resolve to anything routable, so substitute
/// the host's real routable address(es); fall back to the bind address only if
/// none can be found (e.g. fully offline).
fn advertise_addrs(local: std::net::SocketAddr) -> Vec<std::net::IpAddr> {
    if local.ip().is_unspecified() {
        let real = matter_transport::local_advertise_addrs();
        if real.is_empty() {
            vec![local.ip()]
        } else {
            real
        }
    } else {
        vec![local.ip()]
    }
}

/// The high-level Matter controller. Cloneable; all clones talk to one
/// owning task.
#[derive(Clone)]
pub struct MatterController {
    tx: mpsc::Sender<Command>,
    /// Retained so the OTA provider server (`serve_provider_once`) can
    /// load the stable, committed operational identity without routing through
    /// the actor (the identity is minted once and never mutated after).
    store: Arc<dyn ControllerStore>,
}

impl MatterController {
    /// Begin configuring a controller (attestation trust, admin vendor id).
    #[must_use]
    pub fn builder(store: Arc<dyn ControllerStore>) -> MatterControllerBuilder {
        MatterControllerBuilder::new(store)
    }

    /// Open a controller with default settings and **no** attestation trust —
    /// sufficient for operating already-commissioned devices, but `commission`
    /// will return [`Error::NoTrust`]. Use [`Self::builder`] to commission.
    ///
    /// # Errors
    ///
    /// As [`MatterControllerBuilder::build`].
    pub async fn open(store: Arc<dyn ControllerStore>) -> Result<Self, Error> {
        Self::spawn_default(
            store,
            None,
            crate::builder::DEFAULT_ADMIN_VENDOR_ID,
            None,
            crate::actor::DEFAULT_RESPONSE_DEADLINE,
        )
        .await
    }

    pub(crate) async fn spawn_default(
        store: Arc<dyn ControllerStore>,
        trust: Option<AttestationTrust>,
        admin_vendor_id: u16,
        multicast_if: Option<u32>,
        response_deadline: std::time::Duration,
    ) -> Result<Self, Error> {
        let transport =
            matter_transport::TokioUdpTransport::bind_with_multicast_if(0, multicast_if)
                .await
                .map_err(|e| Error::Operational(format!("bind: {e}")))?;
        let discovery = matter_transport::MdnsSdDiscovery::new()
            .map_err(|e| Error::Operational(format!("mdns: {e}")))?;
        Self::with_components_and_multicast_if(
            store,
            transport,
            discovery,
            Arc::new(SystemNocRng),
            trust,
            admin_vendor_id,
            multicast_if,
            response_deadline,
        )
    }

    /// Construct over caller-supplied transport + discovery (used by tests to
    /// inject `InMemoryDatagram` + a mock `Discovery`).
    ///
    /// # Errors
    ///
    /// [`Error::Store`] / [`Error::Snapshot`] if the persisted snapshot is
    /// unreadable.
    #[cfg(test)] // production construction goes through `with_components_and_multicast_if`.
    pub(crate) fn with_components<T, D>(
        store: Arc<dyn ControllerStore>,
        transport: T,
        discovery: D,
        rng: Arc<dyn NocRng>,
        trust: Option<AttestationTrust>,
        admin_vendor_id: u16,
    ) -> Result<Self, Error>
    where
        T: AsyncDatagram + Send + Sync + 'static,
        D: Discovery + Send + 'static,
    {
        Self::with_components_and_multicast_if(
            store,
            transport,
            discovery,
            rng,
            trust,
            admin_vendor_id,
            None,
            crate::actor::DEFAULT_RESPONSE_DEADLINE,
        )
    }

    #[allow(clippy::too_many_arguments)] // Component-injection seam; mirrors Actor::new.
    pub(crate) fn with_components_and_multicast_if<T, D>(
        store: Arc<dyn ControllerStore>,
        transport: T,
        discovery: D,
        rng: Arc<dyn NocRng>,
        trust: Option<AttestationTrust>,
        admin_vendor_id: u16,
        multicast_if: Option<u32>,
        response_deadline: std::time::Duration,
    ) -> Result<Self, Error>
    where
        // `Sync` because the spawned actor future holds `&self.transport`
        // across awaits (inside `run_case`/`secured_round_trip`); `Send` so the
        // future can be `tokio::spawn`ed onto the multi-thread runtime.
        T: AsyncDatagram + Send + Sync + 'static,
        D: Discovery + Send + 'static,
    {
        let state = match store.load()? {
            Some(bytes) => snapshot::deserialize(&bytes)?,
            None => ControllerState::default(),
        };
        let (tx, rx) = mpsc::channel(COMMAND_CHANNEL_DEPTH);
        let actor = Actor::new(
            transport,
            discovery,
            store.clone(),
            rng,
            state,
            trust,
            admin_vendor_id,
        )
        .with_multicast_if(multicast_if)
        .with_response_deadline(response_deadline);
        tokio::spawn(actor.run(rx));
        Ok(Self { tx, store })
    }

    /// Serve the OTA **provider** role once: advertise our operational service,
    /// accept one inbound CASE session, and dispatch up to `max_invokes`
    /// server-side `InvokeRequest`s through `handler`, then withdraw the
    /// advertisement. `handler` maps a parsed request to the encoded
    /// `InvokeResponse` bytes (e.g. via `matter_interaction::build_invoke_response_*`).
    ///
    /// The server runs on its **own** freshly-bound UDP socket and its own mDNS
    /// daemon — it does not touch the client actor (the long-running accept is
    /// kept off the proven request/MRP loop). It authenticates as our persisted
    /// operational identity (the M8 commissioner NOC/IPK/root).
    ///
    /// This ships the generic provider plumbing; the OTA `QueryImage` handler
    /// and the BDX transfer build on it. Note: advertising a wildcard-bound
    /// address may not be routable to a foreign requestor — see the runbook
    /// for the interface-selection caveat (the automated validation is the
    /// in-process loopback test).
    ///
    /// # Errors
    ///
    /// [`Error::NotCommissioned`] if no fabric exists; [`Error::Operational`] on
    /// bind / mDNS / clock failure; otherwise any CASE-accept or dispatch error
    /// from [`crate::provider_server::ProviderServer`].
    #[cfg(feature = "unstable-provider")]
    pub async fn serve_provider_once<H>(
        &self,
        port: u16,
        handler: H,
        max_invokes: usize,
    ) -> Result<usize, Error>
    where
        H: FnMut(&matter_interaction::ParsedInvokeRequest) -> Vec<u8>,
    {
        use crate::provider_server::{build_operational_service, ProviderServer};

        // 1. Load our persisted fabric + build the responder identity.
        let state = match self.store.load()? {
            Some(bytes) => snapshot::deserialize(&bytes)?,
            None => return Err(Error::NotCommissioned("no fabric to serve from".into())),
        };
        let fabric = state
            .fabrics
            .first()
            .ok_or_else(|| Error::NotCommissioned("no fabric to serve from".into()))?;
        let (credentials, roots, compressed) = crate::credentials::operational_credentials(fabric)?;
        let node_id = fabric.commissioner.node_id;
        let now = crate::actor::current_matter_time()?;

        // 2. Bind our own socket + advertise the operational service.
        let socket = matter_transport::TokioUdpTransport::bind(port)
            .await
            .map_err(|e| Error::Operational(format!("provider bind: {e}")))?;
        let local = socket
            .socket()
            .local_addr()
            .map_err(|e| Error::Operational(format!("provider local_addr: {e}")))?;
        let mut discovery = matter_transport::MdnsSdDiscovery::new()
            .map_err(|e| Error::Operational(format!("provider mdns: {e}")))?;
        let service =
            build_operational_service(compressed, node_id, advertise_addrs(local), local.port());
        matter_transport::Discovery::publish(&mut discovery, &service)?;

        // 3. Accept one session + dispatch up to `max_invokes` invokes.
        let result = ProviderServer::new(
            socket,
            vec![credentials],
            roots,
            /* base_session_id */ 0x01,
            now,
        )
        .accept_and_dispatch_once(handler, max_invokes)
        .await;

        // 4. Withdraw the advertisement regardless of outcome.
        let _ = matter_transport::Discovery::unpublish(
            &mut discovery,
            &service.instance_name,
            matter_transport::ServiceKind::Operational,
        );
        result
    }

    /// Announce ourselves as an OTA provider to `target_node_id`, advertise our
    /// operational service, and serve `image` over the full OTA flow (the
    /// requestor resolves us, opens CASE, queries, BDX-downloads, applies, and
    /// — possibly after rebooting into the new image — sends
    /// `NotifyUpdateApplied`). Returns once `NotifyUpdateApplied` is received.
    ///
    /// `software_version` is offered in `QueryImageResponse` (must exceed the
    /// requestor's current version for it to update — and match the version
    /// baked into the `.ota` header for a live requestor). `port` binds the
    /// provider socket (0 = ephemeral). The image is served verbatim over BDX
    /// (unsigned; the requestor parses the `OTAImageHeader`).
    ///
    /// Because a real requestor reboots into the new image before notifying,
    /// the call may block for an extended period. Callers should bound the wait
    /// with [`tokio::time::timeout`]. Each accepted CASE session's resumption
    /// record is persisted immediately via an internal sink (best-effort: a
    /// failed store only costs a future fast path).
    ///
    /// # Errors
    ///
    /// [`Error::NotCommissioned`] if no fabric exists; [`Error::Operational`] on
    /// bind / mDNS / clock failure; otherwise any announce or serve error.
    #[cfg(feature = "ota")]
    pub async fn serve_ota(
        &self,
        target_node_id: u64,
        image: Vec<u8>,
        software_version: u32,
        port: u16,
    ) -> Result<(), Error> {
        // 960 keeps each BDX DataBlock (block + counter + BDX/IM framing) under
        // the transport's 1024-byte secured-payload budget — correct for Wi-Fi
        // and IP. For a Thread-routed requestor use
        // [`Self::serve_ota_with_block_size`] with ~512: at 960 a single block
        // spans ~a dozen 802.15.4 fragments that must ALL arrive, so a smaller
        // block cuts the per-block loss probability on the mesh.
        self.serve_ota_with_block_size(target_node_id, image, software_version, port, 960)
            .await
    }

    /// [`Self::serve_ota`] with an explicit BDX `max_block_size`. Pass a smaller
    /// value (~512) for a Thread-routed requestor so each block fits fewer
    /// 6LoWPAN fragments; 960 is the Wi-Fi/IP default (see [`Self::serve_ota`]).
    ///
    /// # Errors
    ///
    /// Same as [`Self::serve_ota`].
    #[cfg(feature = "ota")]
    pub async fn serve_ota_with_block_size(
        &self,
        target_node_id: u64,
        image: Vec<u8>,
        software_version: u32,
        port: u16,
        max_block_size: u16,
    ) -> Result<(), Error> {
        use crate::provider_server::{build_operational_service, ProviderServer};

        // Credential pool: one identity per CASE accept (first session +
        // post-reboot session + retry slack).
        const PROVIDER_CREDENTIAL_POOL: usize = 4;

        // Identity + offer.
        let state = match self.store.load()? {
            Some(bytes) => snapshot::deserialize(&bytes)?,
            None => return Err(Error::NotCommissioned("no fabric to serve from".into())),
        };
        let fabric = state
            .fabrics
            .first()
            .ok_or_else(|| Error::NotCommissioned("no fabric to serve from".into()))?;
        let mut pool = Vec::with_capacity(PROVIDER_CREDENTIAL_POOL);

        let mut roots_compressed = None;
        for _ in 0..PROVIDER_CREDENTIAL_POOL {
            let (c, r, comp) = crate::credentials::operational_credentials(fabric)?;
            pool.push(c);
            roots_compressed = Some((r, comp));
        }
        let (roots, compressed) =
            roots_compressed.ok_or_else(|| Error::Operational("empty credential pool".into()))?;
        let node_id = fabric.commissioner.node_id;
        let now = crate::actor::current_matter_time()?;
        let offer = matter_ota::ImageOffer {
            software_version,
            software_version_string: software_version.to_string(),
            image_uri: format!("bdx://{node_id:016X}/fw.ota"),
            update_token: vec![0xAB; 16],
        };

        // Bind + advertise.
        let socket = matter_transport::TokioUdpTransport::bind(port)
            .await
            .map_err(|e| Error::Operational(format!("provider bind: {e}")))?;
        let local = socket
            .socket()
            .local_addr()
            .map_err(|e| Error::Operational(format!("provider local_addr: {e}")))?;
        let mut discovery = matter_transport::MdnsSdDiscovery::new()
            .map_err(|e| Error::Operational(format!("provider mdns: {e}")))?;
        let service =
            build_operational_service(compressed, node_id, advertise_addrs(local), local.port());
        matter_transport::Discovery::publish(&mut discovery, &service)?;

        // Announce FIRST, then build the server. The announce is an ordinary
        // invoke on the actor's socket: it rides the node's cached CASE
        // session if there is one (right after `commission()`, the one its
        // post-commission Basic Information read opened) and opens one
        // otherwise. That socket ACKs an inbound QueryImage but never answers
        // it, so chip's requestor, which reuses the existing session with the
        // most recent peer activity (`SessionManager::FindSecureSessionForNode`),
        // first sends QueryImage there, times out after about 10 s and marks
        // the session defunct. Its one retry (`kMaxInvalidSessionRetries = 1`,
        // `DefaultOTARequestorDriver.cpp:50`) resolves the provider record
        // advertised above and opens a new CASE session to this server only
        // if it holds no other active session to this node id; the
        // commissioning driver closes its own with a CloseSession for that
        // reason. That Sigma1 may ask to resume the announce's session, so the
        // server is seeded with the resumption record the announce's connect
        // persisted, which exists only once the announce completes. A Sigma1
        // arriving in the gap waits in the socket buffer (chip MRP-retransmits
        // it regardless).
        let node = self.node(target_node_id);
        let announce_res = node
            .announce_ota_provider(node_id, crate::builder::DEFAULT_ADMIN_VENDOR_ID, 0)
            .await;
        if let Err(e) = announce_res {
            let _ = matter_transport::Discovery::unpublish(
                &mut discovery,
                &service.instance_name,
                matter_transport::ServiceKind::Operational,
            );
            return Err(e);
        }

        // Fetch the announce connect's resumption record from live actor
        // state (guaranteed present: the announce rode that session). A
        // missing/corrupt record only costs the fast path — the server then
        // declines and falls back to a full handshake.
        let records = match self.resumption_record_for(target_node_id).await {
            Ok(Some(r)) => vec![r],
            Ok(None) | Err(_) => Vec::new(),
        };

        let sink_controller = self.clone();
        let server = ProviderServer::new(socket, pool, roots, /* base_session_id */ 0x01, now)
            .with_resumption_records(records)
            .with_expected_peer(target_node_id)
            .with_record_sink(Box::new(move |record| {
                let c = sink_controller.clone();
                tokio::spawn(async move {
                    // Best-effort: a failed store only costs a future fast path.
                    let node = record.peer.node_id;
                    let _ = c.store_resumption_record(node, &record).await;
                });
            }));
        // `max_block_size` must keep each BDX DataBlock (block + 4-byte counter
        // + BDX/IM framing) under the transport's 1024-byte secured-payload
        // budget — 960 is the Wi-Fi/IP default (1024 overflows by 14 bytes once
        // framed); a Thread caller passes ~512.
        let serve_res = server.serve_ota_once(offer, image, max_block_size).await;

        let _ = matter_transport::Discovery::unpublish(
            &mut discovery,
            &service.instance_name,
            matter_transport::ServiceKind::Operational,
        );

        serve_res?;
        Ok(())
    }

    /// Advertise our operational service and listen for ONE inbound Check-In
    /// from a registered ICD, verify it against the stored registration key
    /// (enforcing counter monotonicity), and return it — the caller then
    /// re-establishes a session and reads/subscribes / `stay_active_request`s
    /// while the device is briefly active.
    ///
    /// Runs on its **own** freshly-bound UDP socket + mDNS daemon, off the
    /// client actor. Requires at least one registration from
    /// [`Node::register_icd_client`](crate::Node::register_icd_client).
    ///
    /// # Errors
    ///
    /// [`Error::NotCommissioned`] if no fabric exists; [`Error::Operational`] if
    /// no ICD clients are registered, on bind / mDNS failure, or if no
    /// verifiable Check-In arrives before the internal frame budget is reached.
    pub async fn listen_for_checkin_once(
        &self,
        port: u16,
    ) -> Result<crate::icd_listener::CheckIn, Error> {
        use crate::provider_server::build_operational_service;

        // Load registrations + advertising identity from the persisted fabric.
        let state = match self.store.load()? {
            Some(bytes) => snapshot::deserialize(&bytes)?,
            None => return Err(Error::NotCommissioned("no fabric to listen from".into())),
        };
        let fabric = state
            .fabrics
            .first()
            .ok_or_else(|| Error::NotCommissioned("no fabric to listen from".into()))?;
        let registrations = fabric.icd_clients.clone();
        if registrations.is_empty() {
            return Err(Error::Operational(
                "no registered ICD clients to listen for".into(),
            ));
        }
        let (_creds, _roots, compressed) = crate::credentials::operational_credentials(fabric)?;
        let node_id = fabric.commissioner.node_id;

        // Bind our own socket + advertise (so a registered ICD can resolve us).
        let socket = matter_transport::TokioUdpTransport::bind(port)
            .await
            .map_err(|e| Error::Operational(format!("ICD listener bind: {e}")))?;
        let local = socket
            .socket()
            .local_addr()
            .map_err(|e| Error::Operational(format!("ICD listener local_addr: {e}")))?;
        let mut discovery = matter_transport::MdnsSdDiscovery::new()
            .map_err(|e| Error::Operational(format!("ICD listener mdns: {e}")))?;
        let service =
            build_operational_service(compressed, node_id, advertise_addrs(local), local.port());
        matter_transport::Discovery::publish(&mut discovery, &service)?;

        // Listen for one verifiable Check-In (generous frame budget for noise).
        let result = crate::icd_listener::recv_checkin_once(&socket, &registrations, 256).await;

        let _ = matter_transport::Discovery::unpublish(
            &mut discovery,
            &service.instance_name,
            matter_transport::ServiceKind::Operational,
        );
        result
    }

    /// Create and persist a new fabric (mints the stable commissioner
    /// identity). Returns the new fabric id.
    ///
    /// Refuses to create a fabric whose `fabric_id` already exists on this
    /// controller (issue #110) — call [`Self::fabrics`] first if you are not
    /// sure whether one does. Call this only on a fresh store, or after
    /// checking `fabrics()`; do not call it unconditionally on every startup.
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`] if the task has stopped;
    /// [`Error::FabricAlreadyExists`] if `cfg.fabric_id` already exists;
    /// [`Error::InvalidFabricValidity`] if `cfg.validity` names a window
    /// devices cannot use — a `not_before` at the Matter epoch, a `not_before`
    /// implausibly far ahead of this host's clock, an already-expired
    /// `not_after`, or an inverted/empty window (see
    /// [`FabricConfig::validity`], issue #111);
    /// otherwise any minting / persistence error.
    ///
    /// This call does **not** require a set host clock: the certificates are
    /// minted entirely from `cfg.validity`, and the clock is only used to
    /// sanity-check that window, so on a host whose clock reads before the
    /// Matter epoch (no RTC, before NTP) the clock-relative checks are skipped
    /// with a warning and the fabric is still created. The clock-independent
    /// checks still apply — notably a `not_before` of `MatterTime(0)`, which is
    /// what deriving it from an unset `SystemTime::now()` produces. Operations
    /// that genuinely need a real time — [`Self::commission`] (it mints the
    /// device's NOC) and every operational CASE session — do fail with
    /// [`Error::SystemClockUnset`] until the clock is set.
    pub async fn create_fabric(&self, cfg: FabricConfig) -> Result<u64, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::CreateFabric { cfg, reply })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)?
    }

    /// Commission a device from a QR (`MT:...`) or manual pairing code, bring it
    /// onto the controller's fabric, and persist it. Returns a [`NodeInfo`] for
    /// the commissioned device.
    ///
    /// After the device is on the fabric, a best-effort `BasicInformation` read
    /// captures its `VendorID`/`ProductID` into the returned `NodeInfo` and
    /// persists them on the device entry. That read is best-effort: if it fails,
    /// commissioning still succeeds and `NodeInfo::vendor_id`/`product_id` are
    /// left `None` (re-readable later via [`Self::nodes`]).
    ///
    /// `label` is an opaque, caller-supplied string (e.g. a friendly name like
    /// `"kitchen plug"`) persisted on the device's entry atomically with the
    /// rest of the commissioning result — a crash after this call returns
    /// either sees the fully-commissioned device with its label, or nothing
    /// at all, never a device missing its label. Pass `None` if you have no
    /// label to attach yet; it can be left unset.
    ///
    /// # Errors
    ///
    /// [`Error::NoTrust`] if no attestation trust was configured,
    /// [`Error::SetupCode`] if the code is invalid, [`Error::ControllerStopped`]
    /// if the task stopped, or any driver/commissioning error.
    pub async fn commission(
        &self,
        setup_code: &str,
        label: Option<String>,
    ) -> Result<NodeInfo, Error> {
        let setup_payload = parse_setup_code(setup_code)?;
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Commission {
                setup_payload,
                label,
                reply,
            })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        let mut info = rx.await.map_err(|_| Error::ControllerStopped)??;
        self.capture_basic_info(&mut info).await;
        Ok(info)
    }

    /// Commission a Wi-Fi or Thread device over **BLE/BTP** (feature `ble`):
    /// scan for the device by discriminator, open a BTP session, run PASE and
    /// every pre-operational stage (attestation, NOC install, network
    /// provisioning) over BTP, then complete the operational CASE session over
    /// IP once the device joins the operational network. Brings the device
    /// onto the controller's fabric, persists it, and returns a [`NodeInfo`]
    /// (including a best-effort `BasicInformation` `VendorID`/`ProductID`
    /// capture, exactly as [`Self::commission`]).
    ///
    /// `network` selects which provisioning sub-flow runs after `AddNOC`:
    /// [`NetworkCredentials::WiFi`](matter_commissioning::NetworkCredentials::WiFi)
    /// or
    /// [`NetworkCredentials::Thread`](matter_commissioning::NetworkCredentials::Thread).
    /// Some network credentials are **required** for a BLE-only device with no
    /// operational connectivity yet — a BLE-only device with no network to
    /// join is unprovisionable;
    /// [`NetworkCredentials::AlreadyOnNetwork`](matter_commissioning::NetworkCredentials::AlreadyOnNetwork)
    /// only makes sense for a device that already has operational connectivity
    /// independent of BLE (e.g. Ethernet).
    ///
    /// **Requires macOS Bluetooth permission (TCC).** The first call
    /// instantiates `CoreBluetooth` and may raise the one-time Bluetooth prompt,
    /// attributed to the terminal application — see
    /// `docs/runbooks/ble-commissioning.md`.
    ///
    /// `label` is the same opaque, caller-supplied string as
    /// [`Self::commission`]'s — persisted on the device's entry atomically
    /// with the rest of the commissioning result. Pass `None` if you have no
    /// label to attach yet.
    ///
    /// # Errors
    ///
    /// [`Error::NoTrust`] if no attestation trust was configured,
    /// [`Error::SetupCode`] if the code is invalid, [`Error::ControllerStopped`]
    /// if the task stopped (including a btleplug-internal panic in the spawned
    /// commission task), [`Error::Operational`] for a BLE-layer failure (no
    /// adapter / denied permission, scan timeout, connect, GATT, or BTP
    /// handshake), or any driver/commissioning error.
    #[cfg(feature = "ble")]
    pub async fn commission_ble(
        &self,
        setup_code: &str,
        network: matter_commissioning::NetworkCredentials,
        label: Option<String>,
    ) -> Result<NodeInfo, Error> {
        let setup_payload = parse_setup_code(setup_code)?;
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::CommissionBle {
                setup_payload,
                network,
                label,
                reply,
            })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        let mut info = rx.await.map_err(|_| Error::ControllerStopped)??;
        self.capture_basic_info(&mut info).await;
        Ok(info)
    }

    /// Best-effort: read `VendorID`/`ProductID` from the device's
    /// `BasicInformation` cluster (endpoint 0) and persist them onto the node's
    /// stored entry, filling `info.vendor_id`/`info.product_id`.
    ///
    /// Deliberately infallible from the caller's view: commissioning has
    /// already succeeded and the device is on the fabric, so a flaky metadata
    /// read (or a device that answers something unexpected) must never turn a
    /// completed commission into an error. On any failure the ids stay `None`
    /// and can be re-read later.
    async fn capture_basic_info(&self, info: &mut NodeInfo) {
        let node = self.node(info.node_id);
        let paths = [
            crate::ReadPath::concrete(0, BASIC_INFORMATION_CLUSTER, BASIC_INFO_ATTR_VENDOR_ID),
            crate::ReadPath::concrete(0, BASIC_INFORMATION_CLUSTER, BASIC_INFO_ATTR_PRODUCT_ID),
        ];
        let Ok(reports) = node.read(&paths).await else {
            return;
        };
        let mut vendor_id = None;
        let mut product_id = None;
        for (path, value) in &reports {
            let crate::Value::Uint(n) = value else {
                continue;
            };
            let Ok(n16) = u16::try_from(*n) else { continue };
            if path.attribute == BASIC_INFO_ATTR_VENDOR_ID {
                vendor_id = Some(n16);
            } else if path.attribute == BASIC_INFO_ATTR_PRODUCT_ID {
                product_id = Some(n16);
            }
        }
        if vendor_id.is_none() && product_id.is_none() {
            return;
        }
        info.vendor_id = vendor_id;
        info.product_id = product_id;
        // Persist best-effort — a store failure here only means a future
        // `nodes()` re-reads `None`; it does not fail the commission.
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(Command::SetNodeVidPid {
                node_id: info.node_id,
                vendor_id,
                product_id,
                reply,
            })
            .await
            .is_ok()
        {
            let _ = rx.await;
        }
    }

    /// Enumerate every node this controller has commissioned, across all
    /// fabrics, as typed [`NodeInfo`]. Replaces the need to deserialize the
    /// on-disk snapshot to discover node ids and metadata.
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`] if the owning task has stopped.
    pub async fn nodes(&self) -> Result<Vec<NodeInfo>, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::ListNodes { reply })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)
    }

    /// Enumerate every fabric this controller has created, as typed
    /// [`crate::FabricInfo`]. Check this before calling [`Self::create_fabric`] —
    /// since issue #110, `create_fabric` refuses to create a second fabric
    /// with a `fabric_id` that already exists here.
    ///
    /// This is **our own** fabric list, read from the controller's store with
    /// no network traffic. For the fabrics a *device* is commissioned onto
    /// (including other administrators' fabrics), read the device's own table
    /// with [`Node::list_fabrics`](crate::Node::list_fabrics).
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`] if the owning task has stopped;
    /// [`Error::Operational`] if a fabric's
    /// [`compressed_fabric_id`](crate::FabricInfo::compressed_fabric_id)
    /// cannot be derived (it always can for a valid fabric: the derivation's
    /// error path exists only so the library never panics).
    pub async fn fabrics(&self) -> Result<Vec<crate::FabricInfo>, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::ListFabrics { reply })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)?
    }

    /// Forget a node: drop ALL of the controller's own state for it — the
    /// persisted device record, any cached CASE session, and its resumption
    /// data — WITHOUT contacting the device. Use this to reclaim a node that is
    /// unreachable or already factory-reset (where `remove_fabric` cannot run).
    ///
    /// Returns `true` if a node was found and removed, `false` if no such node
    /// was commissioned. This does NOT remove the controller's fabric from the
    /// device; a still-live device keeps its NOC until it is reset or its fabric
    /// removed via `Node::remove_fabric`.
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`] if the task stopped, or a store error while
    /// persisting the removal.
    pub async fn forget_node(&self, node_id: u64) -> Result<bool, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::ForgetNode { node_id, reply })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)?
    }

    /// Re-establish `node_id`'s subscriptions **now** if they are waiting to be
    /// retried, instead of waiting out the retry backoff.
    ///
    /// When a device stops reporting, the controller retries its subscriptions
    /// on chip's backoff (`ComputeTimeTillNextSubscription`): the first retry is
    /// immediate, later ones wait up to about 63 minutes (about 92 once past the
    /// Fibonacci cap). A device that comes back during a wait stays
    /// unsubscribed until the wait ends. Call this when you learn the device is
    /// reachable again — your own mDNS browse saw it, it sent an ICD check-in,
    /// your network reported it — and every subscription of `node_id` that is
    /// waiting to be re-established is pulled forward:
    ///
    /// - a retry that is **scheduled** fires immediately, with its backoff reset
    ///   to the start of the sequence;
    /// - a retry already **in progress** keeps running (a duplicate attempt is
    ///   never started), but its backoff is reset, so if it fails the next try
    ///   comes seconds later, not minutes;
    /// - a retry that is already **due** (about to fire anyway) keeps its time
    ///   but also has its backoff reset. It is not counted in the return value.
    ///
    /// This is chip's `ReadClient::TriggerResubscribeIfScheduled` together with
    /// `ResetResubscriptionBackoff`, as Apple's `Matter.framework` calls them
    /// when it sees an operational advert.
    ///
    /// It is **not throttled**: every call acts. The controller's own
    /// advert-triggered pulls are limited to one per subscription per 30 s;
    /// calls here ignore that limit, but count toward it.
    ///
    /// # You may not need to call this
    ///
    /// While a subscription waits, the controller watches its fabric's
    /// operational mDNS subtype and does this itself when the device publishes
    /// a new advertisement. It also re-sends its query for that subtype 30 s
    /// after the watch starts, then 60, 120 and 240 s apart, then every
    /// 5 minutes, so a returning device whose announcement it missed is found
    /// within about 5 minutes at worst (subject to the cases below). That
    /// covers most Wi-Fi and Ethernet devices. It does **not** reliably cover:
    ///
    /// - Thread devices behind a border router whose SRP advertising proxy kept
    ///   serving their records while they were offline (no new advertisement
    ///   appears when they return);
    /// - an outage shorter than the device's advertisement TTL (120 s on
    ///   chip-based devices, up to 75 minutes on others). A device that
    ///   reboots quickly (a clean restart, say) typically recovers on the
    ///   early, short retries instead, not through advert detection;
    /// - a device that returns within 5 s of the watch starting;
    /// - a custom [`Discovery`] passed to
    ///   [`MatterControllerBuilder::discovery`](crate::MatterControllerBuilder::discovery)
    ///   that does not override both
    ///   [`query_operational_fabric`](matter_transport::Discovery::query_operational_fabric)
    ///   and [`poll_found`](matter_transport::Discovery::poll_found). One that
    ///   overrides only `poll_found` may see spurious advert pulls, bounded by
    ///   the 30 s per-subscription cooldown. One that does not also override
    ///   [`requery`](matter_transport::Discovery::requery) finds a device whose
    ///   announcement was missed only when its resolver queries on its own.
    ///   One that overrides `requery` but keeps the default
    ///   `query_operational_fabric` has its base-type `_matter._tcp` browse
    ///   re-queried on the cadence above.
    ///   The default `MdnsSdDiscovery` overrides all three.
    ///
    /// In those cases, or to react faster than the re-query cadence, call this
    /// from whatever signal you have.
    ///
    /// The watch has a steady-state cost: while any subscription is waiting
    /// (possibly indefinitely, for a device that never returns), the
    /// controller holds one operational mDNS browse open, wakes about once per
    /// second to drain it, and re-queries it on the cadence above. With the
    /// default `MdnsSdDiscovery`, each re-query restarts the mDNS daemon's own
    /// query sequence for the subtype
    /// — about five small multicast queries in the first 15 s, and about nine
    /// per 5-minute step at the cap, where a settled browse sends about one an
    /// hour — and makes the daemon replay the records it holds for the fabric
    /// (about two events per device) onto the browse's event channel, which
    /// holds 1000 events. For 15 s after each re-query the controller drains
    /// it every 250 ms instead of every second, so the backlog stays short
    /// and a device answering the restarted queries is acted on quickly.
    ///
    /// Returns how many subscriptions it affected: scheduled retries pulled
    /// forward plus in-progress retries whose backoff it reset. `Ok(0)` means
    /// none of `node_id`'s subscriptions was waiting to be re-established: it is
    /// subscribed and healthy, has no subscriptions, or is not a node this
    /// controller knows. A subscription whose
    /// [`Subscription`](crate::subscription::Subscription) was dropped is not
    /// counted.
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`] if the owning task has stopped.
    pub async fn resubscribe_now(&self, node_id: u64) -> Result<usize, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::ResubscribeNow { node_id, reply })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)
    }

    /// Handle addressing a device by node id (single-fabric).
    #[must_use]
    pub fn node(&self, node_id: u64) -> Node {
        Node {
            tx: self.tx.clone(),
            node_id,
        }
    }

    /// Create a group key set on the controller's fabric: mints a fresh 16-byte
    /// epoch key from the CSPRNG, persists a `GroupKeySetConfig` under
    /// `key_set_id`, and returns the [`GroupKeySet`](crate::GroupKeySet) so the caller can program
    /// it onto each member device via
    /// [`Node::write_group_key_set`](crate::Node::write_group_key_set) and map a
    /// group to it. The key set is stored durably before this returns, so the
    /// controller can encrypt outbound group messages for it immediately
    /// (see [`Self::invoke_group`]).
    ///
    /// `epoch_start_time` is the Matter-epoch start time recorded in the
    /// returned `GroupKeySet` (the device-side `KeySetWrite` echoes it).
    ///
    /// # Errors
    ///
    /// [`Error::NotCommissioned`] if no single fabric exists,
    /// [`Error::ControllerStopped`] if the task has stopped, or any
    /// CSPRNG / persistence error.
    pub async fn create_group(
        &self,
        key_set_id: u16,
        epoch_start_time: u64,
    ) -> Result<crate::GroupKeySet, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::CreateGroup {
                key_set_id,
                epoch_start_time,
                reply,
            })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)?
    }

    /// Fire-and-forget multicast group invoke: send `path`/`fields` to every
    /// device in `group_id`, encrypted with the operational group key derived
    /// from the persisted `key_set_id`. Returns as soon as the datagram is sent
    /// — group commands are unacknowledged, so there is no response.
    ///
    /// The caller supplies `key_set_id` (the key set the group was bound to when
    /// it was created): the controller's persisted `group_keys` are keyed by
    /// key set id, avoiding a separate group→key-set map. The outbound group
    /// message counter is bumped and persisted **before** the send so a counter
    /// is never reused across a crash.
    ///
    /// Real multicast delivery requires the host network to route the Matter
    /// site-local group address; on a host without it the send still succeeds at
    /// the socket layer (the bytes are correct — see the loopback test).
    ///
    /// # Errors
    ///
    /// [`Error::GroupNotProvisioned`] if `key_set_id` has no persisted key set,
    /// [`Error::NotCommissioned`] if no single fabric exists,
    /// [`Error::Operational`] on counter exhaustion or send failure,
    /// [`Error::ControllerStopped`] if the task has stopped, or any
    /// crypto / persistence error.
    pub async fn invoke_group(
        &self,
        group_id: u16,
        key_set_id: u16,
        path: crate::CommandPath,
        fields: crate::Value,
    ) -> Result<(), Error> {
        let fields_tlv = crate::node::value_to_tlv(&fields)?;
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::InvokeGroup {
                group_id,
                key_set_id,
                path,
                fields_tlv,
                reply,
            })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)?
    }

    #[cfg(test)]
    pub(crate) async fn session_count(&self) -> usize {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Command::SessionCount { reply }).await.is_err() {
            return 0;
        }
        rx.await.unwrap_or(0)
    }

    /// Test-only: see `Command::SetResubscribeSchedule`. Returns how many queued
    /// resubscribes of `node_id` were changed (0 if the actor is gone).
    #[cfg(test)]
    pub(crate) async fn set_resubscribe_schedule(
        &self,
        node_id: u64,
        retry_count: u32,
        attempt_in: std::time::Duration,
    ) -> usize {
        let (reply, rx) = oneshot::channel();
        if self
            .tx
            .send(Command::SetResubscribeSchedule {
                node_id,
                retry_count,
                attempt_in,
                reply,
            })
            .await
            .is_err()
        {
            return 0;
        }
        rx.await.unwrap_or(0)
    }

    /// Fetch the stored CASE resumption record for `node_id` from the actor's
    /// live state (deserialized; `None` if the device has none). Used by
    /// `serve_ota` to let the provider server accept the requestor's
    /// resumption attempt.
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`] if the owning task stopped,
    /// [`Error::NotCommissioned`] if no sole fabric exists, or a
    /// [`Error::Snapshot`]/[`Error::Codec`]/[`Error::Cert`] deserialization
    /// failure for a corrupt stored record.
    #[cfg(any(feature = "ota", test))]
    pub(crate) async fn resumption_record_for(
        &self,
        node_id: u64,
    ) -> Result<Option<matter_crypto::ResumptionRecord>, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::ResumptionRecordFor { node_id, reply })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)?
    }

    /// Drop every cached CASE resumption record on `fabric_id`, returning how
    /// many were cleared. Saves durably before returning.
    ///
    /// # When you must call this
    ///
    /// After anything that changes this controller's operational identity on
    /// the fabric — its node id, its CATs, or its operational key. Resumption
    /// skips certificate verification and replays a cached authorization
    /// snapshot instead; the ECDH secret that gates it is derived from
    /// *ephemeral* keys, so it remains cryptographically valid across an
    /// identity change and cannot detect one. A surviving record therefore
    /// lets the retired identity be reconstituted one round trip later, with
    /// its old node id and its old CATs. The specification requires the
    /// equivalent for a device: *"All internal data reflecting the prior
    /// operational identifier of the Node within the Fabric SHALL be revoked
    /// and removed."*
    ///
    /// chip wires this to a `FabricTable` delegate that fires on fabric update
    /// as well as removal (`ClearCASEResumptionStateOnFabricChange`). This
    /// library has no fabric-update path yet, so the call is yours to make;
    /// when one is added it should call this.
    ///
    /// # You are not relying on remembering
    ///
    /// Each stored record is also bound to a fingerprint of the commissioner
    /// NOC that minted it, and one that no longer matches is refused at load
    /// in both roles. Forgetting this call cannot cause a stale record to be
    /// *used*; calling it is what gets the dead secret material off disk
    /// promptly.
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`] if the owning task stopped,
    /// [`Error::Operational`] if no such fabric exists, or a store error if
    /// the durable save fails.
    pub async fn invalidate_resumption_records(&self, fabric_id: u64) -> Result<usize, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::InvalidateResumptionRecords { fabric_id, reply })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)?
    }

    /// Store `record` as the CASE resumption record for `node_id` (replacing
    /// any prior one; best-effort persist). Invoked by `serve_ota`'s
    /// provider server's `record_sink`, once per completed CASE accept.
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`] if the owning task stopped,
    /// [`Error::NotCommissioned`] if no sole fabric exists, or
    /// [`Error::Operational`] if the device has no entry on the fabric.
    #[cfg(feature = "ota")]
    pub(crate) async fn store_resumption_record(
        &self,
        node_id: u64,
        record: &matter_crypto::ResumptionRecord,
    ) -> Result<(), Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::StoreResumptionRecord {
                node_id,
                record: Box::new(record.clone()),
                reply,
            })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)?
    }
}

/// Parse a QR (`MT:...`) or manual pairing code into a [`matter_commissioning::SetupPayload`].
///
/// QR codes are identified by the `MT:` prefix (Matter Core Spec §5.1.3.1).
/// Anything else is treated as a manual pairing code.
///
/// # Errors
///
/// Returns [`Error::SetupCode`] if the string is not a valid QR or manual code.
fn parse_setup_code(code: &str) -> Result<matter_commissioning::SetupPayload, Error> {
    let trimmed = code.trim();
    let parsed = if trimmed.starts_with("MT:") {
        matter_commissioning::parse_qr(trimmed)
    } else {
        matter_commissioning::parse_manual_code(trimmed)
    };
    parsed.map_err(|e| Error::SetupCode(format!("{e:?}")))
}
