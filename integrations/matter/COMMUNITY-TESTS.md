# Validations matérielles Matter — guide communautaire

Ce guide permet de compléter les validations sans confondre simulation, revue
de protocole et essai physique. Ne publiez jamais de code d'appairage, PIN,
clé, certificat privé, SSID, adresse publique ni capture brute contenant ces
éléments. Anonymisez les journaux avant partage.

## Préparer un essai

- Utiliser une instance de test et un appareil non critique, avec autorisation
  de son propriétaire. Garder à portée son contrôleur habituel pour le retirer
  du fabric Moli si nécessaire.
- Vérifier IPv6 local, mDNS, UDP dans les deux sens, pare-feu et route IP.
  Pour Thread, ajouter un routeur de bordure fonctionnel et sa route IPv6.
- Suivre [l'installation Matter](README.md), [le parcours Thread](thread.md)
  et la reproduction [E2E virtuelle](../../tools/matter-e2e/README.md).
- Commencer par une commande réversible, noter l'état initial, puis le restaurer.
  Un timeout après envoi signifie « résultat inconnu » : vérifier l'appareil
  avant toute nouvelle commande.
- Archiver version de Moli, version du firmware et résultat de chaque étape.
  Joindre uniquement des journaux expurgés; ne pas activer de traces TLV brutes
  pour un réseau réel.

## Matrice

| Famille / scénario | Prérequis et procédure | Attendu | Précautions et rapport | Validation actuelle |
|---|---|---|---|---|
| Lampes Matter On/Off | Lampe test, appairée au fabric Moli; lire l'état, basculer On puis Off et restaurer l'état initial. | État issu du rapport Matter, commande liée à la capacité annoncée. | Ne pas tester sur une pièce protégée; relever commandes disponibles et erreurs sans setup code. | **Hue v2 réel** : On/Off REST/Matter, état confirmé et restauré. Autres marques non testées physiquement. |
| Prises et interrupteurs | Charge non critique; commande On/Off puis redémarrage Moli. N'utiliser aucune charge moteur, chauffage ou médicale. | État et disponibilité cohérents; reprise du même identifiant. | Indiquer le type de charge sans données personnelles; vérifier la perte d'alimentation de la charge avant test. | Fixtures E2E / replay seulement; matériel non validé. |
| Variation et couleur | Lampe compatible; vérifier métadonnées de niveau/couleur, luminosité positive, 0 %, puis positive, CT et RGB selon modes annoncés. | 0 % éteint uniquement par commande compatible; valeur positive rallume par commande WithOnOff ou séquence On + niveau si annoncée; états relus par abonnement. | Commencer à faible luminosité; ne pas inférer une fonction absente. Vérifier restauration de l'état initial. | E2E indépendant (dimming, XY/RGB, CT); Hue réel seulement On/Off. |
| Volets / stores | Actionneur de test sans personne ni objet dans la course; lire position, ouvrir/fermer/stop puis restaurer. | Position observée, conversion lift conforme (100 % ouvert); commandes seulement si découvertes. | Volontaire informé, dégagement physique et arrêt d'urgence accessibles. Ne pas tester sur une fenêtre en hauteur ou issue. | E2E virtuel pour position/open/close/stop; aucune validation physique. |
| Thermostats | Thermostat virtuel ou circuit de test isolé; comparer limites rapportées, modes, deadband et consignes heat/cool. | Écritures absolues dans les limites; clés chaud/froid fixes; états mesurés distincts des consignes. | Ne pas piloter chaudière, pompe à chaleur ou système occupé pendant la validation. | E2E virtuel (heat/cool/auto); aucune validation physique. |
| Serrures | Serrure de banc ou dispositif non installé; appareil maintenu sous contrôle manuel; commandes par personne autorisée avec approbation. | Contrôle uniquement si politiques l'autorisent; commande temporisée; état relu; refus sans transmission si PIN/politique inconnu. | **Réservé aux volontaires informés**. Exclure portes d'entrée, issues, systèmes de sécurité et locaux occupés. Ne jamais transmettre PIN. | E2E virtuel : approbation, refus sans émission, politique PIN réactive, lock/unlock temporisés; aucune validation physique. |
| Capteurs | Capteur non critique; déclencher les états connus puis rendre l'échantillon invalide/null si possible. | Mesure typée; null invalide efface l'ancienne valeur; pas de zéro synthétique. | Pour fuite/fumée/sécurité, essai uniquement selon consignes constructeur, sans remplacer l'alarme certifiée. | Fixtures de descripteurs/replays; aucune validation physique annoncée. |
| Pont multi-endpoints | Pont Matter avec plusieurs enfants; vérifier noms, Reachable, commande d'un enfant, déconnexion/réapparition d'un enfant. | Identité stable par endpoint; un endpoint injoignable reste indisponible; les autres continuent. | Relever nombre d'endpoints et erreurs de tampon; aucun seuil de capacité universel n'est garanti. | Hue v2 : découverte/état de lampes bridgées; On/Off réel d'un enfant. E2E virtuel limité. |
| Matter-over-Thread | Appareil Thread, routeur de bordure déjà opérationnel; commissionner d'abord avec contrôleur existant, partager le fabric, puis joindre via IPv6. | Moli découvre et pilote par Matter/IP après appairage; reprise du fabric conservée. | Le Bluetooth natif et création/configuration du réseau Thread ne sont pas implémentés. Ne publier aucun dataset Thread. | Documentation et simulation IP seulement; Thread radio non testé. |
| Redémarrage Moli | Appareil appairé; redémarrer seulement Moli sans fournir de nouvel code. | Coffre déchiffré, même identité/fabric, endpoints de nouveau disponibles. | Ne pas supprimer fabric avant d'avoir validé l'accès au coffre. | E2E virtuel + reprise réelle du pont Hue rapportés dans [HARDWARE.md](HARDWARE.md). |
| Reconnexion réseau | Accès à un réseau/lab isolé; interrompre puis rétablir uniquement le lien du banc. | Offline pendant la coupure, reprise subscription et disponibilité; délais de retry bornés. | Ne pas modifier pare-feu de production; noter qui a simulé la coupure et sa durée. | Reconnexion logicielle exercée en E2E; réseau matériel non validé. |
| Perte d'alimentation | Banc isolé avec appareil non critique et restauration électrique maîtrisée. | Fabric/identité persistent si le produit le promet; valeurs observées après reprise, sans supposer la restauration des états. | Ne pas couper une alimentation dont dépend une personne ou un service. Une coupure peut endommager un appareil; volontaire informé uniquement. | Non testé physiquement; pas de revendication de reprise après coupure. |

## Modèle de rapport de compatibilité

Copier et remplir; écrire « non testé » plutôt que supposer une réussite.

```text
Appareil / fabricant :
Modèle exact :
Firmware :
Version / commit Moli :
Connexion : Ethernet / Wi-Fi / Thread (préciser)
Routeur de bordure et firmware (Thread uniquement) :
Contrôleur ayant réalisé le premier appairage :
Capacités annoncées (si disponibles, sans identifiant sensible) :
Étapes testées : découverte / lecture / écriture / abonnement / redémarrage /
                 reconnexion / reprise après coupure
Résultat par étape : PASS / FAIL / non testé + résultat observé
État initial restauré : oui / non / sans objet
Journaux anonymisés joints : oui / non
Limitations ou conditions de reproduction :
Secrets supprimés avant partage : oui / non
```

Un rapport indique uniquement le résultat de cet appareil, ce firmware et ces
conditions. Il ne certifie ni toute une marque, ni toute la norme Matter.
