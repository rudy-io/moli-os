# Ecovacs Deebot

Direct connection to the Ecovacs Italy cloud with the EU device portal. The
driver queries battery, work and charging state, suction, cleaning mode,
mop water flow (0–50), and whether the mop is fitted. It reports station
faults, including empty clean water and full dirty water tanks, as separate
binary points usable in automations. An unanswered status request does not
clear a previous alert. The default is read-only.

With writes enabled, start, stop, pause, resume, return to dock, suction,
cleaning mode, and water flow are guarded controls on the dashboard's robot
card. Changing a setting uses a separate command from starting a clean.

Pairing uses the app's account login and, when prompted, its email verification.
The vault keeps the resulting account session. The email, password and code
are used during pairing and excluded from the vault and logs. If Ecovacs
revokes the session, pair again. The read interval defaults to 30 seconds.

Protocol references: [Ecovacs sign-in and verification](https://github.com/home-assistant/core/issues/194725),
[DeebotUniverse client](https://github.com/DeebotUniverse/client.py) for token
exchange, device discovery, X8 protocol class IDs and command payloads.
These are private vendor APIs. Maps, zones, station settings and other
models are outside this driver. See [onboarding](onboarding.md).

On 2026-11-27, an X8 PRO OMNI on the Italy cloud returned suction, cleaning
mode, water flow, mop presence, and an empty fault list. The anonymized
response fields are in [the settings fixture](fixtures/x8-settings.json).
The empty and full tank branches are exercised with simulated alerts.
