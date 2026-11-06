//! Fuzz target: `Dac::from_der` must not panic on any input.
//!
//! Run locally with `cargo +nightly fuzz run fuzz_dac_from_der`.
//! The weekly fuzz workflow at `.github/workflows/fuzz.yml` should
//! include this target.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = matter_commissioning::Dac::from_der(data);
});
