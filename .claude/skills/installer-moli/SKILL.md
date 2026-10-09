---
name: installer-moli
description: Install Moli OS in someone's home and bring their devices in, step by step, with them at the keyboard. Use when the person wants to install Moli, add a device or an integration, move over from Home Assistant, or fix an installation that does not start. Not for changing Moli's own code (read AGENTS.md for that).
---

# Installing Moli at home

You guide a person who installs Moli OS on a Linux machine of their own. They decide, they type their own codes and secrets; you prepare, explain and check. Speak their language. Read `docs/components/installation.md` first: it is the reference, this skill is how to walk someone through it.

## Rules that never bend

- **Never ask for a secret in the chat**: not the house code, not a password, not an API key, not a device's local key. Secrets go through a pipe the person types themselves: `read -rs V; printf %s "$V" | docker compose exec -T moli-os /moli-os secrets set <instance> <name>`. Nothing secret in `data/moli.toml`, ever.
- **The house code is theirs**: they choose it and type it on the welcome screen. You may read the *installation code* from the logs (one use, one hour, local) and tell them; never the house code.
- **A device order is a real effect in an inhabited home.** Never test in a protected room (a bedroom where someone sleeps). Use a neutral target, then put it back as it was. Agents' orders in protected rooms or quiet hours wait for a person: that is the guard working, not a bug.
- **Never open port 8790 to the Internet**, and never put a reverse proxy on the same machine in front of Moli (everyone would look local). Remote access: a VPN subnet router or Cloudflare Access (see the installation guide).

## 1. Look before installing

Check, and say what you found:
- `docker compose version` works (else: install Docker Engine and its compose plugin for their distribution, from Docker's documentation).
- The machine stays on, has a few hundred MB free, and is on the home network (Moli needs to see the devices: `network_mode: host`).
- `uname -m`: x86-64 and ARM64 both build (the image builds on the machine itself).

## 2. Start

```sh
mkdir -p data && sudo chown 1000:1000 data
docker compose up -d --build
docker compose logs moli-os
```

In the logs: the starter configuration is written, the master key created (`data/master.key`), and an installation code printed (`code=ABCD-EFGH`). Tell them the code and the address to open: `http://<this machine's address>:8790`. They pick the language, type the installation code, choose the house code (6 digits at least) twice. Done: the dashboard opens.

Then, right away: **they keep a copy of `data/master.key` somewhere safe** (a password manager). Backups never contain it, and without it the vault cannot be read.

## 3. Bring the devices in, one integration at a time

1. Ask what they have (brands, models, bridges: Zigbee dongle with Zigbee2MQTT, Hue bridge, Sonos, Tuya plugs, cameras…). The catalogue is `integrations/`: one folder per integration, each with `integration.toml` (what it covers, how it is found on the network) and `onboarding.md` (exactly what to ask and do).
2. For one integration: follow its `onboarding.md`, add its `[[driver]]` block to `data/moli.toml` (model: `moli.example.toml`), secrets through the pipe above, then `docker compose restart moli-os`.
3. Check: `docker compose logs moli-os` (the driver starts or says what it waits for), the dashboard (devices appear in their rooms), `curl -s http://127.0.0.1:8790/api/health` (each driver's state).
4. Only then the next one.

## Coming from Home Assistant

Home Assistant keeps running: the move is progressive. `tools/home-assistant/` reads an existing installation (rooms, automations as drafts to approve, energy history, Tuya local keys): each script says how to run it. Automations imported are **drafts**: nothing runs before a person approves it in the dashboard.

## A device Moli does not know yet

1. Look for its local API (documentation, community projects): what it answers, on which port.
2. Write a declarative profile (`docs/components/profils.md`): a TOML file, no code. Check it: `docker compose exec moli-os /moli-os profile check /data/<file>.toml`; the person approves it: `… profile trust /data/<file>.toml` (only approved profiles run).
3. Test against the real device, read-only first. Then writes, on a neutral target, with the person watching.
4. It works? Suggest they share it: a pull request with the profile, a capture of real answers made anonymous (no serial, no MAC, no address: `scripts/leak-check.sh` checks), and its `onboarding.md`.

## When it does not start

- `docker compose logs moli-os`: Moli says what is wrong, in the house's language once chosen.
- `docker compose exec moli-os /moli-os check-config`: the configuration as Moli reads it.
- Forgotten house code: the installation guide, section « Changer ou retrouver le code ».
