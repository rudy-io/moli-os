# Klipper / Moonraker

- Origine : non notée dans le code. Le profil cite comme cibles Snapmaker U1, Voron, Prusa
  sous Klipper.
- `fixtures/objects_query.json` : la réponse de `objects/query` des tests (nom de fichier
  abrégé en `a.gcode`) ; `profils.md` la dit réelle, sans plus de détail. `printer/info`
  (identité `hostname`) n'a pas de fixture.
- A tourné sur un vrai Snapmaker U1 (port 7125) ; remplacé depuis par le pilote natif
  (paquet `klipper`) pour qui veut plus que l'état. Lecture seule : aucun contrôle de
  l'impression.
