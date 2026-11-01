//! What the IzyPower cloud answers, read as Moli devices (pure, no I/O).
//!
//! An account holds stations (« centrales »); a station holds
//! micro-inverters. The station's info gives its power and its production
//! (day, month, year, lifetime); the device page gives each micro-inverter
//! (online, Wi-Fi signal, production of the day); the day's power curve
//! (`layoutPower`, a point every 3 minutes) gives each PV input's power.
//!
//! Times: `lastUpdate` is the server's clock (UTC+8); `lastUpdateStr` says
//! its own offset (`2026-01-01 13:14:45 UTC+01:00`) and is preferred.

use std::collections::HashMap;

use moli_core::{Access, Device, DeviceId, InstanceId, Kind, PointSpec, Semantic, Unit, Value};
use serde_json::Value as Json;

/// A station not updated for this long is producing nothing (night, power
/// cut, inverters off): its last power is not repeated as if current.
pub const STALE_SECS: i64 = 20 * 60;

/// The cloud's own clock.
const SERVER_OFFSET_SECS: i64 = 8 * 3600;

#[derive(Clone, Debug, PartialEq)]
pub struct Station {
    pub id: u64,
    pub name: String,
    pub power: Option<f64>,
    pub today: Option<f64>,
    pub month: Option<f64>,
    pub year: Option<f64>,
    pub total: Option<f64>,
    pub capacity: Option<f64>,
    pub online: bool,
    /// When the cloud last heard from it (Unix seconds).
    pub updated: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Inverter {
    /// Serial number, as the cloud writes it (upper case).
    pub sn: String,
    pub model: Option<String>,
    pub inputs: u32,
    pub online: bool,
    pub rssi: Option<f64>,
    pub today: Option<f64>,
    pub rated: Option<f64>,
}

fn number(v: &Json) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        .filter(|n| n.is_finite())
}

/// `1.75KWh`, `1000W` → the number.
fn leading_number(text: &str) -> Option<f64> {
    let end = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .unwrap_or(text.len());
    text[..end].parse().ok().filter(|n: &f64| n.is_finite())
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `YYYY-MM-DD HH:MM:SS` read as if at UTC.
fn civil_seconds(text: &str) -> Option<i64> {
    let (date, time) = text.trim().split_once(' ')?;
    let mut ymd = date.split('-').map(|p| p.parse::<i64>().ok());
    let (year, month, day) = (ymd.next()??, ymd.next()??, ymd.next()??);
    let mut hms = time.split(':').map(|p| p.parse::<i64>().ok());
    let (hour, minute, second) = (hms.next()??, hms.next()??, hms.next()??);
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// When the station was last heard from, in Unix seconds.
fn updated_at(info: &Json) -> Option<i64> {
    if let Some(text) = info["lastUpdateStr"].as_str()
        && let Some((civil, zone)) = text.rsplit_once(" UTC")
        && let Some(local) = civil_seconds(civil)
    {
        let sign = if zone.starts_with('-') { -1 } else { 1 };
        let (h, m) = zone
            .trim_start_matches(['+', '-'])
            .split_once(':')
            .unwrap_or((zone.trim_start_matches(['+', '-']), "0"));
        if let (Ok(h), Ok(m)) = (h.parse::<i64>(), m.parse::<i64>()) {
            return Some(local - sign * (h * 3600 + m * 60));
        }
    }
    civil_seconds(info["lastUpdate"].as_str()?).map(|s| s - SERVER_OFFSET_SECS)
}

impl Station {
    /// From `/v3/powerStations/info/<id>` (`data`).
    #[must_use]
    pub fn read(info: &Json) -> Option<Self> {
        Some(Self {
            id: info["stationsId"].as_u64()?,
            name: info["stationName"]
                .as_str()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or("IzyPower")
                .to_owned(),
            power: number(&info["power"]),
            today: number(&info["todaySum"]),
            month: number(&info["monthSum"]),
            year: number(&info["yearSum"]),
            total: number(&info["allSum"]),
            capacity: number(&info["installedCapacity"]),
            online: info["onlineState"].as_i64() == Some(1),
            updated: updated_at(info),
        })
    }

    /// Heard from lately (at `now`, Unix seconds).
    #[must_use]
    pub fn fresh(&self, now: i64) -> bool {
        self.updated.is_some_and(|at| now - at <= STALE_SECS)
    }

    #[must_use]
    pub fn native(&self) -> String {
        format!("station_{}", self.id)
    }
}

impl Inverter {
    /// From a record of `/device/page` (`data.records[]`).
    #[must_use]
    pub fn read(record: &Json) -> Option<Self> {
        let sn = record["sn"].as_str().filter(|s| !s.is_empty())?.to_owned();
        let dto = |key: &str| {
            record["dataDtos"]
                .as_array()?
                .iter()
                .find(|d| d["key"] == key)?["value"]
                .as_str()
                .and_then(leading_number)
        };
        Some(Self {
            sn,
            model: record["device"].as_str().map(str::to_owned),
            inputs: record["pvNum"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0),
            online: record["online"].as_bool().unwrap_or(false),
            rssi: number(&record["rssi"]),
            today: dto("dayEnergy"),
            rated: dto("rate power"),
        })
    }

    #[must_use]
    pub fn native(&self) -> String {
        self.sn.to_ascii_lowercase()
    }
}

/// The micro-inverters of a device page.
#[must_use]
pub fn inverters(page: &Json) -> Vec<Inverter> {
    page["records"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["deviceType"].as_str().is_none_or(|t| t == "vm"))
        .filter_map(Inverter::read)
        .collect()
}

/// Each input's power at the curve's last point: `(sn, input) → W`.
#[must_use]
pub fn latest_inputs(layout: &Json) -> HashMap<(String, u32), f64> {
    let last = layout["data"].as_array().and_then(|points| points.last());
    last.and_then(|p| p["extra"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let sn = e["deviceSn"].as_str()?.to_owned();
            let input = e["pv"].as_str()?.strip_prefix("pv")?.parse().ok()?;
            Some(((sn, input), number(&e["dataVal"])?))
        })
        .collect()
}

fn spec(key: &str, label: String, unit: Option<Unit>, semantic: Semantic) -> PointSpec {
    PointSpec {
        key: key.into(),
        label: label.into(),
        kind: Kind::Numeric {
            min: None,
            max: None,
            step: None,
        },
        access: Access {
            read: true,
            write: false,
        },
        unit,
        semantic,
    }
}

/// The station and its micro-inverters, as Moli devices.
#[must_use]
pub fn devices(
    station: &Station,
    inverters: &[Inverter],
    instance: &InstanceId,
    id: impl Fn(&str) -> DeviceId,
) -> Vec<Device> {
    let kwh = Some(Unit::KiloWattHour);
    let mut out = vec![Device {
        id: id(&station.native()),
        instance: instance.clone(),
        native_name: station.name.as_str().into(),
        manufacturer: Some("IzyPower".into()),
        model: None,
        description: None,
        native_room: None,
        members: Vec::new(),
        points: vec![
            spec(
                "power",
                moli_i18n::tr!("pilotes.izypower.puissance"),
                Some(Unit::Watt),
                Semantic::Power,
            ),
            // Day, month, year start again from zero: summaries, not meters.
            spec(
                "today_kwh",
                moli_i18n::tr!("pilotes.izypower.production_jour"),
                kwh.clone(),
                Semantic::Other,
            ),
            spec(
                "month_kwh",
                moli_i18n::tr!("pilotes.izypower.production_mois"),
                kwh.clone(),
                Semantic::Other,
            ),
            spec(
                "year_kwh",
                moli_i18n::tr!("pilotes.izypower.production_annee"),
                kwh.clone(),
                Semantic::Other,
            ),
            spec(
                "energy",
                moli_i18n::tr!("pilotes.izypower.production_totale"),
                kwh.clone(),
                Semantic::Energy,
            ),
            spec(
                "capacity",
                moli_i18n::tr!("pilotes.izypower.puissance_installee"),
                Some(Unit::Watt),
                Semantic::Other,
            ),
        ],
    }];
    for inverter in inverters {
        let mut points = vec![
            spec(
                "power",
                moli_i18n::tr!("pilotes.izypower.puissance"),
                Some(Unit::Watt),
                Semantic::Power,
            ),
            spec(
                "today_kwh",
                moli_i18n::tr!("pilotes.izypower.production_jour"),
                kwh.clone(),
                Semantic::Other,
            ),
            spec(
                "rssi",
                moli_i18n::tr!("pilotes.izypower.signal"),
                Unit::parse("dBm"),
                Semantic::SignalStrength,
            ),
            spec(
                "rated_power",
                moli_i18n::tr!("pilotes.izypower.puissance_nominale"),
                Some(Unit::Watt),
                Semantic::Other,
            ),
        ];
        for n in 1..=inverter.inputs {
            points.push(spec(
                &format!("pv{n}_power"),
                moli_i18n::tr!("pilotes.izypower.puissance_entree", n = n),
                Some(Unit::Watt),
                Semantic::Power,
            ));
        }
        out.push(Device {
            id: id(&inverter.native()),
            instance: instance.clone(),
            native_name: moli_i18n::tr!("pilotes.izypower.micro_onduleur", sn = inverter.sn).into(),
            manufacturer: Some("IzyPower".into()),
            model: inverter.model.as_deref().map(Into::into),
            description: None,
            native_room: None,
            members: Vec::new(),
            points,
        });
    }
    out
}

/// Every value, as `(native device, key, value)`, at `now` (Unix seconds).
/// A station the cloud has not heard from lately produces nothing: its
/// power, and its inputs', read 0.
#[must_use]
pub fn readings(
    station: &Station,
    inverters: &[Inverter],
    inputs: &HashMap<(String, u32), f64>,
    now: i64,
) -> Vec<(String, String, Value)> {
    let fresh = station.online && station.fresh(now);
    let mut out = Vec::new();
    let mut put = |native: &str, key: &str, value: Option<f64>| {
        if let Some(v) = value {
            out.push((native.to_owned(), key.to_owned(), Value::Float(v)));
        }
    };
    let main = station.native();
    put(
        &main,
        "power",
        Some(if fresh {
            station.power.unwrap_or(0.0)
        } else {
            0.0
        }),
    );
    put(&main, "today_kwh", station.today);
    put(&main, "month_kwh", station.month);
    put(&main, "year_kwh", station.year);
    put(&main, "energy", station.total);
    put(&main, "capacity", station.capacity);
    for inverter in inverters {
        let native = inverter.native();
        let live = fresh && inverter.online;
        let mut sum = 0.0;
        for n in 1..=inverter.inputs {
            let w = if live {
                inputs
                    .get(&(inverter.sn.clone(), n))
                    .copied()
                    .unwrap_or(0.0)
            } else {
                0.0
            };
            sum += w;
            put(&native, &format!("pv{n}_power"), Some(w));
        }
        put(&native, "power", Some((sum * 10.0).round() / 10.0));
        put(&native, "today_kwh", inverter.today);
        put(&native, "rssi", inverter.rssi);
        put(&native, "rated_power", inverter.rated);
    }
    out
}

/// The `exp` (Unix seconds) of a JWT, unchecked: only to know when to log in
/// again.
#[must_use]
pub fn token_expiry(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let (mut bytes, mut buf, mut bits) = (Vec::new(), 0u32, 0);
    for c in payload.bytes().filter(|&c| c != b'=') {
        let v = table.iter().position(|&t| t == c)?;
        buf = (buf << 6) | u32::try_from(v).ok()?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push(u8::try_from((buf >> bits) & 0xFF).ok()?);
        }
    }
    serde_json::from_slice::<Json>(&bytes).ok()?["exp"].as_i64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Json {
        let path = format!(
            "{}/../../integrations/izypower/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn all() -> (Station, Vec<Inverter>, HashMap<(String, u32), f64>) {
        let station = Station::read(&fixture("station-info.json")["data"]).unwrap();
        let inv = inverters(&fixture("devices.json")["data"]);
        let inputs = latest_inputs(&fixture("layout-power.json")["data"]);
        (station, inv, inputs)
    }

    fn get(r: &[(String, String, Value)], native: &str, key: &str) -> Value {
        r.iter()
            .find(|(n, k, _)| n == native && k == key)
            .map_or_else(|| panic!("{native}/{key}"), |(_, _, v)| v.clone())
    }

    #[test]
    fn the_last_update_is_read_with_its_offset() {
        let (station, ..) = all();
        // 13:14:45 at UTC+01:00 = 12:14:45 UTC; the same as 20:14:45 at UTC+8.
        let utc = civil_seconds("2026-10-26 12:14:45").unwrap();
        assert_eq!(station.updated, Some(utc));
        assert_eq!(
            updated_at(&serde_json::json!({"lastUpdate": "2026-10-26 20:14:45"})),
            Some(utc)
        );
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_seconds("2000-03-01 00:00:00"), Some(951_868_800));
    }

    #[test]
    fn a_station_and_its_micro_inverters() {
        let (station, inv, inputs) = all();
        assert_eq!(station.id, 10001);
        assert_eq!(inv.len(), 2);
        assert_eq!(inv[0].inputs, 2);
        assert_eq!(inv[0].today, Some(1.75));
        assert_eq!(inv[0].rated, Some(1000.0));
        assert_eq!(inv[1].inputs, 4);
        let id = InstanceId::from("izy");
        let devices = devices(&station, &inv, &id, |n| DeviceId::new(&id, n));
        assert_eq!(devices.len(), 3);
        assert_eq!(
            devices[0].point("energy").unwrap().semantic,
            Semantic::Energy
        );
        assert_eq!(
            devices[0].point("today_kwh").unwrap().semantic,
            Semantic::Other
        );
        assert!(devices[2].point("pv4_power").is_some());
        assert!(devices[1].point("pv3_power").is_none());
        assert!(
            devices
                .iter()
                .all(|d| d.points.iter().all(|p| !p.access.write))
        );
        assert_eq!(inputs.len(), 6);
    }

    #[test]
    fn fresh_values_are_read_and_stale_ones_are_zero() {
        let (station, inv, inputs) = all();
        let now = station.updated.unwrap() + 120;
        let r = readings(&station, &inv, &inputs, now);
        assert_eq!(get(&r, "station_10001", "power"), Value::Float(1821.0));
        assert_eq!(get(&r, "station_10001", "energy"), Value::Float(2130.8));
        assert_eq!(get(&r, "station_10001", "today_kwh"), Value::Float(5.6));
        assert_eq!(get(&r, "000000000001", "pv1_power"), Value::Float(318.6));
        assert_eq!(get(&r, "000000000001", "power"), Value::Float(639.9));
        assert_eq!(get(&r, "000000000002", "power"), Value::Float(1181.3));
        assert_eq!(get(&r, "000000000002", "rssi"), Value::Float(-90.0));
        // An hour later without news: night, nothing produced; totals kept.
        let r = readings(&station, &inv, &inputs, now + 3600);
        assert_eq!(get(&r, "station_10001", "power"), Value::Float(0.0));
        assert_eq!(get(&r, "000000000002", "pv3_power"), Value::Float(0.0));
        assert_eq!(get(&r, "station_10001", "energy"), Value::Float(2130.8));
    }

    #[test]
    fn a_jwt_says_when_it_ends() {
        // {"alg":"none"} . {"exp":1800000000}
        let token = "eyJhbGciOiJub25lIn0.eyJleHAiOjE4MDAwMDAwMDB9.";
        assert_eq!(token_expiry(token), Some(1_800_000_000));
        assert_eq!(token_expiry("not-a-jwt"), None);
    }
}
