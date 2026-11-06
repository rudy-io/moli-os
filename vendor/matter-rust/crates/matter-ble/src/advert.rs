//! Matter commissionable BLE advertisement parsing.
//!
//! Layout (connectedhomeip `CHIPBleServiceData.h:53-136`, service data under
//! 16-bit UUID 0xFFF6, >= 8 bytes):
//! byte 0 opcode (0x00 = commissionable), byte 1 discriminator low 8 bits,
//! byte 2 bits 0-3 discriminator high 4 / bits 4-7 advertisement version,
//! bytes 3-4 vendor id LE, bytes 5-6 product id LE,
//! byte 7 flags (bit 0 = additional data / C3 present, bit 1 = extended announcement).

use crate::BtpError;

/// Parsed Matter commissionable advertisement (service data under UUID 0xFFF6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommissionableAdvert {
    /// 12-bit commissioning discriminator.
    pub discriminator: u16,
    /// Advertisement version (upper nibble of byte 2); 0 today.
    pub adv_version: u8,
    /// Vendor ID.
    pub vendor_id: u16,
    /// Product ID.
    pub product_id: u16,
    /// Bit 0 of the flags byte: C3 additional-data characteristic present.
    pub has_additional_data: bool,
    /// Bit 1 of the flags byte: extended announcement.
    pub extended_announcement: bool,
}

impl CommissionableAdvert {
    /// Parse the 0xFFF6 service-data payload of a commissionable advertisement.
    ///
    /// # Errors
    /// [`BtpError::AdvertTooShort`] under 8 bytes; [`BtpError::UnsupportedOpcode`]
    /// when byte 0 is not 0x00.
    pub fn parse(service_data: &[u8]) -> Result<Self, BtpError> {
        if service_data.len() < 8 {
            return Err(BtpError::AdvertTooShort(service_data.len()));
        }
        if service_data[0] != 0x00 {
            return Err(BtpError::UnsupportedOpcode(service_data[0]));
        }
        Ok(Self {
            discriminator: u16::from(service_data[1]) | (u16::from(service_data[2] & 0x0F) << 8),
            adv_version: service_data[2] >> 4,
            vendor_id: u16::from_le_bytes([service_data[3], service_data[4]]),
            product_id: u16::from_le_bytes([service_data[5], service_data[6]]),
            has_additional_data: service_data[7] & 0x01 != 0,
            extended_announcement: service_data[7] & 0x02 != 0,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
    use super::*;

    // Vector: test-vectors/btp/advert.json "pi_default_disc"
    #[test]
    fn parses_pi_default_advert() {
        let data = hex("00000ff1ff008000");
        let a = CommissionableAdvert::parse(&data).unwrap();
        assert_eq!(a.discriminator, 3840);
        assert_eq!(a.adv_version, 0);
        assert_eq!(a.vendor_id, 0xFFF1);
        assert_eq!(a.product_id, 0x8000);
        assert!(!a.has_additional_data);
        assert!(!a.extended_announcement);
    }

    // Vector: test-vectors/btp/advert.json "mixed_fields"
    #[test]
    fn parses_mixed_fields_advert() {
        let a = CommissionableAdvert::parse(&hex("00bc0a3412785601")).unwrap();
        assert_eq!(a.discriminator, 0xABC);
        assert_eq!(a.vendor_id, 0x1234);
        assert_eq!(a.product_id, 0x5678);
        assert!(a.has_additional_data);
    }

    #[test]
    fn rejects_short_data() {
        assert_eq!(
            CommissionableAdvert::parse(&[0u8; 7]),
            Err(BtpError::AdvertTooShort(7))
        );
    }

    #[test]
    fn rejects_nonzero_opcode() {
        // 0x01 is the legacy service-data-type enum value, NOT a valid opcode.
        assert_eq!(
            CommissionableAdvert::parse(&hex("01000ff1ff008000")),
            Err(BtpError::UnsupportedOpcode(0x01))
        );
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
}
