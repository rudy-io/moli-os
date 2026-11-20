# Multiprises Meross (MSS425, MSS620)

Multiprise ou prise double, par son API locale signée (HTTP, sans cloud à l'exécution) : `on`
(canal 0 : toutes les sorties) et `on_1` à `on_5` (chaque sortie, les ports USB en dernier),
lus (`Appliance.System.All`) et écrits (`SET` `Appliance.Control.ToggleX`, fait seulement sur
`SETACK`, puis relus). Pas de mesure.

- Réutilisé : le protocole décrit par meross_lan et merossiot (licence MIT), aucun code copié.
- **Lu sur de vrais appareils** : MSS425F en firmware 4.2.3, MSS425FC en 7.3.4. Le canal 0 se
  lit allumé alors que des sorties sont éteintes : il ne vaut donc pas « toutes allumées ».
  Ces modèles n'annoncent pas `Appliance.Encrypt.ECDHE`, donc leurs ordres partent en clair.
  Aucun ordre n'y a encore été essayé.
- `fixtures/all.json` n'est pas une capture : elle est construite d'après les descriptions de
  la communauté (six canaux), avec des identifiants fictifs.
- Une sortie que l'appareil n'a pas (une prise double n'a que `on_1` et `on_2`) reste sans
  valeur, et un ordre dessus est refusé par l'appareil.
- Reste : un essai réel, et le numéro de canal des ports USB selon le modèle.
