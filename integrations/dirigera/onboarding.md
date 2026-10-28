# Onboarding : IKEA Dirigera

1. Demander l'adresse IP du hub (appli IKEA Home smart, liste des appareils de la box, ou
   `dns-sd -B _ihsp._tcp` sur un Mac). Réserver ce bail DHCP.
2. Il faut une clé maître (`MOLI_MASTER_KEY_FILE`) : le pilote range lui-même le jeton et
   l'empreinte du certificat dans le coffre.
3. Déclarer `kind = "dirigera"`, `host = "<ip>"` ; `moli-os check-config`, redémarrer.
4. Appui physique : le tableau de bord dit « Appuie sur le bouton d'action sous le hub
   Dirigera » ; l'humain appuie (un appui bref, sous le hub) ; le pilote redemande toutes les
   2 s et renouvelle le code toutes les 60 s jusqu'à l'appui.
5. Tester : lampes, prises, capteurs et télécommandes publiés avec leur pièce IKEA ; allumer
   puis éteindre une lampe neutre (jamais dans une chambre), humain présent. Stores : ouvrir,
   stop, fermer, puis régler au curseur un store d'une pièce non protégée, humain présent ;
   relever `moving` pendant le mouvement, puis remettre le store où il était.
6. Siri, l'app Maison et l'appli IKEA restent reliés au hub : rien ne change pour eux.
