# Mise en route / Setup

Lire le README et revoir les effets de chaque entité avant toute écriture.
Read the README and review each entity's actual effects before enabling writes.

Example (fictitious endpoint and identifiers only):

```toml
[[driver]]
kind = "homeassistant"
id = "ha-migration"
[driver.options]
host = "bridge.example.org"
port = 8123
transport = "http" # explicit plaintext choice
poll_seconds = 15
[[driver.options.entities]]
native_id = "climate-1"
entity_id = "climate.example"
domain = "climate"
room = "Workshop"
safety = "ordinary"
writable = [] # optionally ["target_temperature"] after review
turn_on_mode = "heat"
```

Use `transport = "pinned_https"` for TLS and set `cert_sha256` to the verified
certificate fingerprint (64 hexadecimal characters, no colons). No discovery or
initial credential-bearing certificate learning takes place.

Créer le jeton depuis un **utilisateur HA dédié à Moli, non administrateur** : un jeton
longue durée a tous les droits de son utilisateur, et le pont reste expérimental.
Create the token from a **dedicated, non-admin HA user**: a long-lived token
carries all of its user's rights.

Stocker le jeton HA dans le coffre de l'instance, clé `token`, par le mécanisme
`moli-os secrets set` et un tube. Ne jamais le mettre dans le TOML, une commande
visible, un journal ou une variable de modèle. Store the HA token through a pipe
into the instance's vault key `token`, never in configuration or logs.

Also store vault key `token_endpoint`, calculated as:

- HTTP: `http://<lowercase-host>:<port>/#`
- pinned HTTPS: `https://<lowercase-host>:<port>/#<lowercase-64-hex-pin>`

This is `Config::token_endpoint()`. For the example, it is
`http://bridge.example.org:8123/#`. It binds the token to exactly that endpoint.
After endpoint/certificate changes, an operator must verify it and explicitly
replace this binding along with the intended token; the old credential is never
forwarded automatically. Restart to load newly stored vault values.

Run `check-config` first; construction validates without network requests. With
no usable vault/token/binding, the driver waits for the operator and makes no
request. Start read-only, inspect availability and typed readings, then authorize
specific point keys on reviewed entities only. Do not enable writes on HA aliases,
custom templates or automation triggers until all downstream effects are known.
