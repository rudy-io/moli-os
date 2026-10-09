//! UPnP for LAN devices: SSDP discovery, device descriptions, SOAP calls.
//! Minimal on purpose: the documents these devices serve are small and
//! regular; tags are read by name, namespace prefixes ignored.

use std::time::Duration;

use anyhow::bail;
use http::{Method, Request};
use http_body_util::Full;
use hyper::body::Bytes;
use tokio::net::UdpSocket;

/// Text between `<name>` and its closing tag (`<ns:name>` too).
#[must_use]
pub fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = xml
        .find(&format!(":{name}>"))
        .or_else(|| xml.find(&format!("<{name}>")))?;
    let start = open + xml[open..].find('>')? + 1;
    let end = start + xml[start..].find("</")?;
    Some(&xml[start..end])
}

#[must_use]
pub fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[must_use]
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// One SOAP action; the response body.
pub async fn soap(
    host: &str,
    port: u16,
    control_path: &str,
    service: &str,
    action: &str,
    args: &[(&str, &str)],
) -> anyhow::Result<String> {
    let args = args.iter().fold(String::new(), |mut out, (k, v)| {
        use std::fmt::Write as _;
        let _ = write!(out, "<{k}>{}</{k}>", escape(v));
        out
    });
    let body = format!(
        r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{action} xmlns:u="{service}">{args}</u:{action}></s:Body></s:Envelope>"#
    );
    let request = Request::builder()
        .method(Method::POST)
        .uri(control_path)
        .header("host", format!("{host}:{port}"))
        .header("content-type", r#"text/xml; charset="utf-8""#)
        .header("soapaction", format!("\"{service}#{action}\""))
        .body(Full::new(Bytes::from(body)))?;
    let (status, bytes) = crate::plain(host, port, request).await?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if !status.is_success() {
        bail!(
            "{action} refused ({status}): {}",
            tag(&text, "errorCode").unwrap_or("?")
        );
    }
    Ok(text)
}

/// GET a document (device description) from a LAN device. Like [`soap`],
/// the answer is capped at [`crate::MAX_BODY`] by [`crate::plain`].
pub async fn fetch(host: &str, port: u16, path: &str) -> anyhow::Result<String> {
    let request = Request::builder()
        .uri(path)
        .header("host", format!("{host}:{port}"))
        .body(Full::new(Bytes::new()))?;
    let (status, bytes) = crate::plain(host, port, request).await?;
    if !status.is_success() {
        bail!("{path}: {status}");
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Where a description lives: host, port, path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Location {
    #[must_use]
    pub fn parse(url: &str) -> Option<Self> {
        let rest = url.trim().strip_prefix("http://")?;
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (h, p.parse().ok()?),
            None => (authority, 80),
        };
        Some(Self {
            host: host.to_owned(),
            port,
            path: format!("/{path}"),
        })
    }
}

/// SSDP search: the `LOCATION`s of devices answering for `target`.
pub async fn discover(target: &str, wait: Duration) -> anyhow::Result<Vec<Location>> {
    let socket = UdpSocket::bind("0.0.0.0:0").await?;
    let search = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {target}\r\n\r\n"
    );
    socket
        .send_to(search.as_bytes(), "239.255.255.250:1900")
        .await?;
    let mut found = Vec::new();
    let mut buf = vec![0u8; 2048];
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Ok((n, _))) = tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await {
        let answer = String::from_utf8_lossy(&buf[..n]);
        let location = answer.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case("location")
                .then(|| value.trim().to_owned())
        });
        if let Some(location) = location.as_deref().and_then(Location::parse)
            && !found.contains(&location)
        {
            found.push(location);
        }
    }
    Ok(found)
}

/// The control path of `service` (a prefix of its type, any version).
#[must_use]
pub fn control_url(description: &str, service: &str) -> Option<(String, String)> {
    description.split("<service>").skip(1).find_map(|block| {
        let kind = tag(block, "serviceType")?;
        if !kind.starts_with(service) {
            return None;
        }
        Some((kind.to_owned(), tag(block, "controlURL")?.to_owned()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations_and_services_parse() {
        assert_eq!(
            Location::parse("http://192.168.0.1:60000/0a1b2c3d/gatedesc.xml"),
            Some(Location {
                host: "192.168.0.1".into(),
                port: 60000,
                path: "/0a1b2c3d/gatedesc.xml".into()
            })
        );
        let desc = "<root><service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:2</serviceType><controlURL>/x/WANIPConn1</controlURL></service></root>";
        assert_eq!(
            control_url(desc, "urn:schemas-upnp-org:service:WANIPConnection:"),
            Some((
                "urn:schemas-upnp-org:service:WANIPConnection:2".into(),
                "/x/WANIPConn1".into()
            ))
        );
        assert_eq!(
            tag("<u:X><NewUptime>42</NewUptime></u:X>", "NewUptime"),
            Some("42")
        );
        assert_eq!(unescape(&escape("a<b&c")), "a<b&c");
    }
}
