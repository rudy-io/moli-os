# Executed release result — 2026-11-05

**PASS**, Moli ↔ independent matter.js protocol test using the static binary
extracted from the production Dockerfile image. Controller and codec resolve
from Git revision `429350c0171d62fbf911b7600231ba0480d4b875`, without a sibling checkout.

Reproduce after building and extracting the image’s `/moli-os`:

```sh
cd tools/matter-e2e
npm ci --ignore-scripts --no-audit --no-fund
MOLI_BIN=/absolute/path/to/moli-os E2E_MEASURE=1 ./run.sh
```

Binary SHA-256:
`f385ea37dce2c2322ac26723dbd48d49ae5c8a08838d7d01cd7c74966e03bbf4`

Binary size: 30 074 832 bytes. Uncompressed Podman image: 30 078 897 bytes.
The image exceeds the 25 MB project target; see `docs/PERFORMANCE.md`.

Observed exit code 0:

```text
MEASURE: {"scope": "one virtual light; process memory, not cgroup; no BLE/OTBR", "seconds": 29.78, "rss_kib_peak": 17156, "anonymous_kib_peak": 3484, "cpu_percent_one_core": 0.0}
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
validation. No physical/Thread/BLE/battery/sensor hardware tests, multi-device
stress tests, power-loss tests or Home Assistant comparison were performed.
The wrong-key check exercises the CLI separately from the driver restart tests.
Raw synthetic logs and measurement JSON remain in ignored `.run/`.
