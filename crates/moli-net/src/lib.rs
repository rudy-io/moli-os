//! HTTPS to LAN appliances (Hue bridge, Reolink station…).
//!
//! They serve certificates no public CA vouches for. Instead of disabling
//! verification, the certificate's SHA-256 fingerprint is pinned the first
//! time (trust on first use) and enforced afterwards: another machine
//! answering on that address is refused.

pub mod lan;
mod legacy;
pub mod proto;
pub mod raop;
pub mod upnp;
pub mod wol;
pub mod ws;
pub mod wyoming;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context as _, bail};
use http::{Request, StatusCode};
use http_body_util::{BodyExt as _, Full, LengthLimitError, Limited};
use hyper::body::Bytes;
use hyper::client::conn::http1::{self, SendRequest};
use hyper_util::rt::TokioIo;
use ring::digest::{SHA256, digest};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use socket2::{SockRef, TcpKeepalive};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

pub use http::Method;
pub use hyper::body::Bytes as Body;
/// A body read as it arrives ([`web_stream`]).
pub use hyper::body::Incoming;

/// Per step (connect, handshake, exchange).
pub const TIMEOUT: Duration = Duration::from_secs(4);

/// Marker of a pin mismatch in handshake errors.
pub const PIN_MISMATCH: &str = "certificate does not match the pinned fingerprint";

const MIB: usize = 1024 * 1024;

/// Largest answer read from a device (JSON states, UPnP documents).
pub const MAX_BODY: usize = 4 * MIB;

/// Largest answer read by [`PinnedHttps::send_slow`] (camera images).
pub const MAX_SLOW_BODY: usize = 16 * MIB;

/// An answer over its limit. Never retried: the device would send it again.
#[derive(Debug)]
struct BodyTooLarge(usize);

impl std::fmt::Display for BodyTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.is_multiple_of(MIB) {
            write!(f, "body too large (over {} MiB)", self.0 / MIB)
        } else {
            write!(f, "body too large (over {} bytes)", self.0)
        }
    }
}

impl std::error::Error for BodyTooLarge {}

/// A whole answer, at most `limit` bytes: a device (or whatever answers in
/// its place) cannot make us buffer without end.
async fn read_body<B>(body: B, limit: usize) -> anyhow::Result<Bytes>
where
    B: hyper::body::Body<Data = Bytes>,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    match Limited::new(body, limit).collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(e) if e.is::<LengthLimitError>() => Err(BodyTooLarge(limit).into()),
        Err(e) => Err(anyhow::anyhow!(e)),
    }
}

#[derive(Debug)]
struct Pin {
    /// The fingerprint enforced. `None` until the first handshake, whose
    /// certificate becomes the only one accepted afterwards.
    expected: Mutex<Option<String>>,
    seen: Mutex<Option<String>>,
    provider: Arc<CryptoProvider>,
}

/// Ranges Moli never dials, even for a literal address written in a config
/// file: the unspecified/broadcast addresses (no destination) and link-local
/// (the cloud metadata address 169.254.169.254, and friends).
fn always_forbidden(addr: &std::net::IpAddr) -> bool {
    match addr {
        std::net::IpAddr::V4(v4) => v4_always_forbidden(*v4),
        // The IPv4-mapped form (`::ffff:a.b.c.d`) carries a v4 address:
        // check that one.
        std::net::IpAddr::V6(v6)
            if v6.segments().iter().take(5).all(|&s| s == 0) && v6.segments()[5] == 0xffff =>
        {
            v4_always_forbidden(std::net::Ipv4Addr::new(
                (v6.segments()[6] >> 8) as u8,
                (v6.segments()[6] & 0xff) as u8,
                (v6.segments()[7] >> 8) as u8,
                (v6.segments()[7] & 0xff) as u8,
            ))
        }
        std::net::IpAddr::V6(v6) => v6.is_unspecified() || v6.is_unicast_link_local(),
    }
}

/// A loopback address: the host's own API, local services. Refused when Moli
/// dials on someone else's behalf (a name resolved through DNS, a profile's
/// request); allowed only when a person wrote it in a config file, pointing
/// at their own machine.
fn is_loopback(addr: &std::net::IpAddr) -> bool {
    match addr {
        std::net::IpAddr::V4(v4) => v4.is_loopback(),
        std::net::IpAddr::V6(v6)
            if v6.segments().iter().take(5).all(|&s| s == 0) && v6.segments()[5] == 0xffff =>
        {
            std::net::Ipv4Addr::new(
                (v6.segments()[6] >> 8) as u8,
                (v6.segments()[6] & 0xff) as u8,
                (v6.segments()[7] >> 8) as u8,
                (v6.segments()[7] & 0xff) as u8,
            )
            .is_loopback()
        }
        std::net::IpAddr::V6(v6) => v6.is_loopback(),
    }
}

/// The forbidden ranges for one IPv4 address: the unspecified/broadcast
/// addresses and link-local (the cloud metadata service at
/// 169.254.169.254).
fn v4_always_forbidden(v4: std::net::Ipv4Addr) -> bool {
    v4.is_unspecified() || v4.is_broadcast() || (v4.octets()[0], v4.octets()[1]) == (169, 254)
}

/// An IP Moli must never dial: [`always_forbidden`], plus loopback. Private
/// (RFC 1918) addresses stay allowed: that is where the appliances live.
fn forbidden(addr: &std::net::IpAddr) -> bool {
    always_forbidden(addr) || is_loopback(addr)
}

/// The addresses of `host:port` Moli may dial. A literal IP is checked as
/// written: loopback only when `allow_literal_loopback`. A name is always
/// resolved up front and its addresses checked strictly (loopback included),
/// so a DNS rebinding pointing the name at `127.0.0.1` or
/// `169.254.169.254` is refused before any byte goes on the wire.
async fn allowed_addresses(
    host: &str,
    port: u16,
    allow_literal_loopback: bool,
) -> anyhow::Result<Vec<std::net::SocketAddr>> {
    match host.parse() {
        Ok(ip) if always_forbidden(&ip) || (!allow_literal_loopback && is_loopback(&ip)) => {
            anyhow::bail!("forbidden address: {ip}")
        }
        Ok(ip) => Ok(vec![std::net::SocketAddr::new(ip, port)]),
        Err(_) => {
            let resolved = tokio::net::lookup_host((host, port))
                .await
                .with_context(|| format!("cannot resolve {host}"))?;
            let addrs: Vec<std::net::SocketAddr> =
                resolved.filter(|addr| !forbidden(&addr.ip())).collect();
            if addrs.is_empty() {
                anyhow::bail!("{host} resolves only to forbidden addresses (loopback, link-local)");
            }
            Ok(addrs)
        }
    }
}

/// Connects to `host:port` with its destination checked (see
/// [`allowed_addresses`]): loopback and link-local are refused, whether the
/// address is written in a config file or reached through a name. The io
/// error of the last attempt stays in the error chain (not only in the
/// message): the profile driver tells « the device is absent » (refused,
/// timed out) from a protocol failure by its type.
pub async fn checked_connect(host: &str, port: u16) -> anyhow::Result<TcpStream> {
    connect_checked(host, port, false).await
}

/// Like [`checked_connect`], except a literal loopback address is allowed:
/// the person wrote it in a config file, pointing at their own machine
/// (Moli's own API, a local LLM, a local camera server). A name still
/// resolves to strictly checked addresses: the anti-rebinding guarantee does
/// not weaken.
pub async fn trusted_connect(host: &str, port: u16) -> anyhow::Result<TcpStream> {
    connect_checked(host, port, true).await
}

async fn connect_checked(
    host: &str,
    port: u16,
    allow_literal_loopback: bool,
) -> anyhow::Result<TcpStream> {
    let addrs = allowed_addresses(host, port, allow_literal_loopback).await?;
    let mut last_error: Option<std::io::Error> = None;
    for addr in &addrs {
        match TcpStream::connect(addr).await {
            Ok(tcp) => return Ok(tcp),
            Err(e) => last_error = Some(e),
        }
    }
    Err(
        anyhow::Error::new(last_error.expect("at least one address"))
            .context(format!("cannot reach {host}")),
    )
}

/// Lowercase hex without separators: `AB:CD:…` as typed in a config file
/// must match what [`fingerprint`] computes.
fn normalize(pin: &str) -> String {
    pin.chars()
        .filter(|c| *c != ':' && !c.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase()
}

fn fingerprint(cert: &CertificateDer<'_>) -> String {
    use std::fmt::Write as _;
    digest(&SHA256, cert.as_ref())
        .as_ref()
        .iter()
        .fold(String::with_capacity(64), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

impl ServerCertVerifier for Pin {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let seen = fingerprint(end_entity);
        *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(seen.clone());
        // Check and record under one lock: two first handshakes racing must
        // not both win.
        let mut expected = self.expected.lock().unwrap_or_else(PoisonError::into_inner);
        match expected.as_deref() {
            Some(pinned) if pinned != seen => Err(rustls::Error::General(PIN_MISMATCH.into())),
            Some(_) => Ok(ServerCertVerified::assertion()),
            None => {
                // First contact: from now on, only this certificate.
                // TOFU: surface the adopted fingerprint so a person can
                // check it against the device before it is stored.
                tracing::warn!(
                    fingerprint = %seen,
                    "certificate pin adopted at first contact (trust on first use): check it matches the device"
                );
                *expected = Some(seen);
                Ok(ServerCertVerified::assertion())
            }
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
        .or_else(|e| legacy_signature(e, message, cert, dss))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
        .or_else(|e| legacy_signature(e, message, cert, dss))
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// webpki cannot parse X.509 v1 certificates: check the handshake
/// signature against such a certificate's key directly. Anything else keeps
/// webpki's verdict.
fn legacy_signature(
    error: rustls::Error,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, rustls::Error> {
    match legacy::verify(cert.as_ref(), dss.scheme, message, dss.signature()) {
        Some(true) => Ok(HandshakeSignatureValid::assertion()),
        Some(false) => Err(rustls::Error::General(
            "handshake signature does not match the certificate".into(),
        )),
        None => Err(error),
    }
}

/// TLS for web APIs: certificates checked against the public CAs (Mozilla's
/// list, built in: the image has no system store).
pub fn public_tls() -> Arc<ClientConfig> {
    static CONFIG: std::sync::OnceLock<Arc<ClientConfig>> = std::sync::OnceLock::new();
    Arc::clone(CONFIG.get_or_init(|| {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("ring supports the default protocol versions")
                .with_root_certificates(roots)
                .with_no_client_auth();
        Arc::new(config)
    }))
}

/// A rustls client configuration that pins a certificate (trust on first
/// use), for any TLS transport (HTTPS here, MQTT in drivers).
#[derive(Clone)]
pub struct PinnedTls {
    pub config: Arc<ClientConfig>,
    pin: Arc<Pin>,
}

impl std::fmt::Debug for PinnedTls {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinnedTls").finish_non_exhaustive()
    }
}

impl PinnedTls {
    /// `pin`: the fingerprint recorded earlier (hex, any case, `:` allowed),
    /// `None` to pin the first certificate seen.
    pub fn new(pin: Option<String>) -> anyhow::Result<Self> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let pin = Arc::new(Pin {
            expected: Mutex::new(pin.map(|pin| normalize(&pin))),
            seen: Mutex::new(None),
            provider: Arc::clone(&provider),
        });
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::clone(&pin) as Arc<dyn ServerCertVerifier>)
            .with_no_client_auth();
        Ok(Self {
            config: Arc::new(config),
            pin,
        })
    }

    /// Same, presenting a client certificate (PEM): appliances that pair a
    /// remote by its certificate (Android TV Remote).
    pub fn with_client_cert(
        pin: Option<String>,
        cert_pem: &str,
        key_pem: &str,
    ) -> anyhow::Result<Self> {
        use rustls::pki_types::PrivateKeyDer;
        use rustls::pki_types::pem::PemObject as _;
        let chain = CertificateDer::pem_slice_iter(cert_pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .context("client certificate (PEM)")?;
        if chain.is_empty() {
            bail!("no certificate in the client certificate PEM");
        }
        let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).context("client key (PEM)")?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let pin = Arc::new(Pin {
            expected: Mutex::new(pin.map(|pin| normalize(&pin))),
            seen: Mutex::new(None),
            provider: Arc::clone(&provider),
        });
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::clone(&pin) as Arc<dyn ServerCertVerifier>)
            .with_client_auth_cert(chain, key)
            .context("client certificate")?;
        Ok(Self {
            config: Arc::new(config),
            pin,
        })
    }

    /// A TLS stream to `host:port` with these settings (raw protocols:
    /// protobuf over TLS, Cast…).
    pub async fn connect(
        &self,
        host: &str,
        port: u16,
    ) -> anyhow::Result<tokio_rustls::client::TlsStream<TcpStream>> {
        let tcp = tokio::time::timeout(TIMEOUT, checked_connect(host, port))
            .await
            .context("connect timeout")??;
        let _ = tcp.set_nodelay(true);
        let name = ServerName::try_from(host.to_owned()).context("server name")?;
        let tls = tokio::time::timeout(
            TIMEOUT,
            TlsConnector::from(Arc::clone(&self.config)).connect(name, tcp),
        )
        .await
        .context("TLS handshake timeout")??;
        Ok(tls)
    }

    /// Fingerprint of the certificate presented during the last handshake.
    pub fn seen_fingerprint(&self) -> Option<String> {
        self.pin
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Fingerprint enforced: the one given, or the first one seen.
    pub fn pinned_fingerprint(&self) -> Option<String> {
        self.pin
            .expected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// One appliance: TLS settings and a reusable connection for requests
/// (streams get their own).
pub struct PinnedHttps {
    host: String,
    port: u16,
    tls: TlsConnector,
    pinned: PinnedTls,
    conn: tokio::sync::Mutex<Option<SendRequest<Full<Bytes>>>>,
    /// Long requests (camera images) keep their own connection between
    /// calls: a new TLS handshake for every image was most of their cost.
    slow: tokio::sync::Mutex<Option<SendRequest<Full<Bytes>>>>,
}

impl std::fmt::Debug for PinnedHttps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PinnedHttps")
            .field("host", &self.host)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl PinnedHttps {
    /// `pin`: the fingerprint recorded earlier, `None` to pin the first
    /// certificate seen (store it before sending any secret).
    pub fn new(host: &str, port: u16, pin: Option<String>) -> anyhow::Result<Self> {
        let pinned = PinnedTls::new(pin)?;
        Ok(Self {
            host: host.to_owned(),
            port,
            tls: TlsConnector::from(Arc::clone(&pinned.config)),
            pinned,
            conn: tokio::sync::Mutex::new(None),
            slow: tokio::sync::Mutex::new(None),
        })
    }

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Fingerprint of the certificate presented during the last handshake.
    pub fn seen_fingerprint(&self) -> Option<String> {
        self.pinned.seen_fingerprint()
    }

    /// Fingerprint enforced: the one given, or the first one seen.
    pub fn pinned_fingerprint(&self) -> Option<String> {
        self.pinned.pinned_fingerprint()
    }

    /// A request builder with the `host` header set.
    #[must_use]
    pub fn request(&self, method: Method, path: &str) -> http::request::Builder {
        // With the port unless it is the default: some appliances route
        // on the whole `Host` (a Philips TV answers 404 otherwise).
        let host = if self.port == 443 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        };
        Request::builder()
            .method(method)
            .uri(path)
            .header("host", host)
    }

    async fn connect(&self) -> anyhow::Result<SendRequest<Full<Bytes>>> {
        // The host comes from the person's config file (a local camera
        // server, a gateway on the LAN): a literal loopback stays reachable,
        // while a name resolving to loopback is still refused.
        let tcp = tokio::time::timeout(TIMEOUT, trusted_connect(self.host.as_str(), self.port))
            .await
            .context("connect timeout")?
            .with_context(|| format!("cannot reach {}", self.host))?;
        // An appliance unplugged mid-stream sends nothing, not even a reset:
        // keepalive probes turn that silence into an error within minutes.
        SockRef::from(&tcp).set_tcp_keepalive(
            &TcpKeepalive::new()
                .with_time(Duration::from_secs(60))
                .with_interval(Duration::from_secs(15)),
        )?;
        let name = ServerName::try_from(self.host.clone()).context("host name")?;
        let tls = tokio::time::timeout(TIMEOUT, self.tls.connect(name, tcp))
            .await
            .context("tls handshake timeout")?
            .context("tls handshake")?;
        let (sender, connection) = http1::handshake(TokioIo::new(tls)).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(sender)
    }

    /// A request on the shared connection. A reused connection the device
    /// silently dropped gets one retry on a fresh one: only send requests
    /// that are safe to repeat.
    pub async fn send(&self, request: Request<Full<Bytes>>) -> anyhow::Result<(StatusCode, Bytes)> {
        self.send_with_challenge(request)
            .await
            .map(|(status, body, _)| (status, body))
    }

    /// Like `send`, with the `WWW-Authenticate` header of the answer (for
    /// digest authentication).
    pub async fn send_with_challenge(
        &self,
        request: Request<Full<Bytes>>,
    ) -> anyhow::Result<(StatusCode, Bytes, Option<String>)> {
        let mut conn = self.conn.lock().await;
        let reused = conn.as_ref().is_some_and(|c| !c.is_closed());
        let retry = reused.then(|| clone_request(&request));
        match self.exchange(&mut conn, request).await {
            Err(e) if retry.is_some() && !e.is::<BodyTooLarge>() => {
                tracing::debug!(host = self.host, error = %e, "stale connection, retrying once");
                self.exchange(&mut conn, retry.expect("checked")).await
            }
            result => result,
        }
    }

    /// A request allowed `limit` instead of the usual few seconds (a
    /// battery camera waking up for an image), never on the shared
    /// connection. It reuses the images' own connection when that one is
    /// free (one image after another: no handshake each time); a second
    /// image in flight gets a fresh connection. Safe to repeat: a reused
    /// connection the device dropped is retried once on a fresh one. The
    /// answer may reach [`MAX_SLOW_BODY`] (every other one: [`MAX_BODY`]).
    pub async fn send_slow(
        &self,
        request: Request<Full<Bytes>>,
        limit: Duration,
    ) -> anyhow::Result<(StatusCode, Bytes)> {
        let mut slot = self.slow.try_lock().ok();
        let reused = slot
            .as_deref_mut()
            .and_then(Option::take)
            .filter(|s| !s.is_closed());
        let retry = reused.is_some().then(|| clone_request(&request));
        let mut sender = match reused {
            Some(sender) => sender,
            None => self.connect().await?,
        };
        let first = Self::exchange_slow(&mut sender, request, limit).await;
        let (result, sender) = match (first, retry) {
            (Err(e), Some(request)) if !e.is::<BodyTooLarge>() => {
                let mut fresh = self.connect().await?;
                let result = Self::exchange_slow(&mut fresh, request, limit).await;
                (result, fresh)
            }
            (result, _) => (result, sender),
        };
        match result {
            Ok(answer) => {
                if let Some(slot) = slot.as_deref_mut() {
                    *slot = Some(sender);
                }
                Ok(answer)
            }
            Err(e) => Err(e.context(format!("{} did not answer", self.host))),
        }
    }

    async fn exchange_slow(
        sender: &mut SendRequest<Full<Bytes>>,
        request: Request<Full<Bytes>>,
        limit: Duration,
    ) -> anyhow::Result<(StatusCode, Bytes)> {
        let exchange = async {
            sender.ready().await?;
            let response = sender.send_request(request).await?;
            let status = response.status();
            let body = read_body(response.into_body(), MAX_SLOW_BODY).await?;
            anyhow::Ok((status, body))
        };
        tokio::time::timeout(limit, exchange)
            .await
            .context("no answer in time")?
    }

    async fn exchange(
        &self,
        conn: &mut Option<SendRequest<Full<Bytes>>>,
        request: Request<Full<Bytes>>,
    ) -> anyhow::Result<(StatusCode, Bytes, Option<String>)> {
        if conn.as_ref().is_none_or(SendRequest::is_closed) {
            *conn = Some(self.connect().await?);
        }
        let sender = conn.as_mut().expect("connection just ensured");
        let exchange = async {
            sender.ready().await?;
            let response = sender.send_request(request).await?;
            let status = response.status();
            let challenge = response
                .headers()
                .get(http::header::WWW_AUTHENTICATE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let body = read_body(response.into_body(), MAX_BODY).await?;
            anyhow::Ok((status, body, challenge))
        };
        match tokio::time::timeout(TIMEOUT, exchange).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(e)) => {
                *conn = None;
                Err(e)
            }
            Err(_) => {
                *conn = None;
                bail!("{} did not answer in time", self.host)
            }
        }
    }

    /// `send_with_challenge`, never retried (a key press: replaying it
    /// would toggle twice).
    pub async fn send_once_with_challenge(
        &self,
        request: Request<Full<Bytes>>,
    ) -> anyhow::Result<(StatusCode, Bytes, Option<String>)> {
        let mut conn = self.conn.lock().await;
        self.exchange(&mut conn, request).await
    }

    /// A request sent once, never retried (a login: a retry could open a
    /// second session).
    pub async fn send_once(
        &self,
        request: Request<Full<Bytes>>,
    ) -> anyhow::Result<(StatusCode, Bytes)> {
        let mut conn = self.conn.lock().await;
        self.exchange(&mut conn, request)
            .await
            .map(|(status, body, _)| (status, body))
    }

    /// Connects and shakes hands, nothing more: records the certificate's
    /// fingerprint before any secret is sent.
    pub async fn handshake(&self) -> anyhow::Result<()> {
        let mut conn = self.conn.lock().await;
        *conn = Some(self.connect().await?);
        Ok(())
    }

    /// A long-lived response (event stream) on a dedicated connection. Its
    /// length has no end by design: the caller bounds what it buffers.
    pub async fn stream(&self, request: Request<Full<Bytes>>) -> anyhow::Result<Incoming> {
        let mut sender = self.connect().await?;
        let response = tokio::time::timeout(TIMEOUT, sender.send_request(request))
            .await
            .context("stream timeout")??;
        if !response.status().is_success() {
            bail!("stream refused: {}", response.status());
        }
        Ok(response.into_body())
    }
}

fn clone_request(request: &Request<Full<Bytes>>) -> Request<Full<Bytes>> {
    let mut copy = Request::builder()
        .method(request.method().clone())
        .uri(request.uri().clone())
        .version(request.version())
        .body(request.body().clone())
        .expect("valid request");
    *copy.headers_mut() = request.headers().clone();
    copy
}

/// A JSON body for a request builder.
pub fn json_body(
    builder: http::request::Builder,
    body: &serde_json::Value,
) -> anyhow::Result<Request<Full<Bytes>>> {
    Ok(builder
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))?)
}

/// A plain text body for a request builder (`content_type` as the device
/// wants it).
pub fn text_body(
    builder: http::request::Builder,
    content_type: &str,
    body: String,
) -> anyhow::Result<Request<Full<Bytes>>> {
    Ok(builder
        .header("content-type", content_type)
        .body(Full::new(Bytes::from(body)))?)
}

/// An empty body for a request builder.
pub fn empty(builder: http::request::Builder) -> anyhow::Result<Request<Full<Bytes>>> {
    Ok(builder.body(Full::new(Bytes::new()))?)
}

/// One plain-HTTP request to a device (UPnP, local JSON APIs), on its own
/// connection: devices like these often close idle ones. The destination is
/// a [`trusted_connect`]: the address comes from the person's config file,
/// so a literal loopback (a service on the host itself) is allowed, while a
/// name is still resolved and checked strictly.
pub async fn plain(
    host: &str,
    port: u16,
    request: Request<Full<Bytes>>,
) -> anyhow::Result<(StatusCode, Bytes)> {
    plain_with_headers(host, port, request)
        .await
        .map(|(status, _, body)| (status, body))
}

/// Same, with the answer's headers (cookies, challenges).
pub async fn plain_with_headers(
    host: &str,
    port: u16,
    request: Request<Full<Bytes>>,
) -> anyhow::Result<(StatusCode, http::HeaderMap, Bytes)> {
    let exchange = async {
        let tcp = trusted_connect(host, port)
            .await
            .with_context(|| format!("cannot reach {host}:{port}"))?;
        let (mut sender, connection) = http1::handshake(TokioIo::new(tcp)).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let response = sender.send_request(request).await?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = read_body(response.into_body(), MAX_BODY).await?;
        anyhow::Ok((status, headers, body))
    };
    tokio::time::timeout(TIMEOUT, exchange)
        .await
        .with_context(|| format!("{host} did not answer in time"))?
}

/// One request to a web API (an LLM, a cloud service), on its own
/// connection: TLS checked against the public CAs when `tls`, the whole
/// exchange within `limit`, the answer capped at `max_body` bytes. The
/// `host` header is added when missing. The destination is a
/// [`trusted_connect`] (a local model behind `http://` stays reachable),
/// never a profile-declared URL.
pub async fn web(
    host: &str,
    port: u16,
    tls: bool,
    request: Request<Full<Bytes>>,
    limit: Duration,
    max_body: usize,
) -> anyhow::Result<(StatusCode, Bytes)> {
    let exchange = async {
        let (status, body) = web_stream(host, port, tls, request, limit).await?;
        let body = http_body_util::Limited::new(body, max_body)
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("{host}: {e}"))?
            .to_bytes();
        anyhow::Ok((status, body))
    };
    tokio::time::timeout(limit, exchange)
        .await
        .with_context(|| format!("{host} did not answer in time"))?
}

/// Like [`web`], with the answer's headers too (a service that sets cookies).
pub async fn web_with_headers(
    host: &str,
    port: u16,
    tls: bool,
    request: Request<Full<Bytes>>,
    limit: Duration,
    max_body: usize,
) -> anyhow::Result<(StatusCode, http::HeaderMap, Bytes)> {
    let exchange = async {
        let (head, body) = web_response(host, port, tls, request, limit)
            .await?
            .into_parts();
        let body = http_body_util::Limited::new(body, max_body)
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("{host}: {e}"))?
            .to_bytes();
        anyhow::Ok((head.status, head.headers, body))
    };
    tokio::time::timeout(limit, exchange)
        .await
        .with_context(|| format!("{host} did not answer in time"))?
}

/// Like [`web`], but the answer's body is handed over as it arrives (audio
/// played while it is made): `limit` bounds the wait for the answer's head
/// only, the caller bounds the body (time and size).
pub async fn web_stream(
    host: &str,
    port: u16,
    tls: bool,
    request: Request<Full<Bytes>>,
    limit: Duration,
) -> anyhow::Result<(StatusCode, Incoming)> {
    let response = web_response(host, port, tls, request, limit).await?;
    Ok((response.status(), response.into_body()))
}

/// The answer's head (and its body, still to read), within `limit`.
async fn web_response(
    host: &str,
    port: u16,
    tls: bool,
    mut request: Request<Full<Bytes>>,
    limit: Duration,
) -> anyhow::Result<http::Response<Incoming>> {
    if !request.headers().contains_key(http::header::HOST) {
        let value = if (tls && port == 443) || (!tls && port == 80) {
            host.to_owned()
        } else {
            format!("{host}:{port}")
        };
        request
            .headers_mut()
            .insert(http::header::HOST, value.parse().context("host header")?);
    }
    let exchange = async {
        let tcp = trusted_connect(host, port)
            .await
            .with_context(|| format!("cannot reach {host}:{port}"))?;
        let response = if tls {
            let name = ServerName::try_from(host.to_owned()).context("host name")?;
            let stream = TlsConnector::from(public_tls())
                .connect(name, tcp)
                .await
                .with_context(|| format!("{host}: TLS"))?;
            let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await?;
            tokio::spawn(async move {
                let _ = connection.await;
            });
            sender.send_request(request).await?
        } else {
            let (mut sender, connection) = http1::handshake(TokioIo::new(tcp)).await?;
            tokio::spawn(async move {
                let _ = connection.await;
            });
            sender.send_request(request).await?
        };
        anyhow::Ok(response)
    };
    tokio::time::timeout(limit, exchange)
        .await
        .with_context(|| format!("{host} did not answer in time"))?
}

/// An `http(s)://host[:port]/path?query` address, for [`web`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebUrl {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    /// Path and query, starting with `/`.
    pub path: String,
}

impl WebUrl {
    pub fn parse(url: &str) -> anyhow::Result<Self> {
        let url = url.trim();
        let (tls, rest) = if let Some(rest) = url.strip_prefix("https://") {
            (true, rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            (false, rest)
        } else {
            bail!("the address must start with https:// or http://");
        };
        let cut = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, path) = rest.split_at(cut);
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, port.parse().context("invalid port")?),
            None => (authority, if tls { 443 } else { 80 }),
        };
        if host.is_empty() || host.contains('@') {
            bail!("the address has no usable host");
        }
        let path = if path.is_empty() {
            "/".to_owned()
        } else if path.starts_with('?') {
            format!("/{path}")
        } else {
            path.to_owned()
        };
        Ok(Self {
            tls,
            host: host.to_owned(),
            port,
            path,
        })
    }
}

#[cfg(test)]
mod pin_tests {
    use super::*;

    /// The verifier only hashes the certificate's bytes: any bytes will do.
    fn verify(tls: &PinnedTls, cert: Vec<u8>) -> Result<ServerCertVerified, rustls::Error> {
        tls.pin.verify_server_cert(
            &CertificateDer::from(cert),
            &[],
            &ServerName::try_from("bridge").unwrap(),
            &[],
            UnixTime::now(),
        )
    }

    #[test]
    fn first_certificate_is_pinned_then_enforced() {
        let tls = PinnedTls::new(None).unwrap();
        assert_eq!(tls.pinned_fingerprint(), None);
        assert!(verify(&tls, vec![1, 2, 3]).is_ok());
        let pinned = tls.pinned_fingerprint().unwrap();
        assert_eq!(tls.seen_fingerprint().as_ref(), Some(&pinned));

        let refused = verify(&tls, vec![4, 5, 6]).unwrap_err();
        assert!(refused.to_string().contains(PIN_MISMATCH), "{refused}");
        assert_eq!(tls.pinned_fingerprint().as_ref(), Some(&pinned));
        assert!(verify(&tls, vec![1, 2, 3]).is_ok());
    }

    #[test]
    fn recorded_fingerprint_is_normalized() {
        let hex = fingerprint(&CertificateDer::from(vec![1, 2, 3]));
        let typed = hex
            .to_uppercase()
            .as_bytes()
            .chunks(2)
            .map(|pair| String::from_utf8_lossy(pair).into_owned())
            .collect::<Vec<_>>()
            .join(":");
        let tls = PinnedTls::new(Some(typed)).unwrap();
        assert_eq!(tls.pinned_fingerprint().as_ref(), Some(&hex));
        assert!(verify(&tls, vec![1, 2, 3]).is_ok());
        assert!(verify(&tls, vec![4, 5, 6]).is_err());
    }
}

#[cfg(test)]
mod body_tests {
    use super::*;

    #[tokio::test]
    async fn bodies_are_capped() {
        let body = Full::new(Bytes::from(vec![0u8; 10]));
        assert_eq!(read_body(body.clone(), 10).await.unwrap().len(), 10);
        let refused = read_body(body, 9).await.unwrap_err();
        assert!(refused.is::<BodyTooLarge>());
        assert_eq!(refused.to_string(), "body too large (over 9 bytes)");
        assert_eq!(
            BodyTooLarge(MAX_BODY).to_string(),
            "body too large (over 4 MiB)"
        );
    }
}

#[cfg(test)]
mod connect_tests {
    use super::*;
    use std::net::IpAddr;
    use std::str::FromStr;

    fn ip(addr: &str) -> IpAddr {
        addr.parse().expect("valid address")
    }

    #[test]
    fn forbidden_addresses_are_loopback_and_link_local() {
        for addr in [
            "127.0.0.1",
            "127.5.5.5",
            "0.0.0.0",
            "169.254.169.254",
            "169.254.0.1",
            "255.255.255.255",
            "::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(forbidden(&ip(addr)), "{addr} must be forbidden");
        }
        for addr in [
            "192.168.1.10",
            "10.0.0.1",
            "172.16.0.5",
            "8.8.8.8",
            "192.0.2.1",
        ] {
            assert!(!forbidden(&ip(addr)), "{addr} must be allowed");
        }
    }

    #[test]
    fn always_forbidden_keeps_the_dangerous_ranges_without_loopback() {
        for addr in [
            "0.0.0.0",
            "169.254.169.254",
            "169.254.0.1",
            "255.255.255.255",
            "::",
            "fe80::1",
        ] {
            assert!(
                always_forbidden(&ip(addr)),
                "{addr} must always be forbidden"
            );
        }
        for addr in ["127.0.0.1", "::1", "::ffff:127.0.0.1", "192.168.1.10"] {
            assert!(
                !always_forbidden(&ip(addr)),
                "{addr} must not be in the hard set"
            );
        }
    }

    #[tokio::test]
    async fn a_forbidden_literal_address_is_refused_before_any_connect() {
        for host in ["127.0.0.1", "169.254.169.254", "0.0.0.0", "::1"] {
            let err = checked_connect(host, 9).await.unwrap_err();
            assert!(err.to_string().contains("forbidden"), "{host}: {err}");
        }
    }

    #[tokio::test]
    async fn a_forbidden_name_resolution_is_refused() {
        // `localhost` resolves to loopback: the connection must be refused,
        // never dialed.
        let err = checked_connect("localhost", 9).await.unwrap_err();
        assert!(!err.to_string().contains("cannot reach"), "{err}");
    }

    #[tokio::test]
    async fn a_literal_address_passes_the_range_check() {
        let addrs = allowed_addresses("192.0.2.1", 8080, false).await.unwrap();
        assert_eq!(
            addrs,
            vec![std::net::SocketAddr::from_str("192.0.2.1:8080").unwrap()]
        );
    }

    #[tokio::test]
    async fn a_trusted_literal_loopback_passes_the_range_check() {
        // Written by a person in a config file: Moli's own API, a local LLM.
        for host in ["127.0.0.1", "::1", "::ffff:127.0.0.1"] {
            let addrs = allowed_addresses(host, 8790, true).await.unwrap();
            assert_eq!(addrs.len(), 1, "{host}");
            assert!(is_loopback(&addrs[0].ip()), "{host}");
        }
        // The same literals stay refused on the strict path.
        for host in ["127.0.0.1", "::1"] {
            assert!(
                allowed_addresses(host, 8790, false).await.is_err(),
                "{host}"
            );
        }
    }

    #[tokio::test]
    async fn a_trusted_connect_refuses_the_dangerous_ranges() {
        for host in ["169.254.169.254", "0.0.0.0", "::"] {
            let err = trusted_connect(host, 9).await.unwrap_err();
            assert!(err.to_string().contains("forbidden"), "{host}: {err}");
        }
    }

    #[tokio::test]
    async fn a_trusted_name_rebound_to_loopback_is_refused() {
        // Even with the relaxed literal check, a name must never resolve to
        // loopback: the anti-rebinding guarantee is total.
        let err = trusted_connect("localhost", 9).await.unwrap_err();
        assert!(!err.to_string().contains("cannot reach"), "{err}");
    }

    #[tokio::test]
    async fn a_trusted_literal_loopback_reaches_the_io_layer() {
        // Port 9: the range check passes, the dial is attempted and fails
        // with an io error (connection refused) kept in the error chain.
        let err = trusted_connect("127.0.0.1", 9).await.unwrap_err();
        assert!(err.to_string().contains("cannot reach"), "{err}");
        assert!(
            err.chain()
                .any(<dyn std::error::Error>::is::<std::io::Error>),
            "{err}"
        );
    }
}

#[cfg(test)]
mod web_url_tests {
    use super::WebUrl;

    #[test]
    fn addresses_parse() {
        let u = WebUrl::parse("https://n8n.example.fr/webhook/abc?x=1").unwrap();
        assert_eq!(
            (u.tls, u.host.as_str(), u.port, u.path.as_str()),
            (true, "n8n.example.fr", 443, "/webhook/abc?x=1")
        );
        let l = WebUrl::parse("http://192.168.0.62:5678").unwrap();
        assert_eq!((l.tls, l.port, l.path.as_str()), (false, 5678, "/"));
        assert!(WebUrl::parse("ftp://x").is_err());
        assert!(WebUrl::parse("https://user@host/").is_err());
    }
}
