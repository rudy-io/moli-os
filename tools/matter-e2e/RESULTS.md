# Executed release result — 2026-11-06

**PASS**, Moli ↔ independent matter.js protocol test using the static binary
extracted from the production Dockerfile image. Controller and codec resolve
from Git revision `d80b1a3fd2204119ddf47406f4bb59916ba7576b`, without a sibling checkout.

Reproduce after building and extracting the image’s `/moli-os`:

```sh
cd tools/matter-e2e
npm ci --ignore-scripts --no-audit --no-fund
MOLI_BIN=/absolute/path/to/moli-os E2E_MEASURE=1 ./run.sh
```

Binary SHA-256:
`1504a03ed30432cd4e38c115e99a6fcd96741d0694b65d92fdf8c2cf416929bf`

Binary size: 30 109 368 bytes. Uncompressed Podman image: 30 113 713 bytes.
The image exceeds the 25 MB project target; see `docs/PERFORMANCE.md`.

Observed exit code 0:

```text
MEASURE: {"scope": "one virtual light; process memory, not cgroup; no BLE/OTBR", "seconds": 29.78, "rss_kib_peak": 17324, "anonymous_kib_peak": 3460, "cpu_percent_one_core": 0.0}
PASS: commissioning, descriptor, OnOff, subscription, restart identity
PASS: malformed state, invalid snapshot, wrong vault key rejected
```

The accessory uses pinned matter.js 0.18.0 and explicit synthetic test PAA/CD
trust roots. Attestation remains enabled. Test processes run in an isolated
Linux user/network namespace with no physical uplink. OnOff changes are verified
independently at the accessory and through Moli’s subscription. Restart preserves
identity and control without a new code. Malformed hex, corrupt snapshots and the
wrong vault key are rejected.

These are process measurements over one short idle interval, not cgroup budget
validation. This isolated run performs no hardware/radio tests, multi-device
stress tests, power-loss tests or Home Assistant comparison. Separate real Matter IP
evidence is recorded in `integrations/matter/HARDWARE.md`.
The wrong-key check exercises the CLI separately from the driver restart tests.
Raw synthetic logs and measurement JSON remain in ignored `.run/`.
