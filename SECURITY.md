# Security

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private vulnerability reporting (the
**Security** tab of the repository, **Report a vulnerability**). You will get an answer as soon
as possible; Moli is maintained on spare time, with no service-level promise.

## What Moli trusts, and what it does not

Moli runs a home: knowing its trust model helps you install it safely and find what matters.

- **The home network is trusted by default.** In the default `trusted` dashboard mode, a device
  on the local network commands the house without a code, like a light switch on the wall. The
  house code (a human session) is needed to approve what commits the house: an automation, an
  order an agent asked for in a protected room or during quiet hours. Set `[guard] dashboard =
  "pin"` to require the code for every command.
- **Agents never act freely.** Orders from agents (MCP), scripts (REST) and the command line go
  through the guard: in protected rooms and quiet hours they wait for a person. An automation
  written by an agent stays a draft until a person approves its exact version.
- **Never expose Moli to the Internet directly**, and never put a reverse proxy on the same
  machine in front of it (every request would look local; Moli treats a relayed local request
  as coming from outside). For remote access: a VPN subnet router, or Cloudflare Access, whose
  signed identity Moli verifies itself (`[server.access]`).
- **DNS rebinding**: only the names configured in `allowed_hosts`, `localhost` and addresses
  typed as such are accepted in `Host`.
- **Secrets** (device keys, tokens, the house code) live in an encrypted store
  (ChaCha20-Poly1305), keyed by a master key given by the environment or, on a new house,
  written to `data/master.key` (mode 0600, never in a backup). Nothing secret goes in
  `moli.toml`.
- **The installation code** of a new house is printed once in the logs, is good for one hour,
  and opens only a house that never had a code. Whoever reads the machine's logs can install
  it: finish the installation right after the first start.

Out of scope: a compromised host machine, a malicious device on a network you declared trusted,
physical access to the data folder together with its master key.
