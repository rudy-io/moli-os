//! `dehors:soleil` (#258): where the sun is over the house, computed here
//! from the house's position with the same formulas as sunrise and sunset
//! ([`moli_core::sun`]). No station, no forecast, no internet: it is there
//! as soon as the house is placed, `[outdoor]` or not.
//!
//! Two read-only points, in degrees, for the automations
//! (`when_threshold`: « when the sun drops below 5° ») and the dashboard:
//! `elevation` (negative at night) and `azimuth` (clockwise from north).
//! Recomputed every minute, published by steps of [`STEP`]: at noon the sun
//! climbs about 0.25° a minute, and the formulas are good to about 0.5°.
//!
//! With `[[sun.facade]]` in `moli.toml`, two more points per façade: its
//! incidence (the angle between the sun and the façade's perpendicular) and
//! whether the sun is on it, with a hysteresis (`enter` / `exit`) so that a
//! shutter does not go to and fro when the sun hesitates at a threshold.

use std::collections::HashSet;
use std::sync::Arc;

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit};
use serde::Deserialize;

/// The device, inside the outdoor instance.
pub const NATIVE: &str = "soleil";
/// Its points.
pub const ELEVATION: &str = "elevation";
pub const AZIMUTH: &str = "azimuth";
/// The step its values are published by (degrees).
pub const STEP: f64 = 0.5;
/// The façades and their names, for the dashboard: one `id=name` per line.
pub const FACADES: &str = "facades";
/// A façade's points: `<id>_incidence`, `<id>_sun`.
pub const INCIDENCE: &str = "_incidence";
pub const ON: &str = "_sun";
/// The sun starts to be on a façade below this incidence (degrees): about
/// half of what it would give straight in front (cos 62° ≈ 0.47).
pub const ENTER: f64 = 62.0;
/// …and is no longer on it above this one (cos 75° ≈ 0.26).
pub const EXIT: f64 = 75.0;

/// `[sun]`, as `moli.toml` declares it: thresholds for every façade, and
/// the façades.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Incidence below which the sun is on a façade (62° by default).
    #[serde(default)]
    pub enter: Option<f64>,
    /// Incidence above which it no longer is (75° by default); above `enter`.
    #[serde(default)]
    pub exit: Option<f64>,
    #[serde(default)]
    pub facade: Vec<FacadeConfig>,
}

/// `[[sun.facade]]`: a wall and the way it looks. Several windows facing the
/// same way share one façade.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FacadeConfig {
    /// Letters, digits and `_`: the start of its points' keys.
    pub id: String,
    /// What the dashboard and the conversation call it (`id` if absent).
    #[serde(default)]
    pub name: Option<String>,
    /// The way the façade looks, clockwise from north (180 = south).
    pub azimuth: f64,
    /// This façade's own thresholds, instead of `[sun]`'s.
    #[serde(default)]
    pub enter: Option<f64>,
    #[serde(default)]
    pub exit: Option<f64>,
    /// Below this elevation the sun does not count (a hedge, a neighbour):
    /// 0° by default.
    #[serde(default)]
    pub min_elevation: Option<f64>,
}

/// A façade, its thresholds resolved.
#[derive(Clone, Debug, PartialEq)]
pub struct Facade {
    pub id: String,
    pub name: String,
    pub azimuth: f64,
    pub enter: f64,
    pub exit: f64,
    pub min_elevation: f64,
}

impl Config {
    /// The façades, with `[sun]`'s thresholds where they do not set theirs.
    #[must_use]
    pub fn facades(&self) -> Vec<Facade> {
        self.facade
            .iter()
            .map(|f| Facade {
                id: f.id.clone(),
                name: f.name.clone().unwrap_or_else(|| f.id.clone()),
                azimuth: f.azimuth,
                enter: f.enter.or(self.enter).unwrap_or(ENTER),
                exit: f.exit.or(self.exit).unwrap_or(EXIT),
                min_elevation: f.min_elevation.unwrap_or(0.0),
            })
            .collect()
    }
}

/// `[sun]` checked: a façade at least, simple and unique ids, angles in
/// their ranges, and `exit` above `enter` (else no hysteresis: the state
/// would go to and fro between the two).
pub fn check(config: &Config) -> Result<(), String> {
    if config.facade.is_empty() {
        return Err("a [[sun.facade]] at least".into());
    }
    let mut seen = HashSet::new();
    for f in &config.facade {
        let simple =
            f.id.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if f.id.is_empty()
            || f.id.len() > 32
            || !simple
            || !f.id.starts_with(|c: char| c.is_ascii_lowercase())
        {
            return Err(format!(
                "facade id {:?}: lower-case letters, digits and _, starting with a letter (32 at most)",
                f.id
            ));
        }
        if !seen.insert(f.id.as_str()) {
            return Err(format!("facade id {:?} is given twice", f.id));
        }
        if f.name.as_deref().is_some_and(|n| n.trim().is_empty()) {
            return Err(format!("facade {:?}: an empty name", f.id));
        }
        if f.name.as_deref().is_some_and(|n| n.contains(['\n', '\r'])) {
            return Err(format!("facade {:?}: a name on one line", f.id));
        }
        if !(0.0..360.0).contains(&f.azimuth) {
            return Err(format!("facade {:?}: azimuth must be in 0..360", f.id));
        }
        if f.min_elevation.is_some_and(|m| !(0.0..90.0).contains(&m)) {
            return Err(format!("facade {:?}: min_elevation must be in 0..90", f.id));
        }
    }
    for f in config.facades() {
        if f.enter <= 0.0 || f.enter >= 90.0 || f.exit <= 0.0 || f.exit > 90.0 {
            return Err(format!(
                "facade {:?}: enter must be in 0..90 and exit in 0..=90 (degrees of incidence)",
                f.id
            ));
        }
        if f.exit <= f.enter {
            return Err(format!(
                "facade {:?}: exit ({}) must be above enter ({}): the sun is on the façade \
                 below enter, and no longer is above exit",
                f.id, f.exit, f.enter
            ));
        }
    }
    Ok(())
}

/// The angle between the sun and the perpendicular of a façade looking
/// towards `azimuth`, in degrees: 0 = straight in front, 90 = grazing,
/// above 90 = behind the façade.
#[must_use]
pub fn incidence(sun: &moli_core::sun::Position, azimuth: f64) -> f64 {
    let c = sun.elevation.to_radians().cos() * (sun.azimuth - azimuth).to_radians().cos();
    c.clamp(-1.0, 1.0).acos().to_degrees()
}

/// Whether the sun is on a façade now, knowing whether it was: up above
/// `min_elevation`, and the incidence below `enter` to start, until it goes
/// above `exit`.
#[must_use]
pub fn on(was: bool, sun: &moli_core::sun::Position, facade: &Facade) -> bool {
    if sun.elevation <= facade.min_elevation {
        return false;
    }
    let i = incidence(sun, facade.azimuth);
    if was {
        i <= facade.exit
    } else {
        i < facade.enter
    }
}

/// Where the house is (latitude, longitude), read at every minute: a
/// position chosen from the conversation moves the sun without a restart.
pub type Place = Arc<dyn Fn() -> Option<(f64, f64)> + Send + Sync>;

/// Where the sun is at `now` (ms since the epoch), in degrees.
#[must_use]
pub fn at(now: u64, latitude: f64, longitude: f64) -> Option<moli_core::sun::Position> {
    let at = jiff::Timestamp::from_millisecond(i64::try_from(now).ok()?).ok()?;
    let utc = at.to_zoned(jiff::tz::TimeZone::UTC);
    let day = u16::try_from(utc.day_of_year()).ok()?;
    let minutes =
        f64::from(utc.hour()) * 60.0 + f64::from(utc.minute()) + f64::from(utc.second()) / 60.0;
    Some(moli_core::sun::position(day, minutes, latitude, longitude))
}

/// A value rounded to [`STEP`]; an azimuth of 360° is 0°.
#[must_use]
pub fn stepped(degrees: f64, azimuth: bool) -> f64 {
    let v = (degrees / STEP).round() * STEP;
    if azimuth && v >= 360.0 { v - 360.0 } else { v }
}

fn degrees(key: &str, label: String, min: f64, max: f64) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Numeric {
            min: Some(min),
            max: Some(max),
            step: Some(STEP),
        },
        access: Access {
            read: true,
            write: false,
        },
        unit: Some(Unit::Degree),
        semantic: Semantic::Other,
    }
}

fn binary(key: &str, label: String) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Binary,
        access: Access {
            read: true,
            write: false,
        },
        unit: None,
        semantic: Semantic::Other,
    }
}

/// The `facades` point's text: one `id=name` per line, in their order.
#[must_use]
pub fn names(facades: &[Facade]) -> String {
    facades
        .iter()
        .map(|f| format!("{}={}", f.id, f.name))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The device: the sun's elevation and azimuth, then each façade's
/// incidence and whether the sun is on it.
#[must_use]
pub fn device(id: DeviceId, instance: &InstanceId, facades: &[Facade]) -> Device {
    let mut points = vec![
        degrees(
            ELEVATION,
            moli_i18n::tr!("pilotes.dehors.soleil.elevation"),
            -90.0,
            90.0,
        ),
        degrees(
            AZIMUTH,
            moli_i18n::tr!("pilotes.dehors.soleil.azimut"),
            0.0,
            359.5,
        ),
    ];
    if !facades.is_empty() {
        points.push(PointSpec {
            key: FACADES.into(),
            label: moli_i18n::tr!("pilotes.dehors.soleil.facades").into(),
            kind: Kind::Text,
            access: Access {
                read: true,
                write: false,
            },
            unit: None,
            semantic: Semantic::Config,
        });
    }
    for f in facades {
        points.push(degrees(
            &format!("{}{INCIDENCE}", f.id),
            moli_i18n::tr!("pilotes.dehors.soleil.incidence", facade = f.name.as_str()),
            0.0,
            180.0,
        ));
        points.push(binary(
            &format!("{}{ON}", f.id),
            moli_i18n::tr!("pilotes.dehors.soleil.sur_facade", facade = f.name.as_str()),
        ));
    }
    Device {
        id,
        instance: instance.clone(),
        native_name: moli_i18n::tr!("pilotes.dehors.soleil.nom").into(),
        manufacturer: Some("Moli".into()),
        // What the dashboard recognizes it by: never translated.
        model: Some("Soleil".into()),
        description: None,
        native_room: None,
        members: Vec::new(),
        points,
    }
}

/// What to publish now, against what was published last: each value that
/// moved by a step, and whether the device must be (re)declared or removed.
#[derive(Debug, Default, PartialEq)]
pub struct Shown {
    /// Where the house was last placed, and the values published there.
    pub at: Option<(f64, f64)>,
    pub elevation: Option<f64>,
    pub azimuth: Option<f64>,
    /// Per façade, in their order: the incidence published, and whether the
    /// sun is on it (also the hysteresis' memory).
    pub facades: Vec<(Option<f64>, Option<bool>)>,
}

/// One minute of the sun: what changes.
#[derive(Debug, PartialEq)]
pub enum Change {
    /// Not placed, and nothing shown: nothing to do.
    Nothing,
    /// No longer placed: the device goes.
    Remove,
    /// The values to publish (`None`: unchanged); `declare`: the device is
    /// new (or the house moved).
    Show {
        declare: bool,
        elevation: Option<f64>,
        azimuth: Option<f64>,
        /// Per façade, in their order: its incidence, whether the sun is on it.
        facades: Vec<(Option<f64>, Option<bool>)>,
    },
}

/// `value`, if it is not what was published (and it now is).
fn fresh<T: PartialEq + Copy>(was: &mut Option<T>, value: T) -> Option<T> {
    (*was != Some(value)).then(|| {
        *was = Some(value);
        value
    })
}

/// What a minute changes, from the house's position, the clock and the
/// façades.
pub fn tick(shown: &mut Shown, place: Option<(f64, f64)>, now: u64, facades: &[Facade]) -> Change {
    let Some((lat, lon)) = place else {
        if shown.at.take().is_some() {
            *shown = Shown::default();
            return Change::Remove;
        }
        return Change::Nothing;
    };
    let Some(sun) = at(now, lat, lon) else {
        return Change::Nothing;
    };
    let declare = shown.at.is_none();
    if declare {
        *shown = Shown {
            at: Some((lat, lon)),
            ..Shown::default()
        };
    }
    shown.at = Some((lat, lon));
    shown.facades.resize(facades.len(), (None, None));
    let facades = facades
        .iter()
        .zip(shown.facades.iter_mut())
        .map(|(f, (incidence_was, on_was))| {
            let now_on = on(on_was.unwrap_or(false), &sun, f);
            (
                fresh(incidence_was, stepped(incidence(&sun, f.azimuth), false)),
                fresh(on_was, now_on),
            )
        })
        .collect();
    Change::Show {
        declare,
        elevation: fresh(&mut shown.elevation, stepped(sun.elevation, false)),
        azimuth: fresh(&mut shown.azimuth, stepped(sun.azimuth, true)),
        facades,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-06-21 12:00 UTC, in ms.
    const SOLSTICE_NOON: u64 = 1_782_043_200_000;
    const PARIS: (f64, f64) = (48.85, 2.35);

    #[test]
    fn values_go_by_half_degrees() {
        assert!((stepped(12.24, false) - 12.0).abs() < f64::EPSILON);
        assert!((stepped(12.26, false) - 12.5).abs() < f64::EPSILON);
        assert!((stepped(-3.3, false) + 3.5).abs() < f64::EPSILON);
        assert!(stepped(359.8, true).abs() < f64::EPSILON, "360° is 0°");
    }

    #[test]
    fn the_clock_is_read_as_utc() {
        // Paris, 21 June, 12:00 UTC (14:00 at the house's clock): just past
        // solar noon (11:52 UTC there), so nearly due south and as high as
        // it gets. Read as local time, it would be two hours further west.
        let sun = at(SOLSTICE_NOON, PARIS.0, PARIS.1).unwrap();
        assert!((sun.elevation - 64.5).abs() < 0.5, "{sun:?}");
        assert!((sun.azimuth - 184.5).abs() < 1.0, "{sun:?}");
    }

    #[test]
    fn a_value_is_published_once_it_moved_by_a_step() {
        // 08:00 UTC: the sun climbs and turns (at noon, its height stalls).
        const MORNING: u64 = SOLSTICE_NOON - 4 * 3_600_000;
        let mut shown = Shown::default();
        assert_eq!(tick(&mut shown, None, MORNING, &[]), Change::Nothing);
        let Change::Show {
            declare: true,
            elevation: Some(_),
            azimuth: Some(_),
            ..
        } = tick(&mut shown, Some(PARIS), MORNING, &[])
        else {
            panic!("placed: declared, both values")
        };
        // Ten seconds later: the sun has not moved by half a degree.
        assert_eq!(
            tick(&mut shown, Some(PARIS), MORNING + 10_000, &[]),
            Change::Show {
                declare: false,
                elevation: None,
                azimuth: None,
                facades: vec![],
            }
        );
        // Ten minutes later it has.
        let Change::Show {
            declare: false,
            elevation: Some(_),
            azimuth: Some(_),
            ..
        } = tick(&mut shown, Some(PARIS), MORNING + 600_000, &[])
        else {
            panic!("ten minutes: both moved")
        };
        // No longer placed: the device goes, once.
        assert_eq!(tick(&mut shown, None, MORNING, &[]), Change::Remove);
        assert_eq!(tick(&mut shown, None, MORNING, &[]), Change::Nothing);
    }

    #[test]
    fn the_house_moved_everything_is_published_again() {
        let mut shown = Shown::default();
        let _ = tick(&mut shown, Some(PARIS), SOLSTICE_NOON, &[]);
        let Change::Show {
            elevation: Some(_),
            azimuth: Some(_),
            ..
        } = tick(&mut shown, Some((-33.87, 151.21)), SOLSTICE_NOON, &[])
        else {
            panic!("another sky")
        };
    }

    #[test]
    fn the_device_is_read_only_degrees() {
        let d = device(
            DeviceId::from("dehors:soleil"),
            &InstanceId::from("dehors"),
            &[],
        );
        assert_eq!(d.points.len(), 2);
        for p in &d.points {
            assert!(p.access.read && !p.access.write);
            assert_eq!(p.unit, Some(Unit::Degree));
            assert_eq!(p.semantic, Semantic::Other);
        }
    }

    fn facade(id: &str, azimuth: f64) -> Facade {
        Facade {
            id: id.into(),
            name: id.into(),
            azimuth,
            enter: ENTER,
            exit: EXIT,
            min_elevation: 0.0,
        }
    }

    /// The sun `elevation` high, turned so that a façade facing south sees
    /// it at `incidence`.
    fn sun_at(incidence_deg: f64, elevation: f64) -> moli_core::sun::Position {
        let turn = (incidence_deg.to_radians().cos() / elevation.to_radians().cos())
            .clamp(-1.0, 1.0)
            .acos()
            .to_degrees();
        moli_core::sun::Position {
            elevation,
            azimuth: 180.0 + turn,
        }
    }

    #[test]
    fn the_incidence_is_the_angle_with_the_facades_perpendicular() {
        let at = |elevation, azimuth| moli_core::sun::Position { elevation, azimuth };
        assert!(
            incidence(&at(0.0, 180.0), 180.0).abs() < 1e-9,
            "straight in front"
        );
        assert!(
            (incidence(&at(0.0, 270.0), 180.0) - 90.0).abs() < 1e-9,
            "grazing"
        );
        assert!(
            (incidence(&at(60.0, 180.0), 180.0) - 60.0).abs() < 1e-9,
            "high in front"
        );
        assert!(
            (incidence(&at(0.0, 0.0), 180.0) - 180.0).abs() < 1e-9,
            "behind"
        );
        // The azimuth difference goes round: a façade facing north (0°)
        // sees a sun at 350° at 10°.
        assert!((incidence(&at(0.0, 350.0), 0.0) - 10.0).abs() < 1e-9);
        assert!((incidence(&sun_at(70.0, 20.0), 180.0) - 70.0).abs() < 1e-6);
    }

    #[test]
    fn the_sun_comes_in_below_enter_and_leaves_above_exit() {
        let south = facade("sud", 180.0);
        // Between the two thresholds, the state stays what it was.
        assert!(
            !on(false, &sun_at(70.0, 20.0), &south),
            "not yet: 70° > enter"
        );
        assert!(on(true, &sun_at(70.0, 20.0), &south), "still: 70° < exit");
        assert!(on(false, &sun_at(61.0, 20.0), &south), "in below enter");
        assert!(
            on(true, &sun_at(74.9, 20.0), &south),
            "just under exit: still"
        );
        assert!(!on(true, &sun_at(76.0, 20.0), &south), "out above exit");
        // Behind the façade, or down: never.
        assert!(!on(true, &sun_at(120.0, 20.0), &south));
        let at = moli_core::sun::Position {
            elevation: -1.0,
            azimuth: 180.0,
        };
        assert!(!on(true, &at, &south), "set");
        // A hedge: the low sun does not count, even straight in front.
        let hedged = Facade {
            min_elevation: 8.0,
            ..south
        };
        let low = moli_core::sun::Position {
            elevation: 6.0,
            azimuth: 180.0,
        };
        assert!(!on(false, &low, &hedged));
        assert!(on(false, &sun_at(30.0, 9.0), &hedged));
    }

    #[test]
    fn over_a_day_the_sun_comes_on_a_facade_once_and_leaves_once() {
        // Paris, 21 June, minute by minute: a façade facing south-west and
        // one facing east. Each state changes twice at most: no to and fro.
        const DAY: u64 = SOLSTICE_NOON - 12 * 3_600_000;
        let facades = [facade("sud_ouest", 205.0), facade("est", 113.0)];
        let mut shown = Shown::default();
        let mut changes: Vec<Vec<(u64, bool)>> = vec![Vec::new(); 2];
        for minute in 0..24 * 60 {
            let now = DAY + minute * 60_000;
            if let Change::Show { facades: f, .. } = tick(&mut shown, Some(PARIS), now, &facades) {
                for (i, (_, on)) in f.into_iter().enumerate() {
                    if let Some(on) = on {
                        changes[i].push((minute, on));
                    }
                }
            }
        }
        for (i, c) in changes.iter().enumerate() {
            // The first value (off, at midnight UTC), then on, then off.
            let states: Vec<bool> = c.iter().map(|(_, on)| *on).collect();
            assert_eq!(states, [false, true, false], "{}: {c:?}", facades[i].id);
        }
        // The east façade gets the morning, the south-west one the afternoon
        // (UTC: the house's clock is two hours ahead).
        let (east_on, east_off) = (changes[1][1].0, changes[1][2].0);
        let (sw_on, sw_off) = (changes[0][1].0, changes[0][2].0);
        assert!(
            east_on < 6 * 60 && east_off < 12 * 60,
            "east {east_on}..{east_off}"
        );
        assert!(
            sw_on > 9 * 60 && sw_off > 15 * 60,
            "south-west {sw_on}..{sw_off}"
        );
    }

    #[test]
    fn a_facade_adds_its_incidence_and_whether_the_sun_is_on_it() {
        let d = device(
            DeviceId::from("dehors:soleil"),
            &InstanceId::from("dehors"),
            &[facade("sud", 180.0), facade("est", 90.0)],
        );
        let keys: Vec<&str> = d.points.iter().map(|p| &*p.key).collect();
        assert_eq!(
            keys,
            [
                "elevation",
                "azimuth",
                "facades",
                "sud_incidence",
                "sud_sun",
                "est_incidence",
                "est_sun"
            ]
        );
        for p in &d.points {
            assert!(p.access.read && !p.access.write, "{}", p.key);
        }
        assert_eq!(d.points[2].semantic, Semantic::Config);
        assert_eq!(d.points[3].unit, Some(Unit::Degree));
        assert_eq!(d.points[4].kind, Kind::Binary);
        assert_eq!(
            names(&[facade("sud", 180.0), facade("est", 90.0)]),
            "sud=sud\nest=est"
        );
        // Without façades, no `facades` point.
        let bare = device(
            DeviceId::from("dehors:soleil"),
            &InstanceId::from("dehors"),
            &[],
        );
        assert!(bare.point("facades").is_none());
    }

    fn config(text: &str) -> Config {
        #[derive(Deserialize)]
        struct File {
            sun: Config,
        }
        toml::from_str::<File>(text).unwrap().sun
    }

    #[test]
    fn the_thresholds_go_from_sun_to_each_facade() {
        let c = config(
            r#"
            [sun]
            exit = 78

            [[sun.facade]]
            id = "sud"
            name = "Façade sud"
            azimuth = 180

            [[sun.facade]]
            id = "ouest"
            azimuth = 270
            enter = 55
            min_elevation = 8
            "#,
        );
        assert_eq!(check(&c), Ok(()));
        let f = c.facades();
        assert_eq!(
            (f[0].name.as_str(), f[0].enter, f[0].exit),
            ("Façade sud", ENTER, 78.0)
        );
        assert_eq!(
            (f[1].name.as_str(), f[1].enter, f[1].exit),
            ("ouest", 55.0, 78.0)
        );
        assert!((f[1].min_elevation - 8.0).abs() < f64::EPSILON);
    }

    #[test]
    fn an_incoherent_sun_is_refused() {
        let refused = |text: &str| check(&config(text)).unwrap_err();
        assert!(refused("[sun]\n").contains("at least"));
        let one = |rest: &str| format!("[sun]\n[[sun.facade]]\n{rest}\n");
        assert!(
            refused(&one("id = \"sud\"\nazimuth = 180\nenter = 75\nexit = 62"))
                .contains("above enter")
        );
        assert!(refused(&one("id = \"sud\"\nazimuth = 180\nexit = 62")).contains("above enter"));
        assert!(refused(&one("id = \"Sud\"\nazimuth = 180")).contains("lower-case"));
        assert!(refused(&one("id = \"sud-est\"\nazimuth = 135")).contains("lower-case"));
        assert!(refused(&one("id = \"sud\"\nazimuth = 360")).contains("azimuth"));
        assert!(refused(&one("id = \"sud\"\nazimuth = 180\nenter = 95")).contains("enter"));
        assert!(
            refused(&one("id = \"sud\"\nazimuth = 180\nmin_elevation = 90"))
                .contains("min_elevation")
        );
        assert!(refused(&one("id = \"sud\"\nazimuth = 180\nname = \" \"")).contains("empty"));
        assert!(
            refused(&one("id = \"sud\"\nazimuth = 180\nname = \"a\\nb\"")).contains("one line")
        );
        let twice = "[sun]\n[[sun.facade]]\nid = \"sud\"\nazimuth = 180\n[[sun.facade]]\nid = \"sud\"\nazimuth = 200\n";
        assert!(refused(twice).contains("twice"));
    }
}
