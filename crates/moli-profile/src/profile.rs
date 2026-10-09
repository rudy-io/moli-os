//! The profile format and its validation. Pure: no IO.
//!
//! A profile says *where* to read (HTTP requests and their format) and
//! *what* it means (typed points, units, value maps). Validation is strict
//! and explains itself: it is the lock that lets agents write profiles
//! without being able to install a broken one.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use moli_core::{Access, Kind, PointSpec, Semantic, Unit, Value};
use serde::Deserialize;
use serde_json::Value as Json;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub profile: Meta,
    #[serde(rename = "request")]
    pub requests: Vec<Request>,
    #[serde(rename = "point")]
    pub points: Vec<PointDef>,
    #[serde(default, rename = "write")]
    pub writes: Vec<Write>,
}

/// Where a profile comes from: what it may do with a secret depends on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Shipped in the binary, reviewed with it.
    Builtin,
    /// A file (written by an agent, downloaded…): runs once a human approved it.
    File,
}

/// A declared write: the only way a profile changes a device. Each one is a
/// command, so it goes through the hub (guard, journal) like any driver's.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Write {
    /// The `[[point]]` key it makes writable.
    pub point: String,
    /// `GET` path. `{value}`: the commanded value; `{request.path}`: a value
    /// of that request's latest answer (`{a.x|b.y}`: the first present; the
    /// path may hold `{value}`, as in `{control.dt{value}}`).
    pub path: String,
    /// Commanded value (as text) → text sent.
    #[serde(default)]
    pub map: BTreeMap<String, String>,
    /// Request read again right after the write.
    #[serde(default)]
    pub refresh: Option<String>,
    /// Decimals of a numeric value (`25.0`); default: as short as possible.
    #[serde(default)]
    pub decimals: Option<u32>,
    /// Text the answer must contain for the write to count as done (devices
    /// that answer 200 to a refusal, like Daikin's `ret=PARAM NG`).
    #[serde(default)]
    pub expect: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Default TCP port (80, or 443 with `https`).
    #[serde(default)]
    pub port: Option<u16>,
    /// HTTPS with a publicly trusted certificate (web APIs).
    #[serde(default)]
    pub https: bool,
    /// Default host (web APIs); a device's host comes from the driver config.
    #[serde(default)]
    pub host: Option<String>,
    /// `request.path` of a value that identifies the device for good
    /// (MAC, serial…). Without it, the host is the identity.
    #[serde(default)]
    pub identity: Option<String>,
    /// `request.path` of the name the device gives itself.
    #[serde(default)]
    pub name_from: Option<String>,
    /// Name of the secret (per driver instance, in the encrypted store)
    /// a signing transport needs: `moli-os secrets set <instance> <name>`.
    #[serde(default)]
    pub secret: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub path: String,
    pub format: Format,
    /// `"30s"`, `"5m"`, `"1h"`.
    pub every: String,
    /// `get` (default) or `meross` (signed JSON POST, Meross local API).
    #[serde(default)]
    pub transport: Transport,
    /// Meross: message namespace (`Appliance.Control.ElectricityX`).
    #[serde(default)]
    pub namespace: Option<String>,
    /// Meross: message payload, as JSON text.
    #[serde(default)]
    pub payload: Option<String>,
    /// Extra request headers (`{ "x-api-key" = "{secret}" }`).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    #[default]
    Get,
    Meross,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    Json,
    /// `key=value,key=value` with URL-encoded values (Daikin and kin).
    Kv,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PointDef {
    pub key: String,
    pub label: String,
    /// `request_id.path.in.the.answer`
    pub from: String,
    pub kind: String,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub semantic: Option<Semantic>,
    /// Enum values (required for `kind = "enum"`).
    #[serde(default)]
    pub values: Vec<String>,
    /// Raw value (as text) → meaning. Applied before anything else.
    #[serde(default)]
    pub map: BTreeMap<String, toml::Value>,
    #[serde(default)]
    pub scale: Option<f64>,
    /// Decimal places kept after scaling.
    #[serde(default)]
    pub round: Option<u32>,
    /// Bounds a command must respect (numeric only; checked by the hub).
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    /// Increment offered to humans (numeric only).
    #[serde(default)]
    pub step: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("profile {profile:?}: {message}")]
pub struct ProfileError {
    pub profile: String,
    pub message: String,
}

/// Decimals `round` may keep.
const MAX_ROUND: u32 = 6;

/// Units accepted beyond those the core knows by name.
const EXTRA_UNITS: &[&str] = &[
    "mired", "lqi", "dBm", "Hz", "L", "m³", "µg/m³", "ppb", "rpm", "km/h", "mm", "°", "UV",
];

/// Raw values that mean « no reading ».
const NO_READING: &[&str] = &["", "-", "--", "null", "nan", "NaN"];

/// Read requests may carry a query only on these endpoints, with these
/// parameters: a mutating `GET` (`set_control_info?pow=1`, `relay/0?turn=on`,
/// `cm?cmnd=Power%20On`) cannot hide in a read polled every few seconds,
/// outside the guard. List an endpoint only once its API documents it as
/// read-only, and say why.
const SAFE_QUERIES: &[(&str, &[&str])] = &[
    // Open-Meteo forecast (public, read-only API): position, fields wanted, time zone.
    (
        "/v1/forecast",
        &[
            "latitude",
            "longitude",
            "current",
            "daily",
            "timezone",
            "forecast_days",
        ],
    ),
    // Open-Meteo air quality (public, read-only API).
    (
        "/v1/air-quality",
        &["latitude", "longitude", "current", "timezone"],
    ),
    // Moonraker: each parameter names a printer object whose status is read.
    (
        "/printer/objects/query",
        &["print_stats", "heater_bed", "extruder", "display_status"],
    ),
];

/// A validated profile, ready to run.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub meta: Meta,
    pub requests: Vec<CompiledRequest>,
    pub points: Vec<CompiledPoint>,
    pub writes: Vec<CompiledWrite>,
    /// Placeholders the driver configuration must fill (`vars`).
    pub vars: BTreeSet<String>,
    pub source: Source,
}

/// A validated write.
#[derive(Debug, Clone)]
pub struct CompiledWrite {
    pub key: Arc<str>,
    /// The template as declared: shown to humans, named in errors (never the
    /// filled-in path, which may hold a secret).
    pub path: String,
    pieces: Vec<Piece>,
    map: BTreeMap<String, String>,
    decimals: Option<u32>,
    pub expect: Option<String>,
    /// Requests whose answers the path echoes (indexes): read again just
    /// before writing, so a change made meanwhile (remote, app) is not undone.
    pub reads: Vec<usize>,
    /// Request read again right after.
    pub refresh: Option<usize>,
}

/// A part of a write path.
#[derive(Debug, Clone, PartialEq)]
enum Piece {
    Text(String),
    Value,
    /// `{secret}` or a var.
    Name(String),
    /// `(request, path)` alternatives: the first present wins. The path may
    /// hold `{value}`.
    Answer(Vec<(String, String)>),
}

#[derive(Debug, Clone)]
pub struct CompiledRequest {
    pub id: String,
    /// May hold `{placeholders}`: `{secret}` and the driver's `vars`.
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub format: Format,
    pub every: Duration,
    pub call: Call,
}

/// How a request is made on the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    Get,
    /// Meross local API: signed `GET` message posted as JSON.
    Meross {
        namespace: String,
        payload: Json,
    },
}

#[derive(Debug, Clone)]
pub struct CompiledPoint {
    pub spec: PointSpec,
    pub request: String,
    pub path: Vec<String>,
    map: BTreeMap<String, Value>,
    scale: Option<f64>,
    round: Option<u32>,
}

/// `"30s"` → 30 s. Bounded to [5 s, 1 day]: no hammering, no forgetting.
pub fn parse_every(s: &str) -> Option<Duration> {
    let s = s.trim();
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = n.parse().ok()?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        _ => return None,
    };
    (5..=86_400)
        .contains(&secs)
        .then(|| Duration::from_secs(secs))
}

fn toml_to_value(v: &toml::Value) -> Option<Value> {
    Some(match v {
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::Integer(i) => Value::Int(*i),
        toml::Value::Float(f) => Value::Float(*f),
        toml::Value::String(s) => Value::Text(s.as_str().into()),
        _ => return None,
    })
}

impl Profile {
    pub fn parse(text: &str) -> Result<Self, ProfileError> {
        toml::from_str(text).map_err(|e| ProfileError {
            profile: "?".into(),
            message: e.to_string(),
        })
    }

    /// Validates everything a running driver will rely on.
    pub fn compile(self, source: Source) -> Result<Compiled, ProfileError> {
        let fail = |message: String| ProfileError {
            profile: self.profile.id.clone(),
            message,
        };
        if self.profile.id.is_empty() || self.profile.id.contains(['/', ':']) {
            return Err(fail("id must be non-empty, without '/' or ':'".into()));
        }
        if self.requests.is_empty() {
            return Err(fail("at least one [[request]] is required".into()));
        }
        let secrets = Secrets {
            declared: self.profile.secret.is_some(),
            https: self.profile.https,
        };
        let mut requests = Vec::new();
        let mut vars = BTreeSet::new();
        for r in &self.requests {
            if requests
                .iter()
                .any(|known: &CompiledRequest| known.id == r.id)
            {
                return Err(fail(format!("duplicate request id {:?}", r.id)));
            }
            let request = compile_request(r, secrets, &mut vars)
                .map_err(|e| fail(format!("request {:?}: {e}", r.id)))?;
            requests.push(request);
        }
        let answers = Answers(&requests);
        for (what, path) in [
            ("identity", &self.profile.identity),
            ("name_from", &self.profile.name_from),
        ] {
            if let Some(path) = path {
                answers.reference(what, path).map_err(fail)?;
            }
        }
        if self.points.is_empty() {
            return Err(fail("at least one [[point]] is required".into()));
        }
        let mut keys = HashSet::new();
        let mut points = Vec::new();
        for p in &self.points {
            if p.key.is_empty() || p.key.contains('/') || !keys.insert(p.key.as_str()) {
                return Err(fail(format!(
                    "point key {:?} must be unique, non-empty, without '/'",
                    p.key
                )));
            }
            let (request, path) = answers
                .reference(&format!("point {:?}: from", p.key), &p.from)
                .map_err(fail)?;
            let point = compile_point(p, request, path)
                .map_err(|e| fail(format!("point {:?}: {e}", p.key)))?;
            points.push(point);
        }
        let mut writes: Vec<CompiledWrite> = Vec::new();
        for w in &self.writes {
            let point = points
                .iter_mut()
                .find(|p| *p.spec.key == *w.point)
                .ok_or_else(|| fail(format!("write: unknown point {:?}", w.point)))?;
            if writes.iter().any(|known| *known.key == *w.point) {
                return Err(fail(format!("two [[write]] for point {:?}", w.point)));
            }
            let write = compile_write(w, &point.spec, &answers, secrets, &mut vars)
                .map_err(|e| fail(format!("write {:?}: {e}", w.point)))?;
            point.spec.access.write = true;
            writes.push(write);
        }
        // Without a host of its own, a secret goes wherever the instance
        // points: acceptable for a reviewed built-in (a LAN device keyed per
        // device, like Meross), never for a file.
        if self.profile.secret.is_some() && self.profile.host.is_none() && source == Source::File {
            return Err(fail(
                "a profile file with a secret must declare [profile] host: the only place the secret may go".into(),
            ));
        }
        Ok(Compiled {
            meta: self.profile,
            requests,
            points,
            writes,
            vars,
            source,
        })
    }
}

/// What a profile may do with its secret.
#[derive(Debug, Clone, Copy)]
struct Secrets {
    declared: bool,
    https: bool,
}

impl Secrets {
    /// `{secret}` or a var (collected into `vars`).
    fn check(self, name: &str, vars: &mut BTreeSet<String>) -> Result<(), String> {
        if name != "secret" {
            vars.insert(name.to_owned());
        } else if !self.https {
            return Err("{secret} travels only over https".into());
        } else if !self.declared {
            return Err("{secret} needs [profile] secret = \"<name>\"".into());
        }
        Ok(())
    }
}

/// The requests of a profile, as references into their answers see them.
struct Answers<'a>(&'a [CompiledRequest]);

impl Answers<'_> {
    fn index(&self, request: &str) -> Option<usize> {
        self.0.iter().position(|r| r.id == request)
    }

    /// `<request>.<path>`, checked: known request, no empty segment, valid
    /// selectors, one level in a key=value answer.
    fn reference(&self, what: &str, path: &str) -> Result<(String, Vec<String>), String> {
        let mut parts = path.split('.').map(str::to_owned);
        let request = parts.next().unwrap_or_default();
        let rest: Vec<String> = parts.collect();
        let Some(index) = self.index(&request) else {
            return Err(format!(
                "{what} {path:?} must be <request id>.<path> without empty segments"
            ));
        };
        if rest.is_empty() || rest.iter().any(String::is_empty) {
            return Err(format!(
                "{what} {path:?} must be <request id>.<path> without empty segments"
            ));
        }
        if let Some(bad) = rest
            .iter()
            .find(|seg| (seg.starts_with('[') || seg.ends_with(']')) && selector(seg).is_none())
        {
            return Err(format!(
                "{what} {path:?}: {bad:?} is not a selector; write [field=value]"
            ));
        }
        if self.0[index].format == Format::Kv && rest.len() != 1 {
            return Err(format!(
                "{what} {path:?}: a key=value answer has a single level (<request>.<key>)"
            ));
        }
        Ok((request, rest))
    }
}

/// One request: a clean URI path, a sane pace, a coherent transport.
/// The `{name}` placeholders of a template, checked.
pub fn placeholders(template: &str) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let close = rest[open..]
            .find('}')
            .ok_or_else(|| format!("unclosed {{ in {template:?}"))?;
        let name = &rest[open + 1..open + close];
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(format!(
                "placeholder {{{name}}}: lowercase letters, digits and _ only"
            ));
        }
        names.push(name.to_owned());
        rest = &rest[open + close + 1..];
    }
    if rest.contains('}') {
        return Err(format!("stray }} in {template:?}"));
    }
    Ok(names)
}

/// A template with its placeholders filled in. Values are percent-encoded
/// for paths (`encode`), raw for headers.
#[must_use]
pub fn expand(template: &str, value: impl Fn(&str) -> Option<String>, encode: bool) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        out.push_str(&rest[..open]);
        let raw = value(&rest[open + 1..open + close]).unwrap_or_default();
        if encode {
            push_encoded(&mut out, &raw);
        } else {
            out.push_str(&raw);
        }
        rest = &rest[open + close + 1..];
    }
    out.push_str(rest);
    out
}

/// Percent-encodes everything but unreserved characters: a value can never
/// add a parameter (`&`), a segment (`/`) or a query (`?`).
fn push_encoded(out: &mut String, raw: &str) {
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            let _ = std::fmt::Write::write_fmt(out, format_args!("%{b:02X}"));
        }
    }
}

/// Headers the engine sets itself, and method overrides (a read must stay a
/// read).
const RESERVED_HEADERS: &[&str] = &[
    "host",
    "cookie",
    "content-length",
    "content-type",
    "transfer-encoding",
    "connection",
    "accept",
    "x-http-method-override",
    "x-http-method",
    "x-method-override",
];

/// A path with its placeholders filled in must be a clean URI path.
fn check_shape(shape: &str) -> Result<(), String> {
    let printable = shape.bytes().all(|b| b.is_ascii_graphic());
    let valid = shape.parse::<http::uri::PathAndQuery>().is_ok();
    if !shape.starts_with('/') || shape.starts_with("//") || !printable || !valid {
        return Err(
            "path must be a URI path starting with a single '/', printable ASCII only (encode spaces and accents)"
                .into(),
        );
    }
    Ok(())
}

/// A read's query: only on an endpoint listed in [`SAFE_QUERIES`], with its
/// listed parameters, and inert values (letters, digits, `_.,-`, vars).
fn check_read_query(path: &str) -> Result<(), String> {
    let Some((endpoint, query)) = path.split_once('?') else {
        return Ok(());
    };
    let Some((_, allowed)) = SAFE_QUERIES.iter().find(|(e, _)| *e == endpoint) else {
        return Err(format!(
            "a read cannot carry a query on {endpoint} (a GET with parameters can change the device): declare a [[write]], or have {endpoint} listed as read-only in SAFE_QUERIES"
        ));
    };
    for item in query.split('&') {
        let (name, value) = item.split_once('=').unwrap_or((item, ""));
        if !allowed.contains(&name) {
            return Err(format!(
                "query parameter {name:?} is not listed as safe for {endpoint}"
            ));
        }
        let literal = expand(value, |_| Some(String::new()), false);
        if !literal
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.,-".contains(&b))
        {
            return Err(format!(
                "query parameter {name:?}: letters, digits, _ . , - and {{vars}} only"
            ));
        }
    }
    Ok(())
}

fn compile_request(
    r: &Request,
    secrets: Secrets,
    vars: &mut BTreeSet<String>,
) -> Result<CompiledRequest, String> {
    let mut names = placeholders(&r.path)?;
    for (name, value) in &r.headers {
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(format!("header name {name:?}: letters, digits and - only"));
        }
        if RESERVED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(format!(
                "header {name} is reserved (set by the engine, or a method override)"
            ));
        }
        if !value.bytes().all(|b| b.is_ascii_graphic() || b == b' ') {
            return Err(format!("header {name}: printable ASCII only"));
        }
        names.extend(placeholders(value)?);
    }
    for name in names {
        secrets.check(&name, vars)?;
    }
    check_shape(&expand(&r.path, |_| Some("x".into()), true))?;
    check_read_query(&r.path)?;
    let every = parse_every(&r.every).ok_or_else(|| {
        format!(
            "every {:?} must be like 30s, 5m, 1h (5 s to 1 day)",
            r.every
        )
    })?;
    let call = match r.transport {
        Transport::Get if r.namespace.is_none() && r.payload.is_none() => Call::Get,
        Transport::Get => {
            return Err("namespace/payload only apply to transport = \"meross\"".into());
        }
        Transport::Meross => {
            let namespace = r
                .namespace
                .clone()
                .filter(|n| {
                    !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.')
                })
                .ok_or("meross needs a namespace like Appliance.System.All")?;
            let payload: Json = serde_json::from_str(r.payload.as_deref().unwrap_or("{}"))
                .ok()
                .filter(Json::is_object)
                .ok_or("payload must be a JSON object")?;
            if r.format != Format::Json {
                return Err("meross answers are json".into());
            }
            if !secrets.declared {
                return Err("transport meross needs [profile] secret = \"<name>\"".into());
            }
            Call::Meross { namespace, payload }
        }
    };
    Ok(CompiledRequest {
        id: r.id.clone(),
        path: r.path.clone(),
        headers: r.headers.clone().into_iter().collect(),
        format: r.format,
        every,
        call,
    })
}

/// One point: its kind, value map, unit and meaning, checked together.
fn compile_point(
    p: &PointDef,
    request: String,
    path: Vec<String>,
) -> Result<CompiledPoint, String> {
    if p.label.trim().is_empty() {
        return Err("label must not be empty".into());
    }
    let distinct: HashSet<&str> = p.values.iter().map(String::as_str).collect();
    if distinct.len() != p.values.len() || p.values.iter().any(|v| v.trim().is_empty()) {
        return Err("values must be distinct and non-empty".into());
    }
    let bounds = [p.min, p.max, p.step];
    if bounds.iter().flatten().any(|b| !b.is_finite())
        || p.min.zip(p.max).is_some_and(|(min, max)| min >= max)
        || p.step.is_some_and(|s| s <= 0.0)
    {
        return Err("min < max, step > 0, all finite".into());
    }
    if bounds.iter().any(Option::is_some) && p.kind != "numeric" {
        return Err("min/max/step only apply to numeric points".into());
    }
    let kind = match p.kind.as_str() {
        "binary" => Kind::Binary,
        "numeric" => Kind::Numeric {
            min: p.min,
            max: p.max,
            step: p.step,
        },
        "text" => Kind::Text,
        "enum" if !p.values.is_empty() => Kind::Enum {
            values: p.values.iter().map(|v| v.as_str().into()).collect(),
        },
        "enum" => return Err("enum needs values".into()),
        other => return Err(format!("unknown kind {other:?}")),
    };
    if p.scale
        .is_some_and(|s| !s.is_finite() || s == 0.0 || s.abs() > 1e9)
    {
        return Err("scale must be finite, non-zero and reasonable".into());
    }
    if p.round.is_some_and(|r| r > MAX_ROUND) {
        return Err(format!("round keeps at most {MAX_ROUND} decimals"));
    }
    let mut map = BTreeMap::new();
    for (raw, meaning) in &p.map {
        let value = toml_to_value(meaning)
            .ok_or_else(|| format!("map value for {raw:?} must be a scalar"))?;
        if moli_core::validate(&kind, &value).is_err() {
            return Err(format!("map value {meaning} does not fit kind {}", p.kind));
        }
        map.insert(raw.clone(), value);
    }
    if (p.scale.is_some() || p.round.is_some()) && !matches!(kind, Kind::Numeric { .. }) {
        return Err("scale/round only apply to numeric points".into());
    }
    let unit = p.unit.as_deref().and_then(Unit::parse);
    // A typo (« °c », « mn ») must not silently become an unknown unit.
    if let Some(Unit::Other(symbol)) = &unit
        && !EXTRA_UNITS.contains(&&**symbol)
    {
        return Err(format!(
            "unknown unit {symbol:?} (known: W kWh Wh VA V mV A °C % lx hPa dB ppm s min h, or {})",
            EXTRA_UNITS.join(" ")
        ));
    }
    let semantic = p
        .semantic
        .unwrap_or_else(|| Semantic::infer(&p.key, unit.as_ref()));
    Ok(CompiledPoint {
        spec: PointSpec {
            key: p.key.as_str().into(),
            label: p.label.as_str().into(),
            kind,
            // Writable only through a declared [[write]].
            access: Access {
                read: true,
                write: false,
            },
            unit,
            semantic,
        },
        request,
        path,
        map,
        scale: p.scale,
        round: p.round,
    })
}

/// One write: a path that sends `{value}`, references that resolve, a map
/// that fits the point.
fn compile_write(
    w: &Write,
    spec: &PointSpec,
    answers: &Answers,
    secrets: Secrets,
    vars: &mut BTreeSet<String>,
) -> Result<CompiledWrite, String> {
    let pieces = write_template(&w.path)?;
    if !pieces.contains(&Piece::Value) {
        return Err("path must send {value}".into());
    }
    let mut reads = Vec::new();
    for piece in &pieces {
        match piece {
            Piece::Name(name) => secrets.check(name, vars)?,
            Piece::Answer(alternatives) => {
                for (request, path) in alternatives {
                    let reference = format!("{request}.{}", path.replace("{value}", "x"));
                    answers.reference("reference", &reference)?;
                    reads.extend(answers.index(request));
                }
            }
            Piece::Text(_) | Piece::Value => {}
        }
    }
    reads.sort_unstable();
    reads.dedup();
    let shape: String = pieces
        .iter()
        .map(|p| match p {
            Piece::Text(text) => text.as_str(),
            _ => "x",
        })
        .collect();
    check_shape(&shape)?;
    let refresh = w
        .refresh
        .as_deref()
        .map(|r| {
            answers
                .index(r)
                .ok_or_else(|| format!("refresh: unknown request {r:?}"))
        })
        .transpose()?;
    let numeric = matches!(spec.kind, Kind::Numeric { .. });
    if w.decimals.is_some_and(|d| d > MAX_ROUND || !numeric) {
        return Err(format!(
            "decimals apply to numeric points, {MAX_ROUND} at most"
        ));
    }
    check_write_map(&w.map, &spec.kind)?;
    if w.expect.as_deref().is_some_and(str::is_empty) {
        return Err("expect must not be empty".into());
    }
    Ok(CompiledWrite {
        key: spec.key.clone(),
        path: w.path.clone(),
        pieces,
        map: w.map.clone(),
        decimals: w.decimals,
        expect: w.expect.clone(),
        reads,
        refresh,
    })
}

/// When a write map names some values, it must name them all: a forgotten
/// one would send the human label (`froid`) to the device.
fn check_write_map(map: &BTreeMap<String, String>, kind: &Kind) -> Result<(), String> {
    if map.is_empty() {
        return Ok(());
    }
    let expected: Vec<String> = match kind {
        Kind::Binary => vec!["true".into(), "false".into()],
        Kind::Enum { values } => values.iter().map(ToString::to_string).collect(),
        Kind::Numeric { .. } => {
            if let Some(bad) = map.keys().find(|k| k.parse::<f64>().is_err()) {
                return Err(format!("map key {bad:?} is not a number"));
            }
            return Ok(());
        }
        Kind::Text => return Ok(()),
    };
    if let Some(bad) = map.keys().find(|k| !expected.contains(k)) {
        return Err(format!("map key {bad:?} is not a value of the point"));
    }
    if let Some(missing) = expected.iter().find(|v| !map.contains_key(*v)) {
        return Err(format!("map misses the value {missing:?}"));
    }
    Ok(())
}

/// A write path cut into pieces. Only `{value}` may nest, inside a reference.
fn write_template(template: &str) -> Result<Vec<Piece>, String> {
    const VALUE: &str = "{value}";
    let mut pieces = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        if open > 0 {
            pieces.push(Piece::Text(rest[..open].to_owned()));
        }
        let inner = &rest[open + 1..];
        let mut end = 0;
        loop {
            match inner[end..].find(['{', '}']) {
                None => return Err(format!("unclosed {{ in {template:?}")),
                Some(i) if inner[end + i..].starts_with(VALUE) => end += i + VALUE.len(),
                Some(i) if inner[end + i..].starts_with('}') => {
                    end += i;
                    break;
                }
                Some(_) => {
                    return Err(format!(
                        "only {{value}} may appear inside a placeholder ({template:?})"
                    ));
                }
            }
        }
        pieces.push(piece(&inner[..end])?);
        rest = &inner[end + 1..];
    }
    if rest.contains('}') {
        return Err(format!("stray }} in {template:?}"));
    }
    if !rest.is_empty() {
        pieces.push(Piece::Text(rest.to_owned()));
    }
    Ok(pieces)
}

/// `value`, a var or `secret`, or `request.path|request.path`.
fn piece(inner: &str) -> Result<Piece, String> {
    if inner == "value" {
        return Ok(Piece::Value);
    }
    if !inner.contains('.') {
        if inner.is_empty()
            || !inner
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(format!(
                "placeholder {{{inner}}}: lowercase letters, digits and _ only"
            ));
        }
        return Ok(Piece::Name(inner.to_owned()));
    }
    inner
        .split('|')
        .map(|alternative| {
            alternative
                .split_once('.')
                .map(|(request, path)| (request.to_owned(), path.to_owned()))
                .ok_or_else(|| format!("{{{inner}}}: each alternative is <request>.<path>"))
        })
        .collect::<Result<_, _>>()
        .map(Piece::Answer)
}

impl CompiledWrite {
    /// The text `{value}` stands for: through `map`, numbers with `decimals`.
    pub fn value_text(&self, value: &Value) -> Result<String, String> {
        let raw = match value {
            Value::Null => return Err(moli_i18n::tr!("pilotes.profile.aucune_valeur")),
            Value::Bool(b) => b.to_string(),
            Value::Text(text) => text.to_string(),
            Value::Int(_) | Value::Float(_) => {
                let n = value.as_f64().unwrap_or_default();
                match self.decimals {
                    Some(d) => format!("{n:.prec$}", prec = usize::try_from(d).unwrap_or(0)),
                    None => n.to_string(),
                }
            }
        };
        Ok(self.map.get(&raw).cloned().unwrap_or(raw))
    }

    /// The path to send, percent-encoded: `{value}`, vars and the latest
    /// answers filled in. Refuses rather than send a partial order.
    pub fn render(
        &self,
        value: &Value,
        answers: &HashMap<String, Answer>,
        name: impl Fn(&str) -> Option<String>,
    ) -> Result<String, String> {
        let value = self.value_text(value)?;
        let mut out = String::new();
        for piece in &self.pieces {
            match piece {
                Piece::Text(text) => out.push_str(text),
                Piece::Value => push_encoded(&mut out, &value),
                Piece::Name(n) => {
                    let filled = name(n).ok_or_else(|| {
                        moli_i18n::tr!("pilotes.profile.sans_valeur", name = format!("{{{n}}}"))
                    })?;
                    push_encoded(&mut out, &filled);
                }
                Piece::Answer(alternatives) => {
                    let found = alternatives
                        .iter()
                        .find_map(|(request, path)| {
                            let path: Vec<String> = path
                                .replace("{value}", &value)
                                .split('.')
                                .map(str::to_owned)
                                .collect();
                            answers.get(request)?.raw(&path)
                        })
                        .ok_or_else(|| {
                            let names: Vec<String> = alternatives
                                .iter()
                                .map(|(r, p)| format!("{r}.{p}"))
                                .collect();
                            moli_i18n::tr!(
                                "pilotes.profile.absent_reponse",
                                names = names.join(" | ")
                            )
                        })?;
                    push_encoded(&mut out, &found);
                }
            }
        }
        Ok(out)
    }
}

/// A fetched answer, uniformly navigable.
#[derive(Debug, Clone)]
pub enum Answer {
    Json(Json),
    Kv(BTreeMap<String, String>),
}

/// `%53%61%6c%6f%6e` → `Salon`.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len() + 1
            && let Some(hex) = s.get(i + 1..i + 3)
            && let Ok(b) = u8::from_str_radix(hex, 16)
        {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Answer {
    pub fn parse(format: Format, body: &[u8]) -> Option<Self> {
        match format {
            Format::Json => serde_json::from_slice(body).ok().map(Self::Json),
            Format::Kv => {
                let text = String::from_utf8_lossy(body);
                let map: BTreeMap<String, String> = text
                    .trim()
                    .split(',')
                    .filter_map(|kv| kv.split_once('='))
                    .map(|(k, v)| (k.trim().to_owned(), percent_decode(v.trim())))
                    .collect();
                (!map.is_empty()).then_some(Self::Kv(map))
            }
        }
    }

    /// The raw JSON at `path` (kv answers are one level deep).
    fn lookup(&self, path: &[String]) -> Option<Json> {
        match self {
            // In an array, `[channel=1]` picks the element whose field has
            // that value (robust to reordering); a number picks by position.
            Self::Json(json) => path
                .iter()
                .try_fold(json, |j, k| match (j, selector(k), k.parse::<usize>()) {
                    (Json::Array(items), Some((field, want)), _) => {
                        items.iter().find(|item| matches(item.get(field), want))
                    }
                    (Json::Array(items), None, Ok(i)) => items.get(i),
                    _ => j.get(k),
                })
                .cloned(),
            Self::Kv(map) => {
                (path.len() == 1).then(|| map.get(&path[0]).cloned().map(Json::String))?
            }
        }
    }

    /// The value at `path` exactly as the device gave it (echoed by writes).
    pub fn raw(&self, path: &[String]) -> Option<String> {
        match self.lookup(path)? {
            Json::String(s) => Some(s),
            Json::Number(n) => Some(n.to_string()),
            Json::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    /// The text at `path` (identity, names).
    pub fn text(&self, path: &[String]) -> Option<String> {
        match self.lookup(path)? {
            Json::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
            Json::Number(n) => Some(n.to_string()),
            _ => None,
        }
    }
}

/// `[field=value]` → `(field, value)`.
fn selector(segment: &str) -> Option<(&str, &str)> {
    let (field, want) = segment
        .strip_prefix('[')?
        .strip_suffix(']')?
        .split_once('=')?;
    (!field.is_empty() && !want.is_empty()).then_some((field, want))
}

/// Whether a field's JSON value is the selector's text (`1` matches 1 and "1").
fn matches(field: Option<&Json>, want: &str) -> bool {
    match field {
        Some(Json::String(s)) => s == want,
        Some(Json::Number(n)) => n.to_string() == want,
        Some(Json::Bool(b)) => b.to_string() == want,
        _ => false,
    }
}

impl CompiledPoint {
    /// Converts the raw answer into this point's typed value. `None` when
    /// the answer does not contain it at all.
    #[must_use]
    pub fn read(&self, answer: &Answer) -> Option<Value> {
        let raw = answer.lookup(&self.path)?;
        let as_text = match &raw {
            Json::String(s) => s.clone(),
            Json::Null => String::new(),
            other => other.to_string(),
        };
        if let Some(mapped) = self.map.get(&as_text) {
            return Some(mapped.clone());
        }
        if NO_READING.contains(&as_text.trim()) {
            return Some(Value::Null);
        }
        Some(match &self.spec.kind {
            Kind::Numeric { .. } => {
                let n = raw.as_f64().or_else(|| as_text.trim().parse::<f64>().ok());
                match n.filter(|n| n.is_finite()) {
                    Some(n) => {
                        let n = n * self.scale.unwrap_or(1.0);
                        let n = self.round.map_or(n, |d| {
                            let f = 10f64.powi(i32::try_from(d).unwrap_or(0));
                            (n * f).round() / f
                        });
                        // Never publish NaN or infinity, whatever the input.
                        if n.is_finite() {
                            Value::Float(n)
                        } else {
                            Value::Null
                        }
                    }
                    None => Value::Null,
                }
            }
            Kind::Binary => match raw {
                Json::Bool(b) => Value::Bool(b),
                _ => match as_text.trim() {
                    "1" | "true" | "on" | "ON" => Value::Bool(true),
                    "0" | "false" | "off" | "OFF" => Value::Bool(false),
                    _ => Value::Null,
                },
            },
            // An enum never leaves its declared values: an unknown code (new
            // firmware mode…) reads as « unknown », and is logged.
            Kind::Enum { values } => {
                if values.iter().any(|v| **v == *as_text) {
                    Value::Text(as_text.into())
                } else {
                    tracing::warn!(point = %self.spec.key, raw = %as_text, "value outside the profile's enum");
                    Value::Null
                }
            }
            Kind::Text => Value::Text(as_text.into()),
        })
    }
}

/// Built-in profiles, shipped in the binary.
#[must_use]
pub fn builtin(id: &str) -> Option<&'static str> {
    BUILTIN
        .iter()
        .find(|(name, _)| *name == id)
        .map(|(_, text)| *text)
}

/// `(id, text)` of each listed package's `integrations/<id>/profile.toml`:
/// the id names the file, they cannot drift apart.
macro_rules! catalogue {
    ($($id:literal),* $(,)?) => {
        &[$(($id, include_str!(concat!("../../../integrations/", $id, "/profile.toml")))),*]
    };
}

/// The profile packages of the catalogue (`integrations/*/profile.toml`).
/// Listed by hand: a built-in profile runs without `profile trust`, so
/// shipping one is a reviewed line, not a file dropped in a folder. A test
/// checks the list against the folder both ways.
pub const BUILTIN: &[(&str, &str)] = catalogue![
    "daikin-brp069",
    "open-meteo",
    "open-meteo-air",
    "tempo",
    "iopool",
    "meross-em06",
    "moonraker",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(id: &str) -> Compiled {
        Profile::parse(builtin(id).unwrap())
            .unwrap()
            .compile(Source::Builtin)
            .unwrap()
    }

    fn try_compile(text: &str) -> Result<Compiled, ProfileError> {
        Profile::parse(text).and_then(|p| p.compile(Source::Builtin))
    }

    #[test]
    fn builtin_profiles_are_valid() {
        for (id, _) in BUILTIN {
            let c = compile(id);
            assert_eq!(c.meta.id, *id);
            let writable: Vec<&str> = c
                .points
                .iter()
                .filter(|p| p.spec.access.write)
                .map(|p| &*p.spec.key)
                .collect();
            let declared: &[&str] = if *id == "daikin-brp069" {
                &["on", "mode", "target_temperature", "fan"]
            } else {
                &[]
            };
            assert_eq!(writable, declared, "{id}: only declared writes");
            assert_eq!(c.writes.len(), declared.len());
        }
    }

    #[test]
    fn builtin_is_every_profile_of_the_catalogue() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|package| package.join("profile.toml").is_file())
            .map(|package| package.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        on_disk.sort();
        let mut listed: Vec<&str> = BUILTIN.iter().map(|(id, _)| *id).collect();
        listed.sort_unstable();
        assert_eq!(on_disk, listed, "BUILTIN lists integrations/*/profile.toml");
        for (id, text) in BUILTIN {
            let file = std::fs::read_to_string(dir.join(id).join("profile.toml")).unwrap();
            assert_eq!(file, *text, "{id}");
        }
    }

    /// A Daikin-like profile with one write, for broken variants.
    const WRITABLE: &str = r#"[profile]
id = "w"
name = "W"

[[request]]
id = "control"
path = "/aircon/get_control_info"
format = "kv"
every = "30s"

[[point]]
key = "on"
label = "Marche"
from = "control.pow"
kind = "binary"

[[point]]
key = "mode"
label = "Mode"
from = "control.mode"
kind = "enum"
values = ["froid", "chaud"]

[[write]]
point = "on"
path = "/aircon/set_control_info?pow={value}&mode={control.mode}"
map = { true = "1", false = "0" }
refresh = "control"
"#;

    #[test]
    fn writes_are_declared_and_checked() {
        let c = try_compile(WRITABLE).unwrap();
        let access = |key: &str| {
            c.points
                .iter()
                .find(|p| &*p.spec.key == key)
                .unwrap()
                .spec
                .access
        };
        assert!(access("on").write, "a [[write]] makes its point writable");
        assert!(!access("mode").write, "the others stay read-only");
        assert_eq!(c.writes[0].reads, [0]);
        assert_eq!(c.writes[0].refresh, Some(0));

        let mode_write = "\n[[write]]\npoint = \"mode\"\npath = \"/s?mode={value}\"\n";
        for (broken, expect) in [
            (
                WRITABLE.replace("pow={value}", "pow=1"),
                "must send {value}",
            ),
            (
                WRITABLE.replace("point = \"on\"", "point = \"power\""),
                "unknown point",
            ),
            (
                WRITABLE.replace("{control.mode}", "{status.mode}"),
                "must be <request id>",
            ),
            (
                WRITABLE.replace("{control.mode}", "{control.mode.x}"),
                "single level",
            ),
            (
                WRITABLE.replace("refresh = \"control\"", "refresh = \"status\""),
                "refresh: unknown request",
            ),
            (
                WRITABLE.replace("{control.mode}", "{control.dt{mode}}"),
                "only {value}",
            ),
            (
                WRITABLE.replace("{control.mode}", "{control.mode"),
                "unclosed",
            ),
            (
                WRITABLE.replace("{control.mode}", "{control.mode|x}"),
                "<request>.<path>",
            ),
            (WRITABLE.replace("{control.mode}", "{secret}"), "https"),
            (WRITABLE.replace(", false = \"0\"", ""), "misses the value"),
            (
                WRITABLE.replace("refresh = ", "decimals = 1\nrefresh = "),
                "decimals",
            ),
            (WRITABLE.replace("pow={value}", "pow={value} x"), "URI path"),
            (
                format!("{WRITABLE}\n[[write]]\npoint = \"on\"\npath = \"/s?pow={{value}}\"\n"),
                "two [[write]]",
            ),
            (
                format!("{WRITABLE}{mode_write}map = {{ froid = \"3\" }}\n"),
                "misses the value \"chaud\"",
            ),
            (
                format!(
                    "{WRITABLE}{mode_write}map = {{ froid = \"3\", \"tiède\" = \"5\", chaud = \"4\" }}\n"
                ),
                "not a value of the point",
            ),
        ] {
            let err = try_compile(&broken).unwrap_err();
            assert!(err.to_string().contains(expect), "{expect}: {err}");
        }
    }

    #[test]
    fn write_templates_cut_into_pieces() {
        assert_eq!(
            write_template("/s?v={value}&m={c.dt{value}|c.m}&k={secret}").unwrap(),
            [
                Piece::Text("/s?v=".into()),
                Piece::Value,
                Piece::Text("&m=".into()),
                Piece::Answer(vec![
                    ("c".into(), "dt{value}".into()),
                    ("c".into(), "m".into())
                ]),
                Piece::Text("&k=".into()),
                Piece::Name("secret".into()),
            ]
        );
        for bad in [
            "/s?v={value",
            "/s?v=value}",
            "/s?v={c.{x}}",
            "/s?v={Lat}",
            "/s?v={}",
        ] {
            assert!(write_template(bad).is_err(), "{bad}");
        }
        // A commanded text can never add a parameter.
        let c = try_compile(&format!(
            "{WRITABLE}\n[[point]]\nkey = \"label\"\nlabel = \"L\"\nfrom = \"control.name\"\nkind = \"text\"\n[[write]]\npoint = \"label\"\npath = \"/s?name={{value}}\"\n"
        ))
        .unwrap();
        let write = c.writes.iter().find(|w| &*w.key == "label").unwrap();
        assert_eq!(
            write
                .render(&Value::from("a&pow=1 é"), &HashMap::new(), |_| None)
                .unwrap(),
            "/s?name=a%26pow%3D1%20%C3%A9"
        );
    }

    #[test]
    fn reads_cannot_hide_a_write() {
        let base = builtin("daikin-brp069").unwrap();
        for (path, expect) in [
            ("/aircon/set_control_info?pow=1", "cannot carry a query"),
            ("/relay/0?turn=on", "cannot carry a query"),
            ("/cm?cmnd=Power%20On", "cannot carry a query"),
            ("/v1/forecast?latitude=1&turn=on", "\"turn\" is not listed"),
            ("/v1/forecast?latitude=1;turn=on", "letters, digits"),
            ("/v1/forecast?latitude=1&", "\"\" is not listed"),
        ] {
            let broken = base.replace("/aircon/get_sensor_info", path);
            let err = try_compile(&broken).unwrap_err();
            assert!(err.to_string().contains(expect), "{path}: {err}");
        }
        let iopool = builtin("iopool").unwrap().replace(
            "headers = { \"x-api-key\" = \"{secret}\" }",
            "headers = { \"x-api-key\" = \"{secret}\", \"X-HTTP-Method-Override\" = \"PUT\" }",
        );
        let err = try_compile(&iopool).unwrap_err();
        assert!(err.to_string().contains("reserved"), "{err}");
    }

    #[test]
    fn a_profile_file_sends_its_secret_only_to_its_own_host() {
        let file = |text: &str| Profile::parse(text).and_then(|p| p.compile(Source::File));
        let iopool = builtin("iopool").unwrap();
        let meross = builtin("meross-em06").unwrap();
        assert!(file(iopool).is_ok(), "a host of its own: fine");
        for text in [
            iopool.replace("host = \"api.iopool.com\"\n", ""),
            meross.into(),
        ] {
            let err = file(&text).unwrap_err();
            assert!(
                err.to_string().contains("must declare [profile] host"),
                "{err}"
            );
        }
        assert!(
            try_compile(meross).is_ok(),
            "built in: the secret goes to the instance's own device"
        );
    }

    #[test]
    fn daikin_answers_decode() {
        let c = compile("daikin-brp069");
        let control =
            Answer::parse(Format::Kv, b"ret=OK,pow=1,mode=2,stemp=25.0,f_rate=A,err=0").unwrap();
        let sensor = Answer::parse(
            Format::Kv,
            include_bytes!("../../../integrations/daikin-brp069/fixtures/sensor_info.kv"),
        )
        .unwrap();
        let basic = Answer::parse(
            Format::Kv,
            include_bytes!("../../../integrations/daikin-brp069/fixtures/basic_info.kv"),
        )
        .unwrap();
        let get = |key: &str, answer: &Answer| {
            c.points
                .iter()
                .find(|p| &*p.spec.key == key)
                .unwrap()
                .read(answer)
        };
        assert_eq!(get("on", &control), Some(Value::Bool(true)));
        assert_eq!(
            get("mode", &control),
            Some(Value::from("déshumidification"))
        );
        assert_eq!(
            get("target_temperature", &control),
            Some(Value::Float(25.0))
        );
        assert_eq!(get("fan", &control), Some(Value::from("auto")));
        assert_eq!(get("temperature", &sensor), Some(Value::Float(23.0)));
        assert_eq!(
            get("humidity", &sensor),
            Some(Value::Null),
            "'-' means no reading"
        );
        assert_eq!(basic.text(&["name".into()]).as_deref(), Some("Chambre"));
        assert_eq!(basic.text(&["mac".into()]).as_deref(), Some("020000000003"));
    }

    #[test]
    fn moonraker_answers_decode() {
        let c = compile("moonraker");
        let q = Answer::parse(
            Format::Json,
            include_bytes!("../../../integrations/moonraker/fixtures/objects_query.json"),
        )
        .unwrap();
        let get = |key: &str| {
            c.points
                .iter()
                .find(|p| &*p.spec.key == key)
                .unwrap()
                .read(&q)
        };
        assert_eq!(get("state"), Some(Value::from("complete")));
        assert_eq!(get("progress"), Some(Value::Float(45.7)));
        assert_eq!(get("print_duration"), Some(Value::Float(462.0)));
        assert_eq!(get("nozzle_temperature"), Some(Value::Float(29.0)));
        assert_eq!(get("file"), Some(Value::from("a.gcode")));
    }

    #[test]
    fn broken_profiles_are_explained() {
        let base = builtin("moonraker").unwrap();
        for (broken, expect) in [
            (base.replace("every = \"15s\"", "every = \"1s\""), "every"),
            (
                base.replace(
                    "from = \"q.result.status.print_stats.state\"",
                    "from = \"nope.x\"",
                ),
                "must be <request id>",
            ),
            (
                base.replace("kind = \"text\"", "kind = \"blob\""),
                "unknown kind",
            ),
            (base.replace("key = \"file\"", "key = \"state\""), "unique"),
            (
                base.replace("round = 0", "round = 0\nbogus = 1"),
                "unknown field",
            ),
            (base.replace("round = 0", "round = 400"), "decimals"),
            (
                base.replace("round = 0", "round = 0\nmin = 5.0\nmax = 1.0"),
                "min < max",
            ),
            (
                base.replace("kind = \"text\"", "kind = \"text\"\nmax = 1.0"),
                "numeric points",
            ),
            (base.replace("scale = 100.0", "scale = nan"), "scale"),
            (base.replace("/printer/info", "/printer info"), "URI path"),
            (base.replace("/printer/info", "//evil/x"), "URI path"),
            (
                base.replace("unit = \"min\"", "unit = \"mn\""),
                "unknown unit",
            ),
            (base.replace("label = \"Fichier\"", "label = \"\""), "label"),
            (
                base.replace("q.result.status.print_stats.state", "q.result..state"),
                "empty segments",
            ),
        ] {
            let err = Profile::parse(&broken)
                .and_then(|p| p.compile(Source::Builtin))
                .unwrap_err();
            assert!(err.to_string().contains(expect), "{err}");
        }
    }

    #[test]
    fn kv_paths_are_one_level_and_unknown_enum_codes_read_as_unknown() {
        let base = builtin("daikin-brp069").unwrap();
        let deep = base.replace("from = \"control.pow\"", "from = \"control.pow.x\"");
        let err = Profile::parse(&deep)
            .and_then(|p| p.compile(Source::Builtin))
            .unwrap_err();
        assert!(err.to_string().contains("single level"), "{err}");

        let c = compile("daikin-brp069");
        let new_firmware = Answer::parse(Format::Kv, b"mode=9,pow=1").unwrap();
        let mode = c.points.iter().find(|p| &*p.spec.key == "mode").unwrap();
        assert_eq!(mode.read(&new_firmware), Some(Value::Null));
    }

    #[test]
    fn meross_em06_answers_decode_with_array_indexes() {
        let c = compile("meross-em06");
        assert!(matches!(c.requests[0].call, Call::Meross { .. }));
        // Captured from the real monitor (channel 1 and 2 shortened).
        let e = Answer::parse(
            Format::Json,
            include_bytes!("../../../integrations/meross-em06/fixtures/electricity.json"),
        )
        .unwrap();
        let get = |key: &str| {
            c.points
                .iter()
                .find(|p| &*p.spec.key == key)
                .unwrap()
                .read(&e)
        };
        assert_eq!(get("power_1"), Some(Value::Float(560.2)));
        assert_eq!(get("current_1"), Some(Value::Float(3.486)));
        assert_eq!(get("voltage_1"), Some(Value::Float(233.2)));
        assert_eq!(get("power_factor_1"), Some(Value::Float(0.69)));
        assert_eq!(get("energy_month_2"), Some(Value::Float(7829.0)));
        assert_eq!(
            get("power_6"),
            None,
            "absent channel reads as missing, not zero"
        );
        // Channels picked by number, not position: a reordered answer
        // still lands on the right circuits.
        let swapped = Answer::parse(
            Format::Json,
            br#"{"payload":{"electricity":[
                {"channel":2,"power":893},{"channel":1,"power":560202}]}}"#,
        )
        .unwrap();
        let get = |key: &str| {
            c.points
                .iter()
                .find(|p| &*p.spec.key == key)
                .unwrap()
                .read(&swapped)
        };
        assert_eq!(get("power_1"), Some(Value::Float(560.2)));
        assert_eq!(get("power_2"), Some(Value::Float(0.9)));
    }

    #[test]
    fn malformed_selectors_are_refused() {
        for bad in ["[channel]", "[=1]", "channel=1]", "[channel=1"] {
            let toml = builtin("meross-em06").unwrap().replace(
                "electricity.[channel=1].power",
                &format!("electricity.{bad}.power"),
            );
            assert!(try_compile(&toml).is_err(), "{bad}");
        }
    }

    #[test]
    fn templates_fill_encode_and_check_their_placeholders() {
        assert_eq!(
            placeholders("/v1?lat={lat}&k={secret}").unwrap(),
            ["lat", "secret"]
        );
        assert!(placeholders("/v1?lat={lat").is_err());
        assert!(placeholders("/v1?lat=}").is_err());
        assert!(placeholders("/v1?x={Lat}").is_err());
        let value = |n: &str| match n {
            "lat" => Some("42.7".into()),
            "secret" => Some("a&b c".into()),
            _ => None,
        };
        assert_eq!(
            expand("/v1?lat={lat}&k={secret}", value, true),
            "/v1?lat=42.7&k=a%26b%20c"
        );
        assert_eq!(expand("Bearer {secret}", value, false), "Bearer a&b c");
        // {secret} without a declared secret is refused; vars are collected.
        let toml = |path: &str, secret: bool| {
            format!(
                "[profile]
id = \"t\"
name = \"T\"
{}[[request]]
id = \"r\"
path = \"{path}\"
format = \"json\"
every = \"1m\"
[[point]]
key = \"v\"
label = \"V\"
from = \"r.v\"
kind = \"numeric\"
",
                if secret {
                    "secret = \"key\"\nhttps = true\n"
                } else {
                    ""
                }
            )
        };
        assert!(
            Profile::parse(&toml("/x/{secret}", false))
                .and_then(|p| p.compile(Source::Builtin))
                .is_err()
        );
        let ok = Profile::parse(&toml("/x/{secret}/{lat}", true))
            .and_then(|p| p.compile(Source::Builtin))
            .unwrap();
        assert!(ok.vars.contains("lat") && !ok.vars.contains("secret"));
    }

    #[test]
    fn signing_transports_need_a_secret_and_valid_messages() {
        let base = builtin("meross-em06").unwrap();
        for (broken, expect) in [
            (
                base.replace("secret = \"key\"\n", ""),
                "needs [profile] secret",
            ),
            (
                base.replace("payload = \"{}\"", "payload = \"[1]\""),
                "JSON object",
            ),
            (
                base.replace(
                    "namespace = \"Appliance.System.All\"",
                    "namespace = \"a b\"",
                ),
                "namespace",
            ),
        ] {
            let err = Profile::parse(&broken)
                .and_then(|p| p.compile(Source::Builtin))
                .unwrap_err();
            assert!(err.to_string().contains(expect), "{err}");
        }
    }

    #[test]
    fn every_bounds() {
        assert_eq!(parse_every("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_every("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_every("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_every("2s"), None);
        assert_eq!(parse_every("2d"), None);
    }
}
