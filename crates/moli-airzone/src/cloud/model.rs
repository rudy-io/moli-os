//! Read-only Web API models. Device documents preserve null/absent distinctions.
use super::Error;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSection {
    User,
    Advanced,
    All,
}
impl ConfigSection {
    pub(super) fn query(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Advanced => "advanced",
            Self::All => "all",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Location {
    #[serde(rename = "_id")]
    pub id: String,
    pub google_place_id: Option<String>,
    #[serde(rename = "timezoneId")]
    pub timezone: Option<String>,
    pub coords: Option<Coordinates>,
    #[serde(default)]
    pub text: Map<String, Value>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Coordinates {
    pub lat: f64,
    pub lng: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Profile {
    #[serde(rename = "_id")]
    pub id: String,
    pub email: String,
    pub confirmation_date: Option<String>,
    pub created_at: Option<String>,
    #[serde(default)]
    pub data: Map<String, Value>,
    #[serde(default)]
    pub config: Map<String, Value>,
}

/// The manufacturer permission is descriptive; this client never enables writes.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Admin,
    Advanced,
    Basic,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Installation {
    pub installation_id: String,
    pub location_id: String,
    pub name: String,
    pub access_type: Access,
    #[serde(default)]
    pub ws_ids: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InstallationDetail {
    #[serde(rename = "_id")]
    pub id: Option<String>,
    pub user_id: Option<String>,
    pub confirmation_date: Option<String>,
    pub installation_id: String,
    pub location_id: String,
    pub name: String,
    pub access_type: Access,
    pub groups: Vec<Group>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Group {
    pub group_id: String,
    pub name: String,
    pub icon: Option<f64>,
    pub devices: Vec<Device>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Device {
    pub device_id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub ws_id: String,
    #[serde(default)]
    pub meta: Option<DeviceMetadata>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DeviceMetadata {
    pub system_number: Option<u32>,
    pub zone_number: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Temperature {
    pub celsius: Option<f64>,
    pub fah: Option<f64>,
}

/// Bounded manufacturer document, not a generic proxy or command capability.
/// Typed accessors are fallible: malformed and absent values never become zero.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(transparent)]
pub struct DeviceDocument(Map<String, Value>);

impl DeviceDocument {
    pub(super) fn parse(value: Value) -> Result<Self, Error> {
        let Value::Object(fields) = value else {
            return Err(Error::InvalidResponse);
        };
        if fields.is_empty() || !bounded(&Value::Object(fields.clone()), 0) {
            return Err(Error::InvalidResponse);
        }
        Ok(Self(fields))
    }

    #[must_use]
    pub fn fields(&self) -> &Map<String, Value> {
        &self.0
    }

    pub fn connected(&self) -> Result<Option<bool>, Error> {
        self.boolean("isConnected")
    }

    pub fn power(&self) -> Result<Option<bool>, Error> {
        self.boolean("power")
    }

    pub fn mode(&self) -> Result<Option<i64>, Error> {
        self.optional("mode")
            .map(|v| v.as_i64().ok_or(Error::InvalidResponse))
            .transpose()
    }

    pub fn room_temperature(&self) -> Result<Option<Temperature>, Error> {
        self.optional("local_temp")
            .map(|v| serde_json::from_value(v.clone()).map_err(|_| Error::InvalidResponse))
            .transpose()
    }

    fn boolean(&self, key: &str) -> Result<Option<bool>, Error> {
        self.optional(key)
            .map(|v| v.as_bool().ok_or(Error::InvalidResponse))
            .transpose()
    }

    fn optional(&self, key: &str) -> Option<&Value> {
        match self.0.get(key) {
            Some(Value::Null) | None => None,
            Some(value) => Some(value),
        }
    }
}

pub(super) fn bounded(value: &Value, depth: usize) -> bool {
    if depth > 12 {
        return false;
    }
    match value {
        Value::Object(values) => {
            values.len() <= 512
                && values.iter().all(|(key, value)| {
                    key.len() <= 128
                        && !matches!(
                            key.to_ascii_lowercase().as_str(),
                            "token"
                                | "refreshtoken"
                                | "refresh_token"
                                | "password"
                                | "authorization"
                        )
                        && bounded(value, depth + 1)
                })
        }
        Value::Array(values) => values.len() <= 512 && values.iter().all(|v| bounded(v, depth + 1)),
        Value::String(value) => value.len() <= 4096,
        _ => true,
    }
}
