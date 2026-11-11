# Collectes de déchets (RecycleApp)

Le calendrier des collectes d'une adresse belge, d'après RecycleApp (Fost Plus), par l'API web
publique qu'utilise recycleapp.be : aucun compte, juste l'adresse.

Un appareil, « Collectes de déchets » :

- par type de déchet connu, la prochaine collecte (`residual`, `pmd`, `organic`, `paper`,
  `garden`, `pruning`, `glass`, `textiles`, `hazardous`, `christmas_trees`, `non_recyclable`,
  `container` : une date `AAAA-MM-JJ`) et le nombre de jours qui la séparent d'aujourd'hui
  (`residual_days`… : 0 le jour même) ; le nom affiché est celui de la commune, dans la langue
  choisie ;
- `today` et `tomorrow` : ce qui est collecté aujourd'hui et demain (les noms, ou vide). Un
  automatisme « la veille à 20:00, si `tomorrow` n'est pas vide, prévenir » suffit pour sortir
  les bons sacs.

Le calendrier est relu toutes les 6 heures, 8 semaines à l'avance ; les jours sont recomptés
toutes les 15 minutes. Une collecte annulée et remplacée par un autre jour n'apparaît qu'à son
nouveau jour.

Les types reconnus sont ceux du composant RecycleApp de Home Assistant (par l'identifiant du
logo que l'API donne à chaque fraction), plus les petits déchets dangereux ; un type inconnu est
laissé de côté.

Fixtures : forme des réponses réelles, valeurs fictives.
