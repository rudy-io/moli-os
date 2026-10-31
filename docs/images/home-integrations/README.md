# Synthetic home integration examples

Captured on 2026-10-31 from the actual dashboard using isolated synthetic Home
Assistant, Airzone and Overkiz services. All names, identifiers and readings are
fixtures. These pictures do not establish compatibility with physical hardware.

- [Home](home.jpg): Airzone zones, a writable HA climate and a read-only HA
  climate, plus writable and read-only lights.
- [Rooms](rooms.jpg): an io shutter with position feedback, an RTS shutter with
  movement-only controls, read-only openings and lights, and a shared Airzone
  mode/fan controller without invented temperature readings.
- [Mobile climate](mobile-climate.jpg): the same capability-driven controls in a
  narrow browser viewport; this is not a physical-iPhone test.

The native integrations and the temporary migration bridge are documented in
`integrations/airzone`, `integrations/overkiz` and `integrations/homeassistant`.
