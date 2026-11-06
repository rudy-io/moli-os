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

## Compact packaging variant — 2026-11-06

Built alongside the standard/diagnostics variants from the same Rust sources and
locked dependencies, using `--build-arg MOLI_STRIP_SYMBOLS=true`. The Rust sources
include two equivalent syntax rewrites needed for the full application to pass
`cargo check --locked -p moli-os` on Rust 1.89.0. The default keeps its symbols.

- Compact binary: **24 822 928 bytes**; image: **24 827 312 bytes**.
- Compact binary SHA-256:
  `f3ba71b6b8f9be4c3de8096e565df86cddb7e8f846af74760fd1f9d4cd7993a6`.
- Matching standard/diagnostics binary: **30 109 520 bytes**, SHA-256
  `1048eb243be4529834863959b5764b94f917f7bd65e1ad9d623bb718b8bc5f8a`.
  The standard image is **30 113 712 bytes**. Both delivered variants passed the
  protocol checks; the earlier standard result above predates the MSRV rewrites.
- Invalid build-argument values were rejected with exit code 2.
- Code, read-only data and unwind/exception sections were compared byte for byte:
  `.text`, `.rodata`, `.eh_frame`, `.eh_frame_hdr`, `.gcc_except_table` all match.

The compact binary passed the same isolated protocol checks (exit 0):

```text
MEASURE: {"scope": "one virtual light; process memory, not cgroup; no BLE/OTBR", "seconds": 29.78, "rss_kib_peak": 17340, "anonymous_kib_peak": 3476, "cpu_percent_one_core": 0.0}
PASS: commissioning, descriptor, OnOff, subscription, restart identity
PASS: malformed state, invalid snapshot, wrong vault key rejected
```

The size saving removes symbol names for in-process backtraces; it does not remove
features. Preserve the matching diagnostics artifact for postmortem address analysis.
This compact run did not perform further physical-device actions or radio tests.
