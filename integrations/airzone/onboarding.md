# Installation / Onboarding

1. FR : Vérifier une passerelle compatible API locale, son adresse et son port.
   EN: Verify a Local API controller, its host and port. Cloud-only equipment
   is not made locally compatible by adding this driver.
2. FR : Créer une instance par passerelle, `read_only = true` explicite pour cet essai,
   puis `moli-os check-config`. EN: Configure one instance per controller with
   read-only explicitly enabled for this trial, validate configuration, then restart Moli.
3. FR : Comparer les systèmes/zones et les capacités publiées avec l'installation.
   EN: Compare discovered systems/zones, units, ranges and capabilities with the
   installation. Assign Moli rooms; protect the relevant rooms. A new zone already
   commissioned in Airzone appears through polling, not a pairing operation.
4. FR : Dans Atelier, consulter les points de la passerelle, des systèmes, de
   l'eau chaude et de la qualité de l'air. EN: Inspect the optional resources and
   section availability in Atelier. Use the read-only `airzone-inspect` commands
   in [README](README.md) for endpoint diagnostics; demo is explicitly synthetic.
5. FR : Si l'eau chaude n'annonce pas d'unité, vérifier celle du matériel avant
   de définir `dhw_units` (0 Celsius, 1 Fahrenheit). EN: Do not infer missing DHW
   units from another zone. Set an explicit verified unit only when needed.
6. FR : Pour HTTPS, choisir le port et une empreinte SHA-256 vérifiée.
   EN: HTTPS uses an explicit certificate fingerprint; a changed certificate
   requires deliberate verification and configuration, never an automatic bypass.
7. FR : Conserver la lecture seule pendant les essais. EN: For a separately
   authorized actuation qualification only, explicitly opt in with
   `read_only = false`, review every shared scope, use a neutral zone with a human
   present, then restore its original state. Never test protected rooms.
8. FR : Qualifier séparément les coupures, reprises, horaires constructeurs et
   compatibilité du firmware. EN: Synthetic protocol tests do not establish
   physical execution, long-running reliability or all firmware compatibility.
