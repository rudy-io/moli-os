//! Stable identities.
//!
//! Identities never derive from a human name: a driver provides a native,
//! stable identifier (IEEE address, bridge UUID…) and names live in labels.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Arc<str>);

        impl $name {
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({:?})", stringify!($name), &*self.0)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.into())
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s.into())
            }
        }
    };
}

id_type!(
    /// A configured driver instance (`z2m`, `hue-salon`…).
    InstanceId
);
id_type!(
    /// A physical or logical device: `"{instance}:{native_id}"`.
    DeviceId
);
id_type!(
    /// A single addressable value: `"{device_id}/{key}"`.
    PointId
);

impl DeviceId {
    /// Builds `"{instance}:{native}"`. `/` is reserved as the point separator
    /// and is replaced so a device id can always be recovered from a point id.
    #[must_use]
    pub fn new(instance: &InstanceId, native: &str) -> Self {
        Self(format!("{instance}:{}", native.replace('/', "_")).into())
    }

    /// The owning driver instance.
    #[must_use]
    pub fn instance(&self) -> InstanceId {
        InstanceId::from(self.0.split_once(':').map_or(&*self.0, |(i, _)| i))
    }
}

impl PointId {
    #[must_use]
    pub fn new(device: &DeviceId, key: &str) -> Self {
        Self(format!("{device}/{key}").into())
    }

    /// Splits into device id and point key. `None` if malformed.
    #[must_use]
    pub fn split(&self) -> Option<(DeviceId, &str)> {
        let (device, key) = self.0.split_once('/')?;
        (!device.is_empty() && !key.is_empty()).then(|| (DeviceId::from(device), key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_and_point_round_trip() {
        let instance = InstanceId::from("z2m");
        let device = DeviceId::new(&instance, "0x00124b0000000001");
        assert_eq!(device.as_str(), "z2m:0x00124b0000000001");
        assert_eq!(device.instance(), instance);

        let point = PointId::new(&device, "color.x");
        let (d, key) = point.split().unwrap();
        assert_eq!(d, device);
        assert_eq!(key, "color.x");
    }

    #[test]
    fn slash_in_native_id_never_breaks_point_split() {
        let device = DeviceId::new(&InstanceId::from("x"), "a/b");
        let point = PointId::new(&device, "state");
        assert_eq!(point.split().unwrap().0, device);
    }

    #[test]
    fn malformed_point_is_rejected() {
        assert!(PointId::from("nokey").split().is_none());
        assert!(PointId::from("dev/").split().is_none());
    }
}
