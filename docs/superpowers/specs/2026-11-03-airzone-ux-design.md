# Airzone observation experience

The generic workshop exposes every parameter at the same visual level. The family needs to find a zone, read its temperature and target, and distinguish an enabled zone from a reported heating/cooling demand. This change adds a focused, read-only Airzone experience without changing the driver or command path.

The official Airzone Cloud screenshots provide the reference for compact zone summaries and a focused temperature/humidity detail screen: https://play.google.com/store/apps/details?id=es.altra.airzone&hl=fr. Retain Moli's existing porcelain/night themes, Figtree, icons and shared formatters. Do not copy branded artwork or imply cloud schedules, energy or pairing are available locally.

## Scope

- A lazy `#/airzone` Maison page, visible in navigation when a native Airzone driver is configured, plus a direct entry from Atelier.
- Zones grouped by actual driver instance and system number; search by displayed name and room, and a system filter. No guessed floor names, combined averages or HA/native deduplication by similar names.
- Compact zone cards: measured temperature, target, reported mode, optional humidity, independent availability/problem/activity signals.
- A linkable zone detail route with large readings and existing point history. Additional parameters are grouped into native disclosures. Shared controls are associated only through same-instance, full-ID membership.
- An installation view separates gateway/system diagnostics and optional resources from everyday zone readings. Unsupported or unknown capabilities are explained without inventing values.
- This view is observational even if a device advertises writable points. It introduces no command, registration, pairing, refresh, or configuration calls; it consumes the existing SSE hub.

## Data and interaction rules

Native identity is determined by joining the public device instance to a driver of kind `airzone`. Labels are presentation only. Hidden devices are excluded before grouping and counting. Unknown systems remain unknown; malformed/deleted detail routes display a clear return path.

Use declared point types and units. Retained offline readings are explicitly described as the last readings. `on` means enabled, not heat production. Only reported demand flags imply demand; never infer it from the temperature gap. Unknown or absent values stay unknown. Empty integration text is a valid unset value. Each additional point appears once and unknown fields remain inspectable.

## Validation

Pure-model tests cover multiple instances, HA lookalikes, hidden devices, explicit membership, missing data, offline/null availability, °C/°F/unknown units, demand semantics and complete section coverage. SSR tests use synthetic devices and assert semantic links, translated summaries and absence of command controls even with writable fixtures. Check the real read-only preview in desktop and phone-sized layouts, keyboard navigation, filtering, back navigation, detail/history and installation disclosures. Never publish private house data or screenshots in GitHub.

No new dependency, poller, backend route or persisted setting. Run the existing full checks before committing, build a committed image, deploy only the test container and preserve its secrets, labels, read-only configuration and rollback image.


## Raffinement de la navigation et des fiches

Les vues conservent les surfaces, la typographie et les icônes Moli. La navigation
Zones / Alertes / Appareils / Installation est regroupée ; l’ajout d’une passerelle
reste accessible dès l’aperçu. Les filtres combinent recherche, système et activité
réellement signalée, sans déduire une demande de l’activation ou d’un écart de
température. Une coupure du flux rend le filtre de demande indisponible ; elle ne
transforme pas tous les appareils en panne matérielle.

Sur téléphone, une seule colonne garde visibles les états et les mesures ; la
recherche et les filtres occupent moins de hauteur. La fiche distingue mesure et
consigne, donne accès à leur historique, puis présente le thermostat et les
demandes. Les paramètres complets et réglages communs restent dans des sections
repliées. Une double consigne conserve ses deux valeurs. « Dernier changement »
désigne le timestamp du point et ne prétend pas dater la dernière interrogation.

Les cartes ont un nom accessible explicite, les filtres un état pressé et les
cibles interactives mesurent au moins 44 px. L’effacement des filtres restaure le
focus sur la recherche ; la fermeture de l’historique le rend à la mesure. Les
transitions respectent la réduction de mouvement. Aucun matériau translucide,
polling, dépendance ou chemin de commande supplémentaire n’est introduit.
