//! TLV8, the pairing messages' format: a type byte, a length byte, up to
//! 255 bytes of value; a longer value is split over consecutive items of
//! the same type.

pub const METHOD: u8 = 0x00;
pub const IDENTIFIER: u8 = 0x01;
pub const SALT: u8 = 0x02;
pub const PUBLIC_KEY: u8 = 0x03;
pub const PROOF: u8 = 0x04;
pub const ENCRYPTED_DATA: u8 = 0x05;
pub const STATE: u8 = 0x06;
pub const ERROR: u8 = 0x07;
pub const SIGNATURE: u8 = 0x0A;
pub const PERMISSIONS: u8 = 0x0B;
pub const SEPARATOR: u8 = 0xFF;

// Error codes.
pub const ERR_AUTHENTICATION: u8 = 0x02;
pub const ERR_BACKOFF: u8 = 0x03;
pub const ERR_MAX_PEERS: u8 = 0x04;
pub const ERR_UNAVAILABLE: u8 = 0x06;
pub const ERR_BUSY: u8 = 0x07;

/// Items in order, as written.
#[derive(Debug, Default)]
pub struct Writer(Vec<u8>);

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn add(mut self, kind: u8, value: &[u8]) -> Self {
        if value.is_empty() {
            self.0.extend_from_slice(&[kind, 0]);
        }
        for chunk in value.chunks(255) {
            self.0.push(kind);
            self.0.push(u8::try_from(chunk.len()).expect("at most 255"));
            self.0.extend_from_slice(chunk);
        }
        self
    }

    #[must_use]
    pub fn byte(self, kind: u8, value: u8) -> Self {
        self.add(kind, &[value])
    }

    pub fn finish(self) -> Vec<u8> {
        self.0
    }
}

/// A decoded message: values by type (fragments joined), first wins.
#[derive(Debug, Default)]
pub struct Items(Vec<(u8, Vec<u8>)>);

impl Items {
    /// `None` if cut short.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let mut items: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut pos = 0;
        let mut last: Option<u8> = None;
        while pos < data.len() {
            let kind = data[pos];
            let len = usize::from(*data.get(pos + 1)?);
            let value = data.get(pos + 2..pos + 2 + len)?;
            pos += 2 + len;
            // A fragment continues the item before it.
            match items.last_mut() {
                Some((k, v)) if last == Some(kind) && *k == kind => v.extend_from_slice(value),
                _ => items.push((kind, value.to_vec())),
            }
            last = Some(kind);
        }
        Some(Self(items))
    }

    pub fn get(&self, kind: u8) -> Option<&[u8]> {
        self.0
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, v)| v.as_slice())
    }

    pub fn byte(&self, kind: u8) -> Option<u8> {
        self.get(kind).and_then(|v| v.first().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_values_are_split_and_joined() {
        let key = vec![7u8; 384];
        let data = Writer::new()
            .byte(STATE, 2)
            .add(PUBLIC_KEY, &key)
            .add(SALT, &[1, 2])
            .finish();
        assert_eq!(&data[..3], &[STATE, 1, 2]);
        assert_eq!(&data[3..5], &[PUBLIC_KEY, 255]);
        let items = Items::parse(&data).unwrap();
        assert_eq!(items.byte(STATE), Some(2));
        assert_eq!(items.get(PUBLIC_KEY).unwrap(), key.as_slice());
        assert_eq!(items.get(SALT).unwrap(), &[1, 2]);
        assert!(Items::parse(&[STATE, 5, 1]).is_none());
    }
}
