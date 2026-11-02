# Synthetic home integration examples

Captured on 2026-11-01 from this branch's dashboard using isolated synthetic Home
Assistant, Airzone and Overkiz services (backend `neopheus/moli-os` at `0675462`). All names, identifiers and readings are
fixtures. These pictures do not establish compatibility with physical hardware.

- [Home](home.jpg): Airzone zones, a writable HA climate and a read-only HA
  climate, plus writable and read-only lights.
- [Rooms](rooms.jpg): an io shutter with position feedback, an RTS shutter with
  movement-only controls, read-only openings and lights, and a shared Airzone
  mode/fan controller without invented temperature readings.
- [Mobile climate](mobile-climate.jpg): the same capability-driven controls in a
  narrow browser viewport; this is not a physical-iPhone test.

The adapter implementations are reviewed separately. These UI examples consume
the standard device/point contract; they do not require a specific native adapter.

## Cover regression checks after #30 (2026-11-02)

- [Desktop](cover-capabilities-desktop.jpg): semantic position on an arbitrary
  point key, read-only position with an older writable `percent_control`, partial
  RTS orders, ambiguous motor targets, legacy position display and offline cover.
- [Mobile](cover-capabilities-mobile.jpg): the same fixture in a 390×844 browser
  viewport. This is a browser layout check, not a physical-iPhone test.

These two captures use the dashboard rebased on `16f2253` with the review fixes,
served by a loopback-only synthetic API without any proxy or outbound transport.
Browser interactions recorded `OPEN` with its original casing, numeric position
`43` on the semantic point, and the two advertised RTS orders (`stop`, `close`).
No control was exposed on read-only, offline or ambiguous fixtures. No real house
configuration, identifier or credential is included.
