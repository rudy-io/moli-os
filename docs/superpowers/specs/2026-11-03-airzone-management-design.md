# Airzone device management and current alerts

Extend the observation dashboard with explicit local metadata management and a persistent view of current reported faults. The household remains physically read-only; tests of names and visibility use fictional devices.

- Rename and room assignment reuse label semantics, IDs and the existing guard. Hidden devices can be restored. Hiding does not remove hardware, history, labels or automations.
- A small generic human-only visibility endpoint reuses the existing atomic layout editor and mutex. It is bounded, journaled, denied to guests, and emits a layout invalidation event after successful persistence. It neither creates an assistant nor dispatches commands.
- The UI retains drafts on errors, defers a rejected operation through the existing PIN sheet, and never blindly retries a background mutation. The server remains the authority.
- Battery and radio coverage are percentages only when reported. Wired and unknown remain distinct. Explicit manufacturer battery warnings determine low-battery alerts; no additional threshold is presented as Airzone policy.
- Current alerts include hidden devices, consolidate explicit shared system faults and do not infer health from missing diagnostics or age from change-only SSE timestamps. No phone notifications are enabled in this scope.
- Pairing/unpairing and hot driver installation are unavailable in the Local API/current Moli lifecycle. Addition therefore explains actual discovery and can prepare, but not apply, a read-only gateway configuration. Reserved/duplicate IDs and injection-like addresses are rejected.

Source: the pinned official Local API 1.78 specification linked by `integrations/airzone/coverage.json`. Automated validation includes HTTP authorization/persistence/SSE, pure battery/alarm and proposal tests, metadata request and component-flow tests, and SSR. Browser checks exercise rename/hide/restore using fictional devices, alerts, preparation and responsive navigation. Real equipment is only read.

## Validation completed

- Full `scripts/check.sh`: 1,166 Rust tests passed, strict fmt/clippy, catalogue and leak check passed. Six existing catalogue warnings are unrelated profile fixtures.
- 152 web tests passed, production build passed, 3,547 translation keys and zero hardcoded strings. The Airzone lazy chunk is 44.46 kB / 13.32 kB gzip; the pre-existing main bundle size advisory remains.
- The build used a clean external output directory mounted as `web/dist` for the full check: the host's duplicated generated artifacts caused Docker `EIO` when RustEmbed traversed the old output. No test was skipped or replaced with placeholder assets.
- Browser checks on a fictional hub: local name/room update, hide, restore, hidden-device alert link, shared-fault consolidation, battery/coverage display, duplicate gateway rejection and read-only configuration proposal. At 390 px, management and alerts fit without horizontal overflow.
- Public screenshots contain only fictional devices. Physical hardware mutations were not exercised.
