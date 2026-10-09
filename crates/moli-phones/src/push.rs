//! Notifications to a phone, through Expo's push service (the app's token,
//! given at pairing), which hands them to Apple or Google. What goes there
//! is short: a title and a line, nothing else of the house.

use std::time::Duration;

use http::{Method, Request};
use http_body_util::Full;
use serde_json::{Value as Json, json};

const HOST: &str = "exp.host";
const PATH: &str = "/--/api/v2/push/send";
const MAX_BODY: usize = 1000;

/// « Title\nBody » or just a body (titled « Moli »), bounded.
pub(crate) fn message(text: &str) -> Option<(String, String)> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let (title, body) = match text.split_once('\n') {
        Some((t, b)) if !t.trim().is_empty() && !b.trim().is_empty() => (t.trim(), b.trim()),
        _ => ("Moli", text),
    };
    let mut body: String = body.chars().take(MAX_BODY).collect();
    if text.chars().count() > MAX_BODY {
        body.push('…');
    }
    Some((title.chars().take(80).collect(), body))
}

/// Sends one notification; the error says why it did not leave.
pub(crate) async fn send(token: &str, title: &str, body: &str) -> Result<(), String> {
    let payload = json!({
        "to": token,
        "title": title,
        "body": body,
        "sound": "default",
        "priority": "high",
    });
    let request = Request::builder()
        .method(Method::POST)
        .uri(PATH)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .body(Full::new(moli_net::Body::from(payload.to_string())))
        .map_err(|e| e.to_string())?;
    let (status, bytes) =
        moli_net::web(HOST, 443, true, request, Duration::from_secs(15), 64 * 1024)
            .await
            .map_err(|e| {
                moli_i18n::tr!("pilotes.phones.push_injoignable", cause = e.root_cause())
            })?;
    let answer: Json = serde_json::from_slice(&bytes)
        .map_err(|_| moli_i18n::tr!("pilotes.phones.push_illisible", status = status.as_u16()))?;
    // One message: `data` is an object; Expo also answers with an array.
    let ticket = match &answer["data"] {
        Json::Array(list) => list.first().cloned().unwrap_or_default(),
        other => other.clone(),
    };
    if ticket["status"] == "ok" {
        return Ok(());
    }
    let why = ticket["details"]["error"]
        .as_str()
        .or_else(|| ticket["message"].as_str())
        .or_else(|| answer["errors"][0]["message"].as_str());
    if why == Some("DeviceNotRegistered") {
        return Err(moli_i18n::tr!("pilotes.phones.plus_de_notifications"));
    }
    Err(moli_i18n::tr!(
        "pilotes.phones.push_refuse",
        status = status.as_u16(),
        why = why.map_or_else(
            || moli_i18n::tr!("pilotes.phones.sans_explication"),
            str::to_owned
        )
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_line_is_the_title() {
        assert_eq!(
            message("Porte d'entrée\nElle est ouverte depuis 10 min"),
            Some((
                "Porte d'entrée".into(),
                "Elle est ouverte depuis 10 min".into()
            ))
        );
        assert_eq!(
            message("Juste une ligne"),
            Some(("Moli".into(), "Juste une ligne".into()))
        );
        assert_eq!(message("  "), None);
        let long = "x".repeat(2000);
        assert_eq!(message(&long).unwrap().1.chars().count(), MAX_BODY + 1);
    }
}
