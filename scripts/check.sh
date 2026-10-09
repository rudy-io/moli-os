#!/bin/sh
# Garde-fou unique : format, lints stricts, tests, catalogue, fuites. Lancé dans l'image de dev.
set -eu
mkdir -p web/dist
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked -q
cargo run -q --locked -p moli-os -- catalogue check integrations
sh scripts/leak-check.sh
