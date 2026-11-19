//! TLV8, the pairing messages of HomeKit (HAP): a tag byte, a length byte,
//! up to 255 bytes of value; a longer value is cut in pieces under the same
//! tag, joined back when read.

use anyhow::bail;

pub const METHOD: u8 = 0x00;
pub const IDENTIFIER: u8 = 0x01;
pub const SALT: u8 = 0x02;
pub const PUBLIC_KEY: u8 = 0x03;
pub const PROOF: u8 = 0x04;
pub const ENCRYPTED_DATA: u8 = 0x05;
pub const STATE: u8 = 0x06;
pub const ERROR: u8 = 0x07;
pub const BACK_OFF: u8 = 0x08;
pub const SIGNATURE: u8 = 0x0A;
pub const NAME: u8 = 0x11;

/// The encoding of `items`, in their order.
#[must_use]
pub fn write(items: &[(u8, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (tag, value) in items {
        if value.is_empty() {
            out.extend_from_slice(&[*tag, 0]);
        }
        for piece in value.chunks(255) {
            out.push(*tag);
            out.push(u8::try_from(piece.len()).unwrap_or(255));
            out.extend_from_slice(piece);
        }
    }
    out
}

/// The items of a message, pieces joined, in the order first seen.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Tlv(Vec<(u8, Vec<u8>)>);

impl Tlv {
    #[must_use]
    pub fn get(&self, tag: u8) -> Option<&[u8]> {
        self.0
            .iter()
            .find(|(t, _)| *t == tag)
            .map(|(_, v)| v.as_slice())
    }
}

pub fn read(mut data: &[u8]) -> anyhow::Result<Tlv> {
    let mut items: Vec<(u8, Vec<u8>)> = Vec::new();
    while !data.is_empty() {
        let [tag, len, rest @ ..] = data else {
            bail!("TLV8: item cut short");
        };
        let len = usize::from(*len);
        if rest.len() < len {
            bail!("TLV8: {len} bytes announced, {} left", rest.len());
        }
        let (value, next) = rest.split_at(len);
        match items.iter_mut().find(|(t, _)| t == tag) {
            Some((_, v)) => v.extend_from_slice(value),
            None => items.push((*tag, value.to_vec())),
        }
        data = next;
    }
    Ok(Tlv(items))
}

/// What a device's error code means, to say it.
#[must_use]
pub fn error_name(code: &[u8]) -> &'static str {
    match code.first() {
        Some(0x02) => "authentication (wrong code?)",
        Some(0x03) => "back off (too many tries, wait)",
        Some(0x04) => "max peers",
        Some(0x05) => "max tries",
        Some(0x06) => "unavailable",
        Some(0x07) => "busy",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_value_is_cut_and_joined() {
        let long = vec![7u8; 300];
        let data = write(&[(STATE, &[1]), (PUBLIC_KEY, &long)]);
        assert_eq!(&data[..3], &[STATE, 1, 1]);
        assert_eq!(&data[3..5], &[PUBLIC_KEY, 255]);
        assert_eq!(data.len(), 3 + 2 + 255 + 2 + 45);
        let tlv = read(&data).unwrap();
        assert_eq!(tlv.get(STATE), Some([1u8].as_slice()));
        assert_eq!(tlv.get(PUBLIC_KEY), Some(long.as_slice()));
        assert_eq!(tlv.get(SALT), None);
    }

    #[test]
    fn a_cut_message_is_refused() {
        assert!(read(&[STATE]).is_err());
        assert!(read(&[STATE, 3, 1]).is_err());
        assert_eq!(read(&[]).unwrap(), Tlv::default());
    }
}
