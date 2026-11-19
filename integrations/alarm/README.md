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
  pas ; armée, un capteur muet depuis une minute donne l'événement `silent` (pas pour une
  coupure de quelques secondes, ni à l'arrêt de Moli).
- Une fois un mode choisi (délai de sortie, armée, délai d'entrée, déclenchée), en changer est
  refusé : ce serait l'affaiblir ou la faire taire, et c'est `off`, avec le code, puis le nouveau
  mode. Le délai de sortie ne repart jamais.
- Un redémarrage ne la fait pas taire : déclenchée, elle le reste ; un délai d'entrée ou de
  sortie en cours repart en entier. Sa phase est gardée dans le coffre à chaque changement :
  une coupure de courant juste après l'armement ou le déclenchement ne la perd pas. Un coffre
  qui refuse l'écriture (disque plein, en lecture seule) est réessayé toutes les 5 s, pas à
  chaque événement de la maison. Sans coffre (pas de clé), seul le cache d'état, écrit chaque
  minute, la retrouve : Moli le dit au démarrage. Un capteur
  actif avant le redémarrage compte de nouveau dès qu'une valeur fraîche le dit (jamais sur le
  cache seul). Les délais se comptent sur une horloge régulière (un saut
  de l'heure au démarrage ne les change pas) ; `until` est donné en heure murale.
- Sans automatisme, une alarme déclenchée ne fait rien de visible. Point de départ :
  `automatisme-declenchee.json` (déclenchée → prévenir la maison et Telegram, avec les capteurs
  en cause) et `automatisme-capteur-muet.json` (armée, un capteur ne répond plus depuis une
  minute → prévenir : débrancher ou brouiller un capteur, c'est ce qu'un intrus ferait), à créer par `POST /api/automations` puis à valider avec le code, ou à demander à
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
