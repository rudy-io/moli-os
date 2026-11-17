# Alarme de la maison

- Le rôle d'Alarmo dans HA, en plus simple : des modes (`home` présence, `night` nuit,
  `away` absence), les capteurs que chacun surveille (une porte, une zone de caméra où
  quelqu'un passe), un délai pour sortir, un délai pour désarmer en rentrant, une durée de
  déclenchement.
- Elle ne fait que décider. Ce que la maison en fait (annoncer sur les enceintes, la sirène,
  prévenir les téléphones, armer à 21 h) : des automatismes qui lisent ses points `state`,
  `event` et `cause`, et écrivent `mode`.
- Armer est refusé, et dit, tant qu'un capteur du mode voit quelque chose, ou ne répond pas
  (point inconnu, appareil hors ligne). Le point `silent` nomme les capteurs qui ne répondent
  pas ; armée, un capteur qui cesse de répondre donne l'événement `silent`.
- Armée, en délai d'entrée ou déclenchée, changer de mode est refusé : ce serait la désarmer,
  et c'est `off`, avec le code, puis le nouveau mode. Pendant le délai de sortie, le mode peut
  encore changer (rien n'est encore surveillé).
- Un redémarrage ne la fait pas taire : déclenchée, elle le reste ; un délai d'entrée ou de
  sortie en cours repart en entier. Les délais se comptent sur une horloge régulière (un saut
  de l'heure au démarrage ne les change pas) ; `until` est donné en heure murale.
- Sans automatisme, une alarme déclenchée ne fait rien de visible. Point de départ :
  `automatisme-declenchee.json` (déclenchée → prévenir la maison et Telegram, avec les capteurs
  en cause), à créer par `POST /api/automations` puis à valider avec le code, ou à demander à
  Moli ; remplacer `alarme` par l'id du pilote s'il diffère.
- Réglages (modes, délais, capteurs) à l'écran (page Alarme, désarmée) : gardés par Moli dans le
  coffre de l'instance, `moli.toml` ne donne que les premiers réglages. Ils tiennent dans un
  point texte de 4 000 caractères : 32 capteurs au plus, moins avec de longs noms (refusé, avec
  la longueur).
- Conseillé dans `[guard]` (la même recommandation dans `onboarding.md`) :

  ```toml
  [[guard.code_each_time]]   # désarmer : le code, à chaque fois
  point = "alarme:maison/mode"
  values = ["off"]
  [[guard.code_each_time]]   # changer ses réglages aussi
  point = "alarme:maison/setup"
  [[guard.agents_may]]       # armer à la voix (facultatif)
  point = "alarme:maison/mode"
  values = ["home", "night", "away"]
  ```

  `agents_may` est sûr : aucun changement de mode ne la fait taire. Il est sans effet avec
  `dashboard = "pin"` (tout ordre y attend le code), et avec `protect_unassigned` tant que
  l'appareil de l'alarme n'a pas de pièce : lui en donner une.
- Rien sur le réseau ; une maison miroir la fait tourner dans l'ombre de la vraie. Ses
  automatismes n'y préviennent pas en double : Telegram, téléphones et enceintes passent par
  des ordres, que le miroir arrête ; seul le canal `maison` s'affiche, sur les écrans du
  miroir.
