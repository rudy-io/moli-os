//! Verhoeff check-digit computation (ISO/IEC 7064 mod-11,10).
//!
//! Used by the Matter manual pairing code (Matter Core Spec §5.1.4) to
//! detect single-digit transcription errors. Private to the `setup`
//! module.

/// Compute the Verhoeff check digit for `digits` (the value to be
/// protected, **without** the check digit appended).
///
/// `digits` must contain only ASCII `0..=9`. Non-digits silently
/// contribute zero — callers should validate input before invoking.
pub(super) fn check_digit(digits: &str) -> u8 {
    let mut c: u8 = 0;
    // Walk RIGHT-to-LEFT. Position index starts at 1 (the check digit
    // would be position 0).
    for (i, ch) in digits.bytes().rev().enumerate() {
        let n = ascii_digit_to_u8(ch);
        let pos = (i + 1) % 8;
        c = D[c as usize][P[pos][n as usize] as usize];
    }
    INV[c as usize]
}

/// Verify a Verhoeff-protected string (digits + check digit at the end).
///
/// Returns `true` iff the check digit validates.
pub(super) fn verify(digits_with_check: &str) -> bool {
    let mut c: u8 = 0;
    for (i, ch) in digits_with_check.bytes().rev().enumerate() {
        let n = ascii_digit_to_u8(ch);
        let pos = i % 8;
        c = D[c as usize][P[pos][n as usize] as usize];
    }
    c == 0
}

fn ascii_digit_to_u8(b: u8) -> u8 {
    // Tolerates non-digits by mapping them to 0; callers must validate.
    if b.is_ascii_digit() {
        b - b'0'
    } else {
        0
    }
}

/// Dihedral D₅ multiplication table.
const D: [[u8; 10]; 10] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
    [1, 2, 3, 4, 0, 6, 7, 8, 9, 5],
    [2, 3, 4, 0, 1, 7, 8, 9, 5, 6],
    [3, 4, 0, 1, 2, 8, 9, 5, 6, 7],
    [4, 0, 1, 2, 3, 9, 5, 6, 7, 8],
    [5, 9, 8, 7, 6, 0, 4, 3, 2, 1],
    [6, 5, 9, 8, 7, 1, 0, 4, 3, 2],
    [7, 6, 5, 9, 8, 2, 1, 0, 4, 3],
    [8, 7, 6, 5, 9, 3, 2, 1, 0, 4],
    [9, 8, 7, 6, 5, 4, 3, 2, 1, 0],
];

/// Permutation table indexed by `position mod 8`.
const P: [[u8; 10]; 8] = [
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
    [1, 5, 7, 6, 2, 8, 3, 0, 9, 4],
    [5, 8, 0, 3, 7, 9, 6, 1, 4, 2],
    [8, 9, 1, 6, 0, 4, 3, 5, 2, 7],
    [9, 4, 5, 3, 1, 2, 6, 8, 7, 0],
    [4, 2, 8, 6, 5, 7, 3, 9, 0, 1],
    [2, 7, 9, 3, 8, 0, 6, 4, 1, 5],
    [7, 0, 4, 6, 9, 1, 3, 2, 5, 8],
];

/// Inverse table.
const INV: [u8; 10] = [0, 4, 3, 2, 1, 5, 6, 7, 8, 9];

#[cfg(test)]
#[allow(clippy::unwrap_used)] // Test code: unwrap/expect are allowed in tests (CONTRIBUTING.md).
mod tests {
    use super::{check_digit, verify};

    /// Wikipedia's worked example: the input `"236"` has check digit `3`,
    /// producing the protected string `"2363"`.
    #[test]
    fn wikipedia_example_236() {
        assert_eq!(check_digit("236"), 3);
        assert!(verify("2363"));
    }

    /// Single-digit transcription error detection. Baseline `"2363"` is
    /// the protected form of `"236"`; single-digit perturbations must fail.
    #[test]
    fn detects_single_digit_swap() {
        assert!(verify("2363"));
        assert!(!verify("2463")); // 3→4 in the second position
        assert!(!verify("2373")); // 6→7 in the third position
        assert!(!verify("2364")); // 3→4 in the check position
    }

    /// Transposition of adjacent digits detection. Verhoeff catches every
    /// adjacent-pair transposition.
    #[test]
    fn detects_adjacent_transposition() {
        assert!(verify("2363"));
        assert!(!verify("3263")); // 2 and 3 swapped
        assert!(!verify("2633")); // 3 and 6 swapped
    }

    /// Self-consistency: appending the computed check digit must verify.
    /// This is the property that actually matters for our use case, and it
    /// holds regardless of whether any specific hand-picked reference is
    /// correct.
    #[test]
    fn self_consistency_short() {
        for n in 0..1000u32 {
            let s = format!("{n:03}");
            let with_check = format!("{s}{}", check_digit(&s));
            assert!(verify(&with_check), "failed self-check for {s}");
        }
    }

    /// Self-consistency at the manual-code length (10 digits + 1 check).
    #[test]
    fn self_consistency_manual_code_length() {
        let cases = [
            "1234567890",
            "0000000001",
            "9999999998",
            "3497011233", // 10-digit prefix shape used by Matter manual codes
        ];
        for s in cases {
            let with_check = format!("{s}{}", check_digit(s));
            assert!(verify(&with_check), "failed for {s}");
        }
    }
}
