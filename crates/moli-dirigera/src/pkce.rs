//! OAuth PKCE (RFC 7636, method `S256`) as the Dirigera pairs a client:
//! a random verifier stays here, its SHA-256 goes with the authorization
//! request, and the verifier itself proves, at the token exchange, that the
//! code is ours.

use moli_core::encoding::base64url;
use ring::digest::{SHA256, digest};
use ring::rand::{SecureRandom, SystemRandom};

/// 96 random bytes: a verifier of 128 characters, the most RFC 7636 allows.
const VERIFIER_BYTES: usize = 96;

/// A fresh code verifier (base64url of 96 random bytes, 128 characters).
pub fn verifier() -> anyhow::Result<String> {
    let mut raw = [0u8; VERIFIER_BYTES];
    SystemRandom::new()
        .fill(&mut raw)
        .map_err(|_| anyhow::anyhow!("no randomness"))?;
    Ok(base64url(&raw))
}

/// `base64url(sha256(verifier))`, without padding.
#[must_use]
pub fn challenge(verifier: &str) -> String {
    base64url(digest(&SHA256, verifier.as_bytes()).as_ref())
}

/// Percent-encodes everything but the unreserved characters (RFC 3986).
#[must_use]
pub fn escape(text: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// `application/x-www-form-urlencoded` (the token request, the authorize
/// query), every name and value escaped.
#[must_use]
pub fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(name, value)| format!("{}={}", escape(name), escape(value)))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_7636_appendix_b() {
        let octets = [
            116, 24, 223, 180, 151, 153, 224, 37, 79, 250, 96, 125, 216, 173, 187, 186, 22, 212,
            37, 77, 105, 214, 191, 240, 91, 88, 5, 88, 83, 132, 141, 121,
        ];
        let verifier = base64url(&octets);
        assert_eq!(verifier, "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(
            challenge(&verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn base64url_matches_rfc_4648_without_padding() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg"),
            ("fo", "Zm8"),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg"),
            ("fooba", "Zm9vYmE"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64url(plain.as_bytes()), encoded);
        }
        // The two characters that differ from standard base64.
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn verifiers_are_long_random_and_url_safe() {
        let a = verifier().unwrap();
        let b = verifier().unwrap();
        assert_eq!(a.len(), 128);
        assert_ne!(a, b);
        assert!(
            a.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        );
    }

    #[test]
    fn the_token_form_is_encoded() {
        let body = form(&[
            ("code", "0a1b-2c3d"),
            ("name", "moli-os#ikea séjour"),
            ("grant_type", "authorization_code"),
            (
                "code_verifier",
                "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            ),
        ]);
        assert_eq!(
            body,
            "code=0a1b-2c3d&name=moli-os%23ikea%20s%C3%A9jour&grant_type=authorization_code\
             &code_verifier=dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        );
    }
}
