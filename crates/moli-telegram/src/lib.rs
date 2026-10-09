//! Telegram, straight to the Bot API: Moli writes to the household itself,
//! no relay in between (no n8n, nothing else to keep alive).
//!
//! One device, « Telegram », whose `notify` point sends its text to the
//! configured chats: a command like any other, so it is journaled, guarded
//! and usable by automations (the « Telegram » channel) and by Moli.
//!
//! The bot's token lives in a secrets manager, never in `moli.toml`: the
//! container's start script hands it over in a tmpfs file (`token_file`). A
//! person writes `/start` to the bot once: Telegram lets a bot answer people
//! only after they spoke to it first.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use http::{Method, Request};
use http_body_util::Full;
use moli_core::{Access, Device, Kind, PointSpec, Semantic, Value};
use moli_runtime::{BoxFuture, Driver, DriverCtx};
use serde::Deserialize;
use serde_json::{Value as Json, json};

const HOST: &str = "api.telegram.org";
/// Telegram's limit is 4096 characters; a little room for the cut mark.
const MAX_TEXT: usize = 4000;
const TIMEOUT: Duration = Duration::from_secs(15);
const MAX_ANSWER: usize = 64 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Where the token is (a tmpfs file written by the start script from the vault).
    #[serde(default = "default_token_file")]
    pub token_file: PathBuf,
    /// The chats messages go to (a person's chat id is their user id).
    pub chats: Vec<i64>,
}

fn default_token_file() -> PathBuf {
    PathBuf::from("/run/secrets/telegram.token")
}

#[derive(Debug)]
pub struct Telegram {
    config: Config,
}

impl Telegram {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !config.chats.is_empty(),
            "chats: at least one chat id (a person's user id)"
        );
        anyhow::ensure!(config.chats.len() <= 10, "chats: 10 at most");
        Ok(Self { config })
    }
}

impl Driver for Telegram {
    fn kind(&self) -> &'static str {
        "telegram"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverCtx) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(run(&self.config, ctx))
    }
}

/// `123456:ABC-def_…`: what BotFather hands out.
fn token_ok(token: &str) -> bool {
    let Some((id, secret)) = token.split_once(':') else {
        return false;
    };
    !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_digit())
        && secret.len() >= 30
        && secret
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The text Telegram gets: trimmed, never longer than it accepts.
fn message(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= MAX_TEXT {
        return Some(text.to_owned());
    }
    let mut cut: String = text.chars().take(MAX_TEXT).collect();
    cut.push('…');
    Some(cut)
}

/// One Bot API call. The token is in the path: it never appears in an
/// error (only Telegram's own description does).
async fn call(token: &str, method: &str, body: &Json) -> anyhow::Result<Json> {
    let request = Request::builder()
        .method(Method::POST)
        .uri(format!("/bot{token}/{method}"))
        .header("content-type", "application/json")
        .body(Full::new(moli_net::Body::from(body.to_string())))
        .context("request")?;
    let (status, bytes) = moli_net::web(HOST, 443, true, request, TIMEOUT, MAX_ANSWER)
        .await
        .map_err(|e| {
            anyhow::anyhow!(moli_i18n::tr!(
                "pilotes.telegram.injoignable",
                cause = e.root_cause()
            ))
        })?;
    let answer: Json = serde_json::from_slice(&bytes).map_err(|_| {
        anyhow::anyhow!(moli_i18n::tr!(
            "pilotes.telegram.sans_json",
            status = status.as_u16()
        ))
    })?;
    if answer["ok"].as_bool() == Some(true) {
        return Ok(answer["result"].clone());
    }
    let why = answer["description"].as_str().map_or_else(
        || moli_i18n::tr!("pilotes.telegram.sans_explication"),
        str::to_owned,
    );
    Err(anyhow::anyhow!(moli_i18n::tr!(
        "pilotes.telegram.refuse",
        status = status.as_u16(),
        why = why
    )))
}

async fn run(config: &Config, ctx: &mut DriverCtx) -> anyhow::Result<()> {
    let token = tokio::fs::read_to_string(&config.token_file)
        .await
        .unwrap_or_default()
        .trim()
        .to_owned();
    if !token_ok(&token) {
        ctx.wait_for(moli_i18n::tr!("pilotes.telegram.pas_de_jeton"));
        ctx.cancelled().await;
        return Ok(());
    }
    let me = match call(&token, "getMe", &json!({})).await {
        Ok(me) => me,
        // A refused token will not get better by retrying every minute.
        Err(e) if e.to_string().contains("(401)") || e.to_string().contains("(404)") => {
            ctx.wait_for(moli_i18n::tr!("pilotes.telegram.jeton_refuse", error = e));
            ctx.cancelled().await;
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    let bot = me["username"].as_str().unwrap_or("bot").to_owned();
    let id = ctx.device_id("bot");
    ctx.upsert_device(Device {
        id: id.clone(),
        instance: ctx.instance().clone(),
        native_name: "Telegram".into(),
        manufacturer: Some("Telegram".into()),
        model: Some(format!("@{bot}").into()),
        description: Some(moli_i18n::tr!("pilotes.telegram.description").into()),
        native_room: None,
        members: Vec::new(),
        points: vec![PointSpec {
            key: "notify".into(),
            label: moli_i18n::tr!("pilotes.telegram.envoyer").into(),
            kind: Kind::Text,
            access: Access {
                read: false,
                write: true,
            },
            unit: None,
            semantic: Semantic::Other,
        }],
    });
    ctx.set_availability(&id, true);
    ctx.ready();
    tracing::info!(instance = %ctx.instance(), bot, chats = config.chats.len(), "telegram ready");

    while let Some(command) = ctx.next_command().await {
        let result = match (&*command.key, &command.value) {
            ("notify", Value::Text(text)) => match message(text) {
                Some(text) => send(&token, &config.chats, &text).await,
                None => Err(moli_i18n::tr!("pilotes.telegram.message_vide")),
            },
            ("notify", _) => Err(moli_i18n::tr!("pilotes.telegram.message_texte")),
            _ => Err(moli_i18n::tr!(
                "pilotes.telegram.ne_s_ecrit_pas",
                point = command.key
            )),
        };
        command.reply(result);
    }
    Ok(())
}

/// To every chat; the first refusal is the answer (with the hint a person
/// needs: write /start to the bot).
async fn send(token: &str, chats: &[i64], text: &str) -> Result<(), String> {
    for chat in chats {
        let body = json!({
            "chat_id": chat,
            "text": text,
            "disable_web_page_preview": true,
        });
        if let Err(e) = call(token, "sendMessage", &body).await {
            let mut why = e.to_string();
            if why.contains("chat not found") || why.contains("can't initiate") {
                why.push_str(&moli_i18n::tr!("pilotes.telegram.ecrire_start"));
            }
            return Err(why);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_look_like_botfathers() {
        assert!(token_ok("123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw"));
        assert!(!token_ok(""));
        assert!(!token_ok("123456789"));
        assert!(!token_ok("abc:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw"));
        assert!(!token_ok("123:short"));
        assert!(!token_ok("123:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw/../x"));
    }

    #[test]
    fn messages_fit_telegram() {
        assert_eq!(message("  "), None);
        assert_eq!(message(" bonjour ").as_deref(), Some("bonjour"));
        let long = "é".repeat(5000);
        let cut = message(&long).unwrap();
        assert_eq!(cut.chars().count(), MAX_TEXT + 1);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn a_driver_needs_someone_to_write_to() {
        let none = Config {
            token_file: default_token_file(),
            chats: vec![],
        };
        assert!(Telegram::new(none).is_err());
    }
}
