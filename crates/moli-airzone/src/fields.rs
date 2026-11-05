//! One bounded parameter registry for publication and addressed commands.
//!
//! The embedded coverage inventory is reviewed protocol data, never downloaded
//! or accepted from a controller. Unknown fields cannot become writable points.
use anyhow::{Context as _, ensure};
use moli_core::{Access, Kind, PointSpec, Semantic, Unit, Value};
use serde::Deserialize;
use serde_json::Value as Json;
use std::{collections::BTreeSet, sync::OnceLock};

const MAX_LABEL: usize = 240;
const MAX_LIST: usize = 32;
const MAX_EXACT_INTEGER: f64 = 9_007_199_254_740_991.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Scope {
    Zone,
    System,
    Dhw,
    Iaq,
    Webserver,
    Version,
    Integration,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FieldKind {
    Binary,
    Integer,
    Number,
    Enum,
    Text,
    List,
    Errors,
}
#[derive(Debug, Deserialize)]
struct Field {
    key: String,
    wire: String,
    aliases: Vec<String>,
    scopes: Vec<Scope>,
    write_scopes: Vec<Scope>,
    kind: FieldKind,
    label: String,
    #[serde(default)]
    choices: Vec<i64>,
    #[serde(default)]
    unit: Option<String>,
    #[serde(default = "other")]
    semantic: Semantic,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    bounds: Option<[String; 2]>,
    #[serde(default)]
    options: Option<String>,
    #[serde(default)]
    constraint: Option<String>,
}
fn other() -> Semantic {
    Semantic::Other
}
#[derive(Deserialize)]
struct Registry {
    fields: Vec<Field>,
}
fn registry() -> &'static [Field] {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    &REGISTRY
        .get_or_init(|| {
            serde_json::from_str(include_str!("../../../integrations/airzone/coverage.json"))
                .expect("embedded Airzone parameter registry is checked by contract tests")
        })
        .fields
}
fn invalid() -> String {
    moli_i18n::tr!("airzone.invalid")
}
/// Allowlist for a redacted diagnostic walk, not an authorization to write.
pub(crate) fn is_known_field(key: &str) -> bool {
    registry().iter().any(|field| {
        field.wire == key
            || field.wire.split('.').any(|part| part == key)
            || field.aliases.iter().any(|alias| alias == key)
    })
}
fn finite(value: &Json) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite())
}
fn integer(value: &Json) -> Option<i64> {
    value.as_i64()
}
fn binary(value: &Json) -> Option<bool> {
    value.as_bool().or_else(|| match value.as_u64() {
        Some(0) => Some(false),
        Some(1) => Some(true),
        _ => None,
    })
}
fn bounded_text(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL)
        .collect()
}
fn lookup<'a>(raw: &'a Json, path: &str) -> Option<&'a Json> {
    path.split('.').try_fold(raw, |node, part| node.get(part))
}
#[allow(clippy::float_cmp)] // Alias values must be exactly equal, not approximately compatible.
fn equivalent(a: &Json, b: &Json) -> bool {
    a == b || finite(a).zip(finite(b)).is_some_and(|(a, b)| a == b)
}
/// Normalize only explicitly documented aliases. A conflict rejects the record
/// rather than choosing which spelling is allowed to authorize a command.
pub(crate) fn normalize(raw: &Json) -> anyhow::Result<Json> {
    let mut normalized = raw.as_object().context(invalid())?.clone();
    for field in registry() {
        for alias in &field.aliases {
            if let Some(value) = normalized.remove(alias) {
                if let Some(canonical) = normalized.get(&field.wire) {
                    ensure!(equivalent(canonical, &value), invalid());
                } else {
                    normalized.insert(field.wire.clone(), value);
                }
            }
        }
    }
    Ok(Json::Object(normalized))
}
fn projected_alarm(value: &Json) -> Json {
    match value {
        Json::String(text) => Json::String(text.chars().take(MAX_LABEL).collect()),
        Json::Number(_) if finite(value).is_some() => value.clone(),
        Json::Bool(_) | Json::Null => value.clone(),
        Json::Object(object) => {
            // Only diagnostic envelope keys documented by the API are retained.
            // Unknown structure becomes an explicit unknown alarm, not a false
            // empty/healthy alarm list or a retained untrusted object.
            let mut retained = serde_json::Map::new();
            for key in [
                "Zone", "zone", "System", "system", "error", "message", "code", "warning",
            ] {
                if let Some(value) = object.get(key)
                    && (value.is_string()
                        || value.is_number()
                        || value.is_boolean()
                        || value.is_null())
                {
                    retained.insert(key.into(), projected_alarm(value));
                }
            }
            if retained.is_empty() {
                Json::Null
            } else {
                Json::Object(retained)
            }
        }
        _ => Json::Null,
    }
}
fn projected_value(field: &Field, source: &Json) -> Json {
    if source.is_null() {
        return Json::Null;
    }
    match field.kind {
        FieldKind::Text => source
            .as_str()
            .map(|text| Json::String(text.chars().take(MAX_LABEL).collect())),
        FieldKind::Binary => (binary(source).is_some()
            || (field.wire == "cloud_connected" && matches!(source.as_str(), Some("0" | "1"))))
        .then(|| source.clone()),
        FieldKind::Integer | FieldKind::Enum => integer(source).map(|_| source.clone()),
        FieldKind::Number => finite(source).map(|_| source.clone()),
        FieldKind::List => list(source).map(|_| source.clone()),
        FieldKind::Errors => source
            .as_array()
            .map(|errors| Json::Array(errors.iter().take(MAX_LIST).map(projected_alarm).collect())),
    }
    // A present-but-invalid field must stay distinguishable from absence. In
    // particular null capabilities prevent fallback to a less precise list;
    // core fields retain a fail-closed signal for the zone parser.
    .unwrap_or(Json::Null)
}
struct RecordBudget(usize);
impl std::io::Write for RecordBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.0 {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
        }
        self.0 -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
/// Only bounded known fields are retained after ingestion. This deliberately
/// does not clone the input object before filtering a potentially large body.
/// Acknowledgement comparison must use `normalize`, which never truncates.
pub(crate) fn project(raw: &Json) -> anyhow::Result<Json> {
    ensure!(raw.is_object(), invalid());
    let mut retained = serde_json::Map::new();
    for field in registry() {
        let mut source = lookup(raw, &field.wire);
        for alias in &field.aliases {
            if let Some(value) = raw.get(alias) {
                if let Some(canonical) = source {
                    ensure!(equivalent(canonical, value), invalid());
                } else {
                    source = Some(value);
                }
            }
        }
        let Some(source) = source else { continue };
        let value = projected_value(field, source);
        if let Some((container, leaf)) = field.wire.split_once('.') {
            let object = retained
                .entry(container.to_owned())
                .or_insert_with(|| Json::Object(serde_json::Map::new()));
            object
                .as_object_mut()
                .expect("registry has only one bounded nesting level")
                .insert(leaf.into(), value);
        } else {
            retained.insert(field.wire.clone(), value);
        }
    }
    let retained = Json::Object(retained);
    // Count the serialized size without allocating another copy of the record.
    serde_json::to_writer(RecordBudget(16 * 1024), &retained).context(invalid())?;
    Ok(retained)
}
fn mode_name(value: i64) -> Option<&'static str> {
    match value {
        1 => Some("off"),
        2 => Some("cool"),
        3 => Some("heat"),
        4 => Some("fan"),
        5 => Some("dry"),
        6 => Some("aux_heat"),
        7 => Some("auto"),
        _ => None,
    }
}
fn option_text(field: &Field, value: i64) -> String {
    if field.wire == "mode" {
        mode_name(value).map_or_else(|| value.to_string(), str::to_owned)
    } else {
        value.to_string()
    }
}
fn list(value: &Json) -> Option<Vec<i64>> {
    let items = value.as_array().filter(|a| a.len() <= MAX_LIST)?;
    let numbers: Vec<_> = items.iter().map(integer).collect::<Option<_>>()?;
    (numbers.iter().collect::<BTreeSet<_>>().len() == numbers.len()).then_some(numbers)
}
fn choices(field: &Field, raw: &Json) -> Vec<i64> {
    if field.constraint.as_deref() == Some("stages") {
        let Some(capacity) = field
            .options
            .as_ref()
            .and_then(|key| integer(&raw[key]))
            .filter(|n| (0..=3).contains(n))
        else {
            return vec![];
        };
        return (1..=3).filter(|n| n & capacity == *n).collect();
    }
    let mut values = if let Some(key) = &field.options {
        if let Some(value) = raw.get(key) {
            let Some(values) = list(value).filter(|v| !v.is_empty()) else {
                return vec![];
            };
            if values.iter().any(|v| {
                !(0..=360).contains(v) || (!field.choices.is_empty() && !field.choices.contains(v))
            }) {
                return vec![];
            }
            values
        } else if field.wire == "speed" {
            let Some(max) = integer(&raw["speeds"]).filter(|n| (1..=7).contains(n)) else {
                return vec![];
            };
            (0..=max).collect()
        } else {
            return vec![];
        }
    } else {
        field.choices.clone()
    };
    if field.constraint.as_deref() == Some("legacy_angle") {
        values.retain(|v| *v < 4);
    }
    values
}
fn unit(field: &Field, raw: &Json) -> Option<Unit> {
    match field.unit.as_deref()? {
        "temperature" => match raw["units"].as_u64() {
            Some(0) => Some(Unit::Celsius),
            Some(1) => Some(Unit::Other("°F".into())),
            _ => None,
        },
        symbol => Unit::parse(symbol),
    }
}
fn range(field: &Field, raw: &Json) -> Option<(f64, f64)> {
    let [low, high] = field.bounds.as_ref()?;
    let (low, high) = (finite(&raw[low])?, finite(&raw[high])?);
    (low < high).then_some((low, high))
}
fn step(field: &Field, raw: &Json) -> Option<f64> {
    if matches!(field.kind, FieldKind::Integer) {
        Some(1.)
    } else if field.bounds.is_some() {
        raw.get("temp_step").and_then(finite).filter(|v| *v > 0.)
    } else {
        None
    }
}
fn writable(field: &Field, raw: &Json, scope: Scope, options: &[i64]) -> bool {
    if !field.write_scopes.contains(&scope) {
        return false;
    }
    if matches!(field.kind, FieldKind::Enum) && options.is_empty() {
        return false;
    }
    if field.bounds.is_some()
        && (range(field, raw).is_none()
            || unit(field, raw).is_none()
            || raw.get("temp_step").is_some_and(|v| {
                finite(v)
                    .is_none_or(|s| s <= 0. || range(field, raw).is_none_or(|(a, b)| s > b - a))
            }))
    {
        return false;
    }
    match field.constraint.as_deref() {
        Some("low_threshold") => {
            integer(&raw["aq_thrhigh"]).is_some_and(|n| (1..=100).contains(&n))
        }
        Some("high_threshold") => integer(&raw["aq_thrlow"]).is_some_and(|n| (0..=99).contains(&n)),
        Some("legacy_angle") => {
            let modern = if field.wire == "heatangle" {
                "heat_angle_conf"
            } else {
                "cold_angle_conf"
            };
            raw.get(modern).is_none_or(Json::is_null)
        }
        Some("iaq_vent") => raw.get("iaq_vmc").is_none_or(|v| binary(v) == Some(true)),
        _ => true,
    }
}
fn resolved(
    field: &Field,
    raw: &Json,
    scope: Scope,
    read_only: bool,
) -> Option<(PointSpec, Value)> {
    if !field.scopes.contains(&scope) {
        return None;
    }
    let source = lookup(raw, &field.wire).filter(|v| !v.is_null())?;
    let options = choices(field, raw);
    let value = match field.kind {
        FieldKind::Binary => {
            // The official webserver example uses a string for this one flag.
            // Do not generalize that compatibility spelling to command fields.
            let value = if field.wire == "cloud_connected" {
                match source.as_str() {
                    Some("0") => Some(false),
                    Some("1") => Some(true),
                    _ => binary(source),
                }
            } else {
                binary(source)
            };
            Value::Bool(value?)
        }
        FieldKind::Integer => {
            let n = integer(source)?;
            Value::Int(n)
        }
        FieldKind::Number => Value::Float(finite(source)?),
        FieldKind::Enum => Value::Text(option_text(field, integer(source)?).into()),
        FieldKind::Text => Value::Text(bounded_text(source.as_str()?).into()),
        FieldKind::List => Value::Text(serde_json::to_string(&list(source)?).ok()?.into()),
        FieldKind::Errors => {
            let errors = source.as_array().filter(|v| v.len() <= 64)?;
            Value::Text(bounded_text(&serde_json::to_string(errors).ok()?).into())
        }
    };
    if let Some(n) = value.as_f64()
        && (field.min.is_some_and(|min| n < min) || field.max.is_some_and(|max| n > max))
    {
        return None;
    }
    let kind = match field.kind {
        FieldKind::Binary => Kind::Binary,
        FieldKind::Integer | FieldKind::Number => {
            let bounds = range(field, raw);
            Kind::Numeric {
                min: bounds.map(|b| b.0).or(field.min),
                max: bounds.map(|b| b.1).or(field.max),
                step: step(field, raw),
            }
        }
        FieldKind::Enum if !options.is_empty() => Kind::Enum {
            values: options
                .iter()
                .map(|n| option_text(field, *n).into())
                .collect(),
        },
        _ => Kind::Text,
    };
    Some((
        PointSpec {
            key: field.key.as_str().into(),
            label: moli_i18n::tr(&field.label).into(),
            kind,
            unit: unit(field, raw),
            semantic: field.semantic,
            access: Access {
                read: true,
                write: !read_only && writable(field, raw, scope, &options),
            },
        },
        value,
    ))
}
/// Only present, well-formed documented fields are published; missing and null
/// readings never turn into invented zeroes or capabilities.
pub(crate) fn points(scope: Scope, raw: &Json, read_only: bool) -> Vec<(PointSpec, Value)> {
    let Ok(raw) = normalize(raw) else {
        return vec![];
    };
    registry()
        .iter()
        .filter_map(|field| resolved(field, &raw, scope, read_only))
        .collect()
}
fn valid_name(name: &str) -> bool {
    const EXTRA: &str = "!-+., @#*%&/():;¿?_ÁÉÍÓÚÀÈÌÒÙÂÊÎÔÛÄËÏÖÜÃÕÑÇáéíóúàèìòùâêîôûäëïöüãõñça-z";
    !name.is_empty()
        && name.chars().count() <= 11
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || EXTRA.contains(c))
}
fn command_value(field: &Field, value: &Value) -> anyhow::Result<Json> {
    match field.kind {
        FieldKind::Binary => {
            let Value::Bool(value) = value else {
                anyhow::bail!(invalid());
            };
            Ok(Json::from(u8::from(*value)))
        }
        FieldKind::Enum => {
            let Value::Text(value) = value else {
                anyhow::bail!(invalid());
            };
            let number = if field.wire == "mode" {
                (1..=7)
                    .find(|n| mode_name(*n) == Some(value.as_ref()))
                    .context(invalid())?
            } else {
                value
                    .parse::<i64>()
                    .map_err(|_| anyhow::anyhow!(invalid()))?
            };
            Ok(Json::from(number))
        }
        FieldKind::Integer => {
            let number = value.as_f64().context(invalid())?;
            ensure!(
                number.is_finite() && number.fract() == 0. && number.abs() <= MAX_EXACT_INTEGER,
                invalid()
            );
            #[allow(clippy::cast_possible_truncation)] // finite exact integer, bounded above
            Ok(Json::from(number as i64))
        }
        FieldKind::Number => Ok(value.to_json()),
        FieldKind::Text => {
            let Value::Text(text) = value else {
                anyhow::bail!(invalid());
            };
            ensure!(!text.chars().any(char::is_control), invalid());
            match field.constraint.as_deref() {
                Some("name") => ensure!(valid_name(text), invalid()),
                Some("driver") => {
                    ensure!(!text.is_empty() && text.chars().count() <= 120, invalid());
                }
                _ => anyhow::bail!(invalid()),
            }
            Ok(Json::from(text.as_ref()))
        }
        FieldKind::List | FieldKind::Errors => anyhow::bail!(invalid()),
    }
}
/// The caller owns identity, current resource scope, read-only enforcement and
/// endpoint addressing. This returns exactly one reviewed wire parameter.
pub(crate) fn encode(
    scope: Scope,
    raw: &Json,
    key: &str,
    value: &Value,
) -> anyhow::Result<(String, Json)> {
    let raw = normalize(raw)?;
    let field = registry()
        .iter()
        .find(|f| f.key == key && f.scopes.contains(&scope))
        .context(invalid())?;
    let (point, _) = resolved(field, &raw, scope, false)
        .filter(|(p, _)| p.access.write)
        .context(invalid())?;
    let value = moli_core::validate(&point.kind, value).map_err(|_| anyhow::anyhow!(invalid()))?;
    if let (
        Some(number),
        Kind::Numeric {
            min,
            step: Some(step),
            ..
        },
    ) = (value.as_f64(), &point.kind)
    {
        let grid = (number - min.unwrap_or(0.)) / step;
        ensure!((grid - grid.round()).abs() < 1e-6, invalid());
    }
    let wire = command_value(field, &value)?;
    if let Some(number) = finite(&wire) {
        match field.constraint.as_deref() {
            Some("low_threshold") => ensure!(
                number < finite(&raw["aq_thrhigh"]).context(invalid())?,
                invalid()
            ),
            Some("high_threshold") => ensure!(
                number > finite(&raw["aq_thrlow"]).context(invalid())?,
                invalid()
            ),
            _ => {}
        }
    }
    Ok((field.wire.clone(), wire))
}

#[cfg(test)]
#[path = "fields_tests.rs"]
mod tests;
