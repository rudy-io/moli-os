use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, fmt};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Region {
    Emea,
    Apac,
    Snaba,
}

/// The setup is the scope. The gateway is a verified anchor, never a device filter.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub site_oid: String,
    pub gateway_id: String,
    pub region: Region,
}
impl fmt::Debug for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Scope([redacted])")
    }
}
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogIdentity {
    pub instance: String,
    pub connection_generation: String,
    pub scope: Scope,
}
impl fmt::Debug for CatalogIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CatalogIdentity([redacted])")
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionKind {
    Scenarios,
    CalendarDays,
    CalendarRules,
    Triggers,
    Schedule,
    DelayedSchedule,
}
impl CollectionKind {
    pub const ALL: [Self; 6] = [
        Self::Scenarios,
        Self::CalendarDays,
        Self::CalendarRules,
        Self::Triggers,
        Self::Schedule,
        Self::DelayedSchedule,
    ];
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionStatus {
    NotObserved,
    Available,
    Unsupported,
    Forbidden,
    Unauthorized,
    RateLimited,
    Unavailable,
    InvalidResponse,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InventoryError {
    ByteLimit,
    DefinitionLimit,
    DepthLimit,
    NodeLimit,
    ObjectLimit,
    StringLimit,
    LabelLimit,
    IdLimit,
    Malformed,
    InvalidStructure,
    DuplicateId,
    IdentityMismatch,
    InvalidIdentity,
    InvalidVersion,
    InconsistentCatalog,
}
impl fmt::Display for InventoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cloud inventory validation: {self:?}")
    }
}
impl std::error::Error for InventoryError {}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CollectionFailure {
    Unsupported,
    Forbidden,
    Unauthorized,
    RateLimited { retry_after_secs: Option<u64> },
    Unavailable,
    InvalidResponse { reason: InventoryError },
}
impl CollectionFailure {
    #[must_use]
    pub const fn status(self) -> CollectionStatus {
        match self {
            Self::Unsupported => CollectionStatus::Unsupported,
            Self::Forbidden => CollectionStatus::Forbidden,
            Self::Unauthorized => CollectionStatus::Unauthorized,
            Self::RateLimited { .. } => CollectionStatus::RateLimited,
            Self::Unavailable => CollectionStatus::Unavailable,
            Self::InvalidResponse { .. } => CollectionStatus::InvalidResponse,
        }
    }
    /// Qualify an unsuccessful HTTP read. A 204 is an invalid inventory, not empty.
    #[must_use]
    pub const fn from_http_status(status: u16) -> Self {
        match status {
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            404 | 405 => Self::Unsupported,
            429 => Self::RateLimited {
                retry_after_secs: None,
            },
            500..=599 => Self::Unavailable,
            _ => Self::InvalidResponse {
                reason: InventoryError::InvalidStructure,
            },
        }
    }
}
impl From<InventoryError> for CollectionFailure {
    fn from(reason: InventoryError) -> Self {
        Self::InvalidResponse { reason }
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InterpretationWarning {
    NotEvaluated,
    UnknownVariant,
}
/// Serialized only for encrypted storage and owner-only responses. Never log the value.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RawDefinition(pub(super) Value);
impl RawDefinition {
    #[must_use]
    pub fn as_value(&self) -> &Value {
        &self.0
    }
}
impl fmt::Debug for RawDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RawDefinition([redacted])")
    }
}
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramReference {
    pub collection: CollectionKind,
    pub id: String,
}
impl fmt::Debug for ProgramReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProgramReference")
            .field("collection", &self.collection)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub id: Option<String>,
    pub label: Option<String>,
    pub variant: Option<String>,
    pub references: Vec<ProgramReference>,
    pub warnings: Vec<InterpretationWarning>,
    pub raw: RawDefinition,
}
impl fmt::Debug for Definition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Definition([redacted])")
    }
}
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionData {
    pub kind: CollectionKind,
    pub definitions: Vec<Definition>,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionState {
    pub status: CollectionStatus,
    pub last_attempt_ms: Option<i64>,
    pub last_observation_ms: Option<i64>,
    pub failure: Option<CollectionFailure>,
    pub data: Option<CollectionData>,
}
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TargetLabel {
    pub device_url: String,
    pub label: Option<String>,
}
impl fmt::Debug for TargetLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TargetLabel([redacted])")
    }
}
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub version: u8,
    pub instance: String,
    pub connection_generation: String,
    pub scope: Scope,
    pub collections: BTreeMap<CollectionKind, CollectionState>,
    /// Provider-supplied syntax-checked name; not verified against a timezone database.
    pub provider_timezone: Option<String>,
    pub target_labels: Vec<TargetLabel>,
}
impl fmt::Debug for Catalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Catalog([redacted])")
    }
}
