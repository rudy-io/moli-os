//! Fuzz target: `parse_nocsr` + `parse_and_verify_csr` must not panic
//! on any input.
//!
//! Run locally with `cargo +nightly fuzz run nocsr_parse`.
//! The weekly fuzz workflow at `.github/workflows/fuzz.yml` should
//! include this target (max-total-time = 1800s).

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = matter_commissioning::parse_nocsr(data);
    let _ = matter_commissioning::parse_and_verify_csr(data);
});
