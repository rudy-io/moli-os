//! A pilot-wire heater's week program, as Tuya FP modules keep it: for each
//! day (1 = Monday … 7 = Sunday), four raw DPs `week_program_18_<day>_<part>`
//! of 36 bytes, a quarter of the day each (part 1 = 0 h – 6 h … part 4 =
//! 18 h – 24 h), in 24 slots of 15 minutes.
//!
//! Each part holds both programs: 12 bytes of orders for four- or six-order
//! wiring, a nibble per slot, high nibble first; then 24 bytes of setpoints
//! for switch wiring, a byte per slot, in half degrees. On four orders the
//! module shows comfort −1 and −2 as comfort: the stored program is one.

/// Slots in a part, and parts in a day.
pub const SLOTS: usize = 24;
pub const PARTS: u8 = 4;
const ORDERS: usize = SLOTS / 2;
const LEN: usize = ORDERS + SLOTS;

/// A part's code, for `day` (1–7) and `part` (1–4).
#[must_use]
pub fn part_code(day: u8, part: u8) -> String {
    format!("week_program_18_{day}_{part}")
}

/// One quarter of an hour: the order (as the module's `mode` says it, for
/// four- and six-order wiring) and the setpoint (switch wiring).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    pub order: Option<&'static str>,
    pub celsius: f64,
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

/// A part's 24 slots; `None` unless it is 36 bytes.
#[must_use]
pub fn decode_part(raw: &[u8]) -> Option<Vec<Slot>> {
    if raw.len() != LEN {
        return None;
    }
    let (orders, setpoints) = raw.split_at(ORDERS);
    let nibbles = orders.iter().flat_map(|b| [b >> 4, b & 0x0F]);
    Some(
        nibbles
            .zip(setpoints)
            .map(|(nibble, half)| Slot {
                order: order(nibble),
                celsius: f64::from(*half) / 2.0,
            })
            .collect(),
    )
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
        let at = |h: usize, m: usize| slots[h * 4 + m / 15].celsius;
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
    fn a_part_of_another_size_is_not_read() {
        assert!(decode_part(&[0x11; 35]).is_none());
        assert!(decode_part(&[]).is_none());
        let unknown = decode_part(&[0x00; 36]).unwrap();
        assert_eq!(
            unknown[0].order, None,
            "an order it does not know stays unnamed"
        );
    }
}
