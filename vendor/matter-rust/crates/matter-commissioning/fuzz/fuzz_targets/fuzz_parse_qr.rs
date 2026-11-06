//! Fuzz target: `setup::parse_qr` must not panic on any input.
//!
//! Run locally with `cargo +nightly fuzz run fuzz_parse_qr`. The weekly
//! CI workflow at `.github/workflows/fuzz.yml` runs this for 5 minutes
//! every Monday once the workflow is updated to include the new target.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Treat the input as a (potentially invalid) UTF-8 string.
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = matter_commissioning::setup::parse_qr(s);
    }
});
