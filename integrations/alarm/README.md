# Alarme de la maison

- Le rôle d'Alarmo dans HA, en plus simple : des modes (`home` présence, `night` nuit,
  `away` absence), les capteurs que chacun surveille (une porte, une zone de caméra où
  quelqu'un passe), un délai pour sortir, un délai pour désarmer en rentrant, une durée de
  déclenchement.
- Elle ne fait que décider. Ce que la maison en fait (annoncer sur les enceintes, la sirène,
  prévenir les téléphones, armer à 21 h) : des automatismes qui lisent ses points `state`,
  `event` et `cause`, et écrivent `mode`.
- Armer est refusé, et dit, tant qu'un capteur du mode voit quelque chose.
- Rien sur le réseau ; une maison miroir la fait tourner dans l'ombre de la vraie.
