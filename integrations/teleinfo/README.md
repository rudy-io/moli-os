# Teleinfo (Linky TIC)

Pilote natif qui lit le compteur d'électricité Linky par sa canalée série TIC,
via une interface USB-TIC (ex. Cartelectronic USBTICLC). Lecture seule : le
compteur pousse une trame ~toutes les 10 s, Moli la parse (contrôle par ligne)
et publie les points sur l'appareil `teleinfo:<id>`.

- **Crate** : `crates/moli-teleinfo`
- **Transport** : port série 7E1 (7 bits, parité paire, 1 stop)
- **Modes** : standard (9600 baud, trames à tabulations) et historique
  (1200 baud, trames à espaces) — le mode est détecté ligne par ligne, et le
  débit est à configurer selon le réglage affiché sur l'écran du Linky
- **Appareil** : `teleinfo:linky` (id natif par défaut)

## Points

- **Puissance** (VA) : `papp` (standard), `sinsts` (TEMPO), `pmax` (souscrite)
- **Intensités** (A) : `iinst`, `irms1..3`, `isousc`, `imax`
- **Tensions** (V) : `uinst01..03`, `urms1..3`
- **Compteurs cumulés** (Wh) : `hchc`, `hchp`, `hchc1..3`, `hchp1..3` (base/HC),
  `base`, `hphc` (historique), `east`, `easf01..10`, `easd01..04` (TEMPO)
- **Texte** : `adco` (n° compteur), `adsc` (n° client), `brid`, `adt`, `ngtf`
  (type de tarif), `ptec` (période courante), `ltarf`, `optarif`, `motdetat`,
  `demain` (couleur TEMPO)

Les sous-unités téléinfo sont converties ici : PAPP/IINST/UINST/SINSTS sont
exprimées en 0,1 de l'unité affichée (`00120` PAPP = 12,0 VA), les compteurs
sont déjà en Wh. L'unité Wh est attendue par le module énergie (le facteur
Wh → kWh est appliqué de son côté).

## Configuration (`moli.toml`)

```toml
[[driver]]
id = "teleinfo"
kind = "teleinfo"

[driver.options]
port = "/dev/ttyUSB0"        # défaut ; le lien udev stable est préférable
baud = 9600                  # 9600 (standard) ou 1200 (historique)
id = "linky"                 # défaut ; l'appareil est <instance>:<id>
name = "Linky"
offline_after = 90           # s sans trame avant « hors ligne »
```

Toutes les options ont des défauts ; le pilote démarre avec `port = "/dev/ttyUSB0"`
et `baud = 9600`.

## Fixtures

- `fixtures/standard_raw.txt` : capture réelle, mode standard (TEMPO, 9600) —
  7 trames, dont 12 lignes corrompues par la ligne série (vérifiées par le
  test du pilote : le checksum rejette chaque ligne cassée sans casser la trame).
- `fixtures/legacy_raw.txt` : capture réelle, mode historique (1200) — 1 trame
  complète.
