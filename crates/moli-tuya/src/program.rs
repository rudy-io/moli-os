//! A pilot-wire heater's week program, as Tuya FP modules keep it: for each
//! day (1 = Monday … 7 = Sunday), four raw DPs `week_program_18_<day>_<part>`,
//! a quarter of the day each (part 1 = 0 h – 6 h … part 4 = 18 h – 24 h), in
//! 24 slots of 15 minutes.
//!
//! A part starts with 12 bytes of orders for four- or six-order wiring, a
//! nibble per slot, high nibble first. On switch wiring 24 bytes of
//! setpoints follow, a byte per slot, in half degrees (36 bytes); on four or
//! six orders the part stops there (12 bytes). On four orders the module
//! shows comfort −1 and −2 as comfort: the stored program is one.

/// Slots in a part, and parts in a day.
pub const SLOTS: usize = 24;
pub const PARTS: u8 = 4;
const ORDERS: usize = SLOTS / 2;

/// A part's code, for `day` (1–7) and `part` (1–4).
#[must_use]
pub fn part_code(day: u8, part: u8) -> String {
    format!("week_program_18_{day}_{part}")
}

/// One quarter of an hour: the order (as the module's `mode` says it, for
/// four- and six-order wiring) and the setpoint (switch wiring; none in a
/// part kept on four or six orders).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    pub order: Option<&'static str>,
    pub celsius: Option<f64>,
}

/// A pilot wire's order, from its nibble; `None` for an unknown one.
fn order(nibble: u8) -> Option<&'static str> {
    Some(match nibble {
        1 => "comfort",
        2 => "comfort_1",
        3 => "comfort_2",
        4 => "hors",
        5 => "eco",
        _ => return None,
    })
}

/// An order's nibble, and the order's own name; `None` for an unknown one.
fn nibble(name: &str) -> Option<(u8, &'static str)> {
    (1..=5).find_map(|n| order(n).filter(|o| *o == name).map(|o| (n, o)))
}

/// Setpoints a module takes, in half degrees (5 °C to 40 °C).
const HALVES: std::ops::RangeInclusive<u8> = 10..=80;

/// Days in a week, slots in a day.
pub const DAYS: u8 = 7;
pub const DAY_SLOTS: usize = SLOTS * PARTS as usize;

/// The point a device with a week program shows it under.
pub const POINT: &str = "week_program";

/// Whether a device keeps a week program (its parts are among its DPs).
#[must_use]
pub fn kept_by(dps: &[crate::Dp]) -> bool {
    let first = part_code(1, 1);
    dps.iter().any(|d| d.code == first)
}

/// The week as the point shows it: for each day (Monday first), its runs —
/// from which quarter of an hour (0–95), which order and which setpoint —
/// a new run wherever either changes. A day with a part missing or
/// unreadable is `null`.
#[must_use]
pub fn week(part: impl Fn(u8, u8) -> Option<Vec<u8>>) -> serde_json::Value {
    let days = (1..=DAYS)
        .map(|day| {
            let slots: Option<Vec<Slot>> = (1..=PARTS)
                .map(|p| part(day, p).as_deref().and_then(decode_part))
                .collect::<Option<Vec<_>>>()
                .map(|parts| parts.concat());
            slots.map_or(serde_json::Value::Null, |slots| runs(&slots))
        })
        .collect();
    serde_json::json!({ "days": serde_json::Value::Array(days) })
}

fn runs(slots: &[Slot]) -> serde_json::Value {
    let mut out = Vec::new();
    let mut last: Option<&Slot> = None;
    for (at, slot) in slots.iter().enumerate() {
        if last != Some(slot) {
            out.push(serde_json::json!({ "at": at, "order": slot.order, "celsius": slot.celsius }));
            last = Some(slot);
        }
    }
    serde_json::Value::Array(out)
}

/// A part's 24 slots; `None` unless it is 12 bytes (orders) or 36 (orders
/// and setpoints).
#[must_use]
pub fn decode_part(raw: &[u8]) -> Option<Vec<Slot>> {
    if raw.len() != ORDERS && raw.len() != ORDERS + SLOTS {
        return None;
    }
    let (orders, setpoints) = raw.split_at(ORDERS);
    let nibbles = orders.iter().flat_map(|b| [b >> 4, b & 0x0F]);
    Some(
        nibbles
            .enumerate()
            .map(|(i, nibble)| Slot {
                order: order(nibble),
                celsius: setpoints.get(i).map(|half| f64::from(*half) / 2.0),
            })
            .collect(),
    )
}

/// Why a week sent to the point is not taken.
#[derive(Debug, PartialEq, thiserror::Error)]
pub enum Refused {
    #[error("not a week: {0}")]
    Shape(String),
    #[error("day {day}: {why}")]
    Day { day: u8, why: String },
    #[error("day {day} part {part}: not read from the module yet")]
    Unread { day: u8, part: u8 },
    #[error("day {day} part {part}: this wiring keeps no setpoints")]
    NoSetpoints { day: u8, part: u8 },
}

/// A week sent to the point, in its own words (`{"days": [...]}`, Monday
/// first; a run's `order` and `celsius` may be left out): each day's 96
/// slots, `None` for a day left as it is (`null`). What a run leaves out is
/// kept from the module.
///
/// # Errors
/// Not seven days, or a day whose runs do not start at 0 and go forward,
/// or an order or a setpoint the module does not take.
pub fn parse_week(text: &str) -> Result<Vec<Option<Vec<Slot>>>, Refused> {
    let json: serde_json::Value =
        serde_json::from_str(text).map_err(|e| Refused::Shape(e.to_string()))?;
    let days = json["days"]
        .as_array()
        .filter(|days| days.len() == usize::from(DAYS))
        .ok_or_else(|| Refused::Shape("seven days wanted".into()))?;
    days.iter()
        .zip(1..=DAYS)
        .map(|(runs, day)| {
            if runs.is_null() {
                return Ok(None);
            }
            day_slots(runs)
                .map(Some)
                .map_err(|why| Refused::Day { day, why })
        })
        .collect()
}

fn day_slots(runs: &serde_json::Value) -> Result<Vec<Slot>, String> {
    let runs = runs.as_array().ok_or("runs wanted")?;
    let at = |run: &serde_json::Value| run["at"].as_u64().and_then(|at| usize::try_from(at).ok());
    let mut slots = Vec::with_capacity(DAY_SLOTS);
    for (i, run) in runs.iter().enumerate() {
        let start = at(run).ok_or("a run without `at`")?;
        let end = runs.get(i + 1).map_or(Some(DAY_SLOTS), at);
        if start != slots.len() || end.is_none_or(|end| end <= start || end > DAY_SLOTS) {
            return Err(format!("run {i}: runs start at 0 and go forward"));
        }
        let order = match run["order"].as_str() {
            Some(name) => Some(nibble(name).ok_or(format!("unknown order {name}"))?.1),
            None => None,
        };
        let celsius = match run["celsius"].as_f64() {
            Some(c) if half(c).is_some() => Some(c),
            Some(c) => return Err(format!("setpoint {c}: 5 to 40 °C, by half degrees")),
            None => None,
        };
        slots.resize(end.unwrap_or(DAY_SLOTS), Slot { order, celsius });
    }
    if slots.len() == DAY_SLOTS {
        Ok(slots)
    } else {
        Err("no runs".into())
    }
}

/// A setpoint in the module's half degrees, when it takes it.
fn half(celsius: f64) -> Option<u8> {
    HALVES
        .into_iter()
        .find(|h| (f64::from(*h) - celsius * 2.0).abs() < 1e-9)
}

/// A part's bytes with `slots` (24) in them, the rest as `current` keeps it
/// (its size too); `None` when a setpoint is asked of a part that holds
/// none (four or six orders).
#[must_use]
pub fn encode_part(slots: &[Slot], current: &[u8]) -> Option<Vec<u8>> {
    if slots.len() != SLOTS
        || (current.len() != ORDERS && current.len() != ORDERS + SLOTS)
        || (current.len() == ORDERS && slots.iter().any(|s| s.celsius.is_some()))
    {
        return None;
    }
    let was = current[..ORDERS].iter().flat_map(|b| [b >> 4, b & 0x0F]);
    let nibbles: Vec<u8> = slots
        .iter()
        .zip(was)
        .map(|(slot, was)| slot.order.and_then(nibble).map_or(was, |(n, _)| n))
        .collect();
    let mut out: Vec<u8> = nibbles
        .chunks(2)
        .map(|pair| (pair[0] << 4) | pair[1])
        .collect();
    out.extend(
        current[ORDERS..]
            .iter()
            .zip(slots)
            .map(|(was, slot)| slot.celsius.and_then(half).unwrap_or(*was)),
    );
    Some(out)
}

/// The parts to send for `week` (the point's words), against what the
/// module keeps (`part(day, part)`, its raw bytes): only those that change.
///
/// # Errors
/// The week is not taken (`parse_week`), a part it changes was never read,
/// or it asks setpoints of a wiring that keeps none.
pub fn changes(
    week: &str,
    part: impl Fn(u8, u8) -> Option<Vec<u8>>,
) -> Result<Vec<(String, Vec<u8>)>, Refused> {
    let mut out = Vec::new();
    for (slots, day) in parse_week(week)?.into_iter().zip(1..=DAYS) {
        let Some(slots) = slots else { continue };
        for (p, wanted) in (1..=PARTS).zip(slots.chunks(SLOTS)) {
            let current = part(day, p).ok_or(Refused::Unread { day, part: p })?;
            let new = encode_part(wanted, &current).ok_or(Refused::NoSetpoints { day, part: p })?;
            if new != current {
                out.push((part_code(day, p), new));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect()
    }

    #[test]
    fn parts_are_named_by_day_and_quarter() {
        assert_eq!(part_code(1, 1), "week_program_18_1_1");
        assert_eq!(part_code(7, 4), "week_program_18_7_4");
    }

    #[test]
    fn a_switch_program_dips_to_15_5_at_half_past_one() {
        // Wednesday, 0 h – 6 h, as a real module keeps it: comfort all along
        // (four or six orders); 17 °C but 15.5 °C from 1:30 to 2:00 (switch).
        let part = hex("11 11 11 11 11 11 11 11 11 11 11 11 \
             22 22 22 22 22 22 1f 1f 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22");
        let slots = decode_part(&part).unwrap();
        assert_eq!(slots.len(), SLOTS);
        assert!(slots.iter().all(|s| s.order == Some("comfort")));
        let at = |h: usize, m: usize| slots[h * 4 + m / 15].celsius.unwrap();
        assert!((at(1, 15) - 17.0).abs() < f64::EPSILON);
        assert!((at(1, 30) - 15.5).abs() < f64::EPSILON);
        assert!((at(1, 45) - 15.5).abs() < f64::EPSILON);
        assert!((at(2, 0) - 17.0).abs() < f64::EPSILON);
    }

    #[test]
    fn six_orders_read_slot_by_slot() {
        // Sunday, 18 h – 24 h: comfort −2, −1 from 19:30, −2 from 20:00,
        // frost guard from 20:30, eco from 21:00, comfort −2 from 21:30.
        let part = hex("33 33 33 22 33 44 55 33 33 33 33 33 \
             22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22");
        let orders: Vec<_> = decode_part(&part)
            .unwrap()
            .iter()
            .map(|s| s.order.unwrap())
            .collect();
        let at = |h: usize, m: usize| orders[(h - 18) * 4 + m / 15];
        assert_eq!(at(19, 15), "comfort_2");
        assert_eq!(at(19, 30), "comfort_1");
        assert_eq!(at(19, 45), "comfort_1");
        assert_eq!(at(20, 0), "comfort_2");
        assert_eq!(at(20, 30), "hors");
        assert_eq!(at(21, 0), "eco");
        assert_eq!(at(21, 30), "comfort_2");
        assert_eq!(at(23, 45), "comfort_2");
    }

    #[test]
    fn a_week_is_told_in_runs() {
        // Comfort at 17 °C all week, but Wednesday's 15.5 °C from 1:30 to
        // 2:00; Sunday's parts are missing.
        let plain = "11 11 11 11 11 11 11 11 11 11 11 11 \
                     22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22";
        let dip = "11 11 11 11 11 11 11 11 11 11 11 11 \
                   22 22 22 22 22 22 1f 1f 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22";
        let week = week(|day, part| match (day, part) {
            (7, _) => None,
            (3, 1) => Some(hex(dip)),
            _ => Some(hex(plain)),
        });
        let days = week["days"].as_array().unwrap();
        assert_eq!(days.len(), 7);
        assert_eq!(
            days[0],
            serde_json::json!([{ "at": 0, "order": "comfort", "celsius": 17.0 }])
        );
        assert_eq!(
            days[2],
            serde_json::json!([
                { "at": 0, "order": "comfort", "celsius": 17.0 },
                { "at": 6, "order": "comfort", "celsius": 15.5 },
                { "at": 8, "order": "comfort", "celsius": 17.0 },
            ])
        );
        assert!(
            days[6].is_null(),
            "a day with a part missing is not made up"
        );
    }

    #[test]
    fn a_device_keeps_a_program_when_it_has_the_parts() {
        let dp = |code: &str| crate::Dp {
            id: 125,
            code: code.into(),
            kind: crate::DpKind::Raw,
            writable: true,
            unit: None,
            scale: 0,
            min: None,
            max: None,
            step: None,
            range: Vec::new(),
        };
        assert!(kept_by(&[dp("switch"), dp("week_program_18_1_1")]));
        assert!(!kept_by(&[dp("switch")]));
    }

    #[test]
    fn on_six_orders_a_part_keeps_its_orders_only() {
        // Sunday, 18 h – 24 h, as a module wired on six orders keeps it.
        let slots = decode_part(&hex("33 33 33 22 33 44 55 33 33 33 33 33")).unwrap();
        assert_eq!(slots.len(), SLOTS);
        assert_eq!(slots[6].order, Some("comfort_1"), "19:30");
        assert_eq!(slots[10].order, Some("hors"), "20:30");
        assert!(
            slots.iter().all(|s| s.celsius.is_none()),
            "no setpoint on six orders"
        );
    }

    #[test]
    fn a_part_of_another_size_is_not_read() {
        assert!(decode_part(&[0x11; 35]).is_none());
        assert!(decode_part(&[]).is_none());
        let unknown = decode_part(&[0x00; 36]).unwrap();
        assert_eq!(
            unknown[0].order, None,
            "an order it does not know stays unnamed"
        );
    }

    /// A week of seven days, `day` given, the others left as they are.
    fn only(day: usize, runs: &serde_json::Value) -> String {
        let mut days = vec![serde_json::Value::Null; 7];
        days[day] = runs.clone();
        serde_json::json!({ "days": days }).to_string()
    }

    #[test]
    fn monday_night_in_eco_is_what_the_module_took() {
        // Six orders, comfort all Monday; eco from 0 h to 3 h, as the real
        // module took it through the cloud (and gave it back).
        let comfort = hex("11 11 11 11 11 11 11 11 11 11 11 11");
        let week = only(
            0,
            &serde_json::json!([{ "at": 0, "order": "eco" }, { "at": 12, "order": "comfort" }]),
        );
        let sent = changes(&week, |_, _| Some(comfort.clone())).unwrap();
        assert_eq!(
            sent,
            vec![(
                "week_program_18_1_1".to_owned(),
                hex("55 55 55 55 55 55 11 11 11 11 11 11")
            )],
            "only the part that changes leaves"
        );
    }

    #[test]
    fn a_part_reads_back_as_it_was_written() {
        for raw in [
            "33 33 33 22 33 44 55 33 33 33 33 33",
            "11 11 11 11 11 11 11 11 11 11 11 11 \
             22 22 22 22 22 22 1f 1f 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22",
        ] {
            let raw = hex(raw);
            let slots = decode_part(&raw).unwrap();
            assert_eq!(encode_part(&slots, &raw).unwrap(), raw);
        }
    }

    #[test]
    fn a_switch_morning_at_19_5_keeps_its_orders() {
        // Switch wiring, 17 °C all along; 19.5 °C from 6:00 to 8:00 (the
        // second part, 6 h - 12 h: its first eight slots).
        let plain = hex("11 11 11 11 11 11 11 11 11 11 11 11 \
             22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22");
        let week = only(
            2,
            &serde_json::json!([
                { "at": 0, "celsius": 17.0 },
                { "at": 24, "celsius": 19.5 },
                { "at": 32, "celsius": 17.0 },
            ]),
        );
        let sent = changes(&week, |_, _| Some(plain.clone())).unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "week_program_18_3_2");
        assert_eq!(
            sent[0].1,
            hex("11 11 11 11 11 11 11 11 11 11 11 11 \
                 27 27 27 27 27 27 27 27 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22 22")
        );
    }

    #[test]
    fn a_week_it_cannot_hold_is_refused() {
        let six = hex("11 11 11 11 11 11 11 11 11 11 11 11");
        let part = |_, _| Some(six.clone());
        let day = |runs: serde_json::Value| changes(&only(0, &runs), part);
        assert!(
            matches!(
                day(serde_json::json!([{ "at": 0, "celsius": 19.0 }])),
                Err(Refused::NoSetpoints { day: 1, part: 1 })
            ),
            "no setpoints on six orders"
        );
        assert!(
            matches!(
                day(serde_json::json!([{ "at": 4, "order": "eco" }])),
                Err(Refused::Day { day: 1, .. })
            ),
            "from midnight"
        );
        assert!(
            matches!(
                day(
                    serde_json::json!([{ "at": 0, "order": "eco" }, { "at": 0, "order": "comfort" }])
                ),
                Err(Refused::Day { .. })
            ),
            "forward only"
        );
        assert!(matches!(
            day(serde_json::json!([{ "at": 0, "order": "turbo" }])),
            Err(Refused::Day { .. })
        ));
        assert!(
            matches!(
                changes(
                    &only(0, &serde_json::json!([{ "at": 0, "celsius": 19.2 }])),
                    |_, _| None
                ),
                Err(Refused::Day { .. })
            ),
            "half degrees only"
        );
        assert!(matches!(
            changes(r#"{"days": [null]}"#, part),
            Err(Refused::Shape(_))
        ));
        assert!(
            matches!(
                day(serde_json::json!([{ "at": 0, "order": "eco" }])).and(changes(
                    &only(0, &serde_json::json!([{ "at": 0, "order": "eco" }])),
                    |_, _| None
                )),
                Err(Refused::Unread { day: 1, part: 1 })
            ),
            "a part never read is not guessed"
        );
        assert_eq!(
            changes(
                &only(0, &serde_json::json!([{ "at": 0, "order": "comfort" }])),
                part
            )
            .unwrap(),
            Vec::new(),
            "nothing changes, nothing leaves"
        );
    }
}
