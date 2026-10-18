//! EZSP frame IDs (16-bit, little-endian on the wire in the extended format).
//!
//! Values transcribed from zigbee-herdsman (MIT),
//! `src/adapter/ember/ezsp/enums.ts`, enum `EzspFrameID`.

// Configuration frames
pub const VERSION: u16 = 0x0000;
pub const GET_CONFIGURATION_VALUE: u16 = 0x0052;
pub const SET_CONFIGURATION_VALUE: u16 = 0x0053;
pub const ADD_ENDPOINT: u16 = 0x0002;
pub const SET_POLICY: u16 = 0x0055;
pub const GET_VALUE: u16 = 0x00aa;
pub const SET_VALUE: u16 = 0x00ab;

// Utilities frames
pub const NOP: u16 = 0x0005;
/// Sent by the NCP instead of a response when it does not know the command.
pub const INVALID_COMMAND: u16 = 0x0058;
pub const CALLBACK: u16 = 0x0006;
pub const GET_EUI64: u16 = 0x0026;
pub const GET_NODE_ID: u16 = 0x0027;
pub const GET_TOKEN_DATA: u16 = 0x0102;
pub const SET_TOKEN_DATA: u16 = 0x0103;

// Networking frames
pub const NETWORK_INIT: u16 = 0x0017;
pub const NETWORK_STATE: u16 = 0x0018;
pub const STACK_STATUS_HANDLER: u16 = 0x0019;
pub const FORM_NETWORK: u16 = 0x001e;
pub const PERMIT_JOINING: u16 = 0x0022;
pub const GET_NETWORK_PARAMETERS: u16 = 0x0028;
pub const SET_RADIO_CHANNEL: u16 = 0x009a;

// Messaging frames
pub const SEND_UNICAST: u16 = 0x0034;
pub const SEND_BROADCAST: u16 = 0x0036;
pub const MESSAGE_SENT_HANDLER: u16 = 0x003f;
pub const INCOMING_MESSAGE_HANDLER: u16 = 0x0045;

// Security frames
pub const SET_INITIAL_SECURITY_STATE: u16 = 0x0068;
pub const GET_CURRENT_SECURITY_STATE: u16 = 0x0069;
pub const EXPORT_KEY: u16 = 0x0114;
pub const IMPORT_KEY: u16 = 0x0115;
pub const CLEAR_KEY_TABLE: u16 = 0x00b1;
pub const CLEAR_TRANSIENT_LINK_KEYS: u16 = 0x006b;
pub const IMPORT_TRANSIENT_KEY: u16 = 0x0111;

// Trust center frames
pub const TRUST_CENTER_JOIN_HANDLER: u16 = 0x0024;

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [u16; 33] = [
        VERSION,
        GET_CONFIGURATION_VALUE,
        SET_CONFIGURATION_VALUE,
        ADD_ENDPOINT,
        SET_POLICY,
        GET_VALUE,
        SET_VALUE,
        NOP,
        INVALID_COMMAND,
        CALLBACK,
        GET_EUI64,
        GET_NODE_ID,
        GET_TOKEN_DATA,
        SET_TOKEN_DATA,
        NETWORK_INIT,
        NETWORK_STATE,
        STACK_STATUS_HANDLER,
        FORM_NETWORK,
        PERMIT_JOINING,
        GET_NETWORK_PARAMETERS,
        SET_RADIO_CHANNEL,
        SEND_UNICAST,
        SEND_BROADCAST,
        MESSAGE_SENT_HANDLER,
        INCOMING_MESSAGE_HANDLER,
        SET_INITIAL_SECURITY_STATE,
        GET_CURRENT_SECURITY_STATE,
        EXPORT_KEY,
        IMPORT_KEY,
        CLEAR_KEY_TABLE,
        CLEAR_TRANSIENT_LINK_KEYS,
        IMPORT_TRANSIENT_KEY,
        TRUST_CENTER_JOIN_HANDLER,
    ];

    #[test]
    fn ids_are_distinct() {
        let mut sorted = ALL;
        sorted.sort_unstable();
        assert!(sorted.windows(2).all(|w| w[0] != w[1]), "{sorted:x?}");
    }
}
