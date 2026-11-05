# Hardware-free Moli / Matter interoperability

This runs the **production Moli driver**, encrypted CLI vault, REST command path,
Matter commissioning/attestation, Descriptor reads, OnOff commands and subscriptions
against an independent **matter.js 0.18.0** virtual OnOffLight. No mock replies,
controller-library-only tests, radios, physical discovery or physical actions.

Requirements: Linux with unprivileged user/network namespaces, `ip` (iproute2),
Bash, Python 3, Node >=20, npm, and a compiled host-compatible `moli-os` containing
`moli-matter` and the pinned matter-controller (`invoke_before`).

```sh
cd tools/matter-e2e
npm ci --ignore-scripts --no-audit --no-fund
MOLI_BIN=/absolute/path/to/moli-os ./run.sh
```

Default binary: the debug binary in Cargo's configured target directory. The harness does
not build or change the source tree. A musl binary also works on Linux if
built for this architecture. Container networking is unnecessary.

`run.sh` creates a new user/network namespace and refuses to proceed unless its
only initial interface is loopback. It creates an isolated veth pair with synthetic
IPv4/IPv6 addresses, no default route, no physical uplink, and no port publication.
Both processes and REST assertions run inside that namespace. npm installation is
outside it and does not launch an accessory. **Never run accessory.mjs directly or
on host/container host networking.** The launcher cleans up child processes on exit.

The lockfile pins the full JS dependency graph. Accessory behavior uses the public
ServerNode / Endpoint API. Its default development attestation uses the well-known
CHIP test PAA and Matter Appendix F test CD signer. The harness extracts those two
public DER certificates from pinned package sources into `.run/paa` and `.run/cd`
and explicitly points Moli at them. Attestation verification stays enabled; no
production defaults are changed. Private development attestation keys remain the
upstream matter.js fixture's responsibility. The synthetic pairing code and master
keys are test-only; CLI writes pairing/state values over stdin to the encrypted
vault. `.run` (mode 0700) and node_modules are ignored. Upstream may log test pairing
information; logs remain inside this disposable directory.

Assertions, in order:

1. Commissioning succeeds and Descriptor publishes an online OnOff endpoint.
2. REST On and Off succeed; Moli subscription state and an independently observed
   accessory OnOff event both agree. Initial state is restored.
3. Restart **only Moli**, without reinjecting a pairing code; the same device ID
   returns online, CASE succeeds, and On/Off still works with fresh accessory events.
4. Malformed hex and syntactically valid but corrupt controller snapshots produce
   driver backoff, no online device and command refusal. Cached metadata is allowed.
5. A wrong vault master key is rejected by the secrets CLI.

A timeout, failed process or failed assertion exits nonzero. Logs and driver failure
JSON remain in `.run` until the next run (which replaces only that directory).
No generic 'skip' counts as success.

Sources: [official Matter.js repository](https://github.com/matter-js/matter.js),
installed `@matter/protocol/src/certificate/{ChipPAAuthorities.ts,kinds/CertificationDeclaration.ts}`.
This validates IP interoperability with a virtual light only. It does not validate
Thread/BLE, sensors, battery devices, real vendor attestation, memory budgets, or
crash-safe counter durability. Wrong-key CLI rejection is distinct from a driver
restart test with a wrong master key.
