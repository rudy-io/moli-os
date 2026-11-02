# Read-only TaHoma programme display

Requested: readable days, times and actions in the existing Somfy catalogue, tests,
updated draft PR and isolated local trial. The established read-only scope is unchanged.

Use a pure bounded frontend projection and existing Svelte components. No new dependency,
request, timer, backend contract, vault write or execution control. Names come from the
catalogue's exact target_labels map; scenarios and days resolve within the same catalogue.
Inline localCalendarDay/localActionGroup definitions are shown without inventing IDs.
Only explicit unambiguous references resolve; historical references remain qualified.

Show native declared activation, dates, weekdays (Monday bit 1 through Sunday bit 64),
fixed times, sunrise/sunset and named ordered actions. Unknown variants, malformed values,
unsafe numbers, unqualified recurrence/solar offsets and missing references are explicit.
Never calculate occurrences, actual activation, priorities, or astronomical times. Native
closure percentages remain closure percentages. Full-precision original JSON stays on its
existing route; do not regenerate it from JavaScript numbers.

The calendar collection opens by default. Detailed action lists expand on demand after a
small preview; count/expansion limits are visible. Preserve existing current/historical,
empty/unobserved and owner-only semantics. All display strings are FR/EN; use existing
porcelain styling and responsive layout. Public fixtures and screenshots are fabricated.

Implementation ownership: pure model and unit tests in somfy-programmes.js/test.js;
components and integration/SSR/i18n handled separately. Verify model edge cases, real SSR
output, escaping, lack of execution controls, language switching, bounded rendering and
existing controller contracts. Run full scripts/check.sh, web suite/build/i18n, independent
review, synthetic desktop/mobile checks, image build and isolated trial check before PR
completion. Document size/performance changes against the preceding build and keep
pre-existing budget limitations explicit.

Protocol evidence: the official TaHoma Classic client at
https://classic.tahomalink.com/enduser-mobile-web/steer-html5-client/tahoma/bootstrap.js
(SHA-256 f8e4b8a50631a51ac22f2c38c4f9e4735f11dcc3f507f835290830b3a0c273ac)
confirms Monday-first mask, 1-based months, whole -1 date bounds, daily cadence and
inline action groups. It does not establish modern trigger offset units or weekly
periods above one; these remain qualified. Modern enabled fields are reported flags,
not a claim that a programme will run.
