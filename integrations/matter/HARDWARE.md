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

## Later live diagnosis and corrected subscription — 2026-11-06

Phase diagnostics corrected the previous, unproven Descriptor-timeout diagnosis.
Secure CASE, Descriptor and capability reads succeeded. The subscription failed
locally with `Transport/PayloadTooLarge`: 212 repeated endpoint-specific filters
exceeded the transport limit. Deduplicated endpoint-wildcard filters reduced that
request to 28 while retaining concrete report paths and endpoint command gates.

With that correction, the bridge's devices became online. One explicitly
authorized neutral-room light completed OnOff through Moli REST/Matter under
ordinary API origin. Independent read-only Hue observations confirmed the change
and restoration of the exact initial OnOff state, with brightness/color unchanged.
No Hue write endpoint was used. The debug binary for this actuation was SHA-256
`81c6bfe8dc97ddcdf1686cdf23c3e6c919940936cd87d70079ed5c38f5df66b2`.

Restart without a new pairing code retained identity and state. An intermediate
90-second idle observation found one liveness timeout and ten offline samples.
That observation prompted a separate protocol audit: subscription reports were
always answered with IM Success even when `SuppressResponse` requested no reply.
The protocol correction retains standalone MRP acknowledgements and exchange
cleanup. Its causal relationship to the earlier lost heartbeat requires observation;
it is not inferred solely from the code defect.

The repeated idle loss was then traced to the host firewall: kernel UFW logs
showed five blocked transmission attempts for the publisher's first idle report
aimed at the controller's ephemeral UDP listener. Reads and commands had succeeded
while the outgoing connection-tracking entry was alive. A configurable fixed
`udp_port` and a source-restricted host rule allow unsolicited reports without
periodic polling, increased timeouts or changes to device policies. Moli itself
never edits firewall rules. The corrected listener's idle validation is recorded
below; no household addresses or rule contents are published.

The fixed-port debug binary (SHA-256 `023b7a653089057629154c66229c674680f9c30c03214e7306f7c16ce8d286bf`) recovered the same identity and initial
state without a pairing code. Over 190 one-second samples after recovery it
remained online throughout: three idle periodic reports, one subscription
establishment, zero liveness timeouts and zero offline samples. The intermediate
suppression-only build still lost the first heartbeat; the firewall evidence and
fixed-port result distinguish the actual idle-loss cause from that separate
protocol correction. This is a bounded observation, not a long-term reliability
or radio certification claim.

## Electrical measurement on an IKEA GRILLPLATS — 2026-11-24

Read-only validation of the 0x0090 / 0x0091 clusters on one IKEA GRILLPLATS smart
plug (Matter 1.4 over Thread, firmware `1.4.6` from BasicInformation). Moli ran as a
separate test instance with only the Matter driver, production attestation roots and
its own vault. No command was sent to the plug; every load change was made by hand.
No household identifiers, setup codes or addresses are included.

- The plug was commissioned onto an existing OpenThread border router with another
  controller, then shared to Moli through a commissioning window (multi-admin). It is
  a Thread end device.
- OnOff is on endpoint 1; both measurement clusters are on endpoint 2. Moli therefore
  lists two devices for one plug, and the power badge of the plug does not show
  consumption. This is the documented separate-endpoint limit, observed on real hardware.
- ElectricalPowerMeasurement announces ALTC and attributes `0x04`, `0x05`, `0x07`,
  `0x08`, `0x0A`, `0x0C` among others. ElectricalEnergyMeasurement announces IMPE and
  CUME only, so `energy_returned` is correctly absent.
- The subscription asked for 21 attribute paths in total; the plug accepted it and
  negotiated a 60 s maximum interval.
- Moli's points matched the raw attribute values exactly (unit conversions only).
  Reports arrive within seconds, grouped under one timestamp, and only on change.
  Resolution: 1 V, 1 mA, 0.1 W, 1 Wh.
- No reference instrument was available. Validation relies on invariants the plug must
  satisfy by itself: `voltage x RMS current` equals apparent power within 1-2 %,
  `voltage x active current` equals active power, active power never exceeds apparent
  power, and idle readings are exactly zero. They held on a three-speed fan (power
  factor 0.88-1.00), a phone charger (0.46), a hair straightener while heating (0.99)
  and with nothing plugged in. Absolute accuracy is therefore not claimed.
- ActiveCurrent under-reads a non-resistive load (phone charger: 42 mA against 90 mA
  RMS), so `current` uses RMSCurrent, with ApparentCurrent as a fallback; both were
  identical on every load where both answered.
- `energy` rose in 1 Wh steps, consistent with the integrated power; the raw value was
  not compared reading by reading.

Not validated: energy accuracy over hours, power-loss and reconnection behavior,
other vendors, a plug with both clusters on the OnOff endpoint, `energy_returned`.
