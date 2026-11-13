# NOC negative-path fixtures

Synthetic NOCSR fixtures for `crates/matter-commissioning/tests/noc_negative.rs`.

**Source:** `scripts/gen-noc-negative-fixtures.py`. Runs against `cryptography>=42`.
**Regenerate:** `python3 scripts/gen-noc-negative-fixtures.py` (output is
committed; CI does NOT recompute).

Each `*.json` file pairs a tampered NOCSR with the expected `NocError`
variant; the table-driven test in `noc_negative.rs` enumerates them.
