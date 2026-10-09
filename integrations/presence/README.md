# Présence

Qui est à la maison, d'après son téléphone sur le Wi-Fi : local, sans appli, sans cloud.
Chaque minute, un petit paquet UDP vers chaque adresse du réseau de la maison, puis la
table des voisins du noyau (`/proc/net/arp`) dit quelles adresses Wi-Fi ont répondu.
Absent après 10 minutes sans signe (un téléphone endormi saute parfois un tour).
Limites : un téléphone en mode avion ou à adresse Wi-Fi « tournante » échappe ; pas de
géolocalisation (l'appli mobile viendra plus tard).
