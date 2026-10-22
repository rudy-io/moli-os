//! The picture of an event: fetched from the `pic_url` its push names, then
//! made a plain JPEG again. Ported from eufy-security-client (`getImageKey`,
//! `decodeImage`, `decodeImageV2.ts`; MIT, see NOTICE).
//!
//! Three shapes arrive: a plain JPEG; `eufysecurity:<serial>:<code>:` then a
//! JPEG whose first 256 bytes are AES-128-ECB encrypted under a key derived
//! from the serial, the station's P2P id and the code; or
//! `v2_eufysecurity:<serial>:<code>:` then a JPEG whose head (tables,
//! dimensions) is hidden but whose scan is plain, so a standard head is
//! spliced back on (288 × 176, the push thumbnail's size).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use aes::Aes128;
use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt as _, KeyInit as _};
use anyhow::{Context as _, bail};
use http::{Method, Request};
use http_body_util::Full;
use md5::{Digest as _, Md5};
use moli_runtime::BoxFuture;
use moli_runtime::media::{Image, SnapshotSource};
use ring::digest::{SHA256, digest};

use crate::crypto;

const LIMIT: Duration = Duration::from_secs(15);
const MAX_PICTURE: usize = 4 << 20;
const V1: &[u8] = b"eufysecurity";
const V2: &[u8] = b"v2_eufysecurity:";

/// The 4 digits of a P2P id (`ABCDEFG-123456-HIJKL`) summed as Eufy does.
fn id_suffix(p2p_did: &str) -> i32 {
    let mut parts = p2p_did.split('-');
    let (Some(head), Some(number), Some(tail), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return 0;
    };
    let upper = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_uppercase());
    if !upper(head) || !upper(tail) || !number.bytes().all(|b| b.is_ascii_digit()) {
        return 0;
    }
    let digit = |i: usize| i32::from(number.as_bytes().get(i).map_or(0, |b| b - b'0'));
    if number.len() < 6 {
        return 0;
    }
    let (n1, n2, n3, n4) = (digit(0), digit(1), digit(3), digit(5));
    let mut result = n1 + n2 + n3;
    if n3 < 5 {
        result += n3;
    }
    result + n4
}

/// The image key (32 hexadecimal characters; its first 16 bytes key AES).
fn key(serial: &str, p2p_did: &str, code: &str) -> Option<String> {
    let last = u32::from_str_radix(&serial[serial.len().checked_sub(1)?..], 16).ok()?;
    let nr = usize::try_from((last + 10) % 10).ok()?;
    let base = format!("{}{}", serial.get(nr..)?, id_suffix(p2p_did));
    let n: u64 = code.get(2..)?.parse().ok()?;
    let seed = crypto::hex(&Md5::digest(
        format!("{}{n}", 1000 - id_suffix(p2p_did)).as_bytes(),
    ))
    .to_ascii_uppercase();
    let hash = digest(&SHA256, format!("01{base}{seed}").as_bytes());
    // Wide integers, as in JavaScript: sums pass 255, wrapped at the end.
    let mut bytes: Vec<i32> = hash.as_ref().iter().map(|&b| i32::from(b)).collect();
    let start = bytes[10];
    for i in 0..32 {
        let byte = bytes[i];
        let fixed = if i < 31 { bytes[i + 1] } else { start };
        if i == 31 || i % 2 == 1 {
            bytes[10] = fixed;
            if 126 < byte || 126 < bytes[10] {
                bytes[i] = if byte <= bytes[10] {
                    bytes[10] - byte
                } else {
                    byte - bytes[10]
                };
            }
        } else if byte < 125 || fixed < 125 {
            bytes[i] = fixed + byte;
        }
    }
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let tail: Vec<u8> = bytes[16..].iter().map(|&b| b as u8).collect();
    Some(crypto::hex(&tail).to_ascii_uppercase())
}

/// A standard baseline head (quality 85, 4:2:0, 288 × 176) up to its
/// DC-chrominance table, which the v2 blob's plain tail starts with.
const V2_HEAD: &str = "ffd8ffe000104a46494600010100000100010000ffdb0043000503040404030504040405050506070c08070707070f0b0b090c110f1212110f111113161c1713141a1511111821181a1d1d1f1f1f13172224221e241c1e1f1effdb0043010505050706070e08080e1e1411141e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1effc000110800b0012003012200021101031101ffc4001f0000010501010101010100000000000000000102030405060708090a0bffc400b5100002010303020403050504040000017d01020300041105122131410613516107227114328191a1082342b1c11552d1f02433627282090a161718191a25262728292a3435363738393a434445464748494a535455565758595a636465666768696a737475767778797a838485868788898a92939495969798999aa2a3a4a5a6a7a8a9aab2b3b4b5b6b7b8b9bac2c3c4c5c6c7c8c9cad2d3d4d5d6d7d8d9dae1e2e3e4e5e6e7e8e9eaf1f2f3f4f5f6f7f8f9fa";
const DC_CHROMA: &[u8] = &[0xFF, 0xC4, 0x00, 0x1F, 0x01];

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The picture as a JPEG, whichever shape it came in.
pub fn decode(p2p_did: &str, data: &[u8]) -> Option<Vec<u8>> {
    if data.starts_with(&[0xFF, 0xD8]) {
        return Some(data.to_vec());
    }
    if let Some(rest) = data.strip_prefix(V2) {
        // `<serial>:<code>:` then binary that may hold colons itself.
        let mut fields = 0;
        let start = rest.iter().position(|&b| {
            fields += usize::from(b == b':');
            fields == 2
        })? + 1;
        let tail = &rest[start..];
        let cut = find(tail, DC_CHROMA)?;
        let mut jpeg = crypto::unhex(V2_HEAD)?;
        jpeg.extend_from_slice(&tail[cut..]);
        return Some(jpeg);
    }
    if data.len() > 41 + 256 && data.starts_with(V1) {
        let serial = std::str::from_utf8(&data[13..29]).ok()?;
        let code = std::str::from_utf8(&data[30..40]).ok()?;
        let key = key(serial, p2p_did, code)?;
        let cipher = Aes128::new(GenericArray::from_slice(&key.as_bytes()[..16]));
        let mut jpeg = data[41..].to_vec();
        for block in jpeg[..256].as_chunks_mut::<16>().0 {
            cipher.decrypt_block(GenericArray::from_mut_slice(block));
        }
        return jpeg.starts_with(&[0xFF, 0xD8]).then_some(jpeg);
    }
    None
}

/// What a picture looked like, for the log: never its content.
pub fn shape(data: &[u8]) -> &'static str {
    if data.starts_with(V2) {
        "v2"
    } else if data.starts_with(V1) {
        "v1"
    } else if data.starts_with(&[0xFF, 0xD8]) {
        "jpeg"
    } else if data.starts_with(b"<") || data.starts_with(b"{") {
        "text"
    } else {
        "other"
    }
}

/// Fetches the picture a push names, following a few redirections (Eufy
/// sends to its storage). Eufy's token goes only to Eufy, all in https.
pub async fn fetch(url: &str, token: &str) -> anyhow::Result<Vec<u8>> {
    let mut url = url.to_owned();
    for _ in 0..4 {
        let target = moli_net::WebUrl::parse(&url)?;
        if !target.tls {
            bail!("picture: not https");
        }
        let mut builder = Request::builder()
            .method(Method::GET)
            .uri(target.path.as_str());
        if is_eufy(&target.host) {
            builder = builder.header("X-Auth-Token", token);
        }
        let request = builder.body(Full::new(moli_net::Body::new()))?;
        let (status, headers, body) = moli_net::web_with_headers(
            &target.host,
            target.port,
            true,
            request,
            LIMIT,
            MAX_PICTURE,
        )
        .await?;
        if status.is_redirection() {
            let next = headers
                .get(http::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .context("picture: redirection without a location")?;
            url = if next.starts_with('/') {
                format!("https://{}:{}{next}", target.host, target.port)
            } else {
                next.to_owned()
            };
            continue;
        }
        if !status.is_success() {
            bail!("picture: {status}");
        }
        return Ok(body.to_vec());
    }
    bail!("picture: too many redirections")
}

fn is_eufy(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host.ends_with(".eufylife.com") || host.ends_with(".eufy.com")
}

/// A camera's last event picture, served when a surface asks.
#[derive(Debug, Default)]
pub struct Last(Mutex<Option<Image>>);

impl Last {
    pub fn set(&self, jpeg: Vec<u8>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(Image {
            content_type: "image/jpeg".into(),
            bytes: jpeg,
        });
    }
}

/// What the hub holds for a camera.
#[derive(Debug)]
pub struct Pictures(pub Arc<Last>);

impl SnapshotSource for Pictures {
    fn snapshot(&self) -> BoxFuture<'_, anyhow::Result<Image>> {
        Box::pin(async move {
            self.0
                .0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .with_context(|| moli_i18n::tr!("pilotes.eufy.aucune_image"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_matches_the_library() {
        // From the library's own functions, run in Node.
        assert_eq!(id_suffix("ABCDEFG-123456-HIJKL"), 17);
        assert_eq!(id_suffix("EUFYCAM-047812-ABCDE"), 14);
        assert_eq!(id_suffix("bad"), 0);
        assert_eq!(
            key("T8111P00000000AB", "ABCDEFG-123456-HIJKL", "0012345678").unwrap(),
            "D2254510FD6D034112641A5A3A751D3B"
        );
        assert_eq!(
            key("T8410P0000000003", "EUFYCAM-047812-ABCDE", "1099887766").unwrap(),
            "9C79193C8D89A85DC3B83E3CCE082172"
        );
    }

    #[test]
    fn an_encrypted_picture_is_a_jpeg_again() {
        // Encrypted by Node with that key: FF D8 then bytes 2, 3, 4…
        let blob = crypto::unbase64(concat!(
            "ZXVmeXNlY3VyaXR5OlQ4MTExUDAwMDAwMDAwQUI6MDAxMjM0NTY3ODp7civthIYIsmM1wCF/VhMKL/Vg",
            "wZi6HQxLNZnWmXkXX1L5TO3QTFgnG2qIY1gm3NREzNXpaNgTciOLpMYDvA25JSoi8fMhg5x85LYkqlCC",
            "oo3qzk9pceQECLE+f7ozSNMIAbEBE7/ochvEY+499Iv288ta5t9J20VxMWLo/XEubOAYZLkYkh9aHLE6",
            "4HGqyYsPR9rLgNcyb6dqFWrXzIUkyHMipnayHKtAuLiPl9NwSiS11SqIBJJNoBmcn074eyzkFCQpHfqk",
            "MO5F/ZX7iqFZZro6J8Ed0X4wFa7fWZxSIWLabkhItKSq6FUuSK3DzmmJXwuMBx73Pt3L4+aaRCmcAAEC",
            "AwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKis=",
        ))
        .unwrap();
        let jpeg = decode("ABCDEFG-123456-HIJKL", &blob).unwrap();
        assert_eq!(jpeg.len(), 300);
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        assert!(
            jpeg[2..]
                .iter()
                .enumerate()
                .all(|(i, &b)| usize::from(b) == (i + 2) & 0xFF)
        );
        // The wrong station: not a JPEG, not shown.
        assert!(decode("EUFYCAM-047812-ABCDE", &blob).is_none());
    }

    #[test]
    fn the_token_goes_only_to_eufy() {
        assert!(is_eufy("security-app-eu.eufylife.com"));
        assert!(is_eufy("Pic.EUFY.com"));
        assert!(!is_eufy("eufylife.com.example.org"));
        assert!(!is_eufy("s3.amazonaws.com"));
    }

    #[test]
    fn a_plain_or_v2_picture_is_kept_or_spliced() {
        assert_eq!(decode("", &[0xFF, 0xD8, 1, 2]).unwrap(), [0xFF, 0xD8, 1, 2]);
        let mut v2 = b"v2_eufysecurity:T8410P0000000003:1099887766:".to_vec();
        v2.extend_from_slice(&[0x3A, 9, 9]);
        v2.extend_from_slice(DC_CHROMA);
        v2.extend_from_slice(&[7, 0xFF, 0xD9]);
        let jpeg = decode("", &v2).unwrap();
        assert!(jpeg.starts_with(&[0xFF, 0xD8]));
        assert!(jpeg.ends_with(&[0xFF, 0xC4, 0x00, 0x1F, 0x01, 7, 0xFF, 0xD9]));
        // Height 176, width 288 in the frame header.
        let sof = find(&jpeg, &[0xFF, 0xC0]).unwrap();
        assert_eq!(&jpeg[sof + 5..sof + 9], &[0x00, 0xB0, 0x01, 0x20]);
        assert!(decode("", b"<html>").is_none());
    }
}
