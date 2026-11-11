use super::{
    Catalog, CatalogIdentity, CollectionData, CollectionFailure, CollectionKind, CollectionState,
    CollectionStatus, Definition, InterpretationWarning, InventoryError, MAX_CATALOG_BYTES,
    MAX_COLLECTION_BYTES, MAX_DEFINITION_BYTES, MAX_DEPTH, MAX_ID_BYTES, MAX_LABEL_BYTES,
    MAX_NODES, MAX_OBJECTS, MAX_STRING_BYTES, ProgramReference, RawDefinition, TargetLabel,
};
use serde_json::{Map, Value};
use std::collections::HashSet;

#[derive(Default)]
struct Budget {
    nodes: usize,
    objects: usize,
}
impl Budget {
    fn add_nodes(&mut self, count: usize) -> Result<(), InventoryError> {
        self.nodes += count;
        if self.nodes > MAX_NODES {
            return Err(InventoryError::NodeLimit);
        }
        Ok(())
    }
    fn add_entries(&mut self, count: usize) -> Result<(), InventoryError> {
        self.objects += count;
        if self.objects > MAX_OBJECTS {
            return Err(InventoryError::ObjectLimit);
        }
        Ok(())
    }
}
fn string(value: &str, limit: usize, error: InventoryError) -> Result<(), InventoryError> {
    if value.len() > limit {
        return Err(error);
    }
    Ok(())
}
fn identifier(value: &str) -> Result<(), InventoryError> {
    string(value, MAX_ID_BYTES, InventoryError::IdLimit)?;
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(InventoryError::InvalidIdentity);
    }
    Ok(())
}
fn id_key(key: &str) -> bool {
    matches!(
        key,
        "oid" | "id" | "triggerId" | "actionGroupOID" | "calendarDayOID" | "blockId"
    )
}
fn inspect(value: &Value, depth: usize, budget: &mut Budget) -> Result<(), InventoryError> {
    if depth > MAX_DEPTH {
        return Err(InventoryError::DepthLimit);
    }
    budget.add_nodes(1)?;
    match value {
        Value::String(value) => string(value, MAX_STRING_BYTES, InventoryError::StringLimit)?,
        Value::Array(values) => {
            for value in values {
                inspect(value, depth + 1, budget)?;
            }
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                budget.add_nodes(1)?; // Object keys count too; they are bounded input strings.
                string(key, MAX_STRING_BYTES, InventoryError::StringLimit)?;
                if key == "label"
                    && let Some(label) = value.as_str()
                {
                    string(label, MAX_LABEL_BYTES, InventoryError::LabelLimit)?;
                }
                if id_key(key)
                    && let Some(id) = value.as_str()
                {
                    string(id, MAX_ID_BYTES, InventoryError::IdLimit)?;
                }
                inspect(value, depth + 1, budget)?;
            }
        }
        _ => {}
    }
    Ok(())
}
// A small lexical guard, not a JSON parser: serde still validates the complete syntax.
// Default serde_json would round wider integer literals through f64. Reject those
// instead, while leaving quoted strings and finite fractional/exponent numbers alone.
fn exact_integer_literals(body: &[u8]) -> Result<(), InventoryError> {
    let mut cursor = 0;
    while cursor < body.len() {
        if body[cursor] == b'"' {
            cursor += 1;
            while cursor < body.len() {
                match body[cursor] {
                    b'\\' => cursor += 2,
                    b'"' => {
                        cursor += 1;
                        break;
                    }
                    _ => cursor += 1,
                }
            }
        } else if body[cursor] == b'-' || body[cursor].is_ascii_digit() {
            let start = cursor;
            while cursor < body.len()
                && matches!(body[cursor], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
            {
                cursor += 1;
            }
            let token = &body[start..cursor];
            if !token.iter().any(|byte| matches!(byte, b'.' | b'e' | b'E')) {
                let token = std::str::from_utf8(token).map_err(|_| InventoryError::Malformed)?;
                if token.parse::<i64>().is_err() && token.parse::<u64>().is_err() {
                    return Err(InventoryError::Malformed);
                }
            }
        } else {
            cursor += 1;
        }
    }
    Ok(())
}

fn body_json(body: &[u8], max: usize) -> Result<Value, InventoryError> {
    if body.len() > max {
        return Err(InventoryError::ByteLimit);
    }
    exact_integer_literals(body)?;
    serde_json::from_slice(body).map_err(|_| InventoryError::Malformed)
}
fn known_wrapper(kind: CollectionKind, name: &str) -> bool {
    match kind {
        CollectionKind::CalendarRules => matches!(
            name,
            "specificDayCalendarRule" | "dailyCalendarRule" | "weeklyCalendarRule"
        ),
        CollectionKind::Triggers => matches!(
            name,
            "ifThenSetupTrigger"
                | "discreteTrigger"
                | "discreteSensorTrigger"
                | "genericCommandSchedulingTrigger"
                | "delayedCommandSchedulingTrigger"
                | "delayedCommandsSchedulingTrigger"
                | "genericPointToPointRegulationTrigger"
        ),
        _ => false,
    }
}
fn optional_string(
    fields: &Map<String, Value>,
    key: &str,
) -> Result<Option<String>, InventoryError> {
    fields
        .get(key)
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or(InventoryError::InvalidStructure)
        })
        .transpose()
}
fn references(value: &Value, result: &mut Vec<ProgramReference>) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                let collection = match key.as_str() {
                    "actionGroupOID" => Some(CollectionKind::Scenarios),
                    "calendarDayOID" => Some(CollectionKind::CalendarDays),
                    "triggerId" => Some(CollectionKind::Triggers),
                    _ => None,
                };
                if let (Some(collection), Some(id)) = (collection, value.as_str()) {
                    result.push(ProgramReference {
                        collection,
                        id: id.into(),
                    });
                }
                references(value, result);
            }
        }
        Value::Array(values) => {
            for value in values {
                references(value, result);
            }
        }
        _ => {}
    }
}
fn definition(kind: CollectionKind, raw: Value) -> Result<Definition, InventoryError> {
    let object = raw.as_object().ok_or(InventoryError::InvalidStructure)?;
    let mut fields = object;
    let mut variant = None;
    let mut unknown = false;
    if matches!(
        kind,
        CollectionKind::CalendarRules | CollectionKind::Triggers
    ) {
        let recognized = object.keys().any(|name| known_wrapper(kind, name));
        if recognized && object.len() != 1 {
            return Err(InventoryError::InvalidStructure);
        }
        if object.len() == 1 {
            let (name, value) = object
                .iter()
                .next()
                .ok_or(InventoryError::InvalidStructure)?;
            variant = Some(name.clone());
            unknown = !known_wrapper(kind, name);
            if recognized {
                fields = value.as_object().ok_or(InventoryError::InvalidStructure)?;
            } else if let Some(inner) = value.as_object() {
                fields = inner;
            }
        } else {
            unknown = true;
        }
        if recognized && !fields.contains_key("oid") {
            return Err(InventoryError::InvalidStructure);
        }
    }
    let id = if unknown {
        fields.get("oid").and_then(Value::as_str).map(str::to_owned)
    } else {
        optional_string(fields, "oid")?
    };
    if let Some(id) = &id {
        identifier(id)?;
    }
    if matches!(
        kind,
        CollectionKind::Scenarios | CollectionKind::CalendarDays
    ) && id.is_none()
    {
        unknown = true;
    }
    let label = if unknown {
        fields
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_owned)
    } else {
        optional_string(fields, "label")?
    };
    let mut links = Vec::new();
    references(&raw, &mut links);
    let mut warnings = vec![InterpretationWarning::NotEvaluated];
    if unknown {
        warnings.push(InterpretationWarning::UnknownVariant);
    }
    Ok(Definition {
        id,
        label,
        variant,
        references: links,
        warnings,
        raw: RawDefinition(raw),
    })
}
fn collection(
    kind: CollectionKind,
    values: Vec<Value>,
    budget: &mut Budget,
) -> Result<CollectionData, InventoryError> {
    budget.add_entries(values.len())?;
    budget.add_nodes(1)?; // The response array itself, excluding its enclosing catalogue DTO.
    let mut ids = HashSet::new();
    let mut definitions = Vec::with_capacity(values.len());
    for value in values {
        if !value.is_object() {
            return Err(InventoryError::InvalidStructure);
        }
        if serde_json::to_vec(&value)
            .map_err(|_| InventoryError::Malformed)?
            .len()
            > MAX_DEFINITION_BYTES
        {
            return Err(InventoryError::DefinitionLimit);
        }
        inspect(&value, 1, budget)?;
        let entry = definition(kind, value)?;
        if let Some(id) = &entry.id
            && !ids.insert(id.clone())
        {
            return Err(InventoryError::DuplicateId);
        }
        definitions.push(entry);
    }
    Ok(CollectionData { kind, definitions })
}
/// Parse an HTTP 200 collection array, retaining every bounded object verbatim as JSON.
/// No operation in this module evaluates programs, computes dates or creates commands.
pub fn parse_collection(
    kind: CollectionKind,
    body: &[u8],
) -> Result<CollectionData, InventoryError> {
    let Value::Array(values) = body_json(body, MAX_COLLECTION_BYTES)? else {
        return Err(InventoryError::InvalidStructure);
    };
    collection(kind, values, &mut Budget::default())
}
fn timezone(value: &str) -> Result<(), InventoryError> {
    string(value, MAX_ID_BYTES, InventoryError::IdLimit)?;
    if value == "UTC" {
        return Ok(());
    }
    if !value.contains('/')
        || value.split('/').any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'+'))
        })
    {
        return Err(InventoryError::InvalidStructure);
    }
    Ok(())
}
/// Keep only a provider-supplied timezone name. Syntax validation is not IANA validation.
pub fn parse_location_timezone(body: &[u8]) -> Result<Option<String>, InventoryError> {
    let value = body_json(body, MAX_COLLECTION_BYTES)?;
    inspect(&value, 1, &mut Budget::default())?;
    let fields = value.as_object().ok_or(InventoryError::InvalidStructure)?;
    let result = optional_string(fields, "timezone")?;
    if let Some(value) = &result {
        timezone(value)?;
    }
    Ok(result)
}
/// Keep target URLs and optional labels, discarding states, locations and other metadata.
pub fn parse_target_labels(body: &[u8]) -> Result<Vec<TargetLabel>, InventoryError> {
    let value = body_json(body, MAX_COLLECTION_BYTES)?;
    inspect(&value, 1, &mut Budget::default())?;
    let values = value.as_array().ok_or(InventoryError::InvalidStructure)?;
    if values.len() > MAX_OBJECTS {
        return Err(InventoryError::ObjectLimit);
    }
    let mut seen = HashSet::new();
    values
        .iter()
        .map(|value| {
            let fields = value.as_object().ok_or(InventoryError::InvalidStructure)?;
            let device_url =
                optional_string(fields, "deviceURL")?.ok_or(InventoryError::InvalidStructure)?;
            if device_url.is_empty() || device_url.chars().any(char::is_control) {
                return Err(InventoryError::InvalidStructure);
            }
            if !seen.insert(device_url.clone()) {
                return Err(InventoryError::DuplicateId);
            }
            Ok(TargetLabel {
                device_url,
                label: optional_string(fields, "label")?,
            })
        })
        .collect()
}
fn validate_identity(identity: &CatalogIdentity) -> Result<(), InventoryError> {
    for value in [
        &identity.instance,
        &identity.connection_generation,
        &identity.scope.site_oid,
        &identity.scope.gateway_id,
    ] {
        identifier(value)?;
    }
    Ok(())
}
fn validate_state(
    kind: CollectionKind,
    state: &CollectionState,
    budget: &mut Budget,
) -> Result<(), InventoryError> {
    if state.last_attempt_ms.is_some_and(|time| time < 0)
        || state.last_observation_ms.is_some_and(|time| time < 0)
        || state
            .last_observation_ms
            .zip(state.last_attempt_ms)
            .is_some_and(|(observed, attempted)| observed > attempted)
        || state.data.is_some() != state.last_observation_ms.is_some()
    {
        return Err(InventoryError::InconsistentCatalog);
    }
    match state.status {
        CollectionStatus::NotObserved
            if state.last_attempt_ms.is_none()
                && state.last_observation_ms.is_none()
                && state.failure.is_none() => {}
        CollectionStatus::Available
            if state.data.is_some()
                && state.last_attempt_ms == state.last_observation_ms
                && state.failure.is_none() => {}
        status
            if state
                .failure
                .is_some_and(|failure| failure.status() == status)
                && state.last_attempt_ms.is_some() => {}
        _ => return Err(InventoryError::InconsistentCatalog),
    }
    if let Some(data) = &state.data {
        if data.kind != kind {
            return Err(InventoryError::InconsistentCatalog);
        }
        let values: Vec<Value> = data
            .definitions
            .iter()
            .map(|entry| entry.raw.0.clone())
            .collect();
        if serde_json::to_vec(&values)
            .map_err(|_| InventoryError::Malformed)?
            .len()
            > MAX_COLLECTION_BYTES
        {
            return Err(InventoryError::ByteLimit);
        }
        let rebuilt = collection(kind, values, budget)?;
        if &rebuilt != data {
            return Err(InventoryError::InconsistentCatalog);
        }
    }
    Ok(())
}
/// Revalidate every persisted definition, derived descriptor, identity and aggregate budget.
/// JSON depth/node budgets cover preserved collection bodies, not the snapshot envelope.
pub fn validate_catalog(catalog: &Catalog) -> Result<(), InventoryError> {
    if catalog.version != 1 {
        return Err(InventoryError::InvalidVersion);
    }
    validate_identity(&catalog.identity())?;
    if catalog.collections.len() != CollectionKind::ALL.len() {
        return Err(InventoryError::InconsistentCatalog);
    }
    let mut budget = Budget::default();
    for kind in CollectionKind::ALL {
        validate_state(
            kind,
            catalog
                .collections
                .get(&kind)
                .ok_or(InventoryError::InconsistentCatalog)?,
            &mut budget,
        )?;
    }
    if let Some(value) = &catalog.provider_timezone {
        timezone(value)?;
    }
    if catalog.target_labels.len() > MAX_OBJECTS {
        return Err(InventoryError::ObjectLimit);
    }
    let mut targets = HashSet::new();
    for target in &catalog.target_labels {
        string(
            &target.device_url,
            MAX_STRING_BYTES,
            InventoryError::StringLimit,
        )?;
        if target.device_url.is_empty() || target.device_url.chars().any(char::is_control) {
            return Err(InventoryError::InvalidStructure);
        }
        if let Some(label) = &target.label {
            string(label, MAX_LABEL_BYTES, InventoryError::LabelLimit)?;
        }
        if !targets.insert(&target.device_url) {
            return Err(InventoryError::DuplicateId);
        }
    }
    if serde_json::to_vec(catalog)
        .map_err(|_| InventoryError::Malformed)?
        .len()
        > MAX_CATALOG_BYTES
    {
        return Err(InventoryError::ByteLimit);
    }
    Ok(())
}
/// Encode only a valid bounded snapshot, suitable for the encrypted inventory key.
pub fn encode_catalog(catalog: &Catalog) -> Result<Vec<u8>, InventoryError> {
    validate_catalog(catalog)?;
    serde_json::to_vec(catalog).map_err(|_| InventoryError::Malformed)
}
/// Cached owner-only reads must use this validator instead of unchecked deserialization.
pub fn decode_catalog(body: &[u8]) -> Result<Catalog, InventoryError> {
    if body.len() > MAX_CATALOG_BYTES {
        return Err(InventoryError::ByteLimit);
    }
    exact_integer_literals(body)?;
    let catalog: Catalog = serde_json::from_slice(body).map_err(|_| InventoryError::Malformed)?;
    validate_catalog(&catalog)?;
    Ok(catalog)
}

// Failure envelopes have no upstream messages. Reserve their computed maximum growth so
// future failures can retain every last-good definition without crossing the hard cap.
fn validate_update_budget(catalog: &Catalog) -> Result<(), InventoryError> {
    validate_catalog(catalog)?;
    let mut size = serde_json::to_vec(catalog)
        .map_err(|_| InventoryError::Malformed)?
        .len();
    for state in catalog.collections.values() {
        let mut envelope = state.clone();
        envelope.data = None; // Definitions are unchanged by failure; compare envelope sizes only.
        let original = serde_json::to_vec(&envelope)
            .map_err(|_| InventoryError::Malformed)?
            .len();
        let mut largest = original;
        for failure in [
            CollectionFailure::Unsupported,
            CollectionFailure::Forbidden,
            CollectionFailure::Unauthorized,
            CollectionFailure::Unavailable,
            CollectionFailure::RateLimited {
                retry_after_secs: Some(u64::MAX),
            },
            // Longest serialized validation reason; no free-form provider strings exist.
            CollectionFailure::InvalidResponse {
                reason: InventoryError::InconsistentCatalog,
            },
        ] {
            envelope.last_attempt_ms = Some(i64::MAX);
            envelope.status = failure.status();
            envelope.failure = Some(failure);
            largest = largest.max(
                serde_json::to_vec(&envelope)
                    .map_err(|_| InventoryError::Malformed)?
                    .len(),
            );
        }
        size += largest - original;
    }
    if size > MAX_CATALOG_BYTES {
        return Err(InventoryError::ByteLimit);
    }
    Ok(())
}

impl Catalog {
    /// Replace minimal metadata atomically while preserving space for future failure statuses.
    pub fn apply_metadata(
        &mut self,
        identity: &CatalogIdentity,
        provider_timezone: Option<String>,
        target_labels: Vec<TargetLabel>,
    ) -> Result<(), InventoryError> {
        if &self.identity() != identity {
            return Err(InventoryError::IdentityMismatch);
        }
        validate_catalog(self)?;
        let mut candidate = self.clone();
        candidate.provider_timezone = provider_timezone;
        candidate.target_labels = target_labels;
        validate_update_budget(&candidate)?;
        *self = candidate;
        Ok(())
    }
    pub fn new(identity: CatalogIdentity) -> Result<Self, InventoryError> {
        validate_identity(&identity)?;
        Ok(Self {
            version: 1,
            instance: identity.instance,
            connection_generation: identity.connection_generation,
            scope: identity.scope,
            collections: CollectionKind::ALL
                .into_iter()
                .map(|kind| {
                    (
                        kind,
                        CollectionState {
                            status: CollectionStatus::NotObserved,
                            last_attempt_ms: None,
                            last_observation_ms: None,
                            failure: None,
                            data: None,
                        },
                    )
                })
                .collect(),
            provider_timezone: None,
            target_labels: vec![],
        })
    }
    #[must_use]
    pub fn identity(&self) -> CatalogIdentity {
        CatalogIdentity {
            instance: self.instance.clone(),
            connection_generation: self.connection_generation.clone(),
            scope: self.scope.clone(),
        }
    }
    /// Apply one independent observation. Failure retains data and its original observation time.
    /// A rejected incoming collection records invalid_response and returns its validation error.
    /// Identity mismatch and invalid timestamps leave the catalogue untouched.
    pub fn apply_collection(
        &mut self,
        identity: &CatalogIdentity,
        kind: CollectionKind,
        attempted_at_ms: i64,
        outcome: Result<CollectionData, CollectionFailure>,
    ) -> Result<(), InventoryError> {
        if &self.identity() != identity {
            return Err(InventoryError::IdentityMismatch);
        }
        validate_catalog(self)?;
        let previous = self
            .collections
            .get(&kind)
            .ok_or(InventoryError::InconsistentCatalog)?;
        if attempted_at_ms < 0
            || previous
                .last_attempt_ms
                .is_some_and(|previous| attempted_at_ms < previous)
        {
            return Err(InventoryError::InconsistentCatalog);
        }
        let mut candidate = self.clone();
        let state = candidate
            .collections
            .get_mut(&kind)
            .ok_or(InventoryError::InconsistentCatalog)?;
        state.last_attempt_ms = Some(attempted_at_ms);
        match outcome {
            Ok(data) => {
                state.status = CollectionStatus::Available;
                state.last_observation_ms = Some(attempted_at_ms);
                state.failure = None;
                state.data = Some(data);
                if let Err(error) = validate_update_budget(&candidate) {
                    self.record_failure(kind, attempted_at_ms, error.into())?;
                    return Err(error);
                }
            }
            Err(failure) => {
                state.status = failure.status();
                state.failure = Some(failure);
            }
        }
        validate_catalog(&candidate)?;
        *self = candidate;
        Ok(())
    }
    fn record_failure(
        &mut self,
        kind: CollectionKind,
        attempted_at_ms: i64,
        failure: CollectionFailure,
    ) -> Result<(), InventoryError> {
        let mut candidate = self.clone();
        let state = candidate
            .collections
            .get_mut(&kind)
            .ok_or(InventoryError::InconsistentCatalog)?;
        state.status = failure.status();
        state.failure = Some(failure);
        state.last_attempt_ms = Some(attempted_at_ms);
        validate_catalog(&candidate)?;
        *self = candidate;
        Ok(())
    }
}
