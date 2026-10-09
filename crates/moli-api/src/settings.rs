//! What the house chose from the dashboard, next to its other data
//! (`settings.json`): for now its language, chosen at the installation. It
//! wins over `[server] language` (the configuration may be read-only).

use std::path::Path;

use serde_json::{Value, json};

/// The language the house chose, if any.
#[must_use]
pub fn language(data_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(data_dir.join("settings.json")).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value["language"].as_str().map(str::to_owned)
}

/// Keeps the house's language (the other settings stay as they are).
pub(crate) fn set_language(path: &Path, language: &str) -> std::io::Result<()> {
    let mut value: Value = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({}));
    value["language"] = json!(language);
    let text = serde_json::to_string_pretty(&value).map_err(std::io::Error::other)?;
    // Written aside then renamed: never half a file.
    let aside = path.with_extension("json.new");
    std::fs::write(&aside, text)?;
    std::fs::rename(&aside, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_language_is_kept_with_the_other_settings() {
        let dir = std::env::temp_dir().join(format!("moli-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(language(&dir), None);
        std::fs::write(dir.join("settings.json"), r#"{"other": 1}"#).unwrap();
        set_language(&dir.join("settings.json"), "en").unwrap();
        assert_eq!(language(&dir).as_deref(), Some("en"));
        let kept: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(kept["other"], 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
