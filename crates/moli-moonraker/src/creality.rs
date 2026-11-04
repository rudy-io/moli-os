//! The live camera of a Creality printer (K2, K1): WebRTC only, no still
//! image (`/server/webcams/list` is empty, the web interface's `/webcam/`
//! has nothing behind it). The browser plays the video itself: Moli only
//! passes its offer to the printer's camera service (port 8000) and brings
//! back the answer. Both are a JSON `{type, sdp}` in base64, as go2rtc
//! reads this camera (AlexxIT/go2rtc#1600).
//!
//! The camera only reads a candidate written as go2rtc (pion) writes it,
//! ending with `ufrag … generation 0`; without one it can read, it never
//! checks the way back and ignores the browser's DTLS hello. A browser
//! hides its address behind a `.local` name, which the camera cannot
//! resolve: Moli writes the browser's address instead, when it knows it
//! (a browser on the home network). Seen on a K2 Combo, November 2026.

use std::net::IpAddr;

use anyhow::{Context as _, bail};
use http::{Method, Request};
use moli_runtime::BoxFuture;
use moli_runtime::media::LiveSource;
use serde_json::{Value as Json, json};

/// Creality's camera service.
pub(crate) const PORT: u16 = 8000;
const PATH: &str = "/call/webrtc_local";

#[derive(Debug)]
pub(crate) struct Camera {
    pub(crate) host: String,
}

impl LiveSource for Camera {
    fn answer(
        &self,
        offer: String,
        viewer: Option<IpAddr>,
    ) -> BoxFuture<'_, anyhow::Result<String>> {
        Box::pin(async move {
            let offer = for_camera(&offer, viewer);
            if !offer.contains("a=candidate:") {
                bail!(
                    "Creality camera: no address of this browser it can reach (home network only)"
                );
            }
            let body = encode(
                json!({ "type": "offer", "sdp": offer })
                    .to_string()
                    .as_bytes(),
            );
            let request = Request::builder().method(Method::POST).uri(PATH);
            let request = moli_net::text_body(request, "text/plain", body)?;
            let (status, reply) = moli_net::plain(&self.host, PORT, request).await?;
            if !status.is_success() {
                bail!("Creality camera: HTTP {}", status.as_u16());
            }
            answer_of(&reply)
        })
    }
}

/// The offer as the camera reads it: each UDP host candidate written as
/// pion writes it, a `.local` name replaced by the viewer's address (or
/// left out when it is unknown), IPv6 left out (the camera speaks IPv4).
fn for_camera(offer: &str, viewer: Option<IpAddr>) -> String {
    let ufrag = offer
        .lines()
        .find_map(|l| l.trim().strip_prefix("a=ice-ufrag:"))
        .unwrap_or("")
        .to_owned();
    let mut out = String::with_capacity(offer.len());
    for line in offer.lines().map(str::trim_end) {
        if line.is_empty() {
            continue;
        }
        if let Some(candidate) = line.strip_prefix("a=candidate:") {
            if let Some(rewritten) = candidate_for_camera(candidate, viewer, &ufrag) {
                out.push_str(&rewritten);
                out.push_str("\r\n");
            }
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out
}

/// `foundation component udp priority address port typ host …`, rewritten.
fn candidate_for_camera(candidate: &str, viewer: Option<IpAddr>, ufrag: &str) -> Option<String> {
    let f: Vec<&str> = candidate.split_whitespace().collect();
    let [
        foundation,
        component,
        protocol,
        priority,
        address,
        port,
        "typ",
        "host",
        ..,
    ] = f[..]
    else {
        return None;
    };
    if !protocol.eq_ignore_ascii_case("udp") {
        return None;
    }
    let mdns = address
        .rsplit_once('.')
        .is_some_and(|(_, tld)| tld.eq_ignore_ascii_case("local"));
    let address: IpAddr = if mdns { viewer? } else { address.parse().ok()? };
    if !address.is_ipv4() {
        return None;
    }
    Some(format!(
        "a=candidate:{foundation} {component} udp {priority} {address} {port} typ host ufrag {ufrag} generation 0"
    ))
}

/// The SDP in the camera's answer (base64 of `{"type":"answer","sdp":…}`).
fn answer_of(reply: &[u8]) -> anyhow::Result<String> {
    let text = std::str::from_utf8(reply).context("Creality camera: not text")?;
    let json = decode(text).context("Creality camera: not base64")?;
    let answer: Json = serde_json::from_slice(&json).context("Creality camera: not JSON")?;
    match answer["sdp"].as_str() {
        Some(sdp) if sdp.starts_with("v=0") => Ok(sdp.to_owned()),
        _ => bail!("Creality camera: no answer (busy, or the camera is off)"),
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text
        .bytes()
        .filter(|c| !c.is_ascii_whitespace() && *c != b'=')
    {
        let v = u32::try_from(ALPHABET.iter().position(|a| *a == c)?).ok()?;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from(acc >> bits & 0xff).ok()?);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_like_the_standard() {
        assert_eq!(encode(b"Man"), "TWFu");
        assert_eq!(encode(b"Ma"), "TWE=");
        assert_eq!(encode(b"M"), "TQ==");
        assert_eq!(encode(b""), "");
        for text in [
            "",
            "M",
            "Ma",
            "Man",
            "{\"type\":\"offer\",\"sdp\":\"v=0\\r\\n\"}",
        ] {
            assert_eq!(decode(&encode(text.as_bytes())).unwrap(), text.as_bytes());
        }
        assert_eq!(decode("TWE=\n").unwrap(), b"Ma");
        assert!(decode("T!E=").is_none());
    }

    /// A browser's offer, as the camera must read it: its `.local` name
    /// becomes its address, each candidate ends as pion writes it, TCP and
    /// IPv6 are left out.
    #[test]
    fn the_offer_names_the_browser_as_the_camera_reads_it() {
        let offer = "v=0\r\na=ice-ufrag:abcd\r\nm=video 9 UDP/TLS/RTP/SAVPF 102\r\n\
            a=candidate:1111 1 udp 2113937151 aaaaaaaa-bbbb.local 50000 typ host generation 0 network-cost 999\r\n\
            a=candidate:1 1 udp 2113939711 fe80::1 50001 typ host generation 0\r\n\
            a=candidate:2 1 tcp 1518280447 192.168.1.20 9 typ host tcptype active\r\n\
            a=candidate:3 1 udp 2113937151 192.168.1.21 50002 typ host\r\n\
            a=end-of-candidates\r\n";
        let viewer: IpAddr = "192.168.1.20".parse().unwrap();
        let sent = for_camera(offer, Some(viewer));
        let candidates: Vec<&str> = sent
            .lines()
            .filter(|l| l.starts_with("a=candidate:"))
            .collect();
        assert_eq!(
            candidates,
            [
                "a=candidate:1111 1 udp 2113937151 192.168.1.20 50000 typ host ufrag abcd generation 0",
                "a=candidate:3 1 udp 2113937151 192.168.1.21 50002 typ host ufrag abcd generation 0",
            ]
        );
        assert!(sent.contains("a=end-of-candidates\r\n") && sent.starts_with("v=0\r\n"));
        // From outside the home network: the `.local` name stays unknown.
        let away = for_camera(offer, None);
        assert_eq!(
            away.lines()
                .filter(|l| l.starts_with("a=candidate:"))
                .count(),
            1
        );
    }

    #[test]
    fn the_answer_is_the_sdp_in_base64_json() {
        let reply = encode(br#"{"type":"answer","sdp":"v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\n"}"#);
        assert_eq!(
            answer_of(reply.as_bytes()).unwrap(),
            "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\n"
        );
        // What the camera says to a plain GET, or when busy: no answer.
        assert!(answer_of(b"{}").is_err());
        assert!(answer_of(encode(b"{}").as_bytes()).is_err());
    }
}
