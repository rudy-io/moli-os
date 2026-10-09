//! `/api/assistant`: one turn of the conversation with Moli, and speech to
//! text for spoken questions. The orders it gives go through the guard as
//! `Origin::Assistant`, whoever is talking.

use std::sync::Arc;

use axum::Extension;
use axum::body::Bytes;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use moli_assistant::{Assistant, AssistantError, Speech, Turn};
use serde_json::json;

use crate::caller::Caller;
use crate::session::Sessions;

#[derive(Clone)]
pub(crate) struct Moli(pub(crate) Option<Assistant>);

pub(crate) async fn status(Extension(Moli(moli)): Extension<Moli>) -> Response {
    match moli {
        Some(moli) => axum::Json(moli.status()).into_response(),
        None => axum::Json(
            json!({ "ready": false, "listen": false, "speak": false, "configured": false }),
        )
        .into_response(),
    }
}

pub(crate) async fn turn(
    Extension(Moli(moli)): Extension<Moli>,
    axum::Json(turn): axum::Json<Turn>,
) -> Response {
    let Some(moli) = moli else {
        return not_configured();
    };
    match moli.turn(turn).await {
        Ok(reply) => axum::Json(reply).into_response(),
        Err(e) => failure(&e),
    }
}

pub(crate) async fn listen(
    Extension(Moli(moli)): Extension<Moli>,
    headers: HeaderMap,
    audio: Bytes,
) -> Response {
    let Some(moli) = moli else {
        return not_configured();
    };
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    match moli.listen(&audio, mime).await {
        Ok(heard) => {
            axum::Json(json!({ "text": heard.text, "engine": heard.engine })).into_response()
        }
        Err(e) => failure(&e),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct Say {
    text: String,
    /// One of OpenAI's voices, to try another than the configured one.
    #[serde(default)]
    voice: Option<String>,
}

/// One sentence of an answer, spoken (MP3 from the cloud, WAV from Piper).
/// With `accept: audio/pcm`, the cloud voice comes as it is made (16-bit PCM,
/// rate in the content type): the voice starts before the sentence is whole.
pub(crate) async fn speak(
    Extension(Moli(moli)): Extension<Moli>,
    headers: HeaderMap,
    axum::Json(say): axum::Json<Say>,
) -> Response {
    let Some(moli) = moli else {
        return not_configured();
    };
    let streamed = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("audio/pcm"));
    let speech = if streamed {
        moli.speak_streamed(&say.text, say.voice.as_deref()).await
    } else {
        moli.speak(&say.text, say.voice.as_deref())
            .await
            .map(Speech::Whole)
    };
    speech_response(speech)
}

/// A voice prepared for a satellite (`<id>.mp3`, 60 s): its player fetches
/// it and plays it while the cloud voice makes it. The id is the only key.
pub(crate) async fn satellite_voice(
    Extension(Moli(moli)): Extension<Moli>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    let Some(moli) = moli else {
        return not_configured();
    };
    speech_response(moli.satellite_voice(&name).await)
}

fn speech_response(speech: Result<Speech, AssistantError>) -> Response {
    let voice = header::HeaderName::from_static("x-moli-voice");
    match speech {
        Ok(Speech::Whole(spoken)) => (
            [
                (header::CONTENT_TYPE, spoken.mime),
                (header::CACHE_CONTROL, "no-store"),
                (voice, spoken.engine),
            ],
            spoken.audio,
        )
            .into_response(),
        Ok(Speech::Stream(stream)) => {
            let chunks = futures::stream::unfold(stream.chunks, |mut chunks| async move {
                chunks.recv().await.map(|chunk| (chunk, chunks))
            });
            (
                [
                    (header::CONTENT_TYPE, stream.mime),
                    (header::CACHE_CONTROL, "no-store"),
                    (voice, stream.engine),
                ],
                axum::body::Body::from_stream(chunks),
            )
                .into_response()
        }
        Err(e) => failure(&e),
    }
}

/// The settings card: key filed or not (never the key), voice, tone.
pub(crate) async fn settings(Extension(Moli(moli)): Extension<Moli>) -> Response {
    match moli {
        Some(moli) => axum::Json(moli.settings()).into_response(),
        None => not_configured(),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct Prefs {
    #[serde(default)]
    voice: Option<String>,
    #[serde(default)]
    style: Option<String>,
    #[serde(default)]
    pace: Option<String>,
}

/// The voice, the tone and the pace: a taste, not a power, so no code asked.
pub(crate) async fn set_settings(
    Extension(Moli(moli)): Extension<Moli>,
    axum::Json(prefs): axum::Json<Prefs>,
) -> Response {
    let Some(moli) = moli else {
        return not_configured();
    };
    match moli
        .set_voice_prefs(
            prefs.voice.as_deref(),
            prefs.style.as_deref(),
            prefs.pace.as_deref(),
        )
        .await
    {
        Ok(settings) => axum::Json(settings).into_response(),
        Err(e) => failure(&e),
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct Key {
    api_key: String,
}

/// The API key, given by a person (code): tested, then filed encrypted.
pub(crate) async fn set_key(
    Extension(Moli(moli)): Extension<Moli>,
    Extension(humans): Extension<Arc<Sessions>>,
    caller: Caller,
    headers: HeaderMap,
    axum::Json(key): axum::Json<Key>,
) -> Response {
    if !humans.is_human(&headers, &caller.key) {
        return (
            StatusCode::FORBIDDEN,
            axum::Json(json!({ "error": moli_i18n::tr!("serveur.cle.assistant_humain") })),
        )
            .into_response();
    }
    let Some(moli) = moli else {
        return not_configured();
    };
    match moli.set_key(&key.api_key).await {
        Ok(()) => axum::Json(json!({ "stored": true })).into_response(),
        Err(e) => failure(&e),
    }
}

fn not_configured() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(
            json!({ "error": "the assistant is not configured ([assistant] in moli.toml)" }),
        ),
    )
        .into_response()
}

pub(crate) fn failure(e: &AssistantError) -> Response {
    let status = match e {
        AssistantError::NotConfigured(_) => StatusCode::SERVICE_UNAVAILABLE,
        AssistantError::Busy => StatusCode::TOO_MANY_REQUESTS,
        AssistantError::Invalid(_) => StatusCode::BAD_REQUEST,
        AssistantError::Upstream(_) => StatusCode::BAD_GATEWAY,
    };
    if status != StatusCode::BAD_REQUEST {
        tracing::warn!(error = %e, "assistant");
    }
    (status, axum::Json(json!({ "error": e.to_string() }))).into_response()
}
