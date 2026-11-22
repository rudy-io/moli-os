//! The Proxmox VE REST API: one pinned HTTPS connection, an API token.
//!
//! Every call is a GET (the driver reads, it never changes anything) with
//! `Authorization: PVEAPIToken=<user>@<realm>!<token>=<secret>`: no session,
//! no password, nothing in an address.

use anyhow::{Context as _, bail};
use moli_net::{Method, PinnedHttps};
use serde_json::Value as Json;
use std::fmt;

/// Proxmox refused the token (wrong, expired, deleted, or without the right
/// to that path). Asking again changes nothing until someone fixes it.
#[derive(Debug)]
pub(crate) struct Refused(pub u16);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Proxmox refused the API token (HTTP {})", self.0)
    }
}
impl std::error::Error for Refused {}

pub(crate) struct Client {
    http: PinnedHttps,
    /// `PVEAPIToken=…`: kept here only, never logged.
    authorization: String,
}

impl Client {
    pub(crate) fn new(http: PinnedHttps, token_id: &str, secret: &str) -> Self {
        Self {
            http,
            authorization: authorization(token_id, secret),
        }
    }

    /// `data` of the answer to `GET /api2/json<path>`.
    pub(crate) async fn get(&self, path: &str) -> anyhow::Result<Json> {
        let request = moli_net::empty(
            self.http
                .request(Method::GET, &format!("/api2/json{path}"))
                .header("authorization", &self.authorization)
                .header("accept", "application/json"),
        )
        .context("request")?;
        let (status, bytes) = self.http.send(request).await?;
        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(Refused(status.as_u16()).into());
        }
        if !status.is_success() {
            bail!("Proxmox answered {status} for {path}");
        }
        let value: Json = serde_json::from_slice(&bytes).context("unreadable Proxmox answer")?;
        Ok(value["data"].clone())
    }
}

/// The token header: `PVEAPIToken=moli@pve!moli=<secret>`.
fn authorization(token_id: &str, secret: &str) -> String {
    format!("PVEAPIToken={token_id}={secret}")
}

/// A token id as Proxmox writes it: `user@realm!name`, plain characters only
/// (it goes in a header).
pub(crate) fn valid_token_id(id: &str) -> bool {
    let Some((user, name)) = id.split_once('!') else {
        return false;
    };
    let Some((login, realm)) = user.split_once('@') else {
        return false;
    };
    let plain = |s: &str, extra: &str| {
        !s.is_empty()
            && s.len() <= 64
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
    };
    plain(login, "-_.") && plain(realm, "-_.") && plain(name, "-_.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_header() {
        assert_eq!(
            authorization("moli@pve!moli", "0000-1111"),
            "PVEAPIToken=moli@pve!moli=0000-1111"
        );
    }

    #[test]
    fn token_ids_are_checked() {
        assert!(valid_token_id("moli@pve!moli"));
        assert!(valid_token_id("monitor@pam!infra-1"));
        for bad in [
            "moli@pve",
            "moli!x",
            "@pve!x",
            "moli@pve!",
            "a b@pve!x",
            "moli@pve!x\r\n",
        ] {
            assert!(!valid_token_id(bad), "{bad:?}");
        }
    }
}
