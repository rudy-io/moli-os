//! A cheap handle addressing one device node. Holds no session state.

use tokio::sync::oneshot;

use matter_codec::{Tag, TlvReader, TlvWriter, Value};
use matter_interaction::{
    build_invoke_request, build_invoke_request_timed, build_list_write_chunks,
    build_read_request_full, build_read_request_paths, build_write_request,
    build_write_request_timed, parse_invoke_response, parse_invoke_response_checked,
    parse_write_response, parse_write_response_checked, AttributePath, AttributeWriteRequest,
    CommandPath, EventFilter, EventPath, EventReport, ImStatus, InvokeResponse, ReadPath,
    ReportAccumulator, ReportData,
};

use crate::actor::Command;
use crate::error::Error;

pub(crate) const OP_READ_REQUEST: u8 = 0x02;
const OP_WRITE_REQUEST: u8 = 0x06;
pub(crate) const OP_INVOKE_REQUEST: u8 = 0x08;

/// Budget for a single `WriteRequestMessage` when writing the ACL list.
/// Stays well under `MAX_PAYLOAD_LEN` (1024 post-encryption); reserves
/// headroom for the secured-message header, MRP acks, and AES tag.
const WRITE_CHUNK_BUDGET: usize = 800;

/// Default timed-interaction timeout (milliseconds) used by
/// [`Node::write_timed`] / [`Node::invoke_timed`] when the caller passes `None`.
///
/// This is the window the **device** holds open for the follow-up Write/Invoke
/// after our `TimedRequest`. We send the action immediately, so this only needs
/// to cover the round-trip plus MRP retransmits; a chip-aligned 10s is generous.
pub const TIMED_DEFAULT_MS: u16 = 10_000;

/// Outcome of [`Node::invoke`].
///
/// `Debug` is written by hand, not derived: it prints every byte string in
/// `fields` as its length, and every string in the two responses known to
/// carry a PIN as a placeholder, because those hold credentials (see the
/// impl). Read `fields` directly to use the values.
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub enum InvokeResult {
    /// The device returned a response command with (anonymous-tagged) fields.
    Data {
        /// The response command path.
        path: CommandPath,
        /// The decoded response fields.
        ///
        /// Plaintext: some responses carry credentials in a byte string
        /// (a Wi-Fi passphrase, a Thread dataset). This `InvokeResult`'s
        /// `Debug` output shows each byte string's length only, and hides
        /// every string in the two responses that carry a PIN as a string
        /// (`AccountLogin` `GetSetupPINResponse`, `ContentControl`
        /// `ResetPINResponse`).
        fields: Value,
    },
    /// The device returned a bare status (e.g. `Success`).
    Status(ImStatus),
}

/// Hand-written so the response fields' secrets stay out of logs.
///
/// A `Data` result is the decoded response command, and some responses
/// carry credentials in octet strings: Wi-Fi Network Management
/// `NetworkPassphraseResponse` returns the Wi-Fi passphrase, and Thread
/// Border Router Management `DatasetResponse` and Thread Network Directory
/// `OperationalDatasetResponse` return an operational dataset, network key
/// included. So `fields` prints with its `Value` tree intact
/// (variant names, tags, integers, strings, nesting) but every byte-string
/// leaf as its length: `Bytes("<N bytes>")`.
///
/// Two responses carry a PIN in a character string instead (the private
/// `STRING_SECRET_RESPONSES` table): `AccountLogin` `GetSetupPINResponse` and
/// `ContentControl` `ResetPINResponse`. In those, every string leaf also
/// prints as `Utf8("<redacted>")`. Everything else prints exactly as a
/// derive would. Read `fields` directly to use the values.
impl std::fmt::Debug for InvokeResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Data { path, fields } => f
                .debug_struct("Data")
                .field("path", path)
                .field(
                    "fields",
                    &RedactedValue {
                        value: fields,
                        hide_strings: carries_string_secret(path),
                    },
                )
                .finish(),
            Self::Status(status) => f.debug_tuple("Status").field(status).finish(),
        }
    }
}

/// The response commands that carry a secret in a character string, as
/// `(cluster id, response command id)`. [`InvokeResult`]'s `Debug` prints
/// every string in these responses' fields as `"<redacted>"`.
///
/// Why a table of paths: a byte string can be hidden by its type alone,
/// because general-purpose payload bytes are worth little in a log. A
/// string cannot: names, labels and URLs are strings too, and hiding every
/// string would blind every invoke log. Nothing in a raw `Value` marks a
/// string as secret, so the only way to find one is by the response it
/// arrived in.
///
/// Add a response here whenever a new one carries a secret in a string.
/// The source list is the cluster dump's `SECRET_FIELDS`
/// (`xtask/scripts/dump-model/index.js`), which marks the fields whose
/// generated `matter-clusters` types redact: every string-typed entry there
/// needs a row here. The xtask test `controller_string_secret_table` reads
/// this table from source and fails when the two disagree. The ids are written out rather than taken from
/// `matter-clusters` so this table builds against every `matter-clusters`
/// release the controller supports, including ones that predate these
/// clusters.
///
/// Both ids are checked against connectedhomeip's
/// `data_model/1.4.2/clusters/AccountLogin.xml` and `ContentControl.xml`
/// (each identical at `v1.4.2.0` and master), its zap
/// `account-login-cluster.xml` / `content-control-cluster.xml`, and the
/// generated `matter-clusters` constants.
const STRING_SECRET_RESPONSES: [(u32, u32); 2] = [
    // AccountLogin (0x050E) GetSetupPINResponse (0x01): field 0 `SetupPIN`,
    // the setup PIN for PASE with the Commissionee. Generated as
    // `account_login::CLUSTER_ID` / `command_id::GET_SETUP_PIN_RESPONSE`.
    (0x050E, 0x01),
    // ContentControl (0x050F) ResetPINResponse (0x02): field 0 `PINCode`,
    // the new content-control PIN. Generated as
    // `content_control::CLUSTER_ID` / `command_id::RESET_PIN_RESPONSE`.
    (0x050F, 0x02),
];

/// Whether `path` is a response listed in [`STRING_SECRET_RESPONSES`]. The
/// endpoint plays no part: the same response carries the same secret on
/// any endpoint.
fn carries_string_secret(path: &CommandPath) -> bool {
    STRING_SECRET_RESPONSES.contains(&(path.cluster, path.command))
}

/// `Debug` view of a [`Value`] that prints each byte-string leaf as its
/// length, each string leaf as `"<redacted>"` when `hide_strings` is set,
/// and everything else as `Value`'s derived `Debug` does.
///
/// Containers are walked here, so a byte string or a hidden string at any
/// depth is caught. Scalars (`Bool`, `Uint`, `Int`, `Float`, `Double`,
/// `Null`, and `Utf8` when strings are shown) go to `Value`'s own `Debug`.
/// `Value` is `#[non_exhaustive]`, so a variant added to matter-codec later
/// also falls through to its own `Debug`: if it can hold bytes or text, add
/// an arm here.
struct RedactedValue<'a> {
    /// The value to print.
    value: &'a Value,
    /// Print every `Utf8` leaf as `"<redacted>"` (the response is in
    /// [`STRING_SECRET_RESPONSES`]).
    hide_strings: bool,
}

impl RedactedValue<'_> {
    /// The same view of a member of this value: `hide_strings` carries down.
    fn child<'b>(&self, value: &'b Value) -> RedactedValue<'b> {
        RedactedValue {
            value,
            hide_strings: self.hide_strings,
        }
    }
}

impl std::fmt::Debug for RedactedValue<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.value {
            Value::Bytes(bytes) => f
                .debug_tuple("Bytes")
                .field(&format!("<{} bytes>", bytes.len()))
                .finish(),
            Value::Utf8(_) if self.hide_strings => {
                f.debug_tuple("Utf8").field(&"<redacted>").finish()
            }
            Value::Structure(members) => f
                .debug_tuple("Structure")
                .field(&RedactedMembers(self, members))
                .finish(),
            Value::List(members) => f
                .debug_tuple("List")
                .field(&RedactedMembers(self, members))
                .finish(),
            Value::Array(items) => f
                .debug_tuple("Array")
                .field(&RedactedItems(self, items))
                .finish(),
            scalar => std::fmt::Debug::fmt(scalar, f),
        }
    }
}

/// The `(Tag, Value)` members of a structure or list, printed as the derive
/// prints a `Vec<(Tag, Value)>` but with each value redacted as its parent
/// (the first field) is.
struct RedactedMembers<'a, 'p>(&'p RedactedValue<'a>, &'a [(Tag, Value)]);

impl std::fmt::Debug for RedactedMembers<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.1.iter().map(|(tag, value)| (tag, self.0.child(value))))
            .finish()
    }
}

/// The elements of an array, printed as the derive prints a `Vec<Value>`
/// but with each element redacted as its parent (the first field) is.
struct RedactedItems<'a, 'p>(&'p RedactedValue<'a>, &'a [Value]);

impl std::fmt::Debug for RedactedItems<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.1.iter().map(|value| self.0.child(value)))
            .finish()
    }
}

/// `TimeSynchronization.Granularity` (Matter Core §11.17) — how precise the time
/// passed to [`Node::set_utc_time`] is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TimeGranularity {
    /// Time is not currently known.
    NoTime,
    /// Accurate to the minute.
    Minutes,
    /// Accurate to the second.
    Seconds,
    /// Accurate to the millisecond.
    Milliseconds,
    /// Accurate to the microsecond.
    Microseconds,
}

impl TimeGranularity {
    fn to_u8(self) -> u8 {
        match self {
            Self::NoTime => 0,
            Self::Minutes => 1,
            Self::Seconds => 2,
            Self::Milliseconds => 3,
            Self::Microseconds => 4,
        }
    }
}

/// One `TimeZoneStruct` entry for [`Node::set_time_zone`].
///
/// `#[non_exhaustive]`: construct via [`TimeZoneEntry::new`] so future optional
/// spec fields can be added without a breaking change.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct TimeZoneEntry {
    /// Offset from UTC in seconds (−43200..=50400).
    pub offset_seconds: i32,
    /// The `UTCTime` (epoch µs) at which this offset takes effect.
    pub valid_at_us: u64,
    /// Optional IANA time-zone name.
    pub name: Option<String>,
}

impl TimeZoneEntry {
    /// A time-zone entry: `offset_seconds` from UTC (−43200..=50400) taking
    /// effect at `valid_at_us` (epoch µs), with an optional IANA `name`.
    #[must_use]
    pub fn new(offset_seconds: i32, valid_at_us: u64, name: Option<String>) -> Self {
        Self {
            offset_seconds,
            valid_at_us,
            name,
        }
    }
}

/// One `DSTOffsetStruct` entry for [`Node::set_dst_offset`].
///
/// `#[non_exhaustive]`: construct via [`DstOffsetEntry::new`] so future optional
/// spec fields can be added without a breaking change.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DstOffsetEntry {
    /// DST offset in seconds added to the standard offset.
    pub offset_seconds: i32,
    /// The `UTCTime` (epoch µs) at which this DST offset starts.
    pub valid_starting_us: u64,
    /// The `UTCTime` (epoch µs) at which it stops, or `None` for indefinite.
    pub valid_until_us: Option<u64>,
}

impl DstOffsetEntry {
    /// A DST-offset entry: `offset_seconds` added to the standard offset,
    /// starting at `valid_starting_us` and ending at `valid_until_us` (both
    /// epoch µs; `None` end means indefinite).
    #[must_use]
    pub fn new(offset_seconds: i32, valid_starting_us: u64, valid_until_us: Option<u64>) -> Self {
        Self {
            offset_seconds,
            valid_starting_us,
            valid_until_us,
        }
    }
}

/// Extract `SetTimeZoneResponse.DSTOffsetRequired` (ctx0 bool) from a decoded
/// response `Value`.
fn dst_required_from_response(fields: &Value) -> Result<bool, Error> {
    if let Value::Structure(members) = fields {
        for (tag, v) in members {
            if *tag == Tag::Context(0) {
                if let Value::Bool(b) = v {
                    return Ok(*b);
                }
            }
        }
    }
    Err(Error::Operational(
        "SetTimeZoneResponse missing DSTOffsetRequired".into(),
    ))
}

/// Extract a `u32` from ctx0 of a decoded response `Value` (used for
/// `RegisterClientResponse.ICDCounter` and `StayActiveResponse.PromisedActiveDuration`).
fn u32_ctx0_from_response(fields: &Value, what: &'static str) -> Result<u32, Error> {
    if let Value::Structure(members) = fields {
        for (tag, v) in members {
            if *tag == Tag::Context(0) {
                if let Value::Uint(n) = v {
                    return u32::try_from(*n)
                        .map_err(|_| Error::Operational(format!("{what} exceeds u32 range")));
                }
            }
        }
    }
    Err(Error::Operational(format!("response missing {what}")))
}

/// Extract `RegisterClientResponse.ICDCounter` (ctx0 u32).
fn icd_counter_from_response(fields: &Value) -> Result<u32, Error> {
    u32_ctx0_from_response(fields, "ICDCounter")
}

/// Extract `StayActiveResponse.PromisedActiveDuration` (ctx0 u32).
fn promised_duration_from_response(fields: &Value) -> Result<u32, Error> {
    u32_ctx0_from_response(fields, "PromisedActiveDuration")
}

/// Encode a `Value` into a standalone anonymous-tagged TLV blob.
///
/// Exposed as `pub(crate)` so tests in sibling modules can encode ACL entry
/// values for chunk-count calculations without reaching through the public API.
///
/// # Errors
///
/// Returns [`Error::Codec`] if the TLV writer fails.
pub(crate) fn value_to_tlv(value: &Value) -> Result<Vec<u8>, Error> {
    let mut buf = Vec::new();
    let mut w = TlvWriter::new(&mut buf);
    w.write_value(Tag::Anonymous, value)?;
    Ok(buf)
}

/// Decode an anonymous-tagged TLV blob back into a `Value`.
///
/// # Errors
///
/// Returns [`Error::Codec`] if the TLV reader fails.
fn tlv_to_value(bytes: &[u8]) -> Result<Value, Error> {
    let mut r = TlvReader::new(bytes);
    let (_tag, value) = r.read_value()?;
    Ok(value)
}

/// Handle to one commissioned device. Obtain via
/// [`MatterController::node`](crate::controller::MatterController::node).
#[derive(Clone)]
pub struct Node {
    pub(crate) tx: tokio::sync::mpsc::Sender<Command>,
    pub(crate) node_id: u64,
}

impl Node {
    /// The device's operational node ID.
    #[must_use]
    pub fn node_id(&self) -> u64 {
        self.node_id
    }

    /// Send a raw secured Interaction-Model payload and await the response
    /// payload. Establishes/caches the CASE session transparently.
    ///
    /// A generic primitive retained for tests that exercise connect/cache/demux
    /// without IM payloads; the production verbs (`read`/`write`/`invoke`/
    /// `subscribe`) use the specialized actor commands.
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`] if the owning task has stopped, or any
    /// connect / transport / driver error.
    #[cfg(test)]
    pub(crate) async fn round_trip(
        &self,
        opcode: u8,
        protocol_id: matter_transport::ProtocolId,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::RoundTrip {
                node_id: self.node_id,
                opcode,
                protocol_id,
                payload,
                reply: reply.into(),
            })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)?
    }

    /// Send a chunked read request and collect every `ReportData` chunk payload
    /// in order. A non-chunked read yields a single-element `Vec`.
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`] if the owning task has stopped, or any
    /// connect / transport / driver error.
    pub(crate) async fn round_trip_chunked(
        &self,
        payload: Vec<u8>,
    ) -> Result<Vec<ReportData>, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Read {
                node_id: self.node_id,
                payload,
                reply,
            })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)?
    }

    /// Run a timed interaction: send a `TimedRequest`, await
    /// `StatusResponse(SUCCESS)`, then send `action_payload` (opcode
    /// `action_opcode`) on the same exchange and return its response bytes.
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`] if the owning task stopped, or any
    /// connect / transport / driver error.
    pub(crate) async fn round_trip_timed(
        &self,
        timeout_ms: u16,
        action_opcode: u8,
        action_payload: Vec<u8>,
    ) -> Result<Vec<u8>, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::TimedRoundTrip {
                node_id: self.node_id,
                timeout_ms,
                action_opcode,
                action_payload,
                reply: reply.into(),
            })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)?
    }

    /// Send a multi-chunk write: each element of `chunks` is one
    /// `WriteRequestMessage` (built by
    /// [`build_list_write_chunks`](matter_interaction::build_list_write_chunks),
    /// which encodes `MoreChunkedMessages` explicitly on every chunk of a
    /// multi-chunk sequence — `true` on all but the last, explicit `false` on
    /// the last, as chip's sticky-flag parser requires). All chunks go on
    /// ONE exchange, ONE in flight at a time — the actor sends the next chunk
    /// only after the device's `WriteResponseMessage` to the previous one
    /// (chip's `WriteClient` parity; see `Actor::handle_chunked_write` /
    /// `Actor::resolve_chunked_write`).
    ///
    /// Every chunk's `WriteResponseMessage` is parsed and its per-path
    /// statuses accumulated; the next chunk is sent regardless of what those
    /// statuses were (chip's `WriteClient` does not abort on a non-Success
    /// element status — it pumps every chunk unconditionally). Returns the
    /// FULL accumulated per-path status list on success.
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`] if the owning task stopped; an interaction
    /// error if a `WriteResponse` fails to parse; [`Error::Operational`] if
    /// the device rejects a chunk outright (e.g. with a `StatusResponse`
    /// instead of a `WriteResponse`) rather than returning a per-path status;
    /// or any connect / transport / driver error.
    pub(crate) async fn chunked_write(
        &self,
        chunks: Vec<Vec<u8>>,
    ) -> Result<Vec<(AttributePath, ImStatus)>, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::ChunkedWrite {
                node_id: self.node_id,
                chunks,
                reply,
            })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)?
    }

    /// The controller's commissioner node id (the sole fabric's
    /// `commissioner.node_id`). Used by the ACL lockout guard to avoid writing
    /// an ACL that would lock the commissioner out of the device.
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`] if the owning task stopped, or
    /// [`Error::NotCommissioned`] if no sole fabric exists.
    pub(crate) async fn commissioner_node_id(&self) -> Result<u64, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::CommissionerNodeId { reply })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)?
    }

    /// Read attributes (concrete or wildcard paths). Returns the device's
    /// `(path, value)` reports keyed by the concrete paths it reports. Values
    /// are raw [`Value`]; decode them with `matter-clusters` codecs.
    ///
    /// A wildcard read (e.g. [`ReadPath::all`]) whose response spans multiple
    /// `ReportData` chunks is reassembled transparently — every chunk is
    /// solicited and merged through [`ReportAccumulator`], so the result is the
    /// device's complete attribute set, not just the first chunk.
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`], any connect/transport error, or
    /// [`Error::InteractionModel`] if a response chunk cannot be parsed.
    pub async fn read(&self, paths: &[ReadPath]) -> Result<Vec<(AttributePath, Value)>, Error> {
        let req = build_read_request_paths(paths);
        // Chunks arrive already parsed (the actor's receive path parses each
        // `ReportData` exactly once); merge them without re-walking the TLV.
        let chunks = self.round_trip_chunked(req).await?;
        let mut acc = ReportAccumulator::new();
        for chunk in chunks {
            acc.push(chunk)?;
        }
        Ok(acc.finish())
    }

    /// Read events for the given (concrete or wildcard) event paths, optionally
    /// filtered to events with number `>= event_min` (via [`EventFilter`]).
    /// Returns every reported [`EventReport`] in wire order, reassembled across
    /// chunks. Decode the event payloads with `matter-clusters` codecs.
    ///
    /// Events are discrete records (not list attributes), so — unlike
    /// [`read`](Self::read) — there is no merge step: each chunk's events are
    /// concatenated in arrival order.
    ///
    /// # Errors
    ///
    /// [`Error::ControllerStopped`], any connect/transport error, or
    /// [`Error::InteractionModel`] if a response chunk cannot be parsed.
    pub async fn read_events(
        &self,
        paths: &[EventPath],
        filters: &[EventFilter],
    ) -> Result<Vec<EventReport>, Error> {
        let req = build_read_request_full(&[], paths, filters);
        let chunks = self.round_trip_chunked(req).await?;
        let mut events = Vec::new();
        for chunk in chunks {
            events.extend(chunk.events);
        }
        Ok(events)
    }

    /// Run a write/invoke `Action` through the actor: the actor consults the
    /// learned timed-cache (skips the plain attempt for known-timed paths) and
    /// transparently retries timed on a `NEEDS_TIMED_INTERACTION` rejection.
    /// Returns the final response bytes.
    ///
    /// `timed_payload` is a *builder*: the timed variant is encoded only if the
    /// actor actually needs it (cache hit or `0xc6` escalation), so the common
    /// plain path pays for one encode, not two.
    async fn action(
        &self,
        opcode: u8,
        plain_payload: Vec<u8>,
        timed_payload: crate::actor::TimedPayload,
        keys: Vec<(u32, u32)>,
    ) -> Result<Vec<u8>, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Action {
                node_id: self.node_id,
                opcode,
                plain_payload,
                timed_payload,
                keys,
                timeout_ms: TIMED_DEFAULT_MS,
                reply: reply.into(),
            })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)?
    }

    /// Write attributes. Each `Value` is TLV-encoded into the write payload.
    /// Returns the per-path statuses the device reported.
    ///
    /// Timed writes are handled transparently: if the device rejects the write
    /// with `NEEDS_TIMED_INTERACTION`, the controller retries it as a timed
    /// interaction and remembers the path so later writes skip the wasted attempt.
    /// Use [`write_timed`](Self::write_timed) to force the timed path explicitly.
    ///
    /// # Errors
    ///
    /// As [`Self::read`], plus [`Error::Codec`] if a value fails to encode.
    pub async fn write(
        &self,
        writes: &[(AttributePath, Value)],
    ) -> Result<Vec<(AttributePath, ImStatus)>, Error> {
        let mut reqs = Vec::with_capacity(writes.len());
        for (path, value) in writes {
            reqs.push(AttributeWriteRequest {
                path: *path,
                value_tlv: value_to_tlv(value)?,
            });
        }
        let keys = writes
            .iter()
            .map(|(p, _)| (p.cluster, p.attribute))
            .collect();
        let plain = build_write_request(&reqs);
        let resp = self
            .action(
                OP_WRITE_REQUEST,
                plain,
                Box::new(move || build_write_request_timed(&reqs)),
                keys,
            )
            .await?;
        Ok(parse_write_response(&resp)?)
    }

    /// Write before an absolute deadline, including queueing and timed fallback.
    /// Dropping the future abandons further sends; sent packets cannot be recalled.
    /// Replies must unambiguously cover every requested path exactly once.
    ///
    /// # Errors
    /// As [`Self::write`]; deadline expiry returns [`Error::Operational`].
    pub async fn write_before(
        &self,
        writes: &[(AttributePath, Value)],
        deadline: std::time::Instant,
    ) -> Result<Vec<(AttributePath, ImStatus)>, Error> {
        let mut reqs = Vec::with_capacity(writes.len());
        for (path, value) in writes {
            reqs.push(AttributeWriteRequest {
                path: *path,
                value_tlv: value_to_tlv(value)?,
            });
        }
        let (reply, rx) = oneshot::channel();
        let operation = async {
            self.tx
                .send(Command::Action {
                    node_id: self.node_id,
                    opcode: OP_WRITE_REQUEST,
                    plain_payload: build_write_request(&reqs),
                    timed_payload: Box::new(move || build_write_request_timed(&reqs)),
                    keys: writes
                        .iter()
                        .map(|(p, _)| (p.cluster, p.attribute))
                        .collect(),
                    timeout_ms: TIMED_DEFAULT_MS,
                    reply: crate::actor::ActionReply::before(reply, deadline),
                })
                .await
                .map_err(|_| Error::ControllerStopped)?;
            rx.await.map_err(|_| Error::ControllerStopped)?
        };
        let resp = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), operation)
            .await
            .map_err(|_| Error::Operational("action deadline elapsed".into()))??;
        Ok(parse_write_response_checked(
            &resp,
            &writes.iter().map(|(path, _)| *path).collect::<Vec<_>>(),
        )?)
    }

    /// Like [`write`](Self::write) but always performs a **timed** interaction:
    /// a `TimedRequest` precedes the write (required by some attributes, e.g.
    /// certain `DoorLock` settings). `timeout_ms` defaults to [`TIMED_DEFAULT_MS`].
    ///
    /// Plain [`write`](Self::write) already auto-upgrades to timed on a
    /// `NEEDS_TIMED_INTERACTION` rejection; use this when you want to force the
    /// timed path explicitly (e.g. to avoid the first wasted round-trip, or for
    /// testing).
    ///
    /// # Errors
    ///
    /// As [`Self::write`].
    pub async fn write_timed(
        &self,
        writes: &[(AttributePath, Value)],
        timeout_ms: Option<u16>,
    ) -> Result<Vec<(AttributePath, ImStatus)>, Error> {
        let mut reqs = Vec::with_capacity(writes.len());
        for (path, value) in writes {
            reqs.push(AttributeWriteRequest {
                path: *path,
                value_tlv: value_to_tlv(value)?,
            });
        }
        let payload = build_write_request_timed(&reqs);
        let resp = self
            .round_trip_timed(
                timeout_ms.unwrap_or(TIMED_DEFAULT_MS),
                OP_WRITE_REQUEST,
                payload,
            )
            .await?;
        Ok(parse_write_response(&resp)?)
    }

    /// Invoke a command with raw `Value` fields (TLV-encoded into the payload).
    ///
    /// # Errors
    ///
    /// As [`Self::read`], plus [`Error::Codec`] if the fields fail to encode
    /// or the response fields cannot be decoded.
    pub async fn invoke(&self, path: CommandPath, fields: Value) -> Result<InvokeResult, Error> {
        self.invoke_tlv(path, value_to_tlv(&fields)?).await
    }

    /// Invoke before an absolute deadline, including queued work, timed fallback
    /// and reliable retransmissions. Dropping this future abandons further sends.
    /// Already transmitted packets cannot be recalled.
    /// The reply must contain exactly one response: status paths must match
    /// the full request; command-data paths must match its endpoint and cluster.
    ///
    /// # Errors
    /// As [`Self::invoke`]; deadline expiry returns [`Error::Operational`].
    pub async fn invoke_before(
        &self,
        path: CommandPath,
        fields: Value,
        deadline: std::time::Instant,
    ) -> Result<InvokeResult, Error> {
        let fields_tlv = value_to_tlv(&fields)?;
        let (reply, rx) = oneshot::channel();
        let operation = async {
            self.tx
                .send(Command::Action {
                    node_id: self.node_id,
                    opcode: OP_INVOKE_REQUEST,
                    plain_payload: build_invoke_request(path, &fields_tlv),
                    timed_payload: Box::new(move || build_invoke_request_timed(path, &fields_tlv)),
                    keys: vec![(path.cluster, path.command)],
                    timeout_ms: TIMED_DEFAULT_MS,
                    reply: crate::actor::ActionReply::before(reply, deadline),
                })
                .await
                .map_err(|_| Error::ControllerStopped)?;
            rx.await.map_err(|_| Error::ControllerStopped)?
        };
        let resp = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), operation)
            .await
            .map_err(|_| Error::Operational("action deadline elapsed".into()))??;
        match parse_invoke_response_checked(&resp, path)? {
            InvokeResponse::Status(s) => Ok(InvokeResult::Status(s)),
            InvokeResponse::Command { path, fields_tlv } => Ok(InvokeResult::Data {
                path,
                fields: tlv_to_value(&fields_tlv)?,
            }),
        }
    }

    /// Invoke a command with **pre-encoded** TLV command fields — e.g. the
    /// `Vec<u8>` returned by
    /// `matter_clusters::gen::<cluster>::encode_<command>()` — passed straight
    /// into the wire payload, avoiding a decode-then-re-encode round trip
    /// through [`Value`].
    ///
    /// `fields_tlv` must be the TLV-encoded command fields structure (an
    /// anonymous-tagged struct), exactly what the generated `encode_*` helpers
    /// produce; pass the empty-structure encoding for a no-field command.
    ///
    /// # Errors
    ///
    /// As [`Self::read`], plus [`Error::Codec`] if the response fields cannot be
    /// decoded.
    pub async fn invoke_tlv(
        &self,
        path: CommandPath,
        fields_tlv: Vec<u8>,
    ) -> Result<InvokeResult, Error> {
        let plain = build_invoke_request(path, &fields_tlv);
        let resp = self
            .action(
                OP_INVOKE_REQUEST,
                plain,
                Box::new(move || build_invoke_request_timed(path, &fields_tlv)),
                vec![(path.cluster, path.command)],
            )
            .await?;
        match parse_invoke_response(&resp)? {
            InvokeResponse::Status(s) => Ok(InvokeResult::Status(s)),
            InvokeResponse::Command { path, fields_tlv } => Ok(InvokeResult::Data {
                path,
                fields: tlv_to_value(&fields_tlv)?,
            }),
        }
    }

    /// Like [`invoke`](Self::invoke) but always performs a **timed** interaction
    /// (a `TimedRequest` precedes the command — required by some commands, e.g.
    /// `DoorLock` lock/unlock). `timeout_ms` defaults to [`TIMED_DEFAULT_MS`].
    ///
    /// Plain [`invoke`](Self::invoke) already auto-upgrades to timed on a
    /// `NEEDS_TIMED_INTERACTION` rejection; use this to force the timed path.
    ///
    /// # Errors
    ///
    /// As [`Self::invoke`].
    pub async fn invoke_timed(
        &self,
        path: CommandPath,
        fields: Value,
        timeout_ms: Option<u16>,
    ) -> Result<InvokeResult, Error> {
        self.invoke_timed_tlv(path, value_to_tlv(&fields)?, timeout_ms)
            .await
    }

    /// Invoke timed from the first packet, bounded by an absolute deadline.
    /// The deadline also covers queueing; dropping the future abandons further sends.
    /// Already transmitted packets cannot be recalled.
    /// The reply must contain exactly one response: status paths must match
    /// the full request; command-data paths must match its endpoint and cluster.
    ///
    /// # Errors
    /// As [`Self::invoke_timed`]; deadline expiry returns [`Error::Operational`].
    pub async fn invoke_timed_before(
        &self,
        path: CommandPath,
        fields: Value,
        timeout_ms: Option<u16>,
        deadline: std::time::Instant,
    ) -> Result<InvokeResult, Error> {
        let payload = build_invoke_request_timed(path, &value_to_tlv(&fields)?);
        let (reply, rx) = oneshot::channel();
        let operation = async {
            self.tx
                .send(Command::TimedRoundTrip {
                    node_id: self.node_id,
                    timeout_ms: timeout_ms.unwrap_or(TIMED_DEFAULT_MS),
                    action_opcode: OP_INVOKE_REQUEST,
                    action_payload: payload,
                    reply: crate::actor::ActionReply::before(reply, deadline),
                })
                .await
                .map_err(|_| Error::ControllerStopped)?;
            rx.await.map_err(|_| Error::ControllerStopped)?
        };
        let resp = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), operation)
            .await
            .map_err(|_| Error::Operational("action deadline elapsed".into()))??;
        match parse_invoke_response_checked(&resp, path)? {
            InvokeResponse::Status(s) => Ok(InvokeResult::Status(s)),
            InvokeResponse::Command { path, fields_tlv } => Ok(InvokeResult::Data {
                path,
                fields: tlv_to_value(&fields_tlv)?,
            }),
        }
    }

    /// Like [`invoke_tlv`](Self::invoke_tlv) (pre-encoded TLV fields) but always
    /// performs a **timed** interaction, mirroring
    /// [`invoke_timed`](Self::invoke_timed). `timeout_ms` defaults to
    /// [`TIMED_DEFAULT_MS`].
    ///
    /// # Errors
    ///
    /// As [`Self::invoke_tlv`].
    pub async fn invoke_timed_tlv(
        &self,
        path: CommandPath,
        fields_tlv: Vec<u8>,
        timeout_ms: Option<u16>,
    ) -> Result<InvokeResult, Error> {
        let payload = build_invoke_request_timed(path, &fields_tlv);
        let resp = self
            .round_trip_timed(
                timeout_ms.unwrap_or(TIMED_DEFAULT_MS),
                OP_INVOKE_REQUEST,
                payload,
            )
            .await?;
        match parse_invoke_response(&resp)? {
            InvokeResponse::Status(s) => Ok(InvokeResult::Status(s)),
            InvokeResponse::Command { path, fields_tlv } => Ok(InvokeResult::Data {
                path,
                fields: tlv_to_value(&fields_tlv)?,
            }),
        }
    }

    /// Trigger `AnnounceOTAProvider` on this device's
    /// `OtaSoftwareUpdateRequestor` (0x002A) cluster — telling the device that
    /// *we* (`provider_node_id`) are an OTA Provider it may query for firmware.
    /// Sent as a `SimpleAnnouncement` (the device decides when to act): it
    /// resolves us via operational mDNS, opens a CASE session to us, and invokes
    /// `QueryImage`.
    ///
    /// `provider_node_id` is our own operational node id; `vendor_id` is our
    /// vendor id; `endpoint` is the endpoint **on us** that hosts the
    /// `OtaSoftwareUpdateProvider` (0x0029) cluster. The command itself is
    /// invoked on the device's endpoint 0.
    ///
    /// This only fires the announcement — the provider-server half (serving the
    /// image over BDX) is a separate piece.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InteractionModel`] if the invoke fails to build or parse,
    /// or [`Error::Operational`] if the device rejects the command with a
    /// non-success IM status or answers with an unexpected response command.
    #[cfg(feature = "ota")]
    pub async fn announce_ota_provider(
        &self,
        provider_node_id: u64,
        vendor_id: u16,
        endpoint: u16,
    ) -> Result<(), Error> {
        use matter_clusters::gen::ota_software_update_requestor::{
            command_id::ANNOUNCE_OTA_PROVIDER, encode_announce_ota_provider,
            AnnouncementReasonEnum, CLUSTER_ID,
        };
        let fields_tlv = encode_announce_ota_provider(
            provider_node_id,
            vendor_id,
            AnnouncementReasonEnum::SimpleAnnouncement,
            None,
            endpoint,
        );
        let fields = tlv_to_value(&fields_tlv)?;
        let path = CommandPath {
            endpoint: 0,
            cluster: CLUSTER_ID,
            command: ANNOUNCE_OTA_PROVIDER,
        };
        match self.invoke(path, fields).await? {
            InvokeResult::Status(ImStatus::Success) => Ok(()),
            InvokeResult::Status(ImStatus::Failure(code)) => Err(Error::Operational(format!(
                "AnnounceOTAProvider rejected (IM status {code:#04x})"
            ))),
            InvokeResult::Status(_) => Err(Error::Operational(
                "unrecognised IM status for AnnounceOTAProvider".into(),
            )),
            InvokeResult::Data { .. } => Err(Error::Operational(
                "unexpected response command for AnnounceOTAProvider".into(),
            )),
        }
    }

    /// Set the device's wall-clock via `TimeSynchronization.SetUTCTime`
    /// (0x0038 cmd 0x00). `utc_us` is microseconds since the Matter epoch
    /// (2000-01-01 UTC); `granularity` describes its precision.
    ///
    /// # Errors
    ///
    /// [`Error::Operational`] if the device rejects it (e.g. it already has a
    /// finer-granularity time), else an interaction error.
    pub async fn set_utc_time(
        &self,
        utc_us: u64,
        granularity: TimeGranularity,
    ) -> Result<(), Error> {
        let fields = Value::Structure(vec![
            (Tag::Context(0), Value::Uint(utc_us)),
            (Tag::Context(1), Value::Uint(u64::from(granularity.to_u8()))),
        ]);
        let path = CommandPath {
            endpoint: 0,
            cluster: 0x0038,
            command: 0x00,
        };
        match self.invoke(path, fields).await? {
            InvokeResult::Status(ImStatus::Success) => Ok(()),
            InvokeResult::Status(ImStatus::Failure(code)) => Err(Error::Operational(format!(
                "SetUTCTime rejected (IM status {code:#04x})"
            ))),
            InvokeResult::Status(_) => Err(Error::Operational(
                "unrecognised IM status for SetUTCTime".into(),
            )),
            InvokeResult::Data { .. } => Err(Error::Operational(
                "unexpected response command for SetUTCTime".into(),
            )),
        }
    }

    /// Set the device's time zone via `SetTimeZone` (0x0038 cmd 0x02). Returns
    /// the device's `DSTOffsetRequired` flag (whether you must also call
    /// [`Self::set_dst_offset`]).
    ///
    /// # Errors
    ///
    /// [`Error::Operational`] on device rejection or a malformed response, else
    /// an interaction error.
    pub async fn set_time_zone(&self, entries: &[TimeZoneEntry]) -> Result<bool, Error> {
        let list = entries
            .iter()
            .map(|e| {
                let mut members = vec![
                    (Tag::Context(0), Value::Int(i64::from(e.offset_seconds))),
                    (Tag::Context(1), Value::Uint(e.valid_at_us)),
                ];
                if let Some(name) = &e.name {
                    members.push((Tag::Context(2), Value::Utf8(name.clone())));
                }
                Value::Structure(members)
            })
            .collect();
        let fields = Value::Structure(vec![(Tag::Context(0), Value::Array(list))]);
        let path = CommandPath {
            endpoint: 0,
            cluster: 0x0038,
            command: 0x02,
        };
        match self.invoke(path, fields).await? {
            InvokeResult::Data { fields, .. } => dst_required_from_response(&fields),
            InvokeResult::Status(ImStatus::Failure(code)) => Err(Error::Operational(format!(
                "SetTimeZone rejected (IM status {code:#04x})"
            ))),
            InvokeResult::Status(_) => Err(Error::Operational(
                "SetTimeZone returned status, expected response".into(),
            )),
        }
    }

    /// Set the device's DST offsets via `SetDSTOffset` (0x0038 cmd 0x04).
    ///
    /// # Errors
    ///
    /// [`Error::Operational`] on device rejection, else an interaction error.
    pub async fn set_dst_offset(&self, entries: &[DstOffsetEntry]) -> Result<(), Error> {
        let list = entries
            .iter()
            .map(|e| {
                Value::Structure(vec![
                    (Tag::Context(0), Value::Int(i64::from(e.offset_seconds))),
                    (Tag::Context(1), Value::Uint(e.valid_starting_us)),
                    (
                        Tag::Context(2),
                        e.valid_until_us.map_or(Value::Null, Value::Uint),
                    ),
                ])
            })
            .collect();
        let fields = Value::Structure(vec![(Tag::Context(0), Value::Array(list))]);
        let path = CommandPath {
            endpoint: 0,
            cluster: 0x0038,
            command: 0x04,
        };
        match self.invoke(path, fields).await? {
            InvokeResult::Status(ImStatus::Success) => Ok(()),
            InvokeResult::Status(ImStatus::Failure(code)) => Err(Error::Operational(format!(
                "SetDSTOffset rejected (IM status {code:#04x})"
            ))),
            InvokeResult::Status(_) => Err(Error::Operational(
                "unrecognised IM status for SetDSTOffset".into(),
            )),
            InvokeResult::Data { .. } => Err(Error::Operational(
                "unexpected response command for SetDSTOffset".into(),
            )),
        }
    }

    /// Read the device's current `UTCTime` (0x0038 attr 0x00). `None` if the
    /// device reports a null time (clock not set).
    ///
    /// # Errors
    ///
    /// An interaction error if the read fails.
    pub async fn read_utc_time(&self) -> Result<Option<u64>, Error> {
        let reports = self.read(&[ReadPath::concrete(0, 0x0038, 0x0000)]).await?;
        Ok(reports.iter().find_map(|(p, v)| {
            if p.attribute == 0x0000 {
                if let Value::Uint(u) = v {
                    return Some(*u);
                }
            }
            None
        }))
    }

    /// Read this device's `Binding` list on `endpoint` (0x001E attr 0x0000) —
    /// the targets it is wired to send to.
    ///
    /// # Errors
    ///
    /// An interaction error if the read fails.
    pub async fn read_binding(
        &self,
        endpoint: u16,
    ) -> Result<Vec<crate::binding::BindingTarget>, Error> {
        let reports = self
            .read(&[ReadPath::concrete(
                endpoint,
                crate::binding::BINDING_CLUSTER,
                crate::binding::ATTR_BINDING,
            )])
            .await?;
        Ok(crate::binding::parse_bindings(&reports))
    }

    /// Replace this device's `Binding` list on `endpoint` with `targets` (a
    /// full-list, fabric-scoped write). Returns the per-path device status.
    ///
    /// # Errors
    ///
    /// An interaction error, or a per-path device status.
    pub async fn write_binding(
        &self,
        endpoint: u16,
        targets: &[crate::binding::BindingTarget],
    ) -> Result<Vec<(AttributePath, ImStatus)>, Error> {
        let path = AttributePath {
            endpoint,
            cluster: crate::binding::BINDING_CLUSTER,
            attribute: crate::binding::ATTR_BINDING,
        };
        let element_tlvs: Vec<Vec<u8>> = targets
            .iter()
            .map(|t| value_to_tlv(&crate::binding::binding_target_value(t)))
            .collect::<Result<_, _>>()?;
        let mut chunks = build_list_write_chunks(path, &element_tlvs, WRITE_CHUNK_BUDGET, false);
        if chunks.len() == 1 {
            // Single message: reuse the plain Action path. The timed retry —
            // if the device demands timed — re-sends identical bytes (safe for
            // a full-list replace), so the builder just hands back the chunk.
            let chunk = chunks.swap_remove(0);
            let plain = chunk.clone();
            let resp = self
                .action(
                    OP_WRITE_REQUEST,
                    plain,
                    Box::new(move || chunk),
                    vec![(path.cluster, path.attribute)],
                )
                .await?;
            Ok(parse_write_response(&resp)?)
        } else {
            self.chunked_write(chunks).await
        }
    }

    /// Register the controller as a check-in client with this ICD
    /// (`IcdManagement.RegisterClient`, 0x0046 cmd 0x00). Generates a fresh
    /// 16-byte symmetric key, registers our commissioner node id as the
    /// `CheckInNodeID`, persists an [`IcdRegistration`](crate::IcdRegistration)
    /// (so the check-in listener can later verify this device's Check-Ins), and
    /// returns it. `monitored_subject` is the subject the ICD watches for us
    /// (usually our node id).
    ///
    /// # Errors
    ///
    /// [`Error::Operational`] on RNG failure or device rejection; an interaction
    /// error; or a persistence error.
    pub async fn register_icd_client(
        &self,
        monitored_subject: u64,
        client_type: crate::icd::IcdClientType,
    ) -> Result<crate::icd::IcdRegistration, Error> {
        let check_in_node_id = self.commissioner_node_id().await?;
        let mut key = [0u8; 16];
        matter_crypto::random_bytes(&mut key)
            .map_err(|e| Error::Operational(format!("ICD key generation failed: {e}")))?;
        let fields = crate::icd::register_client_fields(
            check_in_node_id,
            monitored_subject,
            &key,
            client_type,
        );
        let path = CommandPath {
            endpoint: 0,
            cluster: crate::icd::ICD_MANAGEMENT_CLUSTER,
            command: 0x00,
        };
        let icd_counter = match self.invoke(path, fields).await? {
            InvokeResult::Data { fields, .. } => icd_counter_from_response(&fields)?,
            InvokeResult::Status(ImStatus::Failure(code)) => {
                return Err(Error::Operational(format!(
                    "RegisterClient rejected (IM status {code:#04x})"
                )))
            }
            InvokeResult::Status(_) => {
                return Err(Error::Operational(
                    "RegisterClient returned status, expected RegisterClientResponse".into(),
                ))
            }
        };
        let registration = crate::icd::IcdRegistration::new(
            self.node_id,
            check_in_node_id,
            monitored_subject,
            key,
            icd_counter,
        );
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::PersistIcdRegistration {
                registration: registration.clone(),
                reply,
            })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        rx.await.map_err(|_| Error::ControllerStopped)??;
        Ok(registration)
    }

    /// Unregister the controller from this ICD (`UnregisterClient`, cmd 0x02),
    /// using our commissioner node id as the `CheckInNodeID`.
    ///
    /// # Errors
    ///
    /// [`Error::Operational`] on device rejection, else an interaction error.
    pub async fn unregister_icd_client(&self) -> Result<(), Error> {
        let check_in_node_id = self.commissioner_node_id().await?;
        let fields = Value::Structure(vec![(Tag::Context(0), Value::Uint(check_in_node_id))]);
        let path = CommandPath {
            endpoint: 0,
            cluster: crate::icd::ICD_MANAGEMENT_CLUSTER,
            command: 0x02,
        };
        match self.invoke(path, fields).await? {
            InvokeResult::Status(ImStatus::Success) => Ok(()),
            InvokeResult::Status(ImStatus::Failure(code)) => Err(Error::Operational(format!(
                "UnregisterClient rejected (IM status {code:#04x})"
            ))),
            InvokeResult::Status(_) => Err(Error::Operational(
                "unrecognised IM status for UnregisterClient".into(),
            )),
            InvokeResult::Data { .. } => Err(Error::Operational(
                "unexpected response command for UnregisterClient".into(),
            )),
        }
    }

    /// Ask this ICD to stay in active mode for at least `stay_active_ms`
    /// (`StayActiveRequest`, cmd 0x03). Returns the device's promised active
    /// duration (ms).
    ///
    /// # Errors
    ///
    /// [`Error::Operational`] on device rejection or a malformed response, else
    /// an interaction error.
    pub async fn stay_active_request(&self, stay_active_ms: u32) -> Result<u32, Error> {
        let fields = Value::Structure(vec![(
            Tag::Context(0),
            Value::Uint(u64::from(stay_active_ms)),
        )]);
        let path = CommandPath {
            endpoint: 0,
            cluster: crate::icd::ICD_MANAGEMENT_CLUSTER,
            command: 0x03,
        };
        match self.invoke(path, fields).await? {
            InvokeResult::Data { fields, .. } => promised_duration_from_response(&fields),
            InvokeResult::Status(ImStatus::Failure(code)) => Err(Error::Operational(format!(
                "StayActiveRequest rejected (IM status {code:#04x})"
            ))),
            InvokeResult::Status(_) => Err(Error::Operational(
                "StayActiveRequest returned status, expected response".into(),
            )),
        }
    }

    /// Read `AdministratorCommissioning` `WindowStatus`, `AdminFabricIndex`, and
    /// `AdminVendorId` from endpoint 0. Returns a snapshot of the current
    /// commissioning-window state.
    ///
    /// # Errors
    ///
    /// An interaction error if the read fails.
    pub async fn commissioning_window_status(&self) -> Result<crate::admin::WindowStatus, Error> {
        use crate::admin::{
            ADMIN_COMMISSIONING_CLUSTER, ATTR_ADMIN_FABRIC_INDEX, ATTR_ADMIN_VENDOR_ID,
            ATTR_WINDOW_STATUS,
        };
        let paths = [
            ReadPath::concrete(0, ADMIN_COMMISSIONING_CLUSTER, ATTR_WINDOW_STATUS),
            ReadPath::concrete(0, ADMIN_COMMISSIONING_CLUSTER, ATTR_ADMIN_FABRIC_INDEX),
            ReadPath::concrete(0, ADMIN_COMMISSIONING_CLUSTER, ATTR_ADMIN_VENDOR_ID),
        ];
        let reports = self.read(&paths).await?;
        Ok(crate::admin::parse_window_status(&reports))
    }

    /// Read the device's `Fabrics` list (every fabric it is commissioned onto).
    ///
    /// This is the **device's** table, read over the wire — it includes other
    /// administrators' fabrics and says nothing about what this controller has
    /// created. For the controller's own fabrics (local, no network traffic)
    /// use
    /// [`MatterController::fabrics`](crate::MatterController::fabrics).
    ///
    /// # Errors
    ///
    /// An interaction error if the read fails.
    pub async fn list_fabrics(&self) -> Result<Vec<crate::opcreds::FabricDescriptor>, Error> {
        let paths = [ReadPath::concrete(
            0,
            crate::opcreds::OPERATIONAL_CREDENTIALS_CLUSTER,
            crate::opcreds::ATTR_FABRICS,
        )];
        let reports = self.read(&paths).await?;
        Ok(crate::opcreds::parse_fabrics(&reports))
    }

    /// Write the device's `AccessControl.Acl` list to exactly `entries`.
    ///
    /// Refuses (before sending) any list that would strip our own administrative
    /// access ([`Error::AclWouldLockOut`]). Small lists go in one
    /// `WriteRequestMessage` (byte-identical to a normal write); larger lists are
    /// chunked (`ReplaceAll`+`AppendItem`) without ever sending an empty `ReplaceAll`.
    ///
    /// ACL writes are NOT timed (the spec does not require `TimedRequest` for
    /// `AccessControl.Acl`); however, if the device unexpectedly rejects the write
    /// with `NEEDS_TIMED_INTERACTION` the controller's timed-auto-upgrade will
    /// transparently retry on the single-chunk path (the same bytes are safe to
    /// re-send because the whole list is idempotent). The multi-chunk path fails
    /// cleanly on a `0xc6` rejection (the `ChunkedWrite` pending does not carry
    /// a `timed_payload`).
    ///
    /// # Errors
    ///
    /// [`Error::AclWouldLockOut`] if `entries` contains no Administer/CASE entry
    /// covering our commissioner node id; no bytes are sent to the device in that
    /// case. Otherwise returns an interaction error or a per-path device status.
    pub async fn write_acl(
        &self,
        entries: &[crate::acl::AclEntry],
    ) -> Result<Vec<(AttributePath, ImStatus)>, Error> {
        self.write_acl_with_budget(entries, WRITE_CHUNK_BUDGET)
            .await
    }

    /// Inner implementation of [`write_acl`](Node::write_acl) with an injectable
    /// per-chunk byte budget.
    ///
    /// The lockout guard runs before any bytes are sent to the device, regardless
    /// of the budget. `budget` controls the `build_list_write_chunks` split point;
    /// the production verb always passes [`WRITE_CHUNK_BUDGET`] (800 bytes).
    ///
    /// Exposed as `pub(crate)` so tests can force a small budget (e.g. 40 bytes)
    /// to exercise the multi-chunk dispatch branch through `write_acl` itself
    /// rather than calling `chunked_write` directly.
    ///
    /// # Errors
    ///
    /// [`Error::AclWouldLockOut`] if `entries` contains no Administer/CASE entry
    /// covering our commissioner node id; no bytes are sent to the device in that
    /// case. Otherwise returns an interaction error or a per-path device status.
    pub(crate) async fn write_acl_with_budget(
        &self,
        entries: &[crate::acl::AclEntry],
        budget: usize,
    ) -> Result<Vec<(AttributePath, ImStatus)>, Error> {
        // Lockout guard MUST run before any network I/O.
        let our = self.commissioner_node_id().await?;
        if !crate::acl::acl_retains_admin(entries, our) {
            return Err(Error::AclWouldLockOut);
        }
        let path = AttributePath {
            endpoint: 0,
            cluster: crate::acl::ACCESS_CONTROL_CLUSTER,
            attribute: crate::acl::ATTR_ACL,
        };
        let element_tlvs: Vec<Vec<u8>> = entries
            .iter()
            .map(|e| value_to_tlv(&crate::acl::acl_entry_value(e)))
            .collect::<Result<_, _>>()?;
        let mut chunks = build_list_write_chunks(path, &element_tlvs, budget, false);
        if chunks.len() == 1 {
            // Single message: reuse the plain Action path (byte-identical to a
            // normal write, 0xc6 auto-upgrade intact). The timed builder hands
            // back the same chunk, so the retry — if the device demands timed —
            // re-sends identical bytes (safe for a full-list replace).
            let chunk = chunks.swap_remove(0);
            let plain = chunk.clone();
            let resp = self
                .action(
                    OP_WRITE_REQUEST,
                    plain,
                    Box::new(move || chunk),
                    vec![(path.cluster, path.attribute)],
                )
                .await?;
            Ok(parse_write_response(&resp)?)
        } else {
            self.chunked_write(chunks).await
        }
    }

    /// Read the device's `AccessControl.Acl` list (the ACL entries on this fabric).
    ///
    /// Entries belonging to other fabrics (whose fabric-sensitive fields the
    /// device withholds on this unfiltered read) are not returned, nor is any
    /// entry missing its Subjects or Targets field: a withheld field is never
    /// reported as a wildcard, so the result is safe to modify and pass back to
    /// [`Node::write_acl`].
    ///
    /// # Errors
    ///
    /// An interaction error if the read fails.
    pub async fn read_acl(&self) -> Result<Vec<crate::acl::AclEntry>, Error> {
        let paths = [ReadPath::concrete(
            0,
            crate::acl::ACCESS_CONTROL_CLUSTER,
            crate::acl::ATTR_ACL,
        )];
        let reports = self.read(&paths).await?;
        Ok(crate::acl::parse_acl(&reports))
    }

    /// Open an enhanced commissioning window using **caller-supplied** secrets.
    ///
    /// Internal seam behind the public [`Node::open_commissioning_window`], which
    /// generates the secrets. Kept `pub(crate)` rather than exposing a 7-argument
    /// public signature at 1.0; if a consumer ever needs caller-supplied secrets,
    /// re-expose it through an options struct (extend
    /// [`OpenWindowOpts`](crate::admin::OpenWindowOpts)).
    ///
    /// Computes the PAKE passcode verifier from `passcode`/`salt`/`iterations`,
    /// invokes `OpenCommissioningWindow` (a **timed** invoke — `AdminComm` requires
    /// it), and returns the onboarding payload.
    ///
    /// # Errors
    ///
    /// Returns [`Error::CommissioningWindowRejected`] if the device rejects the
    /// command, or a crypto/interaction error.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn open_commissioning_window_with(
        &self,
        timeout_s: u16,
        passcode: u32,
        salt: &[u8],
        discriminator: u16,
        iterations: u32,
        vendor_id: Option<u16>,
        product_id: Option<u16>,
    ) -> Result<crate::admin::CommissioningWindow, Error> {
        let verifier = matter_crypto::pake_passcode_verifier(passcode, salt, iterations)
            .map_err(|e| Error::Operational(format!("verifier: {e}")))?;
        let fields =
            crate::admin::open_window_fields(timeout_s, &verifier, discriminator, iterations, salt);
        let path = CommandPath {
            endpoint: 0,
            cluster: crate::admin::ADMIN_COMMISSIONING_CLUSTER,
            command: crate::admin::CMD_OPEN_COMMISSIONING_WINDOW,
        };
        self.admin_timed_command(path, fields).await?;
        let (manual_code, qr_code) =
            crate::admin::onboarding_payload(passcode, discriminator, vendor_id, product_id)?;
        Ok(crate::admin::CommissioningWindow {
            passcode,
            discriminator,
            iterations,
            salt: salt.to_vec(),
            manual_code,
            qr_code,
        })
    }

    /// Open an enhanced commissioning window so a second admin can commission
    /// this device onto its own fabric. Generates a fresh passcode/salt/
    /// discriminator, computes the PAKE verifier, and returns the onboarding
    /// payload (manual pairing code, plus QR when `opts.vendor_id`/`product_id`
    /// are set). The `AdminComm` command is sent as a timed invoke.
    ///
    /// The returned [`CommissioningWindow::manual_code`] identifies the device
    /// less precisely than its QR code does: a manual pairing code carries
    /// only the upper 4 bits of the discriminator, so parsing it back does not
    /// recover [`CommissioningWindow::discriminator`]. See
    /// [`CommissioningWindow::manual_code`] for the details.
    ///
    /// [`CommissioningWindow::manual_code`]: crate::CommissioningWindow::manual_code
    /// [`CommissioningWindow::discriminator`]: crate::CommissioningWindow::discriminator
    ///
    /// # Errors
    /// Returns [`Error::CommissioningWindowRejected`] if the device rejects it,
    /// or a crypto/RNG/interaction error.
    pub async fn open_commissioning_window(
        &self,
        opts: crate::admin::OpenWindowOpts,
    ) -> Result<crate::admin::CommissioningWindow, Error> {
        let (passcode, salt, discriminator) = crate::admin::random_window_secrets()?;
        self.open_commissioning_window_with(
            opts.timeout_s,
            passcode,
            &salt,
            discriminator,
            opts.iterations,
            opts.vendor_id,
            opts.product_id,
        )
        .await
    }

    /// Open a *basic* commissioning window (reuses the device's original
    /// passcode — no new onboarding payload). Timed invoke.
    ///
    /// # Errors
    /// [`Error::CommissioningWindowRejected`] on device rejection, else an
    /// interaction error.
    pub async fn open_basic_commissioning_window(&self, timeout_s: u16) -> Result<(), Error> {
        let fields = matter_codec::Value::Structure(vec![(
            matter_codec::Tag::Context(0),
            matter_codec::Value::Uint(u64::from(timeout_s)),
        )]);
        let path = CommandPath {
            endpoint: 0,
            cluster: crate::admin::ADMIN_COMMISSIONING_CLUSTER,
            command: crate::admin::CMD_OPEN_BASIC_COMMISSIONING_WINDOW,
        };
        self.admin_timed_command(path, fields).await
    }

    /// Revoke any open commissioning window. Timed invoke. Returns `Ok(())`
    /// even if no window was open (the device reports `WindowNotOpen`, which is
    /// surfaced as [`Error::CommissioningWindowRejected`] only on a hard IM
    /// failure).
    ///
    /// # Errors
    /// [`Error::CommissioningWindowRejected`] on device rejection.
    pub async fn revoke_commissioning(&self) -> Result<(), Error> {
        let fields = matter_codec::Value::Structure(vec![]);
        let path = CommandPath {
            endpoint: 0,
            cluster: crate::admin::ADMIN_COMMISSIONING_CLUSTER,
            command: crate::admin::CMD_REVOKE_COMMISSIONING,
        };
        self.admin_timed_command(path, fields).await
    }

    /// Shared helper: timed-invoke an `AdminComm` command expecting a bare
    /// success status. Maps `Success` to `Ok(())`, `Failure(code)` to
    /// [`Error::CommissioningWindowRejected`], any other `Status(_)` variant
    /// (catch-all for `#[non_exhaustive]` future codes) to an operational
    /// error, and any response command to an operational error.
    async fn admin_timed_command(
        &self,
        path: CommandPath,
        fields: matter_codec::Value,
    ) -> Result<(), Error> {
        match self.invoke_timed(path, fields, None).await? {
            InvokeResult::Status(ImStatus::Success) => Ok(()),
            InvokeResult::Status(ImStatus::Failure(code)) => {
                Err(Error::CommissioningWindowRejected(code))
            }
            InvokeResult::Status(_) => Err(Error::Operational(
                "unrecognised IM status for admin command".into(),
            )),
            InvokeResult::Data { .. } => {
                Err(Error::Operational("unexpected response command".into()))
            }
        }
    }

    /// Remove a fabric from the device by its `fabric_index`.
    ///
    /// Reads `CurrentFabricIndex` first and refuses to remove our OWN fabric
    /// (that would sever this CASE session and orphan persisted state) with
    /// [`Error::WouldRemoveSelf`]. There is intentionally no `force` override.
    ///
    /// # Errors
    /// [`Error::WouldRemoveSelf`] if `fabric_index` is our own;
    /// [`Error::Operational`] if the device does not return a readable
    /// `CurrentFabricIndex` — the call fails without invoking `RemoveFabric` in
    /// that case (fail-closed on a destructive operation);
    /// [`Error::OperationalCredentialsRejected`] if the device rejects it (e.g.
    /// 7 `InvalidFabricIndex`); else an interaction error.
    pub async fn remove_fabric(&self, fabric_index: u8) -> Result<(), Error> {
        // Self-protection: CurrentFabricIndex over our session is OUR fabric's
        // index here. Must check BEFORE invoking — this is a destructive op.
        let cur = self
            .read(&[ReadPath::concrete(
                0,
                crate::opcreds::OPERATIONAL_CREDENTIALS_CLUSTER,
                crate::opcreds::ATTR_CURRENT_FABRIC_INDEX,
            )])
            .await?;
        // Fail CLOSED: if we cannot read CurrentFabricIndex we refuse to
        // proceed. The equality guard `== Some(fabric_index)` would silently
        // fall through when `parse_current_fabric_index` returns `None`,
        // allowing RemoveFabric on an unverified index.
        let cur_idx = crate::opcreds::parse_current_fabric_index(&cur).ok_or_else(|| {
            Error::Operational(
                "could not read CurrentFabricIndex; refusing remove_fabric for safety".into(),
            )
        })?;
        if cur_idx == fabric_index {
            return Err(Error::WouldRemoveSelf);
        }
        let fields = Value::Structure(vec![(
            matter_codec::Tag::Context(0),
            Value::Uint(u64::from(fabric_index)),
        )]);
        let path = CommandPath {
            endpoint: 0,
            cluster: crate::opcreds::OPERATIONAL_CREDENTIALS_CLUSTER,
            command: crate::opcreds::CMD_REMOVE_FABRIC,
        };
        match self.invoke(path, fields).await? {
            InvokeResult::Data { fields, .. } => {
                let status = crate::opcreds::parse_noc_response(&fields);
                crate::opcreds::noc_status_to_result(&status)
            }
            InvokeResult::Status(ImStatus::Success) => Ok(()),
            InvokeResult::Status(ImStatus::Failure(code)) => {
                Err(Error::OperationalCredentialsRejected(code))
            }
            InvokeResult::Status(_) => Err(Error::Operational(
                "unexpected status for RemoveFabric".into(),
            )),
        }
    }

    /// Update the label of OUR fabric on the device (`UpdateFabricLabel` acts on
    /// the accessing fabric; there is no index argument).
    ///
    /// # Errors
    /// [`Error::OperationalCredentialsRejected`] if the device rejects it
    /// (e.g. 9 `LabelConflict`); else an interaction error.
    pub async fn update_fabric_label(&self, label: &str) -> Result<(), Error> {
        let fields = Value::Structure(vec![(
            matter_codec::Tag::Context(0),
            Value::Utf8(label.to_string()),
        )]);
        let path = CommandPath {
            endpoint: 0,
            cluster: crate::opcreds::OPERATIONAL_CREDENTIALS_CLUSTER,
            command: crate::opcreds::CMD_UPDATE_FABRIC_LABEL,
        };
        match self.invoke(path, fields).await? {
            InvokeResult::Data { fields, .. } => {
                let status = crate::opcreds::parse_noc_response(&fields);
                crate::opcreds::noc_status_to_result(&status)
            }
            InvokeResult::Status(ImStatus::Success) => Ok(()),
            InvokeResult::Status(ImStatus::Failure(code)) => {
                Err(Error::OperationalCredentialsRejected(code))
            }
            InvokeResult::Status(_) => Err(Error::Operational(
                "unexpected status for UpdateFabricLabel".into(),
            )),
        }
    }

    /// Add the device endpoint to a group (`Groups.AddGroup`). The endpoint then
    /// joins the group's multicast address and accepts group commands.
    ///
    /// # Errors
    ///
    /// [`Error::GroupCommandRejected`] on a non-success status; else interaction error.
    pub async fn add_group(&self, endpoint: u16, group_id: u16, name: &str) -> Result<(), Error> {
        self.group_command(
            endpoint,
            crate::group::CMD_ADD_GROUP,
            crate::group::add_group_fields(group_id, name),
        )
        .await
    }

    /// Remove the device endpoint from a group (`Groups.RemoveGroup`).
    ///
    /// # Errors
    ///
    /// [`Error::GroupCommandRejected`] on a non-success status; else interaction error.
    pub async fn remove_group(&self, endpoint: u16, group_id: u16) -> Result<(), Error> {
        self.group_command(
            endpoint,
            crate::group::CMD_REMOVE_GROUP,
            crate::group::remove_group_fields(group_id),
        )
        .await
    }

    /// Shared: invoke a `Groups` command and map its response-status to `()`/error.
    ///
    /// `AddGroup`/`RemoveGroup` both return a response command whose `status` field
    /// (context tag 0) is 0 on success or a non-zero `GroupClusterStatus` code on
    /// failure. A bare `Success` IM status is also accepted (some devices skip the
    /// response command on success); bare `Failure` codes become
    /// [`Error::GroupCommandRejected`].
    async fn group_command(&self, endpoint: u16, command: u32, fields: Value) -> Result<(), Error> {
        let path = CommandPath {
            endpoint,
            cluster: crate::group::GROUPS_CLUSTER,
            command,
        };
        match self.invoke(path, fields).await? {
            InvokeResult::Data { fields, .. } => {
                let status = crate::group::parse_group_status(&fields);
                if status == 0 {
                    Ok(())
                } else {
                    Err(Error::GroupCommandRejected(status))
                }
            }
            InvokeResult::Status(ImStatus::Success) => Ok(()),
            InvokeResult::Status(ImStatus::Failure(code)) => Err(Error::GroupCommandRejected(code)),
            InvokeResult::Status(_) => Err(Error::Operational(
                "unexpected status for Groups command".into(),
            )),
        }
    }

    /// Provision a group key set on the device via `KeySetWrite`
    /// (`GroupKeyManagement` cluster, endpoint 0). The epoch key is the
    /// group's symmetric key material. Returns `Ok(())` on a bare
    /// `Success` status from the device.
    ///
    /// `KeySetWrite` is NOT a timed command — the plain `invoke` path is used.
    ///
    /// # Errors
    ///
    /// [`Error::GroupCommandRejected`] if the device returns a non-success IM
    /// status (e.g. `ResourceExhausted`). An interaction or transport error is
    /// surfaced as its corresponding [`Error`] variant.
    pub async fn write_group_key_set(&self, set: &crate::group::GroupKeySet) -> Result<(), Error> {
        let path = CommandPath {
            endpoint: 0,
            cluster: crate::group::GROUP_KEY_MANAGEMENT_CLUSTER,
            command: crate::group::CMD_KEY_SET_WRITE,
        };
        match self
            .invoke(path, crate::group::key_set_write_fields(set))
            .await?
        {
            InvokeResult::Status(ImStatus::Success) => Ok(()),
            InvokeResult::Status(ImStatus::Failure(code)) => Err(Error::GroupCommandRejected(code)),
            InvokeResult::Status(_) => Err(Error::Operational(
                "unexpected status for KeySetWrite".into(),
            )),
            InvokeResult::Data { .. } => Err(Error::Operational(
                "unexpected response command for KeySetWrite".into(),
            )),
        }
    }

    /// Write the device's `GroupKeyMap` list (binds group ids to key sets).
    ///
    /// Small lists go in one `WriteRequestMessage` (byte-identical to a normal
    /// write); larger lists are chunked (`ReplaceAll`+`AppendItem`) without ever
    /// sending an empty `ReplaceAll`. There is no lockout guard — `GroupKeyMap`
    /// has no self-lock concern unlike `AccessControl.Acl`.
    ///
    /// `GroupKeyMap` writes are NOT timed (the spec does not require
    /// `TimedRequest`); however, if the device unexpectedly rejects the write with
    /// `NEEDS_TIMED_INTERACTION` the controller's timed-auto-upgrade will
    /// transparently retry on the single-chunk path.
    ///
    /// # Errors
    ///
    /// Returns an interaction error or a per-path device status from the device.
    pub async fn write_group_key_map(
        &self,
        entries: &[crate::group::GroupKeyMapEntry],
    ) -> Result<Vec<(AttributePath, ImStatus)>, Error> {
        let path = AttributePath {
            endpoint: 0,
            cluster: crate::group::GROUP_KEY_MANAGEMENT_CLUSTER,
            attribute: crate::group::ATTR_GROUP_KEY_MAP,
        };
        let element_tlvs: Vec<Vec<u8>> = entries
            .iter()
            .map(|e| value_to_tlv(&crate::group::group_key_map_entry_value(*e)))
            .collect::<Result<_, _>>()?;
        let mut chunks = build_list_write_chunks(path, &element_tlvs, WRITE_CHUNK_BUDGET, false);
        if chunks.len() == 1 {
            // Single message: reuse the plain Action path (byte-identical to a
            // normal write, 0xc6 auto-upgrade intact). The timed builder hands
            // back the same chunk, so the retry — if the device demands timed —
            // re-sends identical bytes (safe for a full-list replace).
            let chunk = chunks.swap_remove(0);
            let plain = chunk.clone();
            let resp = self
                .action(
                    OP_WRITE_REQUEST,
                    plain,
                    Box::new(move || chunk),
                    vec![(path.cluster, path.attribute)],
                )
                .await?;
            Ok(parse_write_response(&resp)?)
        } else {
            self.chunked_write(chunks).await
        }
    }

    /// Subscribe to attribute reports for `attrs` and/or event reports for
    /// `events` (concrete or wildcard paths) on a **single** subscription. The
    /// device sends the priming values/events, then steady-state changes within
    /// `[min_interval, max_interval]` seconds. Await
    /// [`SubscriptionEvent`](crate::subscription::SubscriptionEvent)s — both
    /// `Report` (attributes) and `Event` (events) — via
    /// [`Subscription::next`](crate::subscription::Subscription::next).
    ///
    /// Pass an empty slice for either to subscribe to only the other. The
    /// subscription auto-resubscribes transparently on staleness/session loss,
    /// re-requesting the same attribute and event paths.
    ///
    /// # Several subscriptions, one process per identity
    ///
    /// Any number of subscriptions to one node may be held at once. Each
    /// `SubscribeRequest` tells the device whether to keep this controller's
    /// other subscriptions (`KeepSubscriptions`): it keeps them whenever
    /// another subscription of this controller to the node exists
    /// (established, re-establishing, or still being set up), and asks the
    /// device to clear them only for a lone subscription — which also clears
    /// any that an earlier run of this process left on the device.
    ///
    /// The device scopes that clearing to the fabric and the subscriber's node
    /// id — the operational identity (NOC) — not to the process. Run **one
    /// controller process per operational identity**: two processes sharing
    /// one would terminate each other's subscriptions.
    ///
    /// # Errors
    ///
    /// - [`Error::SubscribeRejected`] if the device answers the
    ///   `SubscribeRequest` with a non-success Interaction Model status (for
    ///   example 0x89 `ResourceExhausted` when its subscription resources are
    ///   full), as soon as that answer arrives.
    /// - [`Error::ResponseTimeout`] if the device acknowledges the request but
    ///   never answers it within the response deadline.
    /// - [`Error::ControllerStopped`] if the owning task stopped, or any other
    ///   connect / transport / interaction-model error while establishing the
    ///   subscription.
    pub async fn subscribe(
        &self,
        attrs: &[ReadPath],
        events: &[EventPath],
        min_interval: u16,
        max_interval: u16,
    ) -> Result<crate::subscription::Subscription, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Command::Subscribe {
                node_id: self.node_id,
                paths: attrs.to_vec(),
                event_paths: events.to_vec(),
                event_filters: Vec::new(),
                min_interval,
                max_interval,
                reply,
            })
            .await
            .map_err(|_| Error::ControllerStopped)?;
        let (receivers, key) = rx.await.map_err(|_| Error::ControllerStopped)??;
        Ok(crate::subscription::Subscription {
            rx: receivers.report_rx,
            ctrl_rx: receivers.ctrl_rx,
            tx: self.tx.clone(),
            key,
            cancelled: false,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use matter_codec::Value;

    use super::{build_invoke_request, value_to_tlv, CommandPath};

    #[tokio::test]
    async fn deadline_apis_bound_a_full_actor_queue() {
        use super::{Command, Error, Node, OP_INVOKE_REQUEST, TIMED_DEFAULT_MS};
        use std::time::{Duration, Instant};
        for timed in [false, true] {
            let (tx, mut rx) = tokio::sync::mpsc::channel(1);
            let (reply, _reply_rx) = tokio::sync::oneshot::channel();
            tx.send(Command::TimedRoundTrip {
                node_id: 1,
                timeout_ms: TIMED_DEFAULT_MS,
                action_opcode: OP_INVOKE_REQUEST,
                action_payload: vec![],
                reply: reply.into(),
            })
            .await
            .unwrap();
            let node = Node { tx, node_id: 2 };
            let deadline = Instant::now() + Duration::from_millis(20);
            let result = tokio::time::timeout(Duration::from_secs(1), async {
                if timed {
                    node.invoke_timed_before(
                        CommandPath {
                            endpoint: 1,
                            cluster: 0x101,
                            command: 0,
                        },
                        Value::Structure(vec![]),
                        None,
                        deadline,
                    )
                    .await
                    .map(|_| ())
                } else {
                    node.write_before(
                        &[(
                            matter_interaction::AttributePath {
                                endpoint: 1,
                                cluster: 8,
                                attribute: 0,
                            },
                            Value::Uint(1),
                        )],
                        deadline,
                    )
                    .await
                    .map(|_| ())
                }
            })
            .await
            .expect("full queue must respect the outer deadline");
            assert!(matches!(result, Err(Error::Operational(_))));
            assert!(rx.recv().await.is_some());
            assert!(rx.try_recv().is_err(), "expired action entered the queue");
        }
    }

    /// The whole point of [`Node::invoke_tlv`]: a generated
    /// `matter_clusters::gen::*::encode_*()` output can be fed straight in, and
    /// the resulting wire `InvokeRequest` is byte-identical to encoding the
    /// corresponding [`Value`] and calling [`Node::invoke`]. This proves the
    /// triple-hop (encode → decode → re-encode) the raw-TLV path removes is a
    /// no-op transform, so both entry points send the same bytes.
    #[test]
    fn invoke_tlv_matches_invoke_value_wire_payload() {
        let path = CommandPath {
            endpoint: 1,
            cluster: 0x0006,
            command: 0x01,
        };
        // Generated encoder output for OnOff.On (an empty fields struct) — the
        // exact `Vec<u8>` a caller would pass to `invoke_tlv`.
        let gen_tlv = matter_clusters::gen::on_off::encode_on();
        // The equivalent `Value` a caller would otherwise pass to `invoke`.
        let via_value = value_to_tlv(&Value::Structure(vec![])).unwrap();
        assert_eq!(
            gen_tlv, via_value,
            "gen encode_on() must equal value_to_tlv(empty struct) — the raw-TLV \
             and Value paths carry identical field bytes"
        );
        // Therefore the built wire requests are identical: invoke_tlv(gen) and
        // invoke(Value) transmit byte-identical frames.
        assert_eq!(
            build_invoke_request(path, &gen_tlv),
            build_invoke_request(path, &via_value),
            "invoke_tlv and invoke build the same on-wire InvokeRequest"
        );
    }
}
