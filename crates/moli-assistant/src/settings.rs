//! Moli's settings from the dashboard: the API key (tested, then filed in
//! the encrypted store, never sent back) and the voice (which one, which
//! tone), kept in `data/assistant.json` and applied at once.

use std::path::{Path, PathBuf};
use std::sync::PoisonError;

use moli_core::InstanceId;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use crate::{Assistant, AssistantError, CLOUD_VOICES};

/// Tones a person can pick: id, then the catalogue keys of its label and of
/// its instructions for the voice (both in the house's language).
pub(crate) const STYLES: [(&str, &str, &str); 3] = [
    (
        "enjoue",
        "assistant.voice.style.enjoue.label",
        "assistant.voice.style.enjoue.instructions",
    ),
    (
        "pose",
        "assistant.voice.style.pose.label",
        "assistant.voice.style.pose.instructions",
    ),
    (
        "doux",
        "assistant.voice.style.doux.label",
        "assistant.voice.style.doux.instructions",
    ),
];

/// The voice a person chose in the dashboard.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct VoicePrefs {
    pub voice: String,
    pub style: String,
}

/// The instructions of tone `id`, in the house's language.
fn style_text(id: &str) -> Option<String> {
    STYLES
        .iter()
        .find(|(i, ..)| *i == id)
        .map(|(.., key)| moli_i18n::tr(key))
}

/// Which tone says `text` (in the house's language); `perso` for a text of
/// the person's own.
fn style_id(text: &str) -> &'static str {
    STYLES
        .iter()
        .find(|(.., key)| moli_i18n::tr(key) == text)
        .map_or("perso", |(i, ..)| *i)
}

/// The tone `moli.toml` asks for. The built-in default is the cheerful tone,
/// said in the house's language; any other text is the person's own.
fn configured_style(speech_style: &str) -> String {
    if speech_style == crate::default_speech_style() {
        style_text(STYLES[0].0).unwrap_or_default()
    } else {
        speech_style.to_owned()
    }
}

/// An OpenAI key, at first sight: no spaces, a sensible length.
pub(crate) fn plausible_key(key: &str) -> bool {
    (20..=300).contains(&key.len()) && key.chars().all(|c| c.is_ascii_graphic())
}

fn read_prefs(path: &Path) -> Option<VoicePrefs> {
    let prefs: VoicePrefs = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (CLOUD_VOICES.contains(&prefs.voice.as_str()) && style_text(&prefs.style).is_some())
        .then_some(prefs)
}

impl Assistant {
    /// Reads the voice chosen in the dashboard (`data/assistant.json`);
    /// without it, `moli.toml` decides.
    pub fn load_voice_prefs(&self, path: PathBuf) {
        let prefs = read_prefs(&path);
        *self.0.prefs.write().unwrap_or_else(PoisonError::into_inner) = prefs;
        *self
            .0
            .prefs_path
            .write()
            .unwrap_or_else(PoisonError::into_inner) = Some(path);
    }

    /// The cloud voice and its instructions, as chosen now.
    pub(crate) fn voice_and_style(&self) -> (String, String) {
        let c = &self.0.config;
        match &*self.0.prefs.read().unwrap_or_else(PoisonError::into_inner) {
            Some(p) => (
                p.voice.clone(),
                style_text(&p.style).unwrap_or_else(|| configured_style(&c.speech_style)),
            ),
            None => (c.speech_voice.clone(), configured_style(&c.speech_style)),
        }
    }

    /// What the settings card shows (never the key).
    pub fn settings(&self) -> Json {
        let c = &self.0.config;
        let prefs = self
            .0
            .prefs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let (voice, style) = match prefs {
            Some(p) => (p.voice, p.style),
            None => (
                c.speech_voice.clone(),
                style_id(&configured_style(&c.speech_style)).to_owned(),
            ),
        };
        json!({
            "key": self.0.hub.secret(&InstanceId::from("assistant"), "api_key").is_some(),
            "local_model": self.0.endpoint.is_local(),
            "model": c.model,
            "cloud_voice": !c.speech_model.is_empty(),
            "local_voice": crate::voice::host_port(&c.local_speech).is_some(),
            "voice": voice,
            "style": style,
            "voices": CLOUD_VOICES,
            "styles": STYLES.iter().map(|(id, label, _)| json!({ "id": id, "label": moli_i18n::tr(label) })).collect::<Vec<_>>(),
        })
    }

    /// Changes the voice and/or the tone, kept for the next start.
    pub async fn set_voice_prefs(
        &self,
        voice: Option<&str>,
        style: Option<&str>,
    ) -> Result<Json, AssistantError> {
        if voice.is_some_and(|v| !CLOUD_VOICES.contains(&v)) {
            return Err(AssistantError::Invalid("unknown voice".into()));
        }
        if style.is_some_and(|s| style_text(s).is_none()) {
            return Err(AssistantError::Invalid("unknown tone".into()));
        }
        let (current_voice, _) = self.voice_and_style();
        let current_style = self
            .0
            .prefs
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or_else(|| STYLES[0].0.to_owned(), |p| p.style.clone());
        let prefs = VoicePrefs {
            voice: voice.map_or(current_voice, str::to_owned),
            style: style.map_or(current_style, str::to_owned),
        };
        let path = self
            .0
            .prefs_path
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(path) = path {
            let tmp = path.with_extension("json.tmp");
            let body = serde_json::to_vec_pretty(&prefs)
                .map_err(|e| AssistantError::Upstream(e.to_string()))?;
            tokio::fs::write(&tmp, body)
                .await
                .and(tokio::fs::rename(&tmp, &path).await)
                .map_err(|e| AssistantError::Upstream(format!("cannot save the voice: {e}")))?;
        }
        tracing::info!(voice = %prefs.voice, style = %prefs.style, "voice chosen from the dashboard");
        *self.0.prefs.write().unwrap_or_else(PoisonError::into_inner) = Some(prefs);
        Ok(self.settings())
    }

    /// Tests a key against the provider, then files it (encrypted).
    pub async fn set_key(&self, key: &str) -> Result<(), AssistantError> {
        let key = key.trim();
        if !plausible_key(key) {
            return Err(AssistantError::Invalid(moli_i18n::tr!(
                "assistant.settings.key_invalid"
            )));
        }
        let accepted = self
            .0
            .endpoint
            .check(key)
            .await
            .map_err(|e| AssistantError::Upstream(e.to_string()))?;
        if !accepted {
            return Err(AssistantError::Invalid(moli_i18n::tr!(
                "assistant.settings.key_refused"
            )));
        }
        self.0
            .hub
            .store_secret(&InstanceId::from("assistant"), "api_key", key)
            .await
            .map_err(|e| {
                AssistantError::Upstream(moli_i18n::tr!("assistant.settings.vault", error = e))
            })?;
        tracing::info!("assistant API key filed from the dashboard");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_at_first_sight() {
        assert!(plausible_key("sk-proj-abcdefghijklmnopqrstuvwxyz0123"));
        assert!(!plausible_key("sk-court"));
        assert!(!plausible_key("sk-proj abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn prefs_are_read_only_when_valid() {
        let dir = std::env::temp_dir().join(format!("moli-prefs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("assistant.json");
        std::fs::write(&path, r#"{"voice":"nova","style":"doux"}"#).unwrap();
        assert_eq!(
            read_prefs(&path),
            Some(VoicePrefs {
                voice: "nova".into(),
                style: "doux".into()
            })
        );
        std::fs::write(&path, r#"{"voice":"robot","style":"doux"}"#).unwrap();
        assert_eq!(read_prefs(&path), None);
        assert_eq!(read_prefs(&dir.join("absent.json")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn every_tone_has_words() {
        for (id, label, text) in STYLES {
            assert!(!id.is_empty() && !label.is_empty() && text.len() > 30);
        }
    }

    #[test]
    fn the_french_tones_are_what_they_always_were() {
        assert_eq!(STYLES.len(), crate::before::STYLES.len());
        for ((id, label, text), (old_id, old_label, old_text)) in
            STYLES.iter().zip(crate::before::STYLES)
        {
            assert_eq!(*id, old_id);
            assert_eq!(moli_i18n::tr(label), old_label);
            assert_eq!(moli_i18n::tr(text), old_text);
        }
    }

    #[test]
    fn the_default_tone_speaks_the_houses_language() {
        let cheerful = style_text("enjoue").unwrap();
        // moli.toml's built-in default is the cheerful tone, whatever the language.
        assert_eq!(crate::default_speech_style(), cheerful);
        assert_eq!(configured_style(&crate::default_speech_style()), cheerful);
        assert_eq!(configured_style("Voix de sorcière."), "Voix de sorcière.");
        assert_eq!(style_id(&style_text("doux").unwrap()), "doux");
        assert_eq!(style_id("Voix de sorcière."), "perso");
        let en = crate::tests::english();
        for (_, label, text) in STYLES {
            assert!(en[label].is_ascii() && en[text].is_ascii(), "{label}");
        }
    }

    #[tokio::test]
    async fn a_refused_key_is_said_in_the_houses_words() {
        let hub = moli_runtime::Hub::new(moli_runtime::HubOptions::default()).unwrap();
        let config = serde_json::from_value(json!({})).unwrap();
        let moli = Assistant::new(hub, None, None, None, config).unwrap();
        match moli.set_key("sk-court").await {
            Err(AssistantError::Invalid(why)) => assert_eq!(
                why,
                "ce n'est pas une clé d'API (elle commence par sk-, sans espace)"
            ),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            moli_i18n::tr!("assistant.settings.key_refused"),
            "OpenAI refuse cette clé : vérifie-la, ou qu'elle n'a pas été supprimée"
        );
        assert_eq!(
            moli_i18n::tr!("assistant.settings.vault", error = "disque plein"),
            "coffre : disque plein"
        );
    }
}
