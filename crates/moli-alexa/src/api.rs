//! The Alexa app's web API on the account's site (`alexa.amazon.fr`…): a
//! session (cookies and CSRF from the refresh token), the speakers, what they
//! play, and the orders, mostly as « sequences » (the routines' format).

use std::time::Duration;

use anyhow::{Context as _, bail};
use http::{Method, Request, StatusCode};
use http_body_util::Full;
use moli_net::Body as Bytes;
use serde_json::{Value as Json, json};

use crate::auth::{self, Jar, USER_AGENT};
use crate::echo::Echo;

const LIMIT: Duration = Duration::from_secs(20);
const MAX_BODY: usize = 4 * 1024 * 1024;
/// The routines editor's version, which the music providers' list asks for.
const ROUTINES_VERSION: &str = "3.0.264101";

/// An answer that says the session is over: refresh it, then try again.
#[derive(Debug)]
pub(crate) enum Failure {
    Expired,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Expired => f.write_str("the Alexa session expired"),
            Self::Other(e) => write!(f, "{e:#}"),
        }
    }
}

pub(crate) struct Session {
    /// `alexa.amazon.fr`.
    host: String,
    /// `fr-FR`.
    pub locale: String,
    jar: Jar,
    csrf: String,
    /// The account's customer id (the sequences' `customerId`).
    pub customer: String,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("host", &self.host)
            .field("customer", &self.customer)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Cookies from the refresh token, the CSRF cookie, the account's id.
    pub(crate) async fn open(refresh: &str, domain: &str, locale: &str) -> anyhow::Result<Self> {
        let mut jar = auth::cookies(refresh, domain).await?;
        let host = format!("alexa.{domain}");
        let csrf = auth::csrf(&mut jar, &host).await?;
        let mut session = Self {
            host,
            locale: locale.to_owned(),
            jar,
            csrf,
            customer: String::new(),
        };
        let me = session
            .get("/api/users/me?platform=ios&version=2.2.651540.0")
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        me["id"]
            .as_str()
            .context("no customer id in /api/users/me")?
            .clone_into(&mut session.customer);
        if let Some(market) = me["marketPlaceDomainName"].as_str()
            && !market.ends_with(domain)
        {
            tracing::warn!(market, domain, "the Amazon account belongs to another site");
        }
        Ok(session)
    }

    async fn send(&self, method: Method, path: &str, body: Option<&Json>) -> Result<Json, Failure> {
        let referer = format!("https://{}/spa/index.html", self.host);
        let origin = format!("https://{}", self.host);
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header("user-agent", USER_AGENT)
            .header("accept", "application/json; charset=utf-8")
            .header("accept-language", self.locale.as_str())
            .header("referer", referer)
            .header("origin", origin)
            .header("csrf", self.csrf.as_str())
            .header("cookie", self.jar.header());
        if body.is_some() {
            builder = builder.header("content-type", "application/json; charset=utf-8");
        }
        let request = builder
            .body(Full::new(Bytes::from(
                body.map(Json::to_string).unwrap_or_default().into_bytes(),
            )))
            .map_err(|e| Failure::Other(e.into()))?;
        let (status, answer) =
            moli_net::web(&self.host, 443, true, request, LIMIT, MAX_BODY).await?;
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(Failure::Expired);
        }
        if !status.is_success() {
            let text = String::from_utf8_lossy(&answer[..answer.len().min(200)]).into_owned();
            return Err(Failure::Other(anyhow::anyhow!(
                "{}{path}: HTTP {} {text}",
                self.host,
                status.as_u16()
            )));
        }
        // Many orders answer 200 with no body.
        Ok(serde_json::from_slice(&answer).unwrap_or(Json::Null))
    }

    pub(crate) async fn get(&self, path: &str) -> Result<Json, Failure> {
        self.send(Method::GET, path, None).await
    }

    pub(crate) async fn post(&self, path: &str, body: &Json) -> Result<Json, Failure> {
        self.send(Method::POST, path, Some(body)).await
    }

    /// Every device of the account (`devices-v2`).
    pub(crate) async fn devices(&self) -> Result<Json, Failure> {
        self.get("/api/devices-v2/device?cached=false").await
    }

    /// The account's music service by default (`AMAZON_MUSIC`, `SPOTIFY`…).
    pub(crate) async fn default_music(&self) -> Option<String> {
        let path = "/api/behaviors/entities?skillId=amzn1.ask.1p.music";
        let referer = format!("https://{}/spa/index.html", self.host);
        let request = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header("user-agent", USER_AGENT)
            .header("accept", "application/json; charset=utf-8")
            .header("routines-version", ROUTINES_VERSION)
            .header("referer", referer)
            .header("csrf", self.csrf.as_str())
            .header("cookie", self.jar.header())
            .body(Full::new(Bytes::new()))
            .ok()?;
        let (_, answer) = moli_net::web(&self.host, 443, true, request, LIMIT, MAX_BODY)
            .await
            .ok()?;
        default_provider(&serde_json::from_slice(&answer).ok()?)
    }

    /// Runs `start` (one operation, or several side by side).
    pub(crate) async fn run(&self, start: &Json) -> Result<(), Failure> {
        self.post("/api/behaviors/preview", &preview(start))
            .await
            .map(|_| ())
    }

    /// Play, pause, next… (`PlayCommand`, `PauseCommand`, `NextCommand`).
    pub(crate) async fn player(&self, echo: &Echo, command: &str) -> Result<(), Failure> {
        let path = format!(
            "/api/np/command?deviceSerialNumber={}&deviceType={}",
            echo.serial, echo.device_type
        );
        self.post(
            &path,
            &json!({ "type": command, "contentFocusClientId": null }),
        )
        .await
        .map(|_| ())
    }

    /// What `echo` plays now. A speaker with no player at all answers
    /// 400: it plays nothing.
    pub(crate) async fn playing(&self, echo: &Echo) -> Result<Playing, Failure> {
        let path = format!(
            "/api/np/player?deviceSerialNumber={}&deviceType={}&screenWidth=1392",
            echo.serial, echo.device_type
        );
        match self.get(&path).await {
            Ok(answer) => Ok(player_state(&answer)),
            Err(Failure::Other(e)) if format!("{e:#}").contains(": HTTP 400") => {
                Ok(Playing::idle())
            }
            Err(e) => Err(e),
        }
    }

    /// Every speaker's volume: (serial, 0-100).
    pub(crate) async fn volumes(&self) -> Result<Vec<(String, f64)>, Failure> {
        let answer = self
            .get("/api/devices/deviceType/dsn/audio/v1/allDeviceVolumes")
            .await?;
        Ok(volumes(&answer))
    }
}

/// What a speaker plays, as the dashboard shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Playing {
    /// `PLAYING`, `PAUSED`, `IDLE`…
    pub state: String,
    pub title: String,
    pub artist: String,
    pub volume: Option<f64>,
}

impl Playing {
    fn idle() -> Self {
        Self {
            state: "IDLE".into(),
            ..Self::default()
        }
    }
}

pub(crate) fn player_state(answer: &Json) -> Playing {
    let p = &answer["playerInfo"];
    Playing {
        state: p["state"].as_str().unwrap_or("IDLE").to_owned(),
        title: p["infoText"]["title"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        artist: p["infoText"]["subText1"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        volume: p["volume"]["volume"].as_f64(),
    }
}

pub(crate) fn volumes(answer: &Json) -> Vec<(String, f64)> {
    answer["volumes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| Some((v["dsn"].as_str()?.to_owned(), v["speakerVolume"].as_f64()?)))
        .collect()
}

fn default_provider(answer: &Json) -> Option<String> {
    let list = answer.as_array()?;
    let plays = |p: &&Json| {
        p["availability"].as_str().is_none_or(|a| a == "AVAILABLE")
            && ["supportedOperations", "supportedProperties"]
                .iter()
                .any(|k| {
                    p[*k]
                        .as_array()
                        .is_some_and(|ops| ops.iter().any(|o| o == "Alexa.Music.PlaySearchPhrase"))
                })
    };
    list.iter()
        .filter(plays)
        .find(|p| p["providerData"]["isDefaultMusicProvider"].as_bool() == Some(true))
        .or_else(|| list.iter().find(plays))
        .and_then(|p| p["id"].as_str())
        .map(str::to_owned)
}

/// The routines' envelope: `sequenceJson` is JSON in a string.
pub(crate) fn preview(start: &Json) -> Json {
    let sequence = json!({
        "@type": "com.amazon.alexa.behaviors.model.Sequence",
        "startNode": start,
    });
    json!({
        "behaviorId": "PREVIEW",
        "sequenceJson": sequence.to_string(),
        "status": "ENABLED",
    })
}

/// One operation on `echo`; `extra` adds its own fields to the payload.
pub(crate) fn operation(
    kind: &str,
    echo: &Echo,
    customer: &str,
    locale: &str,
    extra: &Json,
) -> Json {
    let mut payload = json!({
        "deviceType": echo.device_type,
        "deviceSerialNumber": echo.serial,
        "locale": locale,
        "customerId": customer,
    });
    if let (Some(payload), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        payload.extend(extra.clone());
    }
    json!({
        "@type": "com.amazon.alexa.behaviors.model.OpaquePayloadOperationNode",
        "type": kind,
        "operationPayload": payload,
    })
}

/// Several operations side by side.
pub(crate) fn together(nodes: Vec<Json>) -> Json {
    if nodes.len() == 1 {
        return nodes.into_iter().next().unwrap_or(Json::Null);
    }
    json!({
        "@type": "com.amazon.alexa.behaviors.model.ParallelNode",
        "nodesToExecute": nodes,
    })
}

pub(crate) fn music(
    echo: &Echo,
    customer: &str,
    locale: &str,
    phrase: &str,
    provider: &str,
) -> Json {
    operation(
        "Alexa.Music.PlaySearchPhrase",
        echo,
        customer,
        locale,
        &json!({
            "searchPhrase": phrase,
            "sanitizedSearchPhrase": phrase,
            "musicProviderId": provider,
        }),
    )
}

pub(crate) fn text_command(echo: &Echo, customer: &str, locale: &str, text: &str) -> Json {
    operation(
        "Alexa.TextCommand",
        echo,
        customer,
        locale,
        &json!({ "text": text.to_lowercase(), "skillId": "amzn1.ask.1p.tellalexa" }),
    )
}

pub(crate) fn speak(echo: &Echo, customer: &str, locale: &str, text: &str) -> Json {
    operation(
        "Alexa.Speak",
        echo,
        customer,
        locale,
        &json!({
            "textToSpeak": text,
            "target": {
                "customerId": customer,
                "devices": [{ "deviceSerialNumber": echo.serial, "deviceTypeId": echo.device_type }],
            },
            "skillId": "amzn1.ask.1p.saysomething",
        }),
    )
}

/// An announcement (its jingle, then the words) on `targets`.
pub(crate) fn announce(targets: &[&Echo], customer: &str, locale: &str, text: &str) -> Json {
    let devices: Vec<Json> = targets
        .iter()
        .map(|e| json!({ "deviceSerialNumber": e.serial, "deviceTypeId": e.device_type }))
        .collect();
    json!({
        "@type": "com.amazon.alexa.behaviors.model.OpaquePayloadOperationNode",
        "type": "AlexaAnnouncement",
        "operationPayload": {
            "customerId": customer,
            "expireAfter": "PT5S",
            "content": [{
                "locale": locale,
                "display": { "title": "Moli", "body": text },
                "speak": { "type": "text", "value": text },
            }],
            "target": { "customerId": customer, "devices": devices },
            "skillId": "amzn1.ask.1p.routines.messaging",
        },
    })
}

pub(crate) fn volume(echo: &Echo, customer: &str, locale: &str, value: i64) -> Json {
    operation(
        "Alexa.DeviceControls.Volume",
        echo,
        customer,
        locale,
        &json!({ "value": value.clamp(0, 100) }),
    )
}

/// What a phrase may hold: Alexa speaks at most 250 characters.
pub(crate) fn sayable(text: &str) -> anyhow::Result<&str> {
    let text = text.trim();
    if text.is_empty() {
        bail!("nothing to say");
    }
    if text.chars().count() > 250 {
        bail!("at most 250 characters");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo(serial: &str) -> Echo {
        Echo {
            serial: serial.into(),
            device_type: "A3S5BH2HU6VAYF".into(),
            name: "Salon".into(),
            family: "ECHO".into(),
            group: false,
            members: Vec::new(),
            online: true,
        }
    }

    #[test]
    fn a_sequence_is_json_in_a_string() {
        let e = echo("G0911");
        let body = preview(&music(&e, "A1CUST", "fr-FR", "du jazz", "AMAZON_MUSIC"));
        assert_eq!(body["behaviorId"], "PREVIEW");
        let seq: Json = serde_json::from_str(body["sequenceJson"].as_str().unwrap()).unwrap();
        assert_eq!(seq["@type"], "com.amazon.alexa.behaviors.model.Sequence");
        let p = &seq["startNode"]["operationPayload"];
        assert_eq!(seq["startNode"]["type"], "Alexa.Music.PlaySearchPhrase");
        assert_eq!(p["deviceSerialNumber"], "G0911");
        assert_eq!(p["customerId"], "A1CUST");
        assert_eq!(p["locale"], "fr-FR");
        assert_eq!(p["searchPhrase"], "du jazz");
        assert_eq!(p["musicProviderId"], "AMAZON_MUSIC");
    }

    #[test]
    fn several_speakers_at_once_run_side_by_side() {
        let (a, b) = (echo("A"), echo("B"));
        let both = together(vec![
            speak(&a, "C", "fr-FR", "Bonjour"),
            speak(&b, "C", "fr-FR", "Bonjour"),
        ]);
        assert_eq!(
            both["@type"],
            "com.amazon.alexa.behaviors.model.ParallelNode"
        );
        assert_eq!(both["nodesToExecute"].as_array().unwrap().len(), 2);
        let one = together(vec![volume(&a, "C", "fr-FR", 140)]);
        assert_eq!(one["operationPayload"]["value"], 100);
        let a_note = announce(&[&a, &b], "C", "fr-FR", "Le linge est prêt");
        assert_eq!(
            a_note["operationPayload"]["target"]["devices"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            text_command(&a, "C", "fr-FR", "Quelle Heure")["operationPayload"]["text"],
            "quelle heure"
        );
    }

    #[test]
    fn what_plays_and_how_loud() {
        let answer = json!({ "playerInfo": {
            "state": "PLAYING",
            "infoText": { "title": "So What", "subText1": "Miles Davis" },
            "volume": { "volume": 35, "muted": false }
        }});
        let p = player_state(&answer);
        assert_eq!(
            (p.state.as_str(), p.title.as_str(), p.artist.as_str()),
            ("PLAYING", "So What", "Miles Davis")
        );
        assert_eq!(p.volume, Some(35.0));
        assert_eq!(player_state(&json!({})).state, "IDLE");
        let v =
            volumes(&json!({ "volumes": [{ "dsn": "A", "speakerVolume": 40 }, { "dsn": "B" }] }));
        assert_eq!(v, [("A".to_owned(), 40.0)]);
    }

    #[test]
    fn the_default_music_service_is_the_accounts() {
        let entities = json!([
            { "id": "TUNEIN", "availability": "AVAILABLE", "supportedOperations": ["Alexa.Music.PlaySearchPhrase"] },
            { "id": "SPOTIFY", "availability": "AVAILABLE", "supportedOperations": ["Alexa.Music.PlaySearchPhrase"],
              "providerData": { "isDefaultMusicProvider": true } },
            { "id": "OFF", "availability": "UNAVAILABLE", "supportedOperations": ["Alexa.Music.PlaySearchPhrase"],
              "providerData": { "isDefaultMusicProvider": true } }
        ]);
        assert_eq!(default_provider(&entities).as_deref(), Some("SPOTIFY"));
        assert_eq!(default_provider(&json!([])), None);
        assert!(sayable("  ").is_err());
        assert!(sayable(&"a".repeat(251)).is_err());
        assert_eq!(sayable(" Bonjour ").unwrap(), "Bonjour");
    }
}
