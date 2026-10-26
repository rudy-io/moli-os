//! The few encodings Amazon's sign-in needs: base64 (standard, and URL-safe
//! without padding), hexadecimal, percent-encoding and query parameters.

pub(crate) use moli_core::encoding::{base64, base64url, hex};

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
