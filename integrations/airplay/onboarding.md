# Onboarding : AirPlay 1 (annonces)

1. Demander l'adresse IP du récepteur (fixe : elle sert d'identité).
2. **Ne proposer que les bons** : `_raop._tcp` répond aussi pour les HomePod, les Apple TV
   récentes, les Mac et les Sonos AirPlay 2, avec eux aussi un 0 dans `et` et `cn`. Lire le TXT
   mDNS et ne proposer que si `et` et `cn` contiennent 0, pas de bit 38 ni 48 dans `ft`
   (AirPlay 2), ni `pw=true` ni `0x8`, `0x80` ou `0x200` dans `sf` (mot de passe, appairage).
   C'est la règle de `moli_net::raop::plain` ; le pilote la relit au démarrage et refuse les
   autres.
3. Il faut un serveur Piper local (Wyoming) : `tts = "hôte:port"`, `voice` facultatif ;
   `volume` = le niveau de départ des annonces (le point `announce_volume` ensuite).
4. Déclarer `kind = "airplay"`, `host`, et `port` = le port de l'enregistrement SRV
   `_raop._tcp` (souvent 5000, la valeur par défaut, mais pas toujours : 7000 sur les
   récepteurs AirPlay 2). Au mauvais port, le pilote ne voit qu'un récepteur absent, jamais
   un refus. `moli-os check-config`, redémarrer.
5. Prévenir : le volume du récepteur est lu au démarrage et à chaque annonce (le lire coupe ce
   qu'il joue), et suivi d'après ce que Moli règle entre les deux. Un réglage fait ailleurs
   n'est vu qu'à l'annonce suivante.
6. Tester, humain dans la pièce : une phrase courte (`announce`). Le récepteur ne dit rien
   quand il jette ce qu'il reçoit : seule l'oreille prouve que ça marche.
