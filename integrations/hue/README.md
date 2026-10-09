# Philips Hue

- Origine : rien de noté (API CLIP v2 locale du pont : `clip/v2/resource`, flux SSE).
- `fixtures/resources.json` : le chargement complet d'un pont réel, rejoué par les tests de
  `crates/moli-hue/src/model.rs` (pièces, zones, boutons, capteurs).
- Certificat épinglé au premier contact, avant toute clé.
- Reste : pas de découverte (adresse donnée) ; les zones restent en lecture seule (une zone
  peut chevaucher une chambre protégée).
