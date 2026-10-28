//! One time zone for the house: `[server].timezone`. It decides when
//! automations fire, where energy days are cut, the assistant's clock and
//! the quiet hours, so it is never guessed in silence (#15).
//!
//! The sections that once had their own (`[automations]`, `[energy]`,
//! `[assistant]`, `[guard.quiet_hours]`) inherit it. Written there, it is
//! still accepted, so existing configurations keep working, but it must
//! agree with the house's, and Moli says at every start that it belongs in
//! `[server]`. Written nowhere, the system's zone is taken and named.

use anyhow::bail;

/// Where a section writes its time zone, as `moli.toml` spells it.
const SECTIONS: [(&str, &[&str]); 4] = [
    ("[automations]", &["automations"]),
    ("[energy]", &["energy"]),
    ("[assistant]", &["assistant"]),
    ("[guard.quiet_hours]", &["guard", "quiet_hours"]),
];

/// The names a system gives to UTC: in a container, usually not where the
/// house is.
const UTC: [&str; 10] = [
    "UTC",
    "Etc/UTC",
    "UCT",
    "Etc/UCT",
    "Universal",
    "Etc/Universal",
    "Zulu",
    "Etc/Zulu",
    "GMT",
    "Etc/GMT",
];

/// Where the house's time zone came from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Source {
    /// `[server].timezone`.
    Server,
    /// The sections that still write one (they agree): the first of them.
    Section(&'static str),
    /// Nothing written: the system's (`TZ`, else `/etc/localtime`).
    System,
    /// Nothing written, nothing from the system: UTC.
    #[default]
    Unknown,
}

/// The house's time zone, as resolved when the configuration is read.
#[derive(Clone, Debug)]
pub struct Zone {
    /// IANA name, e.g. `Indian/Reunion`.
    pub name: String,
    pub source: Source,
    /// The sections that write a time zone of their own.
    pub written: Vec<&'static str>,
}

impl Default for Zone {
    fn default() -> Self {
        Self {
            name: "UTC".into(),
            source: Source::Unknown,
            written: Vec::new(),
        }
    }
}

/// The time zones the sections write, read in the raw configuration (once
/// deserialized, a section's own default could no longer be told apart).
pub fn written(table: &toml::Table) -> anyhow::Result<Vec<(&'static str, String)>> {
    let mut found = Vec::new();
    for (label, path) in SECTIONS {
        let mut at = Some(table);
        for key in path {
            at = at.and_then(|t| t.get(*key)).and_then(toml::Value::as_table);
        }
        match at.and_then(|t| t.get("timezone")) {
            None => {}
            Some(toml::Value::String(name)) => found.push((label, name.clone())),
            Some(_) => bail!("{label} timezone must be a string (an IANA name)"),
        }
    }
    Ok(found)
}

/// The house's time zone: `[server]`'s, else the sections' (which must
/// agree), else `system`'s, else UTC. A zone that contradicts another is
/// refused, naming both.
pub fn settle(
    server: Option<&str>,
    sections: &[(&'static str, String)],
    system: impl FnOnce() -> Option<String>,
) -> anyhow::Result<Zone> {
    let named = server
        .map(|name| ("[server]", name))
        .into_iter()
        .chain(sections.iter().map(|(label, name)| (*label, name.as_str())));
    let mut first: Option<(&str, &str)> = None;
    for (label, name) in named {
        if jiff::tz::TimeZone::get(name).is_err() {
            bail!(
                "{label} timezone = {name:?}: not a known IANA time zone (e.g. \"Europe/Paris\")"
            );
        }
        match first {
            None => first = Some((label, name)),
            Some((house, zone)) if zone != name => bail!(
                "{house} timezone = {zone:?} but {label} timezone = {name:?}: a house has one time zone. Write it once, in [server], and remove the other"
            ),
            Some(_) => {}
        }
    }
    let written = sections.iter().map(|(label, _)| *label).collect();
    if let Some(name) = server {
        return Ok(Zone {
            name: name.to_owned(),
            source: Source::Server,
            written,
        });
    }
    if let Some((label, name)) = sections.first() {
        return Ok(Zone {
            name: name.clone(),
            source: Source::Section(label),
            written,
        });
    }
    Ok(match system() {
        Some(name) => Zone {
            name,
            source: Source::System,
            written,
        },
        None => Zone {
            written,
            ..Zone::default()
        },
    })
}

/// The system's time zone, by its IANA name: `TZ`, else where
/// `/etc/localtime` points, else `/etc/timezone`. `None` when none of them
/// names a zone Moli knows.
pub fn system() -> Option<String> {
    let known = |name: &str| {
        let name = name.trim().trim_start_matches(':');
        jiff::tz::TimeZone::get(name)
            .is_ok()
            .then(|| name.to_owned())
    };
    // A `TZ` that is no zone name (a POSIX string like `CET-1CEST`) falls back to the files.
    if let Some(tz) = std::env::var("TZ").ok().and_then(|tz| known(&tz)) {
        return Some(tz);
    }
    std::fs::read_link("/etc/localtime")
        .ok()
        .and_then(|target| {
            let target = target.to_string_lossy().into_owned();
            target
                .split_once("zoneinfo/")
                .and_then(|(_, name)| known(name))
        })
        .or_else(|| {
            std::fs::read_to_string("/etc/timezone")
                .ok()
                .and_then(|name| known(&name))
        })
}

impl Zone {
    /// What to say at start (and in `check-config`): the sections that
    /// should no longer carry a zone, and a zone nobody wrote.
    pub fn warnings(&self) -> Vec<String> {
        let mut said: Vec<String> = self
            .written
            .iter()
            .map(|section| {
                moli_i18n::tr!(
                    "serveur.fuseau.section",
                    section = section,
                    zone = self.name
                )
            })
            .collect();
        match self.source {
            Source::System if UTC.contains(&self.name.as_str()) => {
                said.push(moli_i18n::tr!(
                    "serveur.fuseau.systeme_utc",
                    zone = self.name
                ));
            }
            Source::System => {
                said.push(moli_i18n::tr!("serveur.fuseau.systeme", zone = self.name));
            }
            Source::Unknown => said.push(moli_i18n::tr!("serveur.fuseau.inconnu")),
            Source::Server | Source::Section(_) => {}
        }
        said
    }

    /// Where it came from, for `check-config`.
    pub fn origin(&self) -> String {
        match self.source {
            Source::Server => "[server] timezone".into(),
            Source::Section(label) => format!("{label} timezone"),
            Source::System => "the system's".into(),
            Source::Unknown => "none written nor found".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sections(text: &str) -> Vec<(&'static str, String)> {
        written(&toml::from_str(text).unwrap()).unwrap()
    }

    #[test]
    fn the_server_zone_wins_and_agreeing_sections_are_only_warned() {
        let found = sections(
            "[server]\ntimezone = \"Indian/Reunion\"\n[automations]\ntimezone = \"Indian/Reunion\"",
        );
        let zone = settle(Some("Indian/Reunion"), &found, || panic!("not asked")).unwrap();
        assert_eq!(zone.name, "Indian/Reunion");
        assert_eq!(zone.source, Source::Server);
        assert_eq!(zone.written, ["[automations]"]);
        assert_eq!(zone.warnings().len(), 1);
    }

    #[test]
    fn sections_that_disagree_are_refused_even_without_a_server_zone() {
        let found = sections(
            "[energy]\ntimezone = \"Indian/Reunion\"\n[guard.quiet_hours]\nfrom = \"21:00\"\nto = \"07:00\"\ntimezone = \"Europe/Paris\"",
        );
        let err = settle(None, &found, || None).unwrap_err().to_string();
        assert!(
            err.contains("[energy]") && err.contains("[guard.quiet_hours]"),
            "{err}"
        );
    }

    #[test]
    fn written_nowhere_the_system_zone_is_taken_and_named() {
        let zone = settle(None, &[], || Some("Indian/Reunion".into())).unwrap();
        assert_eq!(
            (zone.name.as_str(), &zone.source),
            ("Indian/Reunion", &Source::System)
        );
        assert_eq!(zone.warnings().len(), 1);
        // A container's UTC is said to be one, in its own words.
        let utc = settle(None, &[], || Some("Etc/UTC".into())).unwrap();
        assert_ne!(utc.warnings(), zone.warnings());
        let unknown = settle(None, &[], || None).unwrap();
        assert_eq!(
            (unknown.name.as_str(), &unknown.source),
            ("UTC", &Source::Unknown)
        );
        assert_eq!(unknown.warnings().len(), 1);
    }

    #[test]
    fn an_unknown_zone_is_refused_by_its_section() {
        let found = sections("[assistant]\ntimezone = \"Paris\"");
        let err = settle(None, &found, || None).unwrap_err().to_string();
        assert!(err.contains("[assistant]"), "{err}");
    }
}
