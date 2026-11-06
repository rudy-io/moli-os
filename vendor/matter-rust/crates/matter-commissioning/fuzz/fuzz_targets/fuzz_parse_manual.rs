//! Fuzz target: `setup::parse_manual_code` must not panic on any input.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = matter_commissioning::setup::parse_manual_code(s);
    }
});
