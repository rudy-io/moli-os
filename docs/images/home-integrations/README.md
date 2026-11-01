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
