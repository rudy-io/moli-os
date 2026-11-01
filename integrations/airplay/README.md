# AirPlay 1 (annonces)

- RAOP (`moli-net::raop`) : RTSP sur 5000, puis du PCM 16 bits stéréo petit-boutiste en RTP
  (UDP), des SYNC chaque seconde sur le port de contrôle du récepteur, les paquets réclamés
  renvoyés (type 85), les questions d'horloge répondues. Annonces seulement, par
  `moli-announce` comme le Sonos : Piper en local (Wyoming), puis RAOP.
- Seulement un récepteur AirPlay 1 sans chiffrement, sans mot de passe ni appairage. Lu dans
  son TXT `_raop._tcp` (`moli_net::raop::plain`, la règle de pyatv) : `et` et `cn`
  contiennent 0, pas de bit 38 ni 48 dans `ft` (AirPlay 2), ni `pw=true` ni `0x8`, `0x80` ou
  `0x200` dans `sf` (mot de passe, appairage). Les HomePod, Apple TV et Mac mettent eux aussi
  0 dans `et` et `cn` : c'est `ft` qui les écarte. Le pilote
  demande ce TXT au récepteur au démarrage, celui de l'instance dont le SRV donne `port` (un
  hôte peut en annoncer deux, AirPlay 2 sur 7000 et AirPlay 1 sur 5000), et en refuse un qui
  ne passe pas, ou qui refuse la session, avec une phrase qui dit quoi faire. Un récepteur qui
  ne répond pas à cette question, ou sans SRV sur ce port, n'est jugé que sur ses réponses
  RTSP. IPv4 seulement.
- ⚠️ Toute session RTSP coupe ce que le récepteur joue, même lire le volume : le point
  `volume` est lu **au démarrage et dans la session de chaque annonce** (le niveau remis après
  l'annonce est celui que dit le récepteur), et suivi d'après ce que Moli écrit entre les deux.
  Un réglage fait ailleurs (téléphone, télécommande) reste faux jusqu'à l'annonce suivante ; le
  libellé du point le dit, pour qu'un agent le sache. Des volumes écrits en rafale : seul le
  dernier part. Un récepteur qui refuse le volume de l'annonce la dit quand même, à son propre
  volume, avec un avertissement au journal.
- Pas de fixture : les tests jouent des sessions entières contre un faux récepteur.
- Entendu en entier, par le pilote, sur un Freebox Delta Player (firmware 1.5.26,
  `AppleTV3,2`, `srcvers=220.68`) ; volume réglé puis relu au redémarrage.
- Reste : la musique et les flux longs (pas éprouvés), donc pas de `play_url`.
