# Executed result — 2026-11-05

**PASS**, genuine hardware-free Moli ↔ independent matter.js protocol test.

Command: `./e2e/run.sh` from project root. Node v26.7.0 / npm 12.2.0,
`@matter/main` 0.18.0 with committed npm lockfile. Linux unprivileged user/network
namespace, isolated veth pair, no uplink, no container host network.

Existing host debug binary used (no new full build):
the Cargo target directory's `debug/moli-os`

SHA-256: `ce2bf6770b28b82f321673602f0a7d86997bf513482e2ac5b698473cecdfdc53`

Observed stdout (exit 0):

```text
PASS: commissioning, descriptor, OnOff, subscription, restart identity
PASS: malformed state, invalid snapshot, wrong vault key rejected
```

Commissioning completed with explicit test PAA/CD directories and attestation
verification enabled. Moli logged successful CASE and CLI-origin REST commands;
virtual accessory OnOff change events independently confirmed each requested state.
Restarting Moli preserved device identity and restored operational control without
supplying a new pairing code. Both `not-hex` and `00` controller snapshots produced
Matter driver `status.state=backoff` with a 1000 ms retry, offline cached devices,
and refused commands. Wrong master key made the secrets CLI exit nonzero.

Raw logs and failure status JSON are in ignored `e2e/.run/`. They are overwritten
on rerun. The accessory is terminated when the launcher exits. Script syntax checks
(`bash -n`, `node --check`, `python3 -m py_compile`) also passed.

No claim about the parent's later release binary or subsequent driver changes:
rerun against that binary with `MOLI_BIN=...`. No physical/Thread/BLE/sensor tests,
performance measurement or full upstream check was performed here.
