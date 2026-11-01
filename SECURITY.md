# Security

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private vulnerability reporting (the
**Security** tab of the repository, **Report a vulnerability**). You will get an answer as soon
as possible; Moli is maintained on spare time, with no service-level promise.

## What Moli trusts, and what it does not

Moli runs a home: knowing its trust model helps you install it safely and find what matters.

- **The home network is trusted by default, the bedrooms are not.** In the default `trusted`
  dashboard mode, a device on the local network commands the house without a code, like a light
  switch on the wall: anywhere but in a protected room, on a machine (a computer, a print at
  work), or to take a device out of a protected room. There, the house code is needed, because
  any program on the network can claim to be the dashboard: only the code proves a person
  (typed once, a session lasts 30 days on that browser). The code is also needed to approve
  what commits the house: an automation, an order an agent asked for. Set `[guard] dashboard =
  "pin"` to require the code for every command: from the dashboard, an agent, a script or the
  command line alike, the order waits for a person (only automations a person approved still
  run on their own).
- **Opening the house always takes a person.** Unlocking a lock, opening a garage door or a
  gate waits for a person's approval, whatever the room and whatever the dashboard mode: from an
  agent, a script, the command line, the house model, and from the dashboard without a code
  (directly, or by running a routine that would). Closing a gate goes through, and so does
  locking a lock known by its point (a Zigbee lock's `LOCK`/`UNLOCK` state); a lock known by its
  name only (a strike on a relay) is held both ways. Failing a point, a device is a lock
  or a gate by a whole word of its name (« serrure », « verrou », « lock », « portail », « gate »,
  « porte de garage »), its own name and the name it was given both counting. « Garage » alone is
  not a gate (« Prise garage »), and a light opens nothing: name a garage door « Porte de garage »
  for the rule to hold it.
- **Agents never act freely.** Orders from agents (MCP), scripts (REST), the command line, and
  anything that only claims to be the dashboard go through the guard: in protected rooms (and,
  for agents and scripts, during quiet hours; in `pin` mode, everywhere and always) they wait
  for a person. An automation written by
  an agent stays a draft until a person approves its exact version. A change of configuration the
  house model proposes (renaming, moving or hiding a device, renaming a room, adding a person,
  the house's position) only applies when a person confirms it with the code. Correcting one of
  its answers (« Pas ça »), and the rules it learns from that, are a person's too (the code); a
  learned rule answers through the same guard as the model, and what people corrected stays in
  the house (`docs/PRIVACY.md`).
- **Per-caller limits count each address of the home network as a caller.** Write budgets,
  live streams (8 per caller, 64 in all) and orders waiting for a person (32 per caller, 50 in
  all) are counted per caller: on the home network, its address (for waiting orders, the person
  when Moli knows them). A machine there that takes several addresses (any free one on the
  network; many more over IPv6 if Moli listens on it) counts as that many callers: its write
  budgets add up, it can take every live stream (dashboards then go without live updates) and
  push out the orders others left waiting, never approve one. Guessing the code gains nothing
  from it: the home network, this machine and Cloudflare Access share one budget of 20 wrong
  codes an hour. A few addresses do spend that budget, and codes are then off for the household
  for the hour (sessions already open keep working; the house is told). These limits tame a
  misbehaving client, not a hostile one: a malicious device on the home network is out of scope
  (below).
- **Browsers**: a write sent from another site's page (by its `Origin` or `Sec-Fetch-Site`) is
  refused, and no page can show the dashboard inside a frame.
- **A demo house** (`[server] demo = true`) lets anyone play with it, from anywhere, and keeps
  nothing a visitor writes (names, plan, routines, people, code).
- **Guests** see and command their rooms only: no other room, no camera, no one's location, no
  journal, no assistant, no setting, and no phone of their own (notifications go to the
  household's people only). Switching the voice satellites' recording on, and hearing it, takes
  the code. Once their stay is over, or once a person is removed (or an e-mail taken off their
  card), Moli treats that e-mail as a stranger's even if the Cloudflare Access rule still lets
  it in. With `[server.access] known_only = true`, Moli lets in only the e-mails of its people.
  A guest is recognized only signed in (password) or through Cloudflare Access: on the home
  network without a session, a guest is the household like any device there. These limits
  therefore hold for guests who come from outside; at home, give them a separate guest Wi-Fi.
- **Pairing a phone** takes a person: the code, or someone Moli knows by Cloudflare Access or a
  password (never a guest), or this very machine. A paired phone receives the house's
  notifications from anywhere. The house is told of every phone paired, with who or which
  address paired it.
- **The assistant's model** (when a household sets one up, with its own key) is never given
  anyone's precise location or Wi-Fi name, and its key never travels over plain HTTP.
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
- **Dependencies** are checked against the RustSec advisory database on every change and every
  week (`cargo audit`). An advisory is ignored only when it cannot reach Moli, with the reason
  written in `.cargo/audit.toml`.

Out of scope: a compromised host machine, a malicious device on a network you declared trusted,
physical access to the data folder together with its master key.
