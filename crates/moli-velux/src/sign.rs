//! The signature a roof window's order carries. The gateway hands out a key
//! once, when its button is pressed (`hash_sign_key`, 32 bytes in base64,
//! and its id `sign_key_id`); each order then says its position, the time,
//! a nonce never used twice in the same second and the window, all signed
//! with HMAC-SHA512: `target_position{position}{timestamp}{nonce}{window}`,
//! the result in base64 with `-` and `_` for `+` and `/`.

use moli_core::encoding::{base64, from_base64};
use ring::hmac;

pub struct Signer {
    key: hmac::Key,
    pub id: String,
    /// The last order's second and nonce: the cloud refuses a pair twice.
    last: Option<(u64, u32)>,
}

/// The key never shows, even in a debug log.
impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

/// One signed order's stamp.
#[derive(Debug, PartialEq, Eq)]
pub struct Stamp {
    pub timestamp: u64,
    pub nonce: u32,
    pub hash: String,
}

impl Signer {
    /// From the key and its id as the gateway gave them; `None` when the
    /// key is not base64.
    #[must_use]
    pub fn new(key: &str, id: &str) -> Option<Self> {
        let bytes = from_base64(key).filter(|b| !b.is_empty())?;
        Some(Self {
            key: hmac::Key::new(hmac::HMAC_SHA512, &bytes),
            id: id.trim().to_owned(),
            last: None,
        })
    }

    /// Signs `position` for `window` at `now` (seconds since 1970).
    pub fn sign(&mut self, window: &str, position: i64, now: u64) -> Stamp {
        let (timestamp, nonce) = match self.last {
            Some((second, nonce)) if now <= second => (second, nonce + 1),
            _ => (now, 0),
        };
        self.last = Some((timestamp, nonce));
        let message = format!("target_position{position}{timestamp}{nonce}{window}");
        let tag = hmac::sign(&self.key, message.as_bytes());
        let hash = base64(tag.as_ref()).replace('+', "-").replace('/', "_");
        Stamp {
            timestamp,
            nonce,
            hash,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A made-up key: the bytes 0 to 31.
    const KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

    #[test]
    fn base64_both_ways() {
        assert_eq!(base64(&[0xfb, 0xff, 0xfe]), "+//+");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(from_base64("YWI"), Some(b"ab".to_vec()));
        assert_eq!(from_base64("-__-"), Some(vec![0xfb, 0xff, 0xfe]));
        assert_eq!(from_base64(KEY), Some((0..32).collect()));
        assert_eq!(from_base64("pas du base64!"), None);
    }

    /// Hashes computed apart, with Python's `hmac` and `base64`.
    #[test]
    fn an_order_is_signed_as_the_app_does() {
        let mut signer = Signer::new(KEY, "key-id").unwrap();
        let first = signer.sign("aa:bb:cc:dd:ee:04", 100, 1_700_000_000);
        assert_eq!(
            first,
            Stamp {
                timestamp: 1_700_000_000,
                nonce: 0,
                hash: "jz-e9vYUwvJcmH6GrXdmSSaUwzDNOfKaq2w1X-92RZrAE65KaATH3UFo3d-qJbhaW7yR8_ESTmR-6TbmvmrchQ==".into(),
            }
        );
        // The same second: the next nonce, never the same pair.
        let second = signer.sign("aa:bb:cc:dd:ee:04", 0, 1_700_000_000);
        assert_eq!(second.nonce, 1);
        assert_eq!(
            second.hash,
            "mRYVtjM4LBI3BVPudAOxYAgmK4dErkllBOprd4N_3y_9sxoSFSx1rnyYvzaAm8Jq-tWoFIinzldsU01QRF6yYg=="
        );
        // A clock that went back keeps the last second.
        assert_eq!(signer.sign("w", 0, 1_699_999_990).timestamp, 1_700_000_000);
        // A new second starts again at 0.
        let later = signer.sign("w", 0, 1_700_000_001);
        assert_eq!((later.timestamp, later.nonce), (1_700_000_001, 0));
    }

    #[test]
    fn a_key_that_is_not_base64_is_refused() {
        assert!(Signer::new("not base64!", "id").is_none());
        assert!(Signer::new("", "id").is_none());
    }
}
