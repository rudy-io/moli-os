# Matter IP hardware validation — 2026-11-06

Experimental validation with a Philips Hue Bridge v2 (BSB002), production
attestation roots and one explicitly authorized neutral-room bridged light.
Firmware observed at handoff: `1978545030` (Hue API `1.78.0`).
No household identifiers, pairing codes, certificate subjects or raw captures
are included in this report.

- Commissioning succeeded with production attestation after adding the standard
  VID/PID commonName fallback in the pinned stack. Earlier attempts failed with
  `Driver/Commissioning/Attestation/Parse/MissingRequired`; they sent no light commands.
- Descriptor discovery exposed bridged lights with their NodeLabel names and
  online/Reachable state.
- An OnOff request through Moli REST (ordinary API origin, existing hub/guard)
  switched the selected light on. Both the Matter subscription and an independent,
  read-only Hue API observation confirmed the change.
- A second Moli REST request restored the initial off state. The independent
  observer confirmed restoration; brightness and color settings were unchanged.
- Restarting Moli without another pairing code recovered the same device IDs and
  online state from the encrypted vault.

The Hue API was an observer only: light commands used the native Matter driver.
This does not validate BLE provisioning, Thread radio/border-router behavior,
battery devices, power-loss recovery, every cluster or a full production deployment.

Stack revision: `d80b1a3fd2204119ddf47406f4bb59916ba7576b`.
Test binary: musl development build, SHA-256
`4d7bf1195e39ac7e5599af125feef2cd91f7054cb12f983fb06bc65199512853`.
The final production-image binary also resumed the existing real Matter fabric
with the same IDs and online state, without another code or further light commands.
Its SHA-256 is `1504a03ed30432cd4e38c115e99a6fcd96741d0694b65d92fdf8c2cf416929bf`.
Its independent isolated protocol result and measurements are in
`tools/matter-e2e/RESULTS.md` and `docs/PERFORMANCE.md`.
