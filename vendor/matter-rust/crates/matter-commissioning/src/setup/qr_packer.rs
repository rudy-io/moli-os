//! Bit-level pack and unpack of the 88-bit QR-code fixed-field block
//! (Matter Core Spec §5.1.3.1). Private to the `setup` module.

use crate::setup::{
    CommissioningFlow, DiscoveryCapabilities, Discriminator, Error, Passcode, Result, SetupPayload,
};

pub(super) const FIXED_BYTE_LEN: usize = 11;

/// Pack a `SetupPayload` into the 11-byte fixed block.
///
/// # Errors
/// Returns [`Error::QrRequiresVidPid`] if either VID or PID is `None`.
/// Returns [`Error::CustomFlowUnsupported`] for `CommissioningFlow::Custom`
/// (the QR field is encodable but matter-rust deliberately does not emit
/// the vendor-specific fields a custom flow requires).
pub(super) fn pack(payload: &SetupPayload) -> Result<[u8; FIXED_BYTE_LEN]> {
    let vid = payload.vendor_id.ok_or(Error::QrRequiresVidPid)?;
    let pid = payload.product_id.ok_or(Error::QrRequiresVidPid)?;
    if matches!(payload.commissioning_flow, CommissioningFlow::Custom) {
        return Err(Error::CustomFlowUnsupported);
    }

    let mut bits = BitBuffer::new();
    bits.push(u64::from(payload.version), 3);
    bits.push(u64::from(vid), 16);
    bits.push(u64::from(pid), 16);
    bits.push(u64::from(payload.commissioning_flow.as_u8()), 2);
    bits.push(u64::from(payload.discovery_capabilities.bits()), 8);
    bits.push(u64::from(payload.discriminator.as_u16()), 12);
    bits.push(u64::from(payload.passcode.as_u32()), 27);
    bits.push(0, 4); // reserved padding
    Ok(bits.into_bytes())
}

/// Unpack a `SetupPayload` from the 11-byte fixed block.
///
/// # Errors
/// Returns variants from [`Error`] for any field that fails range
/// validation: [`Error::CommissioningFlowReserved`],
/// [`Error::DiscriminatorOutOfRange`], [`Error::PasscodeOutOfRange`],
/// [`Error::PasscodeDisallowedTrivial`].
#[allow(clippy::cast_possible_truncation)] // Each `read(N)` returns a value bounded by N bits, so the narrowing casts below are exact.
pub(super) fn unpack(bytes: &[u8; FIXED_BYTE_LEN]) -> Result<SetupPayload> {
    let mut bits = BitReader::new(bytes);
    let version = bits.read(3) as u8;
    let vid = bits.read(16) as u16;
    let pid = bits.read(16) as u16;
    let flow_raw = bits.read(2) as u8;
    let caps_raw = bits.read(8) as u8;
    let disc_raw = bits.read(12) as u16;
    let pass_raw = bits.read(27) as u32;
    let _padding = bits.read(4);

    Ok(SetupPayload {
        version,
        vendor_id: Some(vid),
        product_id: Some(pid),
        commissioning_flow: CommissioningFlow::from_u8(flow_raw)?,
        discovery_capabilities: DiscoveryCapabilities::from_bits_retain(caps_raw),
        discriminator: Discriminator::new(disc_raw)?,
        passcode: Passcode::new(pass_raw)?,
    })
}

// --- internal bit-buffer helpers --------------------------------------------

struct BitBuffer {
    bytes: [u8; FIXED_BYTE_LEN],
    pos: usize,
}

impl BitBuffer {
    fn new() -> Self {
        Self {
            bytes: [0; FIXED_BYTE_LEN],
            pos: 0,
        }
    }
    fn push(&mut self, value: u64, width: usize) {
        for i in 0..width {
            let bit = ((value >> i) & 1) as u8;
            let byte = (self.pos + i) / 8;
            let off = (self.pos + i) % 8;
            self.bytes[byte] |= bit << off;
        }
        self.pos += width;
    }
    fn into_bytes(self) -> [u8; FIXED_BYTE_LEN] {
        debug_assert_eq!(self.pos, 88);
        self.bytes
    }
}

struct BitReader<'a> {
    bytes: &'a [u8; FIXED_BYTE_LEN],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8; FIXED_BYTE_LEN]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn read(&mut self, width: usize) -> u64 {
        let mut out: u64 = 0;
        for i in 0..width {
            let byte = (self.pos + i) / 8;
            let off = (self.pos + i) % 8;
            let bit = u64::from((self.bytes[byte] >> off) & 1);
            out |= bit << i;
        }
        self.pos += width;
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::{pack, unpack, FIXED_BYTE_LEN};
    use crate::setup::*;

    fn spec_example() -> SetupPayload {
        SetupPayload {
            version: 0,
            vendor_id: Some(0xFFF1),
            product_id: Some(0x8000),
            commissioning_flow: CommissioningFlow::Standard,
            discovery_capabilities: DiscoveryCapabilities::ON_NETWORK,
            discriminator: Discriminator::new(0xF00).unwrap(),
            passcode: Passcode::new(20_202_021).unwrap(),
        }
    }

    #[test]
    fn pack_produces_11_bytes() {
        let bytes = pack(&spec_example()).unwrap();
        assert_eq!(bytes.len(), FIXED_BYTE_LEN);
    }

    #[test]
    fn pack_unpack_roundtrip_spec_example() {
        let p = spec_example();
        let bytes = pack(&p).unwrap();
        let back = unpack(&bytes).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn pack_rejects_missing_vid() {
        let mut p = spec_example();
        p.vendor_id = None;
        let err = pack(&p).unwrap_err();
        assert!(matches!(err, Error::QrRequiresVidPid));
    }

    #[test]
    fn pack_rejects_missing_pid() {
        let mut p = spec_example();
        p.product_id = None;
        let err = pack(&p).unwrap_err();
        assert!(matches!(err, Error::QrRequiresVidPid));
    }

    #[test]
    fn pack_rejects_custom_flow() {
        let mut p = spec_example();
        p.commissioning_flow = CommissioningFlow::Custom;
        let err = pack(&p).unwrap_err();
        assert!(matches!(err, Error::CustomFlowUnsupported));
    }

    #[test]
    fn unpack_reserved_flow_errors() {
        // Hand-craft a buffer with flow bits = 3 (reserved).
        let p = spec_example();
        let mut bytes = pack(&p).unwrap();
        // Flow bits are 35..=36. Byte 4 has bits 32..=39, so flow bits
        // are bits 3..=4 of byte 4. Set both to 1.
        bytes[4] |= 0b0001_1000;
        let err = unpack(&bytes).unwrap_err();
        assert!(matches!(err, Error::CommissioningFlowReserved(3)));
    }

    #[test]
    fn pack_unpack_extremes() {
        let p = SetupPayload {
            version: 0,
            vendor_id: Some(0xFFFF),
            product_id: Some(0xFFFF),
            commissioning_flow: CommissioningFlow::UserIntent,
            discovery_capabilities: DiscoveryCapabilities::BLE
                | DiscoveryCapabilities::ON_NETWORK
                | DiscoveryCapabilities::SOFT_AP,
            discriminator: Discriminator::new(0x0FFF).unwrap(),
            passcode: Passcode::new(99_000_001).unwrap(),
        };
        let bytes = pack(&p).unwrap();
        assert_eq!(unpack(&bytes).unwrap(), p);
    }
}
