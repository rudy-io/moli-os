//! Guard: where and when agents may act on their own.
//!
//! A human clicking in the dashboard is always obeyed. Anyone else (an agent
//! over MCP, a script over the API, the CLI) acting on a *protected room*,
//! or on a guarded room during *quiet hours*, does not act: the order becomes
//! a request a human approves or denies. The policy lives in the
//! configuration file, which only humans edit — no surface can change it.

use jiff::Timestamp;
use jiff::civil::Time;
use jiff::tz::TimeZone;
use moli_core::Origin;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization as _;

#[derive(Clone, Debug, Default)]
pub struct GuardPolicy {
    /// Rooms where agents never act without a human's approval.
    pub protected_rooms: Vec<String>,
    pub quiet_hours: Option<QuietHours>,
    /// Treat devices with no room at all as protected (nobody can tell
    /// where they are, so nobody should act on them unattended).
    pub protect_unassigned: bool,
    /// Who the dashboard is: the household (no code to command), or only a
    /// person with a PIN session.
    pub dashboard: Dashboard,
}

/// How the dashboard is recognized as the family (`[guard] dashboard`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dashboard {
    /// The home network, or a person Cloudflare Access let in: obeyed
    /// without a code. The PIN is kept for approving automations and
    /// releasing what an agent asked for.
    #[default]
    Trusted,
    /// Only a PIN session commands from the dashboard.
    Pin,
}

#[derive(Clone, Debug)]
pub struct QuietHours {
    pub from: Time,
    pub to: Time,
    pub tz: TimeZone,
    pub tz_name: String,
    /// Rooms concerned; empty = the whole home.
    pub rooms: Vec<String>,
}

/// What the guard says about one order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Needs a human: the reason, in words the human will read.
    Approval(String),
}

/// Policy summary for the surfaces (dashboard badges, agents).
#[derive(Clone, Debug, Serialize)]
pub struct GuardStatus {
    pub protected_rooms: Vec<String>,
    /// Protected rooms matching no device: protection silently not applying.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unmatched_protected_rooms: Vec<String>,
    pub protect_unassigned: bool,
    pub dashboard: Dashboard,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiet_hours: Option<QuietStatus>,
}

#[derive(Clone, Debug, Serialize)]
pub struct QuietStatus {
    pub from: String,
    pub to: String,
    pub timezone: String,
    pub rooms: Vec<String>,
    pub active: bool,
}

/// Room names compare case-insensitively, ignoring surrounding and repeated
/// spaces, and accents (typed or not, in whatever Unicode form): « Petite
/// chambre » is « petite  chambre », « Chambre a coucher » is « Chambre à
/// coucher ». Erring towards a match only ever protects more.
pub(crate) fn same_room(a: &str, b: &str) -> bool {
    // Decompose, then drop the accents: « Chambre a coucher » typed without
    // its accent is still « Chambre à coucher ».
    let norm = |s: &str| {
        s.nfd()
            .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    norm(a) == norm(b)
}

impl QuietHours {
    /// Whether `now` falls in the window. A window may cross midnight
    /// (21:00 → 07:30); `from == to` means all day. Local time follows
    /// daylight saving.
    #[must_use]
    pub fn contains(&self, now: Timestamp) -> bool {
        let local = now.to_zoned(self.tz.clone()).time();
        match self.from.cmp(&self.to) {
            std::cmp::Ordering::Equal => true,
            std::cmp::Ordering::Less => self.from <= local && local < self.to,
            std::cmp::Ordering::Greater => local >= self.from || local < self.to,
        }
    }

    fn covers(&self, rooms: &[&str]) -> bool {
        self.rooms.is_empty()
            || rooms
                .iter()
                .any(|r| self.rooms.iter().any(|q| same_room(q, r)))
    }
}

impl GuardPolicy {
    /// `rooms`: every room the device belongs to (the user's label *and* the
    /// source system's own room). Protection applies if *any* matches, so
    /// relabelling a device cannot take it out of a protected room.
    #[must_use]
    pub fn check(&self, rooms: &[&str], origin: Origin, now: Timestamp) -> Verdict {
        // Humans, the system itself, and automations a human approved.
        if matches!(origin, Origin::Ui | Origin::System | Origin::Automation) {
            return Verdict::Allow;
        }
        if let Some(protected) = self.protected(rooms) {
            return Verdict::Approval(moli_i18n::tr!(
                "serveur.garde.piece_protegee",
                room = protected
            ));
        }
        if self.protect_unassigned && rooms.is_empty() {
            return Verdict::Approval(moli_i18n::tr!("serveur.garde.sans_piece_connue"));
        }
        if let Some(quiet) = &self.quiet_hours
            && quiet.covers(rooms)
            && quiet.contains(now)
        {
            return Verdict::Approval(moli_i18n::tr!(
                "serveur.garde.heures_calmes",
                from = quiet.from.strftime("%H:%M"),
                to = quiet.to.strftime("%H:%M")
            ));
        }
        Verdict::Allow
    }

    /// Whether moving a device currently in `rooms` would let an agent
    /// escape a rule: protected room, unassigned device (when protected),
    /// or a room under room-specific quiet hours. Returns why.
    #[must_use]
    pub fn room_change_needs_human(&self, rooms: &[&str]) -> Option<String> {
        if let Some(room) = self.protected(rooms) {
            return Some(moli_i18n::tr!(
                "serveur.garde.deplacer.piece_protegee",
                room = room
            ));
        }
        if self.protect_unassigned && rooms.is_empty() {
            return Some(moli_i18n::tr!("serveur.garde.deplacer.sans_piece"));
        }
        if let Some(quiet) = &self.quiet_hours
            && !quiet.rooms.is_empty()
            && quiet.covers(rooms)
        {
            return Some(moli_i18n::tr!("serveur.garde.deplacer.heures_calmes"));
        }
        None
    }

    /// The protected room among `rooms`, if any.
    #[must_use]
    pub fn protected(&self, rooms: &[&str]) -> Option<&str> {
        self.protected_rooms
            .iter()
            .find(|p| rooms.iter().any(|r| same_room(p, r)))
            .map(String::as_str)
    }

    #[must_use]
    pub fn status(&self, now: Timestamp) -> GuardStatus {
        GuardStatus {
            protected_rooms: self.protected_rooms.clone(),
            unmatched_protected_rooms: Vec::new(),
            protect_unassigned: self.protect_unassigned,
            dashboard: self.dashboard,
            quiet_hours: self.quiet_hours.as_ref().map(|q| QuietStatus {
                from: q.from.strftime("%H:%M").to_string(),
                to: q.to.strftime("%H:%M").to_string(),
                timezone: q.tz_name.clone(),
                rooms: q.rooms.clone(),
                active: q.contains(now),
            }),
        }
    }
}

impl QuietHours {
    /// Parses `"HH:MM"` bounds and an IANA time zone (`Europe/Paris`).
    pub fn parse(from: &str, to: &str, tz: &str, rooms: Vec<String>) -> Result<Self, String> {
        let time = |s: &str| {
            Time::strptime("%H:%M", s.trim()).map_err(|e| {
                moli_i18n::tr!(
                    "serveur.garde.heure_invalide",
                    heure = format!("{s:?}"),
                    error = e
                )
            })
        };
        Ok(Self {
            from: time(from)?,
            to: time(to)?,
            tz: TimeZone::get(tz).map_err(|e| {
                moli_i18n::tr!(
                    "serveur.garde.fuseau_inconnu",
                    fuseau = format!("{tz:?}"),
                    error = e
                )
            })?,
            tz_name: tz.to_owned(),
            rooms,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rfc3339: &str) -> Timestamp {
        rfc3339.parse().unwrap()
    }

    fn policy() -> GuardPolicy {
        GuardPolicy {
            protected_rooms: vec!["Chambre à coucher".into(), "Petite chambre".into()],
            quiet_hours: Some(QuietHours::parse("21:00", "07:30", "Europe/Paris", vec![]).unwrap()),
            protect_unassigned: false,
            dashboard: Dashboard::Trusted,
        }
    }

    #[test]
    fn humans_are_always_obeyed() {
        let night = at("2026-10-02T23:00:00Z");
        assert_eq!(
            policy().check(&["Petite chambre"], Origin::Ui, night),
            Verdict::Allow
        );
    }

    #[test]
    fn protected_rooms_need_approval_day_and_night() {
        let noon = at("2026-10-03T10:00:00Z");
        let verdict = policy().check(&["petite CHAMBRE"], Origin::Mcp, noon);
        assert!(matches!(verdict, Verdict::Approval(r) if r.contains("protégée")));
        assert_eq!(
            policy().check(&["Salon"], Origin::Mcp, noon),
            Verdict::Allow
        );
    }

    #[test]
    fn approved_automations_carry_a_humans_intent() {
        let night = at("2026-10-02T23:00:00Z");
        assert_eq!(
            policy().check(&["Petite chambre"], Origin::Automation, night),
            Verdict::Allow
        );
    }

    #[test]
    fn the_assistant_is_an_agent_like_any_other() {
        let noon = at("2026-10-03T10:00:00Z");
        let night = at("2026-10-02T23:00:00Z");
        assert!(matches!(
            policy().check(&["Petite chambre"], Origin::Assistant, noon),
            Verdict::Approval(_)
        ));
        assert!(matches!(
            policy().check(&["Salon"], Origin::Assistant, night),
            Verdict::Approval(_)
        ));
    }

    #[test]
    fn quiet_hours_cross_midnight_and_follow_daylight_saving() {
        let p = policy();
        // 23:00 Paris in summer time (UTC+2) = 21:00Z: inside.
        assert!(matches!(
            p.check(&["Salon"], Origin::Api, at("2026-10-02T21:00:00Z")),
            Verdict::Approval(_)
        ));
        // 07:29 Paris = 05:29Z (summer): inside; 07:31 = 05:31Z: outside.
        assert!(matches!(
            p.check(&["Salon"], Origin::Api, at("2026-10-03T05:29:00Z")),
            Verdict::Approval(_)
        ));
        assert_eq!(
            p.check(&["Salon"], Origin::Api, at("2026-10-03T05:31:00Z")),
            Verdict::Allow
        );
        // Winter time (UTC+1): 20:30Z = 21:30 Paris → inside; 19:30Z = 20:30 → outside.
        assert!(matches!(
            p.check(&[], Origin::Cli, at("2026-12-01T20:30:00Z")),
            Verdict::Approval(_)
        ));
        assert_eq!(
            p.check(&[], Origin::Cli, at("2026-12-01T19:30:00Z")),
            Verdict::Allow
        );
    }

    #[test]
    fn quiet_hours_can_target_rooms() {
        let p = GuardPolicy {
            protected_rooms: vec![],
            quiet_hours: Some(
                QuietHours::parse("21:00", "07:30", "Europe/Paris", vec!["Salon".into()]).unwrap(),
            ),
            protect_unassigned: false,
            dashboard: Dashboard::Trusted,
        };
        let night = at("2026-10-02T21:30:00Z");
        assert!(matches!(
            p.check(&["salon"], Origin::Mcp, night),
            Verdict::Approval(_)
        ));
        assert_eq!(p.check(&["Grenier"], Origin::Mcp, night), Verdict::Allow);
        assert_eq!(p.check(&[], Origin::Mcp, night), Verdict::Allow);
    }

    #[test]
    fn relabelling_cannot_escape_protection() {
        let noon = at("2026-10-03T10:00:00Z");
        // Label says "Salon", the source system says "Petite chambre": protected.
        let verdict = policy().check(&["Salon", "Petite  chambre "], Origin::Mcp, noon);
        assert!(matches!(verdict, Verdict::Approval(_)));
    }

    #[test]
    fn unicode_forms_and_unassigned_devices() {
        let noon = at("2026-10-03T10:00:00Z");
        // « à » typed as « a » + combining grave accent (U+0300).
        let decomposed = "Chambre a\u{300} coucher";
        assert!(matches!(
            policy().check(&[decomposed], Origin::Mcp, noon),
            Verdict::Approval(_)
        ));

        let strict = GuardPolicy {
            protect_unassigned: true,
            ..policy()
        };
        assert!(matches!(
            strict.check(&[], Origin::Mcp, noon),
            Verdict::Approval(_)
        ));
        assert_eq!(policy().check(&[], Origin::Mcp, noon), Verdict::Allow);
    }

    #[test]
    fn moving_devices_out_of_any_rule_needs_a_human() {
        assert!(
            policy()
                .room_change_needs_human(&["Petite chambre"])
                .is_some()
        );
        assert!(policy().room_change_needs_human(&["Salon"]).is_none());
        let strict = GuardPolicy {
            protect_unassigned: true,
            ..policy()
        };
        assert!(strict.room_change_needs_human(&[]).is_some());
        let quiet_salon = GuardPolicy {
            protected_rooms: vec![],
            quiet_hours: Some(
                QuietHours::parse("21:00", "07:30", "Europe/Paris", vec!["Salon".into()]).unwrap(),
            ),
            protect_unassigned: false,
            dashboard: Dashboard::Trusted,
        };
        assert!(quiet_salon.room_change_needs_human(&["salon"]).is_some());
        assert!(quiet_salon.room_change_needs_human(&["Grenier"]).is_none());
    }

    #[test]
    fn equal_bounds_mean_all_day() {
        let p = GuardPolicy {
            protected_rooms: vec![],
            quiet_hours: Some(QuietHours::parse("00:00", "00:00", "Europe/Paris", vec![]).unwrap()),
            protect_unassigned: false,
            dashboard: Dashboard::Trusted,
        };
        assert!(matches!(
            p.check(&[], Origin::Mcp, at("2026-10-03T12:00:00Z")),
            Verdict::Approval(_)
        ));
    }

    #[test]
    fn bad_config_is_explained() {
        assert!(QuietHours::parse("25:00", "07:00", "Europe/Paris", vec![]).is_err());
        assert!(QuietHours::parse("21:00", "07:00", "Mars/Olympus", vec![]).is_err());
    }
}
