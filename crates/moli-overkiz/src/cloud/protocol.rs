use super::Error;
use crate::inventory::{CollectionKind, MAX_COLLECTION_BYTES, Region};
use moli_runtime::BoxFuture;
use std::time::Duration;

pub(crate) const REQUEST_LIMIT: Duration = Duration::from_secs(10);
pub(crate) const CYCLE_LIMIT: Duration = Duration::from_secs(90);
pub(crate) const TOKEN_LIMIT: usize = 65_536;
pub(crate) const TOKEN_STRING_LIMIT: usize = 16_384;

// Public application constants, not user credentials. Single pinned source:
// pyoverkiz/const.py at 5f31858570e14be4f58e6cc58b7a448000556449.
pub(crate) const CLIENT_ID: &str =
    "0d8e920c-1478-11e7-a377-02dd59bd3041_1ewvaqmclfogo4kcsoo0c8k4kso884owg08sg8c40sk4go4ksg";
pub(crate) const CLIENT_SECRET: &str = "12k73w1n540g8o4cokg0cw84cog840k84cwggscwg884004kgk";
pub(crate) const BOB_KEY: &str = "184638B3FBE874ACD24C14FBD657B";

/// No variant accepts a host, a verb, or a free-form provider path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Endpoint {
    Accounts,
    Exchange,
    Refresh,
    Directory,
    Collection(CollectionKind),
    Location,
    Devices,
    Gateways,
    Scenario,
}
impl Endpoint {
    pub(crate) fn route(self, region: Region) -> (&'static str, &'static str, &'static str) {
        let host = match region {
            Region::Emea => "ha101-1.overkiz.com",
            Region::Apac => "ha201-1.overkiz.com",
            Region::Snaba => "ha401-1.overkiz.com",
        };
        let path = match self {
            Self::Accounts => return ("POST", "accounts.somfy.com", "/oauth/oauth/v2/token/jwt"),
            Self::Exchange | Self::Refresh => {
                return (
                    "POST",
                    "ginaite-prod.ovkube.net",
                    "/realms/somfy-tahoma/protocol/openid-connect/token",
                );
            }
            Self::Directory => {
                return (
                    "GET",
                    "backoffice-service.ovkube.net",
                    "/site-api/public/v1/sites",
                );
            }
            Self::Collection(CollectionKind::Scenarios) => {
                "/enduser-mobile-web/enduserAPI/actionGroups"
            }
            Self::Collection(CollectionKind::CalendarDays) => {
                "/enduser-mobile-web/enduserAPI/setup/calendar/days"
            }
            Self::Collection(CollectionKind::CalendarRules) => {
                "/enduser-mobile-web/enduserAPI/setup/calendar/rules"
            }
            Self::Collection(CollectionKind::Triggers) => "/enduser-mobile-web/enduserAPI/triggers",
            Self::Collection(CollectionKind::Schedule) => {
                "/enduser-mobile-web/enduserAPI/exec/schedule"
            }
            Self::Collection(CollectionKind::DelayedSchedule) => {
                "/enduser-mobile-web/enduserAPI/exec/delayedTriggerSchedule"
            }
            Self::Scenario => return ("POST", host, "/enduser-mobile-web/enduserAPI/exec"),
            Self::Location => "/enduser-mobile-web/enduserAPI/setup/location",
            Self::Devices => "/enduser-mobile-web/enduserAPI/setup/devices",
            Self::Gateways => "/enduser-mobile-web/enduserAPI/setup/gateways",
        };
        ("GET", host, path)
    }
    pub(crate) const fn body_limit(self) -> usize {
        match self {
            Self::Accounts | Self::Exchange | Self::Refresh => TOKEN_LIMIT,
            Self::Scenario => 4096,
            _ => MAX_COLLECTION_BYTES,
        }
    }
}

pub(crate) fn component(value: &str) -> String {
    const HEX: &[u8] = b"0123456789ABCDEF";
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(b));
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(char::from(HEX[usize::from(b >> 4)]));
                out.push(char::from(HEX[usize::from(b & 15)]));
            }
        }
    }
    out
}
pub(crate) fn form(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", component(k), component(v)))
        .collect::<Vec<_>>()
        .join("&")
}

pub(crate) struct Request {
    pub endpoint: Endpoint,
    pub region: Region,
    pub query: Query,
    pub bearer: Option<String>,
    pub body: String,
}
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Request").field(&self.endpoint).finish()
    }
}
#[derive(Default)]
pub(crate) enum Query {
    #[default]
    None,
    Offset(usize),
    Site(String),
    Scenario(String),
}
impl Request {
    pub(super) fn path(&self) -> Result<String, Error> {
        let (_, _, path) = self.endpoint.route(self.region);
        Ok(match (&self.endpoint, &self.query) {
            (_, Query::None)
                if !matches!(self.endpoint, Endpoint::Directory | Endpoint::Scenario) =>
            {
                path.to_owned()
            }
            (Endpoint::Scenario, Query::Scenario(oid))
                if super::execution::valid_oid(oid) && self.body.is_empty() =>
            {
                format!("{path}/{oid}")
            }
            (Endpoint::Directory, Query::Offset(offset))
                if *offset < 200 && offset.is_multiple_of(20) =>
            {
                format!("{path}?withGateways=true&limit=20&offset={offset}")
            }
            (Endpoint::Refresh, Query::Site(site)) if super::bounded_text(site, 256) => {
                format!("{path}?siteOID={}", component(site))
            }
            _ => return Err(Error::InvalidInput),
        })
    }
}
pub(crate) struct Response {
    pub status: u16,
    pub retry_after: Option<String>,
    pub body: Vec<u8>,
}
impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}
pub(crate) trait Transport: Send {
    fn send(&mut self, request: Request) -> BoxFuture<'_, Result<Response, Error>>;
}
#[derive(Debug)]
pub(crate) struct Https;
impl Transport for Https {
    fn send(&mut self, request: Request) -> BoxFuture<'_, Result<Response, Error>> {
        Box::pin(async move {
            let (method, host, _) = request.endpoint.route(request.region);
            let mut builder = http::Request::builder().method(method).uri(request.path()?);
            if let Some(bearer) = request.bearer {
                if !valid_token(&bearer) {
                    return Err(Error::InvalidResponse);
                }
                builder = builder.header("authorization", format!("Bearer {bearer}"));
            }
            if request.endpoint == Endpoint::Directory {
                builder = builder.header("x-api-key", BOB_KEY);
            }
            if matches!(
                request.endpoint,
                Endpoint::Accounts | Endpoint::Exchange | Endpoint::Refresh
            ) {
                builder = builder.header("content-type", "application/x-www-form-urlencoded");
            }
            let mut wire = moli_net::empty(builder).map_err(|_| Error::InvalidInput)?;
            *wire.body_mut() = moli_net::Body::from(request.body).into();
            // web_with_headers performs exactly one verified TLS exchange. It does not follow redirects.
            let (status, headers, body) = moli_net::web_with_headers(
                host,
                443,
                true,
                wire,
                REQUEST_LIMIT,
                request.endpoint.body_limit(),
            )
            .await
            .map_err(|_| Error::Network)?;
            Ok(Response {
                status: status.as_u16(),
                retry_after: headers
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .filter(|v| v.len() <= 128)
                    .map(str::to_owned),
                body: body.to_vec(),
            })
        })
    }
}
pub(crate) fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= TOKEN_STRING_LIMIT
        && token.bytes().all(|b| b.is_ascii_graphic())
        && http::HeaderValue::from_str(token).is_ok()
}
