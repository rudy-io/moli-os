# Installation / Onboarding

1. FR : Allumer une passerelle compatible API locale, sur le réseau de Moli.
   EN: Power on a Local API controller on Moli's network. Cloud-only equipment
   is not made locally compatible by adding this driver.
2. FR : Dans **Système → Ajouter une passerelle**, lancer la recherche, choisir
   une passerelle, puis confirmer son ajout dans Moli. Le code Moli protège
   l’accès à la recherche et à l’installation. Une passerelle déjà ajoutée est
   signalée ; la sélection seule n’ajoute rien. L’enregistrement démarre la
   lecture des zones sans fichier à modifier.
   **Saisir une adresse** reste disponible si la recherche ne trouve pas le
   matériel. L’adresse, le port et l’empreinte HTTPS se règlent alors manuellement.
   EN: In **System → Add a gateway**, search, choose a controller and explicitly
   confirm its read-only addition. The Moli code protects discovery and setup.
   Existing controllers are identified; selecting a result does not install it.
   Use the manual-address fallback if discovery cannot reach the controller.
   Manual configuration remains available for advanced deployment:
   one instance per controller, `read_only = true`, then `moli-os check-config`
   and restart. Do not configure the same gateway through both paths.
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

## Limites de la recherche / Discovery limits

FR : La recherche est ponctuelle et bornée. Elle utilise les adresses privées
déjà connues de Moli pour explorer les réseaux locaux correspondants et vérifie
les réponses avec l’API Airzone. Un résultat vide ne prouve pas l’absence de
matériel : une installation sans indice réseau, un réseau isolé ou une API locale
désactivée nécessitent la saisie manuelle. Un appareil exclusivement cloud ne
devient pas compatible avec l’API locale grâce au scan. La recherche ne commande,
n’ajoute et ne réactive aucun appareil.

EN: Discovery is an on-demand, bounded search based on private addresses already
known to Moli, with responses verified through the Airzone API. Empty results
are not proof that no device exists: an empty installation without network hints,
an isolated network or a disabled Local API may require the manual fallback.
Scanning cannot make cloud-only hardware locally compatible. Discovery never
commands, installs or reactivates a device.
