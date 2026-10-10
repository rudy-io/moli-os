//! The few encodings Amazon's sign-in needs: base64 (standard, and URL-safe
//! without padding), hexadecimal, percent-encoding and query parameters.

const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64_with(bytes: &[u8], alphabet: &[u8; 64], pad: bool) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        let chars = chunk.len() + 1;
        for i in 0..4 {
            if i < chars {
                out.push(char::from(alphabet[(n >> (18 - 6 * i)) as usize & 63]));
            } else if pad {
                out.push('=');
            }
        }
    }
    out
}

/// Standard base64, with padding.
pub(crate) fn base64(bytes: &[u8]) -> String {
    base64_with(bytes, STANDARD, true)
}

/// URL-safe base64, without padding (PKCE).
pub(crate) fn base64url(bytes: &[u8]) -> String {
    base64_with(bytes, URL_SAFE, false)
}

/// Lowercase hexadecimal.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            s.push(char::from(b"0123456789abcdef"[usize::from(b >> 4)]));
            s.push(char::from(b"0123456789abcdef"[usize::from(b & 15)]));
            s
        })
}

/// Percent-encoding of everything but the unreserved characters.
pub(crate) fn percent(text: &str) -> String {
    text.bytes()
        .fold(String::with_capacity(text.len()), |mut s, b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                s.push(char::from(b));
            } else {
                s.push('%');
                s.push(char::from(b"0123456789ABCDEF"[usize::from(b >> 4)]));
                s.push(char::from(b"0123456789ABCDEF"[usize::from(b & 15)]));
            }
            s
        })
}

/// `a=1&b=2`, each value percent-encoded.
pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", percent(k), percent(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..=i + 2]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(b) => {
                        out.push(b);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The decoded value of parameter `name` in an address's query.
pub(crate) fn query_param(address: &str, name: &str) -> Option<String> {
    let query = address.split_once('?')?.1;
    let query = query.split('#').next().unwrap_or(query);
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (decode(k) == name).then(|| decode(v))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_examples() {
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
        assert_eq!(hex(b"#A2"), "234132");
    }

    #[test]
    fn addresses_are_encoded_and_read_back() {
        assert_eq!(percent("a b/é"), "a%20b%2F%C3%A9");
        assert_eq!(form(&[("x", "1 2"), ("y", "&")]), "x=1%202&y=%26");
        let url =
            "https://www.amazon.com/ap/maplanding?a=1&openid.oa2.authorization_code=ANabc%2Fd&b=%";
        assert_eq!(
            query_param(url, "openid.oa2.authorization_code").as_deref(),
            Some("ANabc/d")
        );
        assert_eq!(query_param(url, "b").as_deref(), Some("%"));
        assert_eq!(query_param(url, "missing"), None);
        assert_eq!(query_param("no query", "a"), None);
    }
}
