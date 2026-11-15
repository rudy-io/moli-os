//! Operational mDNS instance names: `<CFID>-<NODEID>`.
//!
//! Every commissioned Matter node advertises itself on `_matter._tcp` under an
//! instance name built from its fabric's compressed fabric id and its own node
//! id, each as 16 uppercase hex digits joined by `-` (Matter Core §4.3.2,
//! Operational Discovery).
//! These two functions let a caller that runs its own mDNS browse build and
//! recognise those names for this controller's fabric, using
//! [`FabricInfo::compressed_fabric_id`](crate::FabricInfo::compressed_fabric_id).

/// Length of one hex half of an instance name: a `u64` as 16 hex digits.
const HALF_LEN: usize = 16;

/// The operational mDNS instance name for `node_id` on the fabric whose
/// compressed fabric id is `compressed_fabric_id`: `<CFID>-<NODEID>`, each as
/// 16 uppercase hex digits (33 characters in all).
///
/// This is the same name the controller itself resolves when it connects to a
/// node. It is the instance label only; the service type
/// (`._matter._tcp.local.`) is not appended.
///
/// Take `compressed_fabric_id` from
/// [`FabricInfo::compressed_fabric_id`](crate::FabricInfo::compressed_fabric_id).
/// The inverse is [`parse_operational_instance_name`].
///
/// # Examples
///
/// ```
/// use matter_controller::operational_instance_name;
///
/// assert_eq!(
///     operational_instance_name(0xC788_0377_5B33_8E16, 12),
///     "C78803775B338E16-000000000000000C",
/// );
/// ```
#[must_use]
pub fn operational_instance_name(compressed_fabric_id: u64, node_id: u64) -> String {
    // Delegate to the formatter the controller resolves with, so there is one
    // definition of the name. It takes the CFID as the 8 big-endian bytes the
    // derivation produces; `to_be_bytes` is the exact inverse of the
    // `u64::from_be_bytes` that `FabricInfo::compressed_fabric_id` is built with.
    matter_commissioning::driver::operational_instance_name(
        compressed_fabric_id.to_be_bytes(),
        node_id,
    )
}

/// Split an operational mDNS instance name `<CFID>-<NODEID>` into
/// `(compressed_fabric_id, node_id)`.
///
/// Pass the instance label only: strip the service type
/// (`._matter._tcp.local.`) first. The name must be exactly 33 ASCII
/// characters — 16 hex digits, `-`, 16 hex digits. Hex digits may be upper or
/// lower case, since mDNS names compare case-insensitively. Anything else
/// returns `None`: another length, a missing or misplaced `-`, a non-hex
/// character (including a `+` sign or whitespace), or a trailing service type.
///
/// To check whether a name belongs to this controller's fabric, compare the
/// first element with
/// [`FabricInfo::compressed_fabric_id`](crate::FabricInfo::compressed_fabric_id).
/// The inverse is [`operational_instance_name`].
///
/// # Examples
///
/// ```
/// use matter_controller::parse_operational_instance_name;
///
/// assert_eq!(
///     parse_operational_instance_name("C78803775B338E16-000000000000000C"),
///     Some((0xC788_0377_5B33_8E16, 12)),
/// );
/// // Case-insensitive, like mDNS itself.
/// assert_eq!(
///     parse_operational_instance_name("c78803775b338e16-000000000000000c"),
///     Some((0xC788_0377_5B33_8E16, 12)),
/// );
/// // The service type must be stripped first.
/// assert_eq!(
///     parse_operational_instance_name("C78803775B338E16-000000000000000C._matter._tcp.local."),
///     None,
/// );
/// ```
#[must_use]
pub fn parse_operational_instance_name(name: &str) -> Option<(u64, u64)> {
    // `split_at_checked` returns `None` rather than panicking when byte 16 is
    // past the end or inside a multi-byte character.
    let (cfid, rest) = name.split_at_checked(HALF_LEN)?;
    let node_id = rest.strip_prefix('-')?;
    Some((parse_hex_half(cfid)?, parse_hex_half(node_id)?))
}

/// Parse exactly 16 ASCII hex digits (either case) as a `u64`.
///
/// The explicit digit check is load-bearing: `u64::from_str_radix` alone
/// would also accept a leading `+` (`"+123456789ABCDEF"`), which is not a
/// valid instance name.
fn parse_hex_half(half: &str) -> Option<u64> {
    if half.len() != HALF_LEN || !half.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u64::from_str_radix(half, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::{operational_instance_name, parse_operational_instance_name};

    /// The worked example used in the rustdoc: a real-looking CFID and node 12.
    const EXAMPLE: &str = "C78803775B338E16-000000000000000C";
    const EXAMPLE_CFID: u64 = 0xC788_0377_5B33_8E16;

    /// Values that exercise the extremes and every hex digit position.
    const SAMPLES: &[(u64, u64)] = &[
        (0, 0),
        (u64::MAX, u64::MAX),
        (0, u64::MAX),
        (u64::MAX, 0),
        (EXAMPLE_CFID, 12),
        (0x0123_4567_89AB_CDEF, 0xFEDC_BA98_7654_3210),
        (1, 1),
    ];

    #[test]
    fn formats_the_worked_example() {
        assert_eq!(operational_instance_name(EXAMPLE_CFID, 12), EXAMPLE);
    }

    #[test]
    fn parses_the_worked_example() {
        assert_eq!(
            parse_operational_instance_name(EXAMPLE),
            Some((EXAMPLE_CFID, 12))
        );
    }

    #[test]
    fn format_then_parse_round_trips() {
        for &(cfid, node_id) in SAMPLES {
            let name = operational_instance_name(cfid, node_id);
            assert_eq!(name.len(), 33, "fixed width for {name}");
            assert_eq!(
                parse_operational_instance_name(&name),
                Some((cfid, node_id)),
                "round trip of {name}"
            );
        }
    }

    #[test]
    fn zero_and_max_format_at_full_width() {
        assert_eq!(
            operational_instance_name(0, 0),
            "0000000000000000-0000000000000000"
        );
        assert_eq!(
            operational_instance_name(u64::MAX, u64::MAX),
            "FFFFFFFFFFFFFFFF-FFFFFFFFFFFFFFFF"
        );
    }

    /// The public formatter and the one the controller resolves with must be
    /// the same: a caller matching names built by one against names browsed
    /// for by the other must never see a mismatch.
    #[test]
    fn matches_the_internal_driver_formatter() {
        for &(cfid, node_id) in SAMPLES {
            assert_eq!(
                operational_instance_name(cfid, node_id),
                matter_commissioning::driver::operational_instance_name(
                    cfid.to_be_bytes(),
                    node_id
                ),
                "cfid {cfid:#x}, node {node_id:#x}"
            );
        }
    }

    #[test]
    fn lower_and_mixed_case_parse() {
        assert_eq!(
            parse_operational_instance_name("c78803775b338e16-000000000000000c"),
            Some((EXAMPLE_CFID, 12))
        );
        assert_eq!(
            parse_operational_instance_name("C78803775b338E16-000000000000000c"),
            Some((EXAMPLE_CFID, 12))
        );
    }

    #[test]
    fn rejects_wrong_lengths() {
        for name in [
            "",
            // 32 characters: one digit short in the node half.
            "C78803775B338E16-00000000000000C",
            // 34 characters: one digit too many in the node half.
            "C78803775B338E16-000000000000000C0",
            // One digit moved from the node half to the CFID half (still 33).
            "C78803775B338E160-00000000000000C",
        ] {
            assert_eq!(parse_operational_instance_name(name), None, "{name:?}");
        }
    }

    #[test]
    fn rejects_a_missing_or_misplaced_dash() {
        for name in [
            // No dash at all, 32 hex digits.
            "C78803775B338E16000000000000000C",
            // No dash, 33 hex digits.
            "C78803775B338E160000000000000000C",
            // Dash replaced by another separator.
            "C78803775B338E16_000000000000000C",
            // Dash one position early.
            "C78803775B338E1-6000000000000000C",
            // Dash at the very start and end.
            "-C78803775B338E16000000000000000C",
            "C78803775B338E16000000000000000C-",
            // Two dashes.
            "C78803775B338E16--00000000000000C",
        ] {
            assert_eq!(parse_operational_instance_name(name), None, "{name:?}");
        }
    }

    #[test]
    fn rejects_non_hex_characters() {
        for name in [
            "G78803775B338E16-000000000000000C",
            "C78803775B338E16-00000000000000G",
            "C78803775B338E16-000000000000000G",
            // Non-ASCII: 'é' is two bytes, so this is 33 bytes too.
            "C78803775B338E1é-00000000000000C",
        ] {
            assert_eq!(parse_operational_instance_name(name), None, "{name:?}");
        }
    }

    /// `u64::from_str_radix` accepts a leading `+`, so this guards the
    /// explicit hex-digit check: `+` must be rejected in either half.
    #[test]
    fn rejects_a_plus_sign_in_either_half() {
        for name in [
            "+C78803775B338E1-000000000000000C",
            "C78803775B338E16-+00000000000000C",
            "+C78803775B338E1-+00000000000000C",
        ] {
            assert_eq!(parse_operational_instance_name(name), None, "{name:?}");
        }
    }

    #[test]
    fn rejects_surrounding_whitespace() {
        for name in [
            " C78803775B338E16-000000000000000C",
            "C78803775B338E16-000000000000000C ",
            "C78803775B338E16-000000000000000C\n",
            "\tC78803775B338E16-000000000000000C",
            // Whitespace in place of a digit keeps the length at 33.
            " 78803775B338E16-000000000000000C",
            "C78803775B338E16- 00000000000000C",
        ] {
            assert_eq!(parse_operational_instance_name(name), None, "{name:?}");
        }
    }

    #[test]
    fn rejects_a_trailing_service_type() {
        for name in [
            "C78803775B338E16-000000000000000C._matter._tcp.local.",
            "C78803775B338E16-000000000000000C._matter._tcp.local",
            "C78803775B338E16-000000000000000C.",
        ] {
            assert_eq!(parse_operational_instance_name(name), None, "{name:?}");
        }
    }
}
