#!/bin/sh
# Garde-fou unique : format, lints stricts, tests, catalogue, fuites. Lancé dans l'image de dev.
set -eu
mkdir -p web/dist
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked -q
# Keep the locally maintained protocol fixes covered, without requiring BLE system libraries.
cargo fmt --manifest-path vendor/matter-rust/Cargo.toml --all -- --check
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target}" cargo test \
    --manifest-path vendor/matter-rust/Cargo.toml --locked --no-default-features \
    -p matter-controller -p matter-interaction -p matter-commissioning --lib -q
cargo run -q --locked -p moli-os -- catalogue check integrations
sh scripts/test-vendor-leak-guard.sh
sh scripts/leak-check.sh
