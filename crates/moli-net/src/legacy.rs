//! Handshake signatures from X.509 **version 1** certificates.
//!
//! Some appliances (Reolink stations) still serve v1 certificates, which
//! webpki refuses to parse. The certificate itself is trusted by its pinned
//! fingerprint; what must still be checked is that the peer holds its
//! private key: the handshake signature. This reads the public key out of
//! the v1 certificate (minimal DER) and verifies the signature with ring.

use ring::signature::{self, UnparsedPublicKey, VerificationAlgorithm};
use rustls::SignatureScheme;

const RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01];
const EC: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
const P256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
const P384: &[u8] = &[0x2B, 0x81, 0x04, 0x00, 0x22];

/// One DER element: (tag, content, rest).
fn element(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        let n = usize::from(first & 0x7F);
        if n == 0 || n > 3 || rest.len() < n {
            return None;
        }
        let len = rest[..n]
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | usize::from(b));
        (len, &rest[n..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// The key type and public key of a certificate, if it is a v1 one.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Key<'a> {
    Rsa(&'a [u8]),
    P256(&'a [u8]),
    P384(&'a [u8]),
}

pub(crate) fn v1_key(cert: &[u8]) -> Option<Key<'_>> {
    let (_, cert, _) = element(cert)?;
    let (_, tbs, _) = element(cert)?;
    if tbs.first() == Some(&0xA0) {
        return None; // explicit version: v2/v3, webpki's business
    }
    let mut rest = tbs;
    // serial, signature algorithm, issuer, validity, subject
    for _ in 0..5 {
        rest = element(rest)?.2;
    }
    let (_, spki, _) = element(rest)?;
    let (_, algorithm, after) = element(spki)?;
    let (_, oid, params) = element(algorithm)?;
    let (tag, bits, _) = element(after)?;
    if tag != 0x03 || bits.first() != Some(&0) {
        return None;
    }
    let key = &bits[1..];
    match oid {
        RSA => Some(Key::Rsa(key)),
        EC => match element(params)?.1 {
            P256 => Some(Key::P256(key)),
            P384 => Some(Key::P384(key)),
            _ => None,
        },
        _ => None,
    }
}

fn algorithm(scheme: SignatureScheme, key: &Key<'_>) -> Option<&'static dyn VerificationAlgorithm> {
    Some(match (scheme, key) {
        (SignatureScheme::RSA_PKCS1_SHA256, Key::Rsa(_)) => &signature::RSA_PKCS1_2048_8192_SHA256,
        (SignatureScheme::RSA_PKCS1_SHA384, Key::Rsa(_)) => &signature::RSA_PKCS1_2048_8192_SHA384,
        (SignatureScheme::RSA_PKCS1_SHA512, Key::Rsa(_)) => &signature::RSA_PKCS1_2048_8192_SHA512,
        (SignatureScheme::RSA_PSS_SHA256, Key::Rsa(_)) => &signature::RSA_PSS_2048_8192_SHA256,
        (SignatureScheme::RSA_PSS_SHA384, Key::Rsa(_)) => &signature::RSA_PSS_2048_8192_SHA384,
        (SignatureScheme::RSA_PSS_SHA512, Key::Rsa(_)) => &signature::RSA_PSS_2048_8192_SHA512,
        (SignatureScheme::ECDSA_NISTP256_SHA256, Key::P256(_)) => {
            &signature::ECDSA_P256_SHA256_ASN1
        }
        (SignatureScheme::ECDSA_NISTP384_SHA384, Key::P384(_)) => {
            &signature::ECDSA_P384_SHA384_ASN1
        }
        _ => return None,
    })
}

/// Whether `signature` over `message` was made with the key of the v1
/// certificate `cert`. `None`: not a v1 certificate (or unsupported key).
pub(crate) fn verify(
    cert: &[u8],
    scheme: SignatureScheme,
    message: &[u8],
    sig: &[u8],
) -> Option<bool> {
    let key = v1_key(cert)?;
    let alg = algorithm(scheme, &key)?;
    let raw = match key {
        Key::Rsa(k) | Key::P256(k) | Key::P384(k) => k,
    };
    Some(
        UnparsedPublicKey::new(alg, raw)
            .verify(message, sig)
            .is_ok(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn station_cert() -> Vec<u8> {
        // The Reolink Home Hub's (public) certificate, as served on the LAN.
        let b64 = include_str!("../../../integrations/reolink/fixtures/home-hub-cert.b64").trim();
        let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        let mut buf = 0u32;
        let mut bits = 0;
        for c in b64.bytes().filter(|&c| c != b'=') {
            let v = u32::try_from(table.iter().position(|&t| t == c).unwrap()).unwrap();
            buf = (buf << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push(u8::try_from((buf >> bits) & 0xFF).unwrap());
            }
        }
        out
    }

    #[test]
    fn reads_the_rsa_key_of_a_v1_certificate() {
        let cert = station_cert();
        let Some(Key::Rsa(key)) = v1_key(&cert) else {
            panic!("no RSA key found");
        };
        // PKCS#1 RSAPublicKey: SEQUENCE { modulus INTEGER (2048 bits), e }
        let (tag, body, _) = element(key).unwrap();
        assert_eq!(tag, 0x30);
        let (tag, modulus, _) = element(body).unwrap();
        assert_eq!(tag, 0x02);
        assert_eq!(modulus.len(), 257, "2048-bit modulus with its leading zero");
        // A wrong signature is refused, not accepted.
        assert_eq!(
            verify(
                &cert,
                SignatureScheme::RSA_PSS_SHA256,
                b"handshake",
                &[0u8; 256]
            ),
            Some(false)
        );
    }

    #[test]
    fn v3_and_garbage_are_left_alone() {
        assert_eq!(v1_key(&[0x30, 0x03, 0x30, 0x01, 0xA0]), None);
        assert_eq!(v1_key(b"not der"), None);
        assert_eq!(v1_key(&[]), None);
    }
}
