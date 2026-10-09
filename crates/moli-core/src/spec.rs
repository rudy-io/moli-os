//! What a point *is*: its kind, access, unit and meaning.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Value;

/// Shape of the values a point accepts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Kind {
    Binary,
    Numeric {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<f64>,
    },
    Enum {
        values: Vec<Arc<str>>,
    },
    Text,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct Access {
    pub read: bool,
    pub write: bool,
}

/// Physical unit. Serialized as its symbol (`"W"`, `"kWh"`, `"°C"`…).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Unit {
    Watt,
    KiloWattHour,
    WattHour,
    VoltAmpere,
    Volt,
    MilliVolt,
    Ampere,
    Celsius,
    Percent,
    Lux,
    HectoPascal,
    Decibel,
    Ppm,
    Second,
    Minute,
    Hour,
    Other(Arc<str>),
}

impl Unit {
    const TABLE: [(Unit, &'static str); 16] = [
        (Unit::Watt, "W"),
        (Unit::KiloWattHour, "kWh"),
        (Unit::WattHour, "Wh"),
        (Unit::VoltAmpere, "VA"),
        (Unit::Volt, "V"),
        (Unit::MilliVolt, "mV"),
        (Unit::Ampere, "A"),
        (Unit::Celsius, "°C"),
        (Unit::Percent, "%"),
        (Unit::Lux, "lx"),
        (Unit::HectoPascal, "hPa"),
        (Unit::Decibel, "dB"),
        (Unit::Ppm, "ppm"),
        (Unit::Second, "s"),
        (Unit::Minute, "min"),
        (Unit::Hour, "h"),
    ];

    /// Parses a unit symbol. Empty → `None`; unknown symbols are preserved.
    #[must_use]
    pub fn parse(symbol: &str) -> Option<Self> {
        let symbol = symbol.trim();
        if symbol.is_empty() {
            return None;
        }
        let symbol = match symbol {
            "seconds" | "sec" => "s",
            "minutes" => "min",
            "hours" => "h",
            other => other,
        };
        Some(
            Self::TABLE
                .iter()
                .find(|(_, s)| *s == symbol)
                .map_or_else(|| Self::Other(symbol.into()), |(u, _)| u.clone()),
        )
    }

    #[must_use]
    pub fn symbol(&self) -> &str {
        match self {
            Self::Other(s) => s,
            known => Self::TABLE
                .iter()
                .find(|(u, _)| u == known)
                .map_or("", |(_, s)| s),
        }
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.symbol())
    }
}

impl Serialize for Unit {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.symbol())
    }
}

impl<'de> Deserialize<'de> for Unit {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).ok_or_else(|| serde::de::Error::custom("empty unit"))
    }
}

/// What a point means, independent of the brand that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Semantic {
    OnOff,
    Brightness,
    ColorTemp,
    Color,
    Power,
    ApparentPower,
    Energy,
    Voltage,
    Current,
    Temperature,
    Humidity,
    Pressure,
    Illuminance,
    Battery,
    BatteryLow,
    SignalStrength,
    Contact,
    Occupancy,
    WaterLeak,
    Smoke,
    Tamper,
    Config,
    /// An order to a machine at work (pause or cancel a print): an agent
    /// asking gets a human's approval first.
    Control,
    Other,
}

impl Semantic {
    /// Brand-agnostic inference from a property name and its unit.
    /// Unit evidence wins over naming, naming over nothing.
    #[must_use]
    pub fn infer(key: &str, unit: Option<&Unit>) -> Self {
        let name = key.rsplit('.').next().unwrap_or(key).to_ascii_lowercase();
        let name = name.trim_end_matches(|c: char| c == '_' || c.is_ascii_digit());
        let name = name.strip_suffix("_l").unwrap_or(name);
        match unit {
            Some(Unit::Watt) => return Self::Power,
            Some(Unit::VoltAmpere) => return Self::ApparentPower,
            Some(Unit::KiloWattHour | Unit::WattHour) => return Self::Energy,
            Some(Unit::Volt | Unit::MilliVolt) if name != "battery" => return Self::Voltage,
            Some(Unit::Ampere) => return Self::Current,
            Some(Unit::Celsius) => return Self::Temperature,
            Some(Unit::Lux) => return Self::Illuminance,
            Some(Unit::HectoPascal) => return Self::Pressure,
            _ => {}
        }
        match name {
            "state" => Self::OnOff,
            "brightness" => Self::Brightness,
            "color_temp" => Self::ColorTemp,
            "color" | "x" | "y" | "hue" | "saturation" => Self::Color,
            "power" => Self::Power,
            "energy" => Self::Energy,
            "temperature" => Self::Temperature,
            "humidity" => Self::Humidity,
            "battery" => Self::Battery,
            "battery_low" => Self::BatteryLow,
            "linkquality" | "rssi" => Self::SignalStrength,
            "contact" => Self::Contact,
            "occupancy" | "presence" | "motion" => Self::Occupancy,
            "water_leak" => Self::WaterLeak,
            "smoke" => Self::Smoke,
            "tamper" => Self::Tamper,
            _ => Self::Other,
        }
    }
}

/// Full description of one point of a device.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PointSpec {
    /// Stable key inside the device (`state_l1`, `color.x`).
    pub key: Arc<str>,
    /// Human label suggested by the driver.
    pub label: Arc<str>,
    pub kind: Kind,
    pub access: Access,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<Unit>,
    pub semantic: Semantic,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ValueError {
    #[error("expected a boolean")]
    ExpectedBool,
    #[error("expected a number")]
    ExpectedNumber,
    #[error("expected text")]
    ExpectedText,
    #[error("{value} is out of range [{min:?}, {max:?}]")]
    OutOfRange {
        value: f64,
        min: Option<f64>,
        max: Option<f64>,
    },
    #[error("{0:?} is not one of the allowed values")]
    NotAllowed(String),
    #[error("text longer than {MAX_TEXT} characters")]
    TooLong,
}

/// Validates a value against a point's kind and returns its canonical form.
/// Numbers are canonicalized to `Float`; enum values must match exactly.
/// Longest text a point accepts: a value is a setting, never a document.
pub const MAX_TEXT: usize = 4_000;

pub fn validate(kind: &Kind, value: &Value) -> Result<Value, ValueError> {
    if let Value::Text(t) = value
        && t.chars().count() > MAX_TEXT
    {
        return Err(ValueError::TooLong);
    }
    match (kind, value) {
        (Kind::Binary, Value::Bool(_)) | (Kind::Text, Value::Text(_)) => Ok(value.clone()),
        (Kind::Binary, _) => Err(ValueError::ExpectedBool),
        (Kind::Numeric { min, max, .. }, v) => {
            let n = v
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or(ValueError::ExpectedNumber)?;
            let below = min.is_some_and(|m| n < m);
            let above = max.is_some_and(|m| n > m);
            if below || above {
                return Err(ValueError::OutOfRange {
                    value: n,
                    min: *min,
                    max: *max,
                });
            }
            Ok(Value::Float(n))
        }
        (Kind::Enum { values }, Value::Text(t)) => values
            .iter()
            .any(|v| v == t)
            .then(|| value.clone())
            .ok_or_else(|| ValueError::NotAllowed(t.to_string())),
        (Kind::Enum { .. } | Kind::Text, _) => Err(ValueError::ExpectedText),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_symbols_round_trip() {
        for (unit, symbol) in &Unit::TABLE {
            assert_eq!(Unit::parse(symbol).as_ref(), Some(unit));
            assert_eq!(unit.symbol(), *symbol);
        }
        assert_eq!(Unit::parse("µg/m³"), Some(Unit::Other("µg/m³".into())));
        assert_eq!(Unit::parse("  "), None);
        assert_eq!(serde_json::to_string(&Unit::Celsius).unwrap(), "\"°C\"");
    }

    #[test]
    fn semantic_inference() {
        assert_eq!(Semantic::infer("state", None), Semantic::OnOff);
        assert_eq!(Semantic::infer("state_l1", None), Semantic::OnOff);
        assert_eq!(
            Semantic::infer("SINSTS", Some(&Unit::VoltAmpere)),
            Semantic::ApparentPower
        );
        assert_eq!(
            Semantic::infer("EAST", Some(&Unit::WattHour)),
            Semantic::Energy
        );
        assert_eq!(
            Semantic::infer("battery", Some(&Unit::Percent)),
            Semantic::Battery
        );
        assert_eq!(
            Semantic::infer("linkquality", None),
            Semantic::SignalStrength
        );
        assert_eq!(Semantic::infer("color.x", None), Semantic::Color);
        assert_eq!(Semantic::infer("whatever", None), Semantic::Other);
    }

    #[test]
    fn validation() {
        let numeric = Kind::Numeric {
            min: Some(0.0),
            max: Some(254.0),
            step: None,
        };
        assert_eq!(validate(&numeric, &Value::Int(10)), Ok(Value::Float(10.0)));
        assert!(matches!(
            validate(&numeric, &Value::Int(300)),
            Err(ValueError::OutOfRange { .. })
        ));
        assert_eq!(
            validate(&numeric, &Value::Bool(true)),
            Err(ValueError::ExpectedNumber)
        );
        assert_eq!(
            validate(&numeric, &Value::Float(f64::NAN)),
            Err(ValueError::ExpectedNumber)
        );

        assert_eq!(
            validate(&Kind::Binary, &Value::Bool(false)),
            Ok(Value::Bool(false))
        );
        assert_eq!(
            validate(&Kind::Binary, &Value::from("ON")),
            Err(ValueError::ExpectedBool)
        );

        let modes = Kind::Enum {
            values: vec!["off".into(), "on".into()],
        };
        assert!(validate(&modes, &Value::from("on")).is_ok());
        assert_eq!(
            validate(&modes, &Value::from("maybe")),
            Err(ValueError::NotAllowed("maybe".into()))
        );
        assert_eq!(
            validate(&Kind::Text, &Value::Int(1)),
            Err(ValueError::ExpectedText)
        );
    }
}
