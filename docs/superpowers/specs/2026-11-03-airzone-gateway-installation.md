# Add an Airzone gateway from Moli

The dashboard now verifies and installs a local gateway without editing a file or restarting the hub. The resulting driver is always read-only. This extends gateway discovery; it does not pair or unpair physical thermostats.

## Boundaries and lifecycle

- The API accepts a stable instance ID, private/loopback IPv4 address, port and optional verified TLS fingerprint. Unknown fields, arbitrary URLs and writable options are rejected. Both probe and install require a human session; guests cannot access them.
- The native driver owns the bounded identity/inventory probe and reuses its fixed read transport. Installation rechecks the previously displayed identity. Missing or malformed metadata cannot become an inferred successful connection.
- The executable owns a versioned, bounded `airzone-gateways.json` registry in its data directory. It preserves `moli.toml`, rejects conflicts/corruption, restores managed drivers at startup and includes the registry in backup/restore.
- Installation is serialized, persisted before activation, and supervised with the existing runtime. Retrying an identical completed request does not create another driver. A disconnected browser cannot strand an accepted installation between persistence and activation.
- IDs, endpoints and controller identities are checked against managed and configured instances, including disabled static entries. The canonical controller identity is pinned in the managed driver.
- `driver_registered` supplies the kind/integration metadata needed by every open dashboard before new readings arrive. Installation has its own journal action; it never dispatches equipment commands.

## Interface

The entry point is available in System even before an Airzone driver exists. A first button verifies the gateway; the second explicitly installs it in read-only mode. Editing the form invalidates verification. PIN cancellation, request errors and interrupted replies retain an actionable form. A saved gateway is described as starting, separately from its later online status and zone discovery.

The pairing explanation and restore-card link remain below the primary gateway form. No configuration text or shell command is required in the normal product flow.

## Verification

Tests cover the real native probe, API authorization before network access, input bounds, persistence/restart, collision/concurrency handling, idempotent retries, cancellation, shutdown and SSE registration. The frontend exercises the two-step protocol and real component handlers, including invalidated previews and authentication cancellation. Browser installation and reboot checks use an isolated Moli instance and a fictional gateway; the existing household is only read.

Local validation: 1,188 Rust tests and 166 web tests pass; strict fmt/Clippy, the integration catalogue, leak check, production build and 3,604 translation keys pass. The catalogue retains six unrelated fixture warnings and the frontend retains its bundle-size advisory.

Browser checks cover the first-gateway System entry, PIN cancellation and resumption, a verified preview, explicit installation, discovery in a second already-open tab and a 390 px layout with no horizontal overflow. The isolated process was restarted: the same gateway and zone returned read-only, an identical request remained idempotent and a duplicate endpoint was refused. One installation journal entry and zero equipment writes were observed. Public screenshots use only fictional equipment.
