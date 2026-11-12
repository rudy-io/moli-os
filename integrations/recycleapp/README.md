# Collectes de déchets (RecycleApp)

Le calendrier des collectes d'une adresse belge, d'après RecycleApp (Fost Plus), par l'API web
publique qu'utilise recycleapp.be : aucun compte, juste l'adresse.

**Vie privée** : le code postal, la rue et le numéro **quittent la maison** vers Fost Plus
(`api.fostplus.be`), à chaque lecture du calendrier (toutes les 6 heures). Le pilote n'envoie
l'adresse qu'en TLS, et seulement aux domaines `recycleapp.be` et `fostplus.be`, quoi que dise
le fichier de réglages du site.

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
toutes les 15 minutes. Après 3 échecs de suite, ou un calendrier vieux de plus d'un jour,
l'appareil passe indisponible et le pilote le signale. Une adresse connue sans aucune collecte
annoncée laisse le pilote en attente, avec un message. Un code postal partagé par plusieurs
communes : la commune retenue est celle où la rue existe. Une collecte annulée et remplacée par un autre jour n'apparaît qu'à son
nouveau jour.

Les types reconnus sont ceux du composant RecycleApp de Home Assistant (par l'identifiant du
logo que l'API donne à chaque fraction), plus les petits déchets dangereux ; un type inconnu est
laissé de côté.

Fixtures : forme des réponses réelles, valeurs fictives.
