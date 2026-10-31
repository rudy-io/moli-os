# Connect a local TaHoma

Check the gateway supports Somfy developer mode. First-generation boxes and the
Connectivity Kit are excluded by Somfy. Follow the official app procedure: gateway
settings, seven taps on the PIN, enable developer mode, generate a dedicated token.
A token is shown once; enter it directly into Moli's encrypted secret store, never
in configuration, chat or a screenshot.

Configure a stable instance ID and the gateway's LAN hostname:

```toml
[[driver]]
id = "somfy"
kind = "overkiz"
# enabled = false
[driver.options]
host = "gateway-example.local"
# port = 8443
# poll_secs = 5
# cert_sha256 = "<64 hexadecimal digits verified independently>"
```

Store the token using a secret-manager pipe into
`moli-os secrets set somfy token`. Start read-only observation first. With no
configured/cached fingerprint the first TLS handshake trusts and persists the
certificate before sending the token. Use a trusted LAN for this first contact or
provide an independently verified fingerprint. A certificate replacement stops the
driver; verify it independently before updating `cert_sha256` or forgetting the
cached `cert_sha256` and restarting. Do not remove a pin merely to silence a warning.

Check URL identities, measured state and availability before any supervised test.
Test a neutral shutter only with an adult present, restore its initial state and
never use a protected room. An accepted execution is not proof of movement.
Compare actual position afterwards.

Windows/gates remain read-only unless a human reviews their exact native URL,
class and individual effects. Opt in only after that review, for example with
synthetic identifiers:

```toml
[[driver.options.writable_openers]]
device_url = "io://example/789"
ui_class = "Gate"
```

Only `Gate` and `Window` can be reviewed (maximum 128 unique entries). The current
nested `definition.uiClass` must exactly match; labels never establish the class.
All writable opener points use Control semantics: API/MCP/CLI/assistant and the
household dashboard require human approval. Authenticated Ui, System and approved
Automation follow the existing bypass policy; this is not a physical interlock.
Cyclic gates expose a distinct advertised `cycle` impulse, which does not promise
open or closed. A cycle-only gate has no open/close buttons or inferred position.
Check the actual device and keep its route clear before any authorized test.
