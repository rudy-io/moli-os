//! Ecovacs Italy / EU cloud protocol. Never log URLs, query strings or bodies.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http::{Method, Request};
use http_body_util::Full;
use md5::{Digest as _, Md5};
use rand_core::{OsRng, RngCore as _};
use rsa::{
    Pkcs1v15Encrypt, RsaPublicKey, pkcs1::DecodeRsaPublicKey as _, pkcs8::DecodePublicKey as _,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroize::Zeroizing;

const LOGIN: &str = "gl-it-api.ecovacs.com";
const OPEN: &str = "gl-it-openapi.ecovacs.com";
const PORTAL: &str = "portal-eu.ecouser.net";
const VERSION: &str = "3.17.2";
const REALM: &str = "ecouser.net";
const TIMEOUT: Duration = Duration::from_secs(8);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(4);
const MAX_RESPONSE: usize = 1024 * 1024;
// Identifiers and signing constants of the published Android app protocol.
// They are shared by all installations and cannot authenticate an account.
const LOGIN_SIGN: (&str, &str) = ("1520391301804", "6c319b2a5cd3e66e39159c2e28f2fce9");
const OPEN_SIGN: (&str, &str) = ("1520391491841", "77ef58ce3afbe337da74aa8c5ab963a9");

#[derive(Serialize, Deserialize)]
pub(crate) struct Account {
    pub(crate) resource: String,
    uid: String,
    access_token: String,
}

pub(crate) struct Session {
    uid: String,
    token: String,
    pub(crate) renew_at: Instant,
}

#[derive(Deserialize)]
pub(crate) struct Robot {
    pub(crate) did: String,
    resource: String,
    #[serde(rename = "class")]
    pub(crate) class: String,
    #[serde(default)]
    company: String,
}

pub(crate) enum Login {
    Complete(Account),
    Verify(Verification),
}

pub(crate) struct Verification {
    resource: String,
    ticket: String,
    encrypted_email: String,
    id: String,
}

fn milliseconds() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

fn md5(value: &str) -> String {
    format!(
        "{:032x}",
        u128::from_be_bytes(Md5::digest(value.as_bytes()).into())
    )
}

fn query(fields: impl IntoIterator<Item = (impl AsRef<str>, impl AsRef<str>)>) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields)
        .finish()
}

fn signed_query(
    mut fields: BTreeMap<&str, String>,
    metadata: &[(&str, &str)],
    (key, app_sign): (&str, &str),
) -> String {
    let mut signature = Zeroizing::new(key.to_owned());
    let mut all = fields.clone();
    all.extend(metadata.iter().map(|&(k, v)| (k, v.to_owned())));
    for (name, value) in all {
        signature.push_str(name);
        signature.push('=');
        signature.push_str(&value);
    }
    fields.insert("authAppkey", key.to_owned());
    signature.push_str(app_sign);
    fields.insert("authSign", md5(&signature));
    query(fields)
}

async fn request(
    host: &str,
    path: &str,
    body: Option<&Value>,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let request = Request::builder()
        .uri(path)
        .method(if body.is_some() {
            Method::POST
        } else {
            Method::GET
        })
        .header("accept", "application/json")
        .header("content-type", "application/json")
        .header(
            "user-agent",
            "Dalvik/2.1.0 (Linux; U; Android 5.1.1; A5010 Build/LMY48Z)",
        )
        .body(Full::new(body.map_or_else(Bytes::new, |body| {
            Bytes::from(body.to_string())
        })))
        .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("ecovacs.response")))?;
    let (status, bytes) = moli_net::web(host, 443, true, request, timeout, MAX_RESPONSE)
        .await
        .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("ecovacs.network")))?;
    ensure!(
        status.is_success(),
        "{} ({status})",
        moli_i18n::tr!("ecovacs.network")
    );
    serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!(moli_i18n::tr!("ecovacs.response")))
}

fn auth_data(response: &Value) -> anyhow::Result<&Value> {
    let code = response["code"]
        .as_str()
        .filter(|code| code.len() == 4 && code.bytes().all(|b| b.is_ascii_digit()));
    ensure!(
        code == Some("0000"),
        "{} ({})",
        moli_i18n::tr!("ecovacs.auth_rejected"),
        code.unwrap_or("?")
    );
    Ok(&response["data"])
}

async fn private(
    version: u8,
    resource: &str,
    endpoint: &str,
    mut fields: BTreeMap<&str, String>,
) -> anyhow::Result<Value> {
    let now = milliseconds();
    fields.extend([
        ("requestId", md5(&now)),
        ("authTimespan", now),
        ("authTimeZone", "GMT-8".into()),
    ]);
    let query = signed_query(
        fields,
        &[
            ("country", "it"),
            ("lang", "EN"),
            ("deviceId", resource),
            ("appCode", "global_e"),
            ("appVersion", VERSION),
            ("channel", "google_play"),
            ("deviceType", "1"),
        ],
        LOGIN_SIGN,
    );
    let path = format!(
        "/v{version}/private/it/EN/{resource}/global_e/{VERSION}/google_play/1/{endpoint}?{query}"
    );
    let response = request(LOGIN, &path, None, TIMEOUT).await?;
    Ok(auth_data(&response)?.clone())
}

fn field<'a>(value: &'a Value, name: &str) -> anyhow::Result<&'a str> {
    value[name]
        .as_str()
        .filter(|text| !text.is_empty() && text.len() <= 4096)
        .with_context(|| moli_i18n::tr!("ecovacs.response"))
}

impl Account {
    fn from_response(resource: String, response: &Value) -> anyhow::Result<Self> {
        Ok(Self {
            resource,
            uid: field(response, "uid")?.into(),
            access_token: field(response, "accessToken")?.into(),
        })
    }

    pub(crate) async fn session(&self) -> anyhow::Result<Session> {
        let fields = BTreeMap::from([
            ("uid", self.uid.clone()),
            ("accessToken", self.access_token.clone()),
            ("bizType", "ECOVACS_IOT".into()),
            ("deviceId", self.resource.clone()),
            ("authTimespan", milliseconds()),
        ]);
        let query = signed_query(fields, &[("openId", "global")], OPEN_SIGN);
        let response = request(
            OPEN,
            &format!("/v1/global/auth/getAuthCode?{query}"),
            None,
            TIMEOUT,
        )
        .await?;
        let auth_code = field(auth_data(&response)?, "authCode")?;
        let response = request(PORTAL, "/api/users/user.do", Some(&json!({
            "edition": "ECOGLOBLE", "userId": self.uid, "token": auth_code, "realm": REALM,
            "resource": self.resource, "org": "ECOWW", "last": "", "country": "IT", "todo": "loginByItToken"
        })), TIMEOUT).await?;
        ensure!(
            response["result"] == "ok",
            "{}",
            moli_i18n::tr!("ecovacs.auth_rejected")
        );
        let lifetime_ms = response["last"]
            .as_u64()
            .or_else(|| response["last"].as_str()?.parse().ok())
            .unwrap_or(3_600_000)
            .clamp(60_000, 604_800_000);
        Ok(Session {
            uid: field(&response, "userId")?.into(),
            token: field(&response, "token")?.into(),
            renew_at: Instant::now() + Duration::from_millis(lifetime_ms * 9 / 10),
        })
    }
}

pub(crate) async fn login(email: &str, password: &str) -> anyhow::Result<Login> {
    let mut random = [0u8; 16];
    OsRng
        .try_fill_bytes(&mut random)
        .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("ecovacs.encryption")))?;
    let resource = format!("{:032x}", u128::from_be_bytes(random));
    let data = private(
        1,
        &resource,
        "common/getConfig",
        BTreeMap::from([("keys", "PUBLIC.KEY.CONFIG".into())]),
    )
    .await?;
    let key = data
        .as_array()
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["key"] == "PUBLIC.KEY.CONFIG")
        })
        .and_then(|entry| entry["value"].as_str())
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .and_then(|config| STANDARD.decode(config["publicKey"].as_str()?).ok())
        .and_then(|der| {
            RsaPublicKey::from_public_key_der(&der)
                .or_else(|_| RsaPublicKey::from_pkcs1_der(&der))
                .ok()
        })
        .with_context(|| moli_i18n::tr!("ecovacs.encryption"))?;
    let encrypt = |value: &str| {
        key.encrypt(&mut OsRng, Pkcs1v15Encrypt, value.as_bytes())
            .map(|bytes| STANDARD.encode(bytes))
            .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("ecovacs.encryption")))
    };
    let encrypted_email = encrypt(email)?;
    let response = private(
        2,
        &resource,
        "user/login2",
        BTreeMap::from([
            ("encryptAccount", encrypted_email.clone()),
            ("encryptPwd", encrypt(password)?),
        ]),
    )
    .await?;
    if response["loginResult"] != "HIGH_RISK" {
        return Ok(Login::Complete(Account::from_response(
            resource, &response,
        )?));
    }
    let ticket = field(&response, "riskTicket")?.to_owned();
    let response = private(
        1,
        &resource,
        "user/sendEmailVerifyCode",
        BTreeMap::from([
            ("encryptEmail", encrypted_email.clone()),
            ("verifyType", "EMAIL_LOGIN_RISK_CHECK".into()),
            ("supportChar", "N".into()),
            ("isForce", "N".into()),
        ]),
    )
    .await?;
    let id = field(&response, "verifyId")?.into();
    Ok(Login::Verify(Verification {
        resource,
        ticket,
        encrypted_email,
        id,
    }))
}

impl Verification {
    pub(crate) async fn confirm(self, code: &str) -> anyhow::Result<Account> {
        let response = private(
            2,
            &self.resource,
            "user/loginByRiskVerify",
            BTreeMap::from([
                ("riskTicket", self.ticket),
                ("encryptEmail", self.encrypted_email),
                ("verifyId", self.id),
                ("verifyCode", code.into()),
            ]),
        )
        .await?;
        Account::from_response(self.resource, &response)
    }
}

impl Session {
    async fn post(
        &self,
        resource: &str,
        path: &str,
        mut payload: Value,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        payload["auth"] = json!({"with":"users", "userid":self.uid, "realm":REALM, "token":self.token, "resource":resource});
        request(PORTAL, path, Some(&payload), timeout).await
    }

    pub(crate) async fn robots(&self, resource: &str) -> anyhow::Result<Vec<Robot>> {
        let get = |path, todo| {
            self.post(
                resource,
                path,
                json!({"userid":self.uid, "todo":todo}),
                TIMEOUT,
            )
        };
        let (regular, global) = tokio::try_join!(
            get("/api/users/user.do", "GetDeviceList"),
            get("/api/appsvr/app.do", "GetGlobalDeviceList"),
        )?;
        let mut robots = BTreeMap::new();
        let mut answered = false;
        for response in [regular, global] {
            if let Some(devices) = response["devices"].as_array() {
                answered = true;
                for device in devices.iter().take(128) {
                    if let Ok(robot) = serde_json::from_value::<Robot>(device.clone())
                        && robot.company == "eco-ng"
                        && !robot.did.is_empty()
                        && robot.did.len() <= 128
                        && robot
                            .did
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    {
                        robots.insert(robot.did.clone(), robot);
                    }
                }
            }
        }
        ensure!(answered, "{}", moli_i18n::tr!("ecovacs.response"));
        Ok(robots.into_values().collect())
    }

    pub(crate) async fn command(
        &self,
        resource: &str,
        robot: &Robot,
        name: &str,
        args: Option<Value>,
        timeout: Duration,
    ) -> anyhow::Result<CommandResult> {
        let query = query([
            ("mid", robot.class.as_str()),
            ("did", &robot.did),
            ("td", "q"),
            ("u", &self.uid),
            ("cv", "1.67.3"),
            ("t", "a"),
            ("av", "1.3.1"),
        ]);
        let mut payload = json!({"header":{"pri":"1","ts": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64(),"tzm":480,"ver":"0.0.50"}});
        if let Some(args) = args {
            payload["body"] = json!({"data":args});
        }
        let response = self
            .post(
                resource,
                &format!("/api/iot/devmanager.do?{query}"),
                json!({
                    "cmdName":name, "payload":payload, "payloadType":"j", "td":"q",
                    "toId":robot.did, "toRes":robot.resource, "toType":robot.class,
                }),
                timeout.min(COMMAND_TIMEOUT),
            )
            .await?;
        if response["ret"] != "ok" {
            return Ok(
                if response["errno"]
                    .as_i64()
                    .or_else(|| response["errno"].as_str()?.parse().ok())
                    == Some(500)
                {
                    CommandResult::Unknown
                } else {
                    CommandResult::Refused
                },
            );
        }
        let body = match response.get("resp") {
            Some(Value::String(body)) => serde_json::from_str::<Value>(body)
                .map_err(|_| anyhow::anyhow!(moli_i18n::tr!("ecovacs.response")))?,
            Some(body) => body.clone(),
            None => response,
        };
        Ok(CommandResult::Body(body["body"].clone()))
    }
}

pub(crate) enum CommandResult {
    Body(Value),
    Refused,
    Unknown,
}
