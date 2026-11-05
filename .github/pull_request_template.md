## What and why

<!-- Ce que ça change, et pourquoi. / What it changes, and why. -->

## Checked

- [ ] `sh scripts/check.sh` (fmt, clippy, tests, catalogue, leak check)
- [ ] `npm run build` in `web/` and `node scripts/i18n-check.mjs`
- [ ] New texts in `locales/fr` **and** `locales/en`
- [ ] Touches who may call what (routes, sessions, access)? `docs/SECURITY-MODEL.md` and `crates/moli-api/tests/routes.txt` updated
- [ ] No personal data (names, addresses, MACs, serials, keys): fake values only
- [ ] Tried on real hardware? Which model, firmware, date (in the integration's README)
