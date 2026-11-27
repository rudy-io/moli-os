# Ecovacs Deebot

Direct connection to the Ecovacs Italy cloud with the EU device portal. The
driver queries battery, work state and charging state of X8 PRO OMNI devices
over HTTPS. Start, stop, pause, resume and return to dock are optional guarded
controls. The default is read-only.

Pairing uses the app's account login and, when prompted, its email verification.
The vault keeps the resulting account session. The email, password and code
are used during pairing and excluded from the vault and logs. If Ecovacs
revokes the session, pair again. The read interval defaults to 30 seconds.

Protocol references: [Ecovacs sign-in and verification](https://github.com/home-assistant/core/issues/194725),
[DeebotUniverse client](https://github.com/DeebotUniverse/client.py) for token
exchange, device discovery, X8 protocol class IDs and command payloads.
These are private vendor APIs. Maps, zones, station settings and other
models are outside this driver. See [onboarding](onboarding.md).
