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

use std::sync::Arc;

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit};

/// The device, inside the outdoor instance.
pub const NATIVE: &str = "soleil";
/// Its points.
pub const ELEVATION: &str = "elevation";
pub const AZIMUTH: &str = "azimuth";
/// The step its values are published by (degrees).
pub const STEP: f64 = 0.5;

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

/// The device: the sun's elevation and azimuth.
#[must_use]
pub fn device(id: DeviceId, instance: &InstanceId) -> Device {
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
        points: vec![
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
        ],
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
    },
}

/// What a minute changes, from the house's position and the clock.
pub fn tick(shown: &mut Shown, place: Option<(f64, f64)>, now: u64) -> Change {
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
    let fresh = |was: &mut Option<f64>, value: f64| {
        (*was != Some(value)).then(|| {
            *was = Some(value);
            value
        })
    };
    Change::Show {
        declare,
        elevation: fresh(&mut shown.elevation, stepped(sun.elevation, false)),
        azimuth: fresh(&mut shown.azimuth, stepped(sun.azimuth, true)),
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
        assert_eq!(tick(&mut shown, None, MORNING), Change::Nothing);
        let Change::Show {
            declare: true,
            elevation: Some(_),
            azimuth: Some(_),
        } = tick(&mut shown, Some(PARIS), MORNING)
        else {
            panic!("placed: declared, both values")
        };
        // Ten seconds later: the sun has not moved by half a degree.
        assert_eq!(
            tick(&mut shown, Some(PARIS), MORNING + 10_000),
            Change::Show {
                declare: false,
                elevation: None,
                azimuth: None
            }
        );
        // Ten minutes later it has.
        let Change::Show {
            declare: false,
            elevation: Some(_),
            azimuth: Some(_),
        } = tick(&mut shown, Some(PARIS), MORNING + 600_000)
        else {
            panic!("ten minutes: both moved")
        };
        // No longer placed: the device goes, once.
        assert_eq!(tick(&mut shown, None, MORNING), Change::Remove);
        assert_eq!(tick(&mut shown, None, MORNING), Change::Nothing);
    }

    #[test]
    fn the_house_moved_everything_is_published_again() {
        let mut shown = Shown::default();
        let _ = tick(&mut shown, Some(PARIS), SOLSTICE_NOON);
        let Change::Show {
            elevation: Some(_),
            azimuth: Some(_),
            ..
        } = tick(&mut shown, Some((-33.87, 151.21)), SOLSTICE_NOON)
        else {
            panic!("another sky")
        };
    }

    #[test]
    fn the_device_is_read_only_degrees() {
        let d = device(DeviceId::from("dehors:soleil"), &InstanceId::from("dehors"));
        assert_eq!(d.points.len(), 2);
        for p in &d.points {
            assert!(p.access.read && !p.access.write);
            assert_eq!(p.unit, Some(Unit::Degree));
            assert_eq!(p.semantic, Semantic::Other);
        }
    }
}
