# Synthetic cloud fixtures

All `fixtures/cloud-*.json` files are fabricated protocol examples. They are not
captures of an account, gateway or Home Assistant session. Labels, identifiers,
device URLs, actions and notification destinations are synthetic. Actions are
data only; never send these fixtures to Somfy.

The shapes for scenarios, calendar days/rules, triggers and scheduled/delayed
executions come from the official public TaHoma Classic client, inspected on
2026-11-01. [CLOUD.md](CLOUD.md#protocol-evidence-and-coverage) records the primary
source, retrieved version and SHA-256. No vendor implementation is bundled.

`cloud-empty.json` and `cloud-error-object.json` distinguish a successful empty
collection from an error-shaped response. `cloud-unknown-trigger.json` exercises
opaque preservation of an unfamiliar wrapper. Unknown enums marked
`SYNTHETIC_UNKNOWN` are deliberately not claims about released TaHoma firmware.

Fixtures test bounded parsing, linking, ordering and failure retention. They do
not establish actual endpoint availability for a modern scoped OAuth account,
the provider's scheduling horizon, or complete TaHoma feature coverage. Never
replace them with raw household data or authenticated provider responses.
