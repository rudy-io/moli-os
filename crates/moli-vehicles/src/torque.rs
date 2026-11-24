//! What Torque Pro sends with « Upload to web server »: one request per
//! logging interval, `?v=…&session=<ms>&id=<hash>&time=<ms>&eml=…&k<pid>=<value>…`,
//! and, at the start of a session, the names and units of what it logs
//! (`userFullName<key>`, `userShortName<key>`, `userUnit<key>`,
//! `defaultUnit<key>`). It wants `OK!` back, or it keeps the data.
//!
//! Keys: `k` + the OBD-II mode 01 PID in hex without its leading zero (`kd`
//! = speed, `kc` = rpm, `k5` = coolant), `kff…` for what Torque computes
//! itself (GPS, adapter voltage, trip). A custom PID keeps its own hex.
//!
//! One more parameter, ours: `dtc=P0401,P2002` (the fault codes, from a
//! companion app; empty = none). Torque itself never uploads them.

use std::collections::BTreeMap;

use anyhow::ensure;

/// Parameters read at most from one request.
pub const MAX_PAIRS: usize = 400;
/// A request's query or body, at most.
pub const MAX_LEN: usize = 32 * 1024;
const MAX_TEXT: usize = 60;
const MAX_CODES: usize = 32;

/// `a=1&b=x%20y+z` → pairs, percent-decoded (`+` is a space). Bounded; a
/// pair that does not decode is dropped.
#[must_use]
pub fn parse_pairs(raw: &str) -> Vec<(String, String)> {
    raw.split('&')
        .filter(|p| !p.is_empty())
        .take(MAX_PAIRS)
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            Some((decode(k)?, decode(v)?))
        })
        .collect()
}

fn decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = s.get(i + 1..i + 3)?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

/// One upload, read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sample {
    /// Torque's session (a trip): changes when logging starts again.
    pub session: Option<String>,
    /// Values by key (`kd`, `kff1006`…), lower case.
    pub values: BTreeMap<String, f64>,
    /// What Torque calls each key (`userFullName…`, else `userShortName…`).
    pub names: BTreeMap<String, String>,
    /// Each key's unit as Torque shows it (`userUnit…`, else `defaultUnit…`).
    pub units: BTreeMap<String, String>,
    /// The fault codes, when the request carries them (`dtc`).
    pub dtc: Option<Vec<String>>,
}

impl Sample {
    /// Nothing worth a look (a test of the settings page, an empty upload).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty() && self.names.is_empty() && self.dtc.is_none()
    }
}

fn is_key(k: &str) -> bool {
    k.len() >= 2
        && k.len() <= 12
        && k.starts_with('k')
        && k[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

fn text_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().count() <= MAX_TEXT && !s.chars().any(char::is_control)
}

/// `P0401` and the like: a letter (powertrain, chassis, body, network) and
/// four hex digits.
fn is_code(c: &str) -> bool {
    c.len() == 5
        && matches!(c.as_bytes()[0], b'P' | b'C' | b'B' | b'U')
        && c[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Reads an upload. Unknown parameters are ignored (Torque adds some over
/// the years); the e-mail and the device hash are never kept.
pub fn read(pairs: &[(String, String)]) -> anyhow::Result<Sample> {
    let mut sample = Sample::default();
    let mut short = BTreeMap::new();
    let mut default_units = BTreeMap::new();
    for (name, value) in pairs {
        let value = value.trim();
        if name == "session" {
            ensure!(
                text_ok(value) && value.bytes().all(|b| b.is_ascii_alphanumeric()),
                "session: an identifier"
            );
            sample.session = Some(value.to_owned());
        } else if name == "dtc" {
            let codes: Vec<String> = value
                .split([',', ' ', ';'])
                .filter(|c| !c.is_empty())
                .map(str::to_ascii_uppercase)
                .collect();
            ensure!(codes.len() <= MAX_CODES, "dtc: {MAX_CODES} codes at most");
            ensure!(codes.iter().all(|c| is_code(c)), "dtc: codes like P0401");
            sample.dtc = Some(codes);
        } else if is_key(name) {
            // « - » or an empty value: the PID has no reading yet.
            if let Ok(v) = value.parse::<f64>()
                && v.is_finite()
            {
                sample.values.insert(name.to_ascii_lowercase(), v);
            }
        } else {
            let label = [
                ("userFullName", &mut sample.names),
                ("userShortName", &mut short),
                ("userUnit", &mut sample.units),
                ("defaultUnit", &mut default_units),
            ]
            .into_iter()
            .find_map(|(prefix, map)| name.strip_prefix(prefix).map(|key| (key, map)));
            if let Some((key, map)) = label
                && is_key(key)
                && text_ok(value)
            {
                map.insert(key.to_ascii_lowercase(), value.to_owned());
            }
        }
    }
    for (key, name) in short {
        sample.names.entry(key).or_insert(name);
    }
    for (key, unit) in default_units {
        sample.units.entry(key).or_insert(unit);
    }
    Ok(sample)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(q: &str) -> Vec<(String, String)> {
        parse_pairs(q)
    }

    #[test]
    fn a_drive_upload_is_read() {
        let s = read(&pairs(
            "eml=user%40example.com&v=9&session=1732450000000&id=0123abcd&time=1732450012345\
             &kff1006=48.85&kff1005=2.35&kd=52&kc=1850.5&k5=88&kff1238=14.1&k2f=-",
        ))
        .unwrap();
        assert_eq!(s.session.as_deref(), Some("1732450000000"));
        assert_eq!(s.values["kd"], 52.0);
        assert_eq!(s.values["kc"], 1850.5);
        assert_eq!(s.values["kff1006"], 48.85);
        // No reading yet: no value, not zero.
        assert!(!s.values.contains_key("k2f"));
        assert_eq!(s.dtc, None);
    }

    #[test]
    fn the_start_of_a_session_names_its_keys() {
        let s = read(&pairs(
            "session=1&userShortNamekd=Speed&userFullNamekd=Vitesse+OBD&defaultUnitkd=km%2Fh\
             &userShortNamekc=Revs&defaultUnitkc=rpm&userUnitkc=tr%2Fmin",
        ))
        .unwrap();
        assert_eq!(s.names["kd"], "Vitesse OBD");
        assert_eq!(s.names["kc"], "Revs");
        assert_eq!(s.units["kd"], "km/h");
        assert_eq!(s.units["kc"], "tr/min");
        assert!(s.values.is_empty());
        assert!(!s.is_empty());
    }

    #[test]
    fn fault_codes_are_read_and_checked() {
        let s = read(&pairs("dtc=p0401,P2002")).unwrap();
        assert_eq!(s.dtc, Some(vec!["P0401".into(), "P2002".into()]));
        assert_eq!(read(&pairs("dtc=")).unwrap().dtc, Some(vec![]));
        assert!(read(&pairs("dtc=hello")).is_err());
        assert!(read(&pairs("dtc=P04")).is_err());
    }

    #[test]
    fn junk_is_bounded_or_ignored() {
        assert!(read(&pairs("session=a%0Ab")).is_err());
        let s = read(&pairs("kd=nan&kc=inf&kzz=3&k=4&userFullNamekd=%0A")).unwrap();
        assert!(s.is_empty());
        let many = "kd=1&".repeat(MAX_PAIRS * 2);
        assert_eq!(parse_pairs(&many).len(), MAX_PAIRS);
        // A broken escape drops the pair, not the request.
        assert_eq!(parse_pairs("a=%zz&b=2"), vec![("b".into(), "2".into())]);
    }
}
