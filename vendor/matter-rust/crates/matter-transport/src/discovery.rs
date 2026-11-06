//! Sans-IO `Discovery` trait + Matter service-record types.
//!
//! The trait is what embedded callers implement to plug their own mDNS
//! stack into the rest of `matter-transport`. The default mdns-sd
//! adapter implements it over `mdns_sd::ServiceDaemon` when the
//! `mdns-sd` Cargo feature is enabled.
#![cfg_attr(
    feature = "mdns-sd",
    doc = "See [`crate::mdns_sd_discovery::MdnsSdDiscovery`] for that adapter."
)]

use std::collections::HashMap;
use std::net::IpAddr;

use crate::error::Result;

/// What kind of Matter service to publish or query.
///
/// Matter Core Spec §5.4 + §4.3.1 define three DNS-SD service types
/// the controller side interacts with.
///
/// `#[non_exhaustive]`: the spec may define further service types (and we may
/// surface group/border-router records later); marking this avoids a semver
/// break. Downstream `match`es must include a `_` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ServiceKind {
    /// `_matterc._udp` — devices in commissioning mode.
    Commissionable,
    /// `_matterd._udp` — commissioner (fabric admin) advertising itself
    /// while a commissioning window is open. Matter spec §5.4.2 requires
    /// commissioners to publish this for the duration of the window.
    Commissioner,
    /// `_matter._tcp` — operational nodes on an existing fabric.
    /// Despite the `_tcp` label (a DNS-SD convention quirk), operational
    /// Matter traffic is still UDP.
    Operational,
}

impl ServiceKind {
    /// DNS-SD service type string (with trailing dot, ready to pass to
    /// most mDNS libraries).
    #[must_use]
    pub const fn service_type(self) -> &'static str {
        match self {
            Self::Commissionable => "_matterc._udp.local.",
            Self::Commissioner => "_matterd._udp.local.",
            Self::Operational => "_matter._tcp.local.",
        }
    }
}

/// One Matter mDNS record — what we publish, what we discover.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct MatterService {
    /// DNS instance name (left-most label, e.g. `"DEADBEEFCAFEBABE-0000000000000001"`).
    pub instance_name: String,
    /// Service kind.
    pub kind: ServiceKind,
    /// Resolved IP addresses (may include both IPv4 and IPv6).
    pub addresses: Vec<IpAddr>,
    /// UDP/TCP port.
    pub port: u16,
    /// TXT records (vendor/product IDs, discriminator, etc.).
    ///
    /// Values are kept as raw bytes rather than `String`: DNS-SD TXT values
    /// are arbitrary octet strings, and while the Matter-defined keys
    /// (`D`, `CM`, `VP`, `PI`, …) carry ASCII, a lossy UTF-8 decode would
    /// silently corrupt any binary-valued or vendor-specific key. Callers
    /// that expect an ASCII/UTF-8 value decode the bytes themselves (see
    /// [`Self::txt_str`]).
    pub txt_records: HashMap<String, Vec<u8>>,
}

impl MatterService {
    /// Construct a [`MatterService`] from its mDNS record fields.
    ///
    /// Provided because the struct is `#[non_exhaustive]`: callers in other
    /// crates cannot use a struct literal, so this constructor is the stable
    /// way to build one. Future spec-driven fields will gain defaults here
    /// without breaking existing callers.
    #[must_use]
    pub fn new(
        instance_name: String,
        kind: ServiceKind,
        addresses: Vec<IpAddr>,
        port: u16,
        txt_records: HashMap<String, Vec<u8>>,
    ) -> Self {
        Self {
            instance_name,
            kind,
            addresses,
            port,
            txt_records,
        }
    }

    /// Look up a TXT key and return its value decoded as UTF-8, or `None`
    /// if the key is absent OR the value is not valid UTF-8.
    ///
    /// All Matter-defined TXT keys (discriminator `D`, commissioning mode
    /// `CM`, vendor/product `VP`, pairing instruction `PI`, …) carry ASCII
    /// values, so this is the right accessor for them. Binary or
    /// vendor-specific values that aren't valid UTF-8 are available raw via
    /// [`Self::txt_records`].
    #[must_use]
    pub fn txt_str(&self, key: &str) -> Option<&str> {
        self.txt_records
            .get(key)
            .and_then(|v| std::str::from_utf8(v).ok())
    }

    /// Parse the peer's advertised MRP retransmit parameters — mDNS TXT keys
    /// `SII` (Session Idle Interval), `SAI` (Session Active Interval), and
    /// `SAT` (Session Active Threshold), each a decimal-ASCII millisecond count
    /// (Matter Core Spec §4.3.1.8) — into an [`MrpConfig`](crate::MrpConfig).
    ///
    /// A device advertises these to tell the initiator how patiently to
    /// retransmit to it; a sleepy device advertises large `SII`. Any key that
    /// is absent, non-ASCII, or not a valid integer falls back to the Matter
    /// spec default inside [`MrpConfig::for_peer`](crate::MrpConfig::for_peer),
    /// which also clamps out-of-range values.
    #[must_use]
    pub fn peer_mrp_config(&self) -> crate::mrp::MrpConfig {
        let ms = |key: &str| {
            self.txt_str(key)
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(std::time::Duration::from_millis)
        };
        crate::mrp::MrpConfig::for_peer(ms("SII"), ms("SAI"), ms("SAT"))
    }
}

/// Opaque handle for an in-progress query. Returned by
/// [`Discovery::query`]; passed back to [`Discovery::poll_results`] and
/// [`Discovery::stop_query`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueryHandle(pub u64);

/// The DNS-SD **compressed-fabric subtype** of `_matter._tcp` for one fabric:
/// `_I<compressed-fabric-id>._sub._matter._tcp.local.`, with the id as
/// fixed-width uppercase hex (16 chars).
///
/// Matter Core Spec §4.3.1 requires every operational node to publish its
/// `_matter._tcp` record under this subtype, and it exists precisely so a
/// controller can ask for *its own fabric's* nodes instead of enumerating every
/// operational node on the link. That matters in practice: browsing the base
/// type on a busy LAN returns every node of every fabric, and an mDNS resolver
/// that resolves instances one at a time with exponential backoff (mdns-sd
/// does) may never reach a given node's SRV/TXT/address resolution inside a
/// caller's budget — the failure reported in issue #113, where 18 base-type
/// instances yielded no usable record in 30 s while the subtype resolved the
/// three nodes of the reporter's fabric in ~266 ms.
///
/// The hex formatting matches the first field of the operational instance name
/// (`matter_commissioning::driver::operational_instance_name`), which is
/// deliberate: subtype and instance name name the same fabric, and a
/// commissioning-side test pins the two against each other.
///
/// ```
/// # use matter_transport::operational_fabric_subtype;
/// assert_eq!(
///     operational_fabric_subtype([0xF5, 0x2A, 0xC1, 0x07, 0xC9, 0x54, 0xE3, 0x8E]),
///     "_IF52AC107C954E38E._sub._matter._tcp.local.",
/// );
/// ```
#[must_use]
pub fn operational_fabric_subtype(compressed_fabric_id: [u8; 8]) -> String {
    let cfid = u64::from_be_bytes(compressed_fabric_id);
    format!(
        "_I{cfid:016X}._sub.{}",
        ServiceKind::Operational.service_type()
    )
}

/// What an mDNS adapter must do to publish and query Matter services.
///
/// The default daemon adapter is available when the `mdns-sd` feature
/// is enabled.
#[cfg_attr(
    feature = "mdns-sd",
    doc = "It is implemented by [`crate::mdns_sd_discovery::MdnsSdDiscovery`]."
)]
pub trait Discovery {
    /// Publish a Matter service. Idempotent — re-publishing the same
    /// `(instance_name, kind)` replaces the prior advertisement.
    ///
    /// # Errors
    #[cfg_attr(
        feature = "mdns-sd",
        doc = "- [`Error::Mdns`](crate::error::Error::Mdns) on daemon failure."
    )]
    #[cfg_attr(
        not(feature = "mdns-sd"),
        doc = "- `Error::Mdns` on daemon failure (only present with the `mdns-sd` feature)."
    )]
    fn publish(&mut self, service: &MatterService) -> Result<()>;

    /// Withdraw a previously-published service.
    ///
    /// # Errors
    #[cfg_attr(
        feature = "mdns-sd",
        doc = "- [`Error::Mdns`](crate::error::Error::Mdns) on daemon failure."
    )]
    #[cfg_attr(
        not(feature = "mdns-sd"),
        doc = "- `Error::Mdns` on daemon failure (only present with the `mdns-sd` feature)."
    )]
    fn unpublish(&mut self, instance_name: &str, kind: ServiceKind) -> Result<()>;

    /// Begin a query for services of `kind`. Returns a handle the
    /// caller passes to [`Self::poll_results`] to drain matches as they
    /// arrive.
    ///
    /// # Every handle must be polled and stopped
    ///
    /// Discovered records are buffered per handle until that handle drains them
    /// with [`Self::poll_results`], so a handle that is registered and never
    /// polled accumulates records for as long as it lives. An implementation
    /// may additionally share one underlying browse between the handles of a
    /// kind and reference-count it — the default `mdns-sd` adapter does — in
    /// which case a leaked handle also holds that shared browse open, and with
    /// it whatever the OS-level browse costs (a socket, a background thread,
    /// periodic multicast queries).
    ///
    /// The realistic way to leak one is a dropped or cancelled future that
    /// never reaches its [`Self::stop_query`] — a `select!` branch that loses,
    /// a timeout that fires around an `await`, a task aborted mid-resolve. Pair
    /// every `query` with a `stop_query` on *all* paths (a guard type, or a
    /// `stop_query` in the caller that owns the handle rather than in the
    /// future that uses it).
    ///
    /// # Errors
    #[cfg_attr(
        feature = "mdns-sd",
        doc = "- [`Error::Mdns`](crate::error::Error::Mdns) on daemon failure."
    )]
    #[cfg_attr(
        not(feature = "mdns-sd"),
        doc = "- `Error::Mdns` on daemon failure (only present with the `mdns-sd` feature)."
    )]
    fn query(&mut self, kind: ServiceKind) -> Result<QueryHandle>;

    /// Begin a query restricted to the operational nodes of **one fabric**, via
    /// that fabric's DNS-SD compressed-fabric subtype
    /// ([`operational_fabric_subtype`]). The returned handle is used exactly
    /// like [`Self::query`]'s, and the records it yields are ordinary
    /// [`ServiceKind::Operational`] records.
    ///
    /// # Default implementation
    ///
    /// Delegates to `query(ServiceKind::Operational)` — i.e. an implementation
    /// that does not override this keeps today's behaviour (browse every
    /// operational node on the link and filter by instance name), which is
    /// always correct, just slower on a busy network. Overriding it is a pure
    /// optimisation, and callers must not assume the handle is subtype-scoped:
    /// they still match the instance name they are looking for.
    ///
    /// Because the default returns a *base-type* browse, a caller that opens
    /// both this and [`Self::query`] may receive the **same handle twice** from
    /// such an implementation; callers that poll both should treat two equal
    /// handles as one browse.
    ///
    /// # Why it exists
    ///
    /// See [`operational_fabric_subtype`]: on a link with many operational
    /// nodes, a resolver that works through base-type instances with
    /// exponential backoff may never resolve a given node inside a caller's
    /// budget, while the fabric subtype narrows the browse to the handful of
    /// nodes the controller actually shares a fabric with (issue #113).
    ///
    /// # Errors
    #[cfg_attr(
        feature = "mdns-sd",
        doc = "- [`Error::Mdns`](crate::error::Error::Mdns) on daemon failure."
    )]
    #[cfg_attr(
        not(feature = "mdns-sd"),
        doc = "- `Error::Mdns` on daemon failure (only present with the `mdns-sd` feature)."
    )]
    fn query_operational_fabric(&mut self, compressed_fabric_id: [u8; 8]) -> Result<QueryHandle> {
        // The id is unused here on purpose: without subtype support the honest
        // answer is the unfiltered operational browse.
        let _ = compressed_fabric_id;
        self.query(ServiceKind::Operational)
    }

    /// Stop an in-progress query. Idempotent — calling on a stopped or
    /// unknown handle is a no-op.
    ///
    /// This is what releases the handle's buffered records and, where the
    /// implementation shares one browse across the handles of a kind, its share
    /// of that browse (see [`Self::query`] on leaking a handle). Not optional
    /// housekeeping: a handle only stops costing anything once it is stopped.
    fn stop_query(&mut self, handle: QueryHandle);

    /// Drain any services discovered for `handle` since the last call.
    /// Non-blocking. Returns an empty vec if no new services have
    /// arrived. Repeat calls accumulate freshly-resolved services.
    fn poll_results(&mut self, handle: QueryHandle) -> Vec<MatterService>;

    /// Instance names (left-most label, the same form as
    /// [`MatterService::instance_name`]) for which this browse saw a **new**
    /// PTR record since the last call: an instance appearing, or re-appearing
    /// after its record expired. Event semantics: each name is returned once,
    /// is never replayed to a handle attached later, and says nothing about
    /// whether the instance has resolved yet. An instance the browse already
    /// reported as found is not reported again unless the resolver reported it
    /// removed in between: a resolver re-delivering a PTR it already holds (as
    /// a [`Self::requery`] may make it do) has not seen a new one. The default
    /// returns nothing; a `Discovery` that cannot observe PTR additions keeps
    /// it.
    ///
    /// # What it is for
    ///
    /// A controller waiting to re-establish a subscription to a node that went
    /// offline uses this as its "the node is back" signal; Apple's
    /// `Matter.framework` reacts to the same PTR add. It is deliberately not
    /// [`Self::poll_results`]: a resolved record is also re-delivered when
    /// nothing about the device changed (for example after another browse that
    /// named the same instances stops and the daemon re-resolves them), while a
    /// new PTR means the instance itself (re)appeared.
    ///
    /// Non-blocking, like [`Self::poll_results`]. An unknown or stopped handle
    /// returns an empty `Vec`.
    ///
    /// # Default implementation
    ///
    /// Returns an empty `Vec` for every handle. A caller reads that as "no
    /// adverts observed", which is always safe: it falls back to whatever it did
    /// without them.
    fn poll_found(&mut self, handle: QueryHandle) -> Vec<String> {
        // Unused on purpose: an implementation that cannot observe PTR
        // additions has nothing to report for any handle.
        let _ = handle;
        Vec::new()
    }

    /// Ask the resolver to send a fresh query for `handle`'s browse now, instead of
    /// waiting for its own retry schedule. Records that appear in response are
    /// reported through [`Self::poll_results`] / [`Self::poll_found`] as usual. A
    /// requery may make the resolver replay records it already holds:
    /// [`Self::poll_results`] may then deliver those records again (latest-wins
    /// consumers are unaffected), and [`Self::poll_found`] does not report an
    /// instance the browse already reported as found unless the resolver reported
    /// it removed in between. May restart the resolver's internal retry schedule.
    /// Poll the handle promptly for a while afterwards: a resolver that replays
    /// into a bounded channel, as `MdnsSdDiscovery`'s does, may stall until the
    /// replay is drained. The default does nothing, for a `Discovery` that
    /// cannot force a query.
    fn requery(&mut self, handle: QueryHandle) {
        // Unused on purpose: an implementation that cannot force a query has
        // nothing to do for any handle.
        let _ = handle;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::net::Ipv6Addr;

    #[test]
    fn service_kind_service_type_strings_exact() {
        assert_eq!(
            ServiceKind::Commissionable.service_type(),
            "_matterc._udp.local.",
        );
        assert_eq!(
            ServiceKind::Commissioner.service_type(),
            "_matterd._udp.local.",
        );
        assert_eq!(
            ServiceKind::Operational.service_type(),
            "_matter._tcp.local.",
        );
    }

    #[test]
    fn operational_fabric_subtype_is_uppercase_16_hex_under_sub() {
        // The exact string a Matter responder publishes its operational record
        // under (issue #113's reporter observed his nodes resolve in ~266 ms
        // through it while the base type stalled).
        assert_eq!(
            operational_fabric_subtype([0xF5, 0x2A, 0xC1, 0x07, 0xC9, 0x54, 0xE3, 0x8E]),
            "_IF52AC107C954E38E._sub._matter._tcp.local.",
        );
        // Leading zeroes are kept: the label is fixed-width, not minimal.
        assert_eq!(
            operational_fabric_subtype([0, 0, 0, 0, 0, 0, 0, 0x01]),
            "_I0000000000000001._sub._matter._tcp.local.",
        );
        // Lower-case hex digits must be emitted upper-case.
        let s = operational_fabric_subtype([0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89]);
        assert_eq!(s, "_IABCDEF0123456789._sub._matter._tcp.local.");
        // Structure: `_I` + 16 hex chars, then `._sub.` + the base type.
        let label = s.split("._sub.").next().unwrap();
        assert_eq!(label.len(), 18, "_I + 16 hex chars");
        assert!(label
            .strip_prefix("_I")
            .unwrap()
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b)));
        assert!(s.ends_with(ServiceKind::Operational.service_type()));
    }

    /// The trait's default `query_operational_fabric` must fall back to the
    /// base operational browse, so an out-of-tree implementation that never
    /// heard of subtypes keeps compiling AND keeps working.
    #[test]
    fn default_query_operational_fabric_falls_back_to_base_type() {
        struct LegacyDiscovery {
            queried: Vec<ServiceKind>,
        }
        impl Discovery for LegacyDiscovery {
            fn publish(&mut self, _s: &MatterService) -> Result<()> {
                Ok(())
            }
            fn unpublish(&mut self, _n: &str, _k: ServiceKind) -> Result<()> {
                Ok(())
            }
            fn query(&mut self, kind: ServiceKind) -> Result<QueryHandle> {
                self.queried.push(kind);
                Ok(QueryHandle(7))
            }
            fn stop_query(&mut self, _h: QueryHandle) {}
            fn poll_results(&mut self, _h: QueryHandle) -> Vec<MatterService> {
                Vec::new()
            }
        }

        let mut d = LegacyDiscovery {
            queried: Vec::new(),
        };
        let h = d.query_operational_fabric([0xF5, 0x2A, 0xC1, 0x07, 0xC9, 0x54, 0xE3, 0x8E]);
        assert_eq!(h.unwrap(), QueryHandle(7));
        assert_eq!(
            d.queried,
            vec![ServiceKind::Operational],
            "the default must open a plain operational browse",
        );
    }

    /// `poll_found` is additive — an implementation written before it
    /// existed keeps compiling and reports no found events for any handle.
    #[test]
    fn default_poll_found_reports_nothing() {
        struct PreFoundDiscovery;
        impl Discovery for PreFoundDiscovery {
            fn publish(&mut self, _s: &MatterService) -> Result<()> {
                Ok(())
            }
            fn unpublish(&mut self, _n: &str, _k: ServiceKind) -> Result<()> {
                Ok(())
            }
            fn query(&mut self, _k: ServiceKind) -> Result<QueryHandle> {
                Ok(QueryHandle(3))
            }
            fn stop_query(&mut self, _h: QueryHandle) {}
            fn poll_results(&mut self, _h: QueryHandle) -> Vec<MatterService> {
                Vec::new()
            }
        }

        let mut d = PreFoundDiscovery;
        let h = d.query(ServiceKind::Operational).unwrap();
        assert_eq!(d.poll_found(h), Vec::<String>::new());
        assert!(
            d.poll_found(QueryHandle(999)).is_empty(),
            "unknown handle too"
        );
    }

    /// `requery` is additive — an implementation written before it existed
    /// keeps compiling — and the default does nothing at all: it calls back
    /// into no method of the implementation.
    #[test]
    fn default_requery_does_nothing() {
        struct PreRequeryDiscovery {
            calls: usize,
        }
        impl Discovery for PreRequeryDiscovery {
            fn publish(&mut self, _s: &MatterService) -> Result<()> {
                self.calls += 1;
                Ok(())
            }
            fn unpublish(&mut self, _n: &str, _k: ServiceKind) -> Result<()> {
                self.calls += 1;
                Ok(())
            }
            fn query(&mut self, _k: ServiceKind) -> Result<QueryHandle> {
                self.calls += 1;
                Ok(QueryHandle(5))
            }
            fn stop_query(&mut self, _h: QueryHandle) {
                self.calls += 1;
            }
            fn poll_results(&mut self, _h: QueryHandle) -> Vec<MatterService> {
                self.calls += 1;
                Vec::new()
            }
        }

        let mut d = PreRequeryDiscovery { calls: 0 };
        let h = d.query(ServiceKind::Operational).unwrap();
        d.requery(h);
        d.requery(QueryHandle(999));
        assert_eq!(d.calls, 1, "only the query above; requery did nothing");
    }

    #[test]
    fn service_kind_hash_consistency() {
        let mut set = HashSet::new();
        set.insert(ServiceKind::Commissionable);
        assert!(set.contains(&ServiceKind::Commissionable));
        assert!(!set.contains(&ServiceKind::Operational));
    }

    #[test]
    fn peer_mrp_config_parses_txt_and_falls_back() {
        use std::time::Duration;
        // A device advertising SII/SAI/SAT (decimal ms) → parsed into MrpConfig.
        let mut txt = HashMap::new();
        txt.insert("SII".to_string(), b"5000".to_vec());
        txt.insert("SAI".to_string(), b"800".to_vec());
        txt.insert("SAT".to_string(), b"6000".to_vec());
        let svc = MatterService::new("N".to_string(), ServiceKind::Operational, vec![], 5540, txt);
        let c = svc.peer_mrp_config();
        assert_eq!(c.initial_idle, Duration::from_secs(5));
        assert_eq!(c.initial_active, Duration::from_millis(800));
        assert_eq!(c.idle_threshold, Duration::from_secs(6));

        // No MRP keys → spec defaults (500/300/4000).
        let bare = MatterService::new(
            "N".to_string(),
            ServiceKind::Operational,
            vec![],
            5540,
            HashMap::new(),
        );
        let d = bare.peer_mrp_config();
        assert_eq!(d.initial_idle, Duration::from_millis(500));
        assert_eq!(d.initial_active, Duration::from_millis(300));
        assert_eq!(d.idle_threshold, Duration::from_secs(4));
    }

    #[test]
    fn matter_service_roundtrip_equality() {
        let mut txt = HashMap::new();
        txt.insert("VP".to_string(), b"65521+32768".to_vec());
        txt.insert("D".to_string(), b"3840".to_vec());
        let svc = MatterService {
            instance_name: "DEADBEEFCAFEBABE-0000000000000001".to_string(),
            kind: ServiceKind::Commissionable,
            addresses: vec![std::net::IpAddr::V6(Ipv6Addr::LOCALHOST)],
            port: 5540,
            txt_records: txt,
        };
        assert_eq!(svc.clone(), svc);
    }

    #[test]
    fn txt_values_preserve_binary_bytes_and_parse_ascii() {
        // A binary (non-UTF-8) TXT value must survive verbatim, while an
        // ASCII key like the discriminator still parses cleanly.
        let mut txt = HashMap::new();
        let binary = vec![0x00u8, 0xFF, 0xFE, 0x80, 0x01];
        assert!(
            std::str::from_utf8(&binary).is_err(),
            "test fixture must be non-UTF-8 to be meaningful"
        );
        txt.insert("BIN".to_string(), binary.clone());
        txt.insert("D".to_string(), b"3840".to_vec());

        let svc = MatterService::new(
            "INSTANCE".to_string(),
            ServiceKind::Commissionable,
            vec![std::net::IpAddr::V6(Ipv6Addr::LOCALHOST)],
            5540,
            txt,
        );

        // Binary value round-trips with no corruption.
        assert_eq!(svc.txt_records.get("BIN"), Some(&binary));
        // The non-UTF-8 value yields None from the str accessor (not garbage).
        assert_eq!(svc.txt_str("BIN"), None);
        // ASCII discriminator parses normally.
        assert_eq!(svc.txt_str("D"), Some("3840"));
        assert_eq!(
            svc.txt_str("D").and_then(|d| d.parse::<u16>().ok()),
            Some(3840)
        );
    }

    #[test]
    fn query_handle_eq_hash_consistency() {
        let a = QueryHandle(1);
        let b = QueryHandle(1);
        let c = QueryHandle(2);
        assert_eq!(a, b);
        assert_ne!(a, c);
        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b));
        assert!(!set.contains(&c));
    }
}
