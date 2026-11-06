//! Builder for [`MatterController`]. Configures attestation trust and the
//! admin vendor id before spawning the owning actor.

use std::sync::Arc;

use crate::controller::MatterController;
use crate::error::Error;
use crate::store::ControllerStore;
use crate::trust::AttestationTrust;

/// Default admin vendor id used in `AddNOC` (CSA test VID). Override via
/// [`MatterControllerBuilder::admin_vendor_id`].
pub const DEFAULT_ADMIN_VENDOR_ID: u16 = 0xFFF1;

/// The pieces [`MatterControllerBuilder::build`] has assembled by the time the
/// actor can be spawned. Exists only to keep [`SpawnWithDiscovery`] a
/// single-argument closure type.
struct SpawnParts {
    store: Arc<dyn ControllerStore>,
    transport: matter_transport::TokioUdpTransport,
    trust: Option<AttestationTrust>,
    admin_vendor_id: u16,
    multicast_if: Option<u32>,
    response_deadline: std::time::Duration,
}

/// The deferred actor spawn installed by [`MatterControllerBuilder::discovery`].
///
/// What is boxed here is the **spawn step**, not the `Discovery` value. That
/// distinction is the whole design:
///
/// * It keeps `MatterControllerBuilder` a plain, non-generic struct, so adding
///   this seam is not a breaking change for anyone who names the type.
/// * It does *not* erase the caller's type. The closure is built inside
///   `discovery::<D>()`, where the concrete `D` is still in scope, so
///   `with_components_and_multicast_if` is monomorphised over that `D` and the
///   actor holds the caller's own type. Every trait call therefore dispatches
///   to the caller's `impl` — including methods that have a default body in
///   the trait and which a `Box<dyn Discovery>` shim would silently resolve to
///   the default instead of the override.
type SpawnWithDiscovery = Box<dyn FnOnce(SpawnParts) -> Result<MatterController, Error> + Send>;

/// Configures and opens a [`MatterController`].
pub struct MatterControllerBuilder {
    store: Arc<dyn ControllerStore>,
    trust: Option<AttestationTrust>,
    admin_vendor_id: u16,
    multicast_if: Option<u32>,
    response_deadline: std::time::Duration,
    /// `None` — the default — means no discovery was supplied, and
    /// [`Self::build`] takes the untouched default path through
    /// `MatterController::spawn_default`.
    spawn_with_discovery: Option<SpawnWithDiscovery>,
}

impl MatterControllerBuilder {
    pub(crate) fn new(store: Arc<dyn ControllerStore>) -> Self {
        Self {
            store,
            trust: None,
            admin_vendor_id: DEFAULT_ADMIN_VENDOR_ID,
            multicast_if: None,
            response_deadline: crate::actor::DEFAULT_RESPONSE_DEADLINE,
            spawn_with_discovery: None,
        }
    }

    /// Set the device-attestation trust material. Required to `commission`.
    #[must_use]
    pub fn attestation_trust(mut self, trust: AttestationTrust) -> Self {
        self.trust = Some(trust);
        self
    }

    /// Override the admin vendor id used in `AddNOC` (default `0xFFF1`).
    #[must_use]
    pub fn admin_vendor_id(mut self, vid: u16) -> Self {
        self.admin_vendor_id = vid;
        self
    }

    /// Set the IPv6 multicast egress interface (an `if_nametoindex` value)
    /// used for group commands (`invoke_group`). On a multi-homed host the
    /// kernel default has no route for the admin-local `ff35:` group address
    /// and group sends fail with "No route to host" — pick the LAN-facing
    /// interface. When unset, the `MATTER_MULTICAST_IF` env var is honored as
    /// a compat fallback, then the kernel default.
    #[must_use]
    pub fn multicast_interface(mut self, if_index: u32) -> Self {
        self.multicast_if = Some(if_index);
        self
    }

    /// Bound how long an operational read/write/invoke waits for its
    /// Interaction Model response (default 30 s).
    ///
    /// Matter's MRP bounds *delivery*, not *response*: once a device
    /// acknowledges a request, the retransmit timer for that exchange is
    /// discarded. A device that accepts a request and then never answers it
    /// therefore has nothing left to expire, and without this deadline the
    /// call waits forever. Real devices do this — a Tapo H100 bridge silently
    /// drops the 9th consecutive read on a session — so every operational verb
    /// is bounded by this value and fails with
    /// [`Error::ResponseTimeout`] when it elapses.
    ///
    /// The request is **not** retried first. Delivery was confirmed, so the
    /// device may already have executed a non-idempotent command; deciding
    /// whether a retry is safe belongs to you, not the library. This is
    /// deliberately unlike a lost-packet timeout, which the controller does
    /// retry once on a fresh session.
    ///
    /// Lower it if you front the controller with your own per-operation
    /// timeout and would rather see the library's error than your own; raise
    /// it for devices that are legitimately slow to answer.
    #[must_use]
    pub fn response_deadline(mut self, deadline: std::time::Duration) -> Self {
        self.response_deadline = deadline;
        self
    }

    /// Supply your own mDNS stack instead of the built-in one.
    ///
    /// # What the default is
    ///
    /// Leave this unset and the controller starts
    /// [`MdnsSdDiscovery`](matter_transport::MdnsSdDiscovery) — a pure-Rust
    /// responder built on the `mdns-sd` crate, with no system daemon required.
    /// That remains the default and is not going away; this method exists so
    /// the mDNS stack is your *choice* rather than something the library
    /// imposes on you.
    ///
    /// # Why you might replace it
    ///
    /// * **You already run a system responder.** On a typical Linux host
    ///   `avahi-daemon` (or `systemd-resolved`) already owns UDP 5353. A second
    ///   in-process responder is a second cache, a second set of probes, and a
    ///   second opinion about what is on the network. Delegating to the daemon
    ///   you already run removes that whole class of disagreement.
    /// * **You want the OS-native stack.** Bonjour on macOS, or a
    ///   platform/embedded resolver that is better placed than we are to know
    ///   about interface changes, sleep/wake, and roaming.
    /// * **You are testing.** A deterministic test double lets you drive
    ///   resolution outcomes — a node that never appears, one that appears late,
    ///   one that resolves to a fixed loopback address — without any real
    ///   network.
    ///
    /// # What your implementation is responsible for
    ///
    /// Implement [`matter_transport::Discovery`]; its own documentation is the
    /// contract. In short: `publish`/`unpublish` advertise and withdraw our
    /// services, and `query` → `poll_results` → `stop_query` is a browse whose
    /// records you buffer per handle and hand over on each drain. Read the notes
    /// on [`query`](matter_transport::Discovery::query) and
    /// [`stop_query`](matter_transport::Discovery::stop_query) about handle
    /// lifetime before you start — a handle that is never stopped keeps costing
    /// resources.
    ///
    /// The trait may also grow methods that carry a **default implementation**,
    /// so that adding one does not break existing implementors. Your type keeps
    /// compiling when that happens, but it silently takes the generic default
    /// until you override it — and a default is by definition the unrefined
    /// path (for instance, a fallback that browses every operational record
    /// rather than a narrowed subset). When you upgrade, check the trait for
    /// defaulted methods worth overriding.
    ///
    /// # Scope: this covers the controller's own resolution, not the servers
    ///
    /// The discovery you pass here is owned by the controller's actor task and
    /// is what every client operation resolves through — connecting to a node,
    /// commissioning, resubscribing.
    ///
    /// It is **not** used by the self-hosted server entry points
    /// ([`listen_for_checkin_once`](MatterController::listen_for_checkin_once),
    /// the `ota` feature's `serve_ota`, and the `unstable-provider` feature's
    /// `serve_provider_once`). Each of those runs
    /// off the actor on its own socket and needs a `Discovery` it exclusively
    /// owns for the duration of the call, which a single value moved into the
    /// actor cannot provide; they each construct their own `MdnsSdDiscovery`
    /// and use it only to publish and withdraw one operational record. So if
    /// you supply an Avahi-backed implementation and then serve OTA, your
    /// backend does the resolving while that record is still advertised through
    /// `mdns-sd`. If that matters to you, say so on [issue #113] — closing the
    /// gap means taking a discovery *factory* here rather than a value, and
    /// that is worth doing on demand rather than on speculation.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use matter_controller::{MatterController, ControllerStore};
    /// # async fn f(
    /// #     store: Arc<dyn ControllerStore>,
    /// #     my_discovery: impl matter_transport::Discovery + Send + 'static,
    /// # ) -> Result<(), Box<dyn std::error::Error>> {
    /// let controller = MatterController::builder(store)
    ///     .discovery(my_discovery)
    ///     .build()
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// [issue #113]: https://github.com/phunapps/matter-rust/issues/113
    #[must_use]
    pub fn discovery<D>(mut self, discovery: D) -> Self
    where
        // Exactly the bounds `Actor<T, D>` already states — `Send` so the
        // spawned actor future can move onto the multi-thread runtime. No
        // `Sync`: only the actor task ever touches it.
        D: matter_transport::Discovery + Send + 'static,
    {
        self.spawn_with_discovery = Some(Box::new(move |parts: SpawnParts| {
            MatterController::with_components_and_multicast_if(
                parts.store,
                parts.transport,
                discovery,
                Arc::new(matter_commissioning::SystemNocRng),
                parts.trust,
                parts.admin_vendor_id,
                parts.multicast_if,
                parts.response_deadline,
            )
        }));
        self
    }

    /// Bind the socket + discovery, load persisted state, and spawn the actor.
    ///
    /// Uses the discovery supplied to [`Self::discovery`], or starts the default
    /// [`MdnsSdDiscovery`](matter_transport::MdnsSdDiscovery) if none was.
    ///
    /// # Errors
    ///
    /// [`Error::Store`] / [`Error::Snapshot`] on load failure, or
    /// [`Error::Operational`] if the socket / mDNS cannot start.
    pub async fn build(self) -> Result<MatterController, Error> {
        let Self {
            store,
            trust,
            admin_vendor_id,
            multicast_if,
            response_deadline,
            spawn_with_discovery,
        } = self;

        let Some(spawn) = spawn_with_discovery else {
            // Untouched default path: bind the socket AND start `mdns-sd`.
            return MatterController::spawn_default(
                store,
                trust,
                admin_vendor_id,
                multicast_if,
                response_deadline,
            )
            .await;
        };

        // Same bind as `spawn_default` — only the discovery differs.
        let transport =
            matter_transport::TokioUdpTransport::bind_with_multicast_if(0, multicast_if)
                .await
                .map_err(|e| Error::Operational(format!("bind: {e}")))?;
        spawn(SpawnParts {
            store,
            transport,
            trust,
            admin_vendor_id,
            multicast_if,
            response_deadline,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use matter_transport::{Discovery, MatterService, QueryHandle, ServiceKind};

    use super::*;
    use crate::fabric::FabricConfig;
    use crate::store::StoreError;

    /// Sentinel carried by the injected discovery's `query` error. Seeing it come
    /// back out of a controller operation is proof that *this* implementation —
    /// not a freshly-constructed `MdnsSdDiscovery` — did the resolving.
    const SENTINEL: &str = "injected-discovery-was-used";

    /// A `Discovery` that counts `query` calls and always fails them, so an
    /// operation needing resolution fails fast and deterministically instead of
    /// waiting out a browse that will never produce a record.
    struct RecordingDiscovery(Arc<AtomicUsize>);

    impl Discovery for RecordingDiscovery {
        fn publish(&mut self, _s: &MatterService) -> matter_transport::Result<()> {
            Ok(())
        }
        fn unpublish(&mut self, _n: &str, _k: ServiceKind) -> matter_transport::Result<()> {
            Ok(())
        }
        fn query(&mut self, _k: ServiceKind) -> matter_transport::Result<QueryHandle> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(matter_transport::Error::Mdns(SENTINEL.to_string()))
        }
        fn stop_query(&mut self, _h: QueryHandle) {}
        fn poll_results(&mut self, _h: QueryHandle) -> Vec<MatterService> {
            Vec::new()
        }
    }

    /// In-memory store, mirroring the one in `actor`'s test module.
    #[derive(Default)]
    struct MemStore(std::sync::Mutex<Option<Vec<u8>>>);

    impl crate::store::ControllerStore for MemStore {
        fn load(&self) -> Result<Option<Vec<u8>>, StoreError> {
            Ok(self.0.lock().unwrap().clone())
        }
        fn save(&self, snapshot: &[u8]) -> Result<(), StoreError> {
            *self.0.lock().unwrap() = Some(snapshot.to_vec());
            Ok(())
        }
    }

    fn fabric_cfg() -> FabricConfig {
        FabricConfig {
            fabric_id: 0xAABB_CCDD_0000_0001,
            rcac_id: 1,
            commissioner_node_id: 1,
            validity: (
                matter_cert::MatterTime::from_unix_secs(1_700_000_000),
                matter_cert::MatterTime::NO_EXPIRY,
            ),
            issue_icac: false,
        }
    }

    /// The seam works: a `Discovery` handed to the builder is the one the actor
    /// resolves through. We drive a `read`, which must connect and therefore
    /// must open an operational browse — and our double is what answers.
    #[tokio::test]
    async fn supplied_discovery_is_used_for_resolution() {
        let queries = Arc::new(AtomicUsize::new(0));
        let controller = MatterController::builder(Arc::new(MemStore::default()))
            .discovery(RecordingDiscovery(queries.clone()))
            .build()
            .await
            .expect("build with injected discovery");

        controller
            .create_fabric(fabric_cfg())
            .await
            .expect("create_fabric");

        // Needs a session → needs a resolve → hits the injected discovery.
        let err = controller
            .node(0x1234)
            .read(&[crate::ReadPath::concrete(0, 0x0028, 0x0001)])
            .await
            .expect_err("read must fail: the injected discovery refuses to browse");

        assert!(
            err.to_string().contains(SENTINEL),
            "error must originate in the injected discovery, got: {err}"
        );
        // Two browses, not one: operational resolution asks for the
        // compressed-fabric subtype first (#113), and because this discovery
        // refuses to open that browse at all, the base-type fallback is opened
        // up front rather than after the usual delay — there is no running
        // subtype browse whose resolutions the fallback could starve. Both
        // browses go to the injected discovery, which is the point of the test.
        assert_eq!(
            queries.load(Ordering::SeqCst),
            2,
            "the injected discovery must serve both the subtype browse and the \
             base-type fallback"
        );
    }

    /// Regression guard for the non-breaking shape: because the builder stays a
    /// plain non-generic struct, a caller that never mentions `discovery` still
    /// compiles with no turbofish and no inference annotation.
    #[tokio::test]
    async fn builder_without_discovery_needs_no_turbofish() {
        let builder = MatterController::builder(Arc::new(MemStore::default()))
            .admin_vendor_id(0xFFF2)
            .multicast_interface(0);
        // Building would start the real `mdns-sd` daemon, which we do not want in
        // a unit test. Constructing and configuring the builder is what pins the
        // inference property; `build()`'s default path is covered elsewhere.
        drop(builder);
    }
}
