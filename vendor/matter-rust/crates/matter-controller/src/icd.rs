//! ICD (Intermittently Connected Device) client registration state + the
//! `IcdManagement` (0x0046) `RegisterClient` command `Value`.
//!
//! When the controller registers as a check-in client with an ICD, it generates
//! a 16-byte symmetric key and records a [`IcdRegistration`] so the check-in
//! listener can later decrypt + verify that device's unsolicited Check-In
//! messages (see [`crate::icd_listener`]).

use matter_codec::{Tag, Value};

/// ICD Management cluster id.
pub(crate) const ICD_MANAGEMENT_CLUSTER: u32 = 0x0046;

/// `IcdManagement.ClientTypeEnum` — whether the registration is permanent or
/// ephemeral (Matter Core §9.17).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum IcdClientType {
    /// The client stays registered across the device's reboots.
    Permanent,
    /// The registration is dropped when the device reboots.
    Ephemeral,
}

impl IcdClientType {
    fn to_u8(self) -> u8 {
        match self {
            Self::Permanent => 0,
            Self::Ephemeral => 1,
        }
    }
}

/// A persisted ICD client registration — the controller registered itself as a
/// check-in client with the device `node_id`, and holds the shared key needed to
/// verify that device's Check-In messages.
///
/// `Debug` is written by hand so that `key` prints as `<redacted>`: anyone
/// holding it can forge this device's Check-In messages, so it must never
/// reach a log through a stray `{:?}`.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IcdRegistration {
    /// The ICD device this registration is with.
    pub node_id: u64,
    /// The node id the ICD sends Check-Ins to (our commissioner node id).
    pub check_in_node_id: u64,
    /// The subject the ICD monitors on our behalf (typically our node id).
    pub monitored_subject: u64,
    /// The 16-byte symmetric key protecting this device's Check-In messages.
    /// Secret: this struct's `Debug` output shows `<redacted>` instead.
    pub key: [u8; 16],
    /// The device's `ICDCounter` at registration time — the floor for the
    /// monotonicity (replay) check on inbound Check-Ins.
    pub start_counter: u32,
}

impl std::fmt::Debug for IcdRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Same name, fields and order as the derive this replaces; only the
        // key's value is withheld.
        f.debug_struct("IcdRegistration")
            .field("node_id", &self.node_id)
            .field("check_in_node_id", &self.check_in_node_id)
            .field("monitored_subject", &self.monitored_subject)
            .field("key", &"<redacted>")
            .field("start_counter", &self.start_counter)
            .finish()
    }
}

impl IcdRegistration {
    /// Construct an [`IcdRegistration`] (the struct is `#[non_exhaustive]`).
    #[must_use]
    pub fn new(
        node_id: u64,
        check_in_node_id: u64,
        monitored_subject: u64,
        key: [u8; 16],
        start_counter: u32,
    ) -> Self {
        Self {
            node_id,
            check_in_node_id,
            monitored_subject,
            key,
            start_counter,
        }
    }
}

/// Build the `RegisterClient` command `Value` (0x0046 cmd 0x00): ctx0
/// `CheckInNodeID`, ctx1 `MonitoredSubject`, ctx2 `Key`, ctx4 `ClientType`. The
/// optional `VerificationKey` (ctx3) is omitted (only needed to re-key an
/// existing registration, which is deferred).
pub(crate) fn register_client_fields(
    check_in_node_id: u64,
    monitored_subject: u64,
    key: &[u8; 16],
    client_type: IcdClientType,
) -> Value {
    Value::Structure(vec![
        (Tag::Context(0), Value::Uint(check_in_node_id)),
        (Tag::Context(1), Value::Uint(monitored_subject)),
        (Tag::Context(2), Value::Bytes(key.to_vec())),
        (Tag::Context(4), Value::Uint(u64::from(client_type.to_u8()))),
    ])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;

    #[test]
    fn register_client_fields_has_expected_tags() {
        let key = [0xABu8; 16];
        let v = register_client_fields(1, 2, &key, IcdClientType::Permanent);
        let Value::Structure(m) = v else {
            panic!("expected struct")
        };
        assert_eq!(m[0], (Tag::Context(0), Value::Uint(1)));
        assert_eq!(m[1], (Tag::Context(1), Value::Uint(2)));
        assert_eq!(m[2], (Tag::Context(2), Value::Bytes(key.to_vec())));
        assert_eq!(m[3], (Tag::Context(4), Value::Uint(0)));
    }
}
