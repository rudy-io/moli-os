# Moli OS

Agent-native, ultra-light home automation, written in Rust. [Version française](README.fr.md).

Home Assistant was designed for a human clicking through a UI. Moli OS is designed for
agents (Claude Code, Codex, any model behind an API) to configure, operate, debug and
eventually **write the drivers**, while never sitting in the real-time control loop. The
binary is fully deterministic.

Moli started as one family's home automation, built with AI agents, and is shared as it is:
free to use, change and redistribute (Apache-2.0). No support is promised; issues and pull
requests are welcome ([`CONTRIBUTING.md`](CONTRIBUTING.md)). The interface speaks French and
English.

Measured on 2026-10-04, next to a real Home Assistant on the same machine
(details and history: [`docs/PERFORMANCE.md`](docs/PERFORMANCE.md)):

| | Moli OS | Home Assistant |
|---|---|---|
| Memory (anonymous pages of the container) | 6.1 MiB (15.9 MiB RSS) | 2.1 GiB |
| Idle CPU | 0.36 % | 1.5 % |
| Image | 17.5 MB (`FROM scratch`) | 2.3 GB |

## What's in it

- **Hub**: typed values with units, stable identities (never derived from names), labels
  (name/room) kept apart from identity, live event bus, supervised drivers (a panicking
  driver is restarted with backoff, the core never goes down), journal of every write,
  last-known-state cache restored with its original age, encrypted secret store, a guard
  that holds agent commands in protected rooms for a human.
- **24 integrations** (`integrations/`): Zigbee2MQTT, Hue, Sonos, Tuya, Reolink, Frigate,
  Philips TV, Bambu Lab, Klipper, Tapo, IPP printers, UPnP routers, presence, Telegram,
  phones, plus declarative profiles (Daikin, Meross, Open-Meteo, Tempo, iopool…).
- **Dashboard** (Svelte 5) embedded in the binary: rooms, lights, climate, cameras, energy,
  a live floor plan in 2D and 3D.
- **Moli, the assistant**: conversation (text or voice) with an OpenAI-compatible model,
  acting under the guard.
- **Automations**: graphs drawn in a visual editor or by Moli, run by a deterministic engine,
  never active until a human validates the exact version.
- **Mobile app** (`mobile/`, Expo): the dashboard, notifications, the phone as a sensor.
- **Surfaces**, one port: REST, server-sent events, **MCP** (streamable HTTP). Host
  allowlist on every route.

## Run

```sh
git clone https://github.com/rudy-io/moli-os && cd moli-os
mkdir -p data && sudo chown 1000:1000 data   # the image runs as 1000:1000
docker compose up -d --build
docker compose logs moli-os                  # the installation code, at first start
```

Open `http://<host>:8790`, choose the language, type the installation code and choose the
house's code. Then add your devices in `data/moli.toml` (one `[[driver]]` per integration).
Guide (in French): [`docs/components/installation.md`](docs/components/installation.md).

**With an agent**: open this repository in Claude Code and ask it to install Moli: the
`installer-moli` skill (`.claude/skills/`) walks you through it, your codes and secrets
always typed by you. Other agents read [`AGENTS.md`](AGENTS.md). Once running, point an
agent at `http://<host>:8790/mcp`.

## Develop

No local Rust toolchain needed: see `AGENTS.md`. Architecture: [`docs/`](docs/README.md).

## Credits

Moli OS is an independent implementation: it contains no code from the projects below. It owes
them protocol knowledge, interoperability cross-checks or data.

- Home Assistant: the home Moli OS was built beside; the import tools read an existing installation.
- tinytuya and localtuya: the Tuya local protocol; frames cross-checked against their output.
- python-kasa: the TP-Link KLAP session, cross-checked against its output.
- pydaikin, meross_lan / MerossIot, ha-bambulab, androidtvremote2: protocol documentation.
- Zigbee2MQTT, Frigate, Moonraker: their documented APIs.
- Powercalc: measured light power profiles (see `NOTICE`).
- Weather data by [Open-Meteo.com](https://open-meteo.com/) (CC BY 4.0).

Product names are trademarks of their respective owners. Moli OS is not affiliated with or
endorsed by them, nor by the Home Assistant project.

License: Apache-2.0 (`LICENSE`, `NOTICE`).
