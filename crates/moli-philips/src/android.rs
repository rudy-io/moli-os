//! The TV's Android side, beside JointSpace: the Android TV core
//! (`moli_androidtv::tv`: apps, keys, text, what plays), and what only a
//! Philips needs from it: its JointSpace keys, pressed (or held) through the
//! Android remote while it is up.

pub(crate) use moli_androidtv::tv::{Android, Waiting, adb_key};

/// The keys that can be held (a video seeks faster, a list scrolls), and
/// the word that lets go.
pub(crate) const HOLD_KEYS: [&str; 7] = [
    "CursorLeft",
    "CursorRight",
    "CursorUp",
    "CursorDown",
    "FastForward",
    "Rewind",
    "release",
];

/// The Android key of a JointSpace key: the same press through the remote's
/// session, open as long as the TV is, where JointSpace goes away for half a
/// minute at times (an app starting…) while the TV plays on.
fn android_key(name: &str) -> Option<u32> {
    Some(match name {
        "CursorUp" => 19,
        "CursorDown" => 20,
        "CursorLeft" => 21,
        "CursorRight" => 22,
        "Confirm" => 23,
        "Back" => 4,
        "Home" => 3,
        "Options" => 82,
        "Info" => 165,
        "PlayPause" => 85,
        "Play" => 126,
        "Pause" => 127,
        "Stop" => 86,
        "Next" => 87,
        "Previous" => 88,
        "FastForward" => 90,
        "Rewind" => 89,
        "VolumeUp" => 24,
        "VolumeDown" => 25,
        "Mute" => 164,
        "ChannelStepUp" => 166,
        "ChannelStepDown" => 167,
        "Source" => 178,
        "WatchTV" => 170,
        _ => return None,
    })
}

/// A JointSpace key through the Android remote while it is up; `None` when
/// it is not (or the key has no Android twin): JointSpace takes it then.
pub(crate) fn press(android: &Android, name: &str) -> Option<anyhow::Result<()>> {
    android.press(android_key(name)?)
}

/// Holds a JointSpace key down through the Android remote (`release` lets
/// it go): only the keys of [`HOLD_KEYS`].
pub(crate) fn hold(android: &mut Android, name: &str) -> anyhow::Result<()> {
    android.hold(held(name)?)
}

/// The Android key a held key name stands for (`None`: let go).
fn held(name: &str) -> anyhow::Result<Option<u32>> {
    Ok(match name {
        "release" => None,
        key if HOLD_KEYS.contains(&key) => android_key(key),
        other => anyhow::bail!(moli_i18n::tr!(
            "pilotes.philips.touche_inconnue",
            key = format!("{other:?}")
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_remote_key_has_its_android_key() {
        for key in crate::KEYS {
            assert!(android_key(key).is_some(), "{key}");
        }
        assert_eq!(android_key("CursorDown"), Some(20));
        assert_eq!(android_key("Confirm"), Some(23));
        assert_eq!(android_key("Standby"), None);
    }

    #[test]
    fn every_held_key_has_its_android_key() {
        for key in HOLD_KEYS.iter().filter(|k| **k != "release") {
            assert!(held(key).unwrap().is_some(), "{key}");
        }
        assert_eq!(held("CursorRight").unwrap(), Some(22));
        assert_eq!(held("release").unwrap(), None);
        assert!(held("Home").is_err(), "not a key to hold");
    }
}
