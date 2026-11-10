//! The TV's Android side, beside JointSpace: the Android TV core
//! (`moli_androidtv::tv`: apps, keys, text, what plays), and what only a
//! Philips needs from it: its JointSpace keys, pressed through the Android
//! remote while it is up.

pub(crate) use moli_androidtv::tv::{Android, Waiting};

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
}
