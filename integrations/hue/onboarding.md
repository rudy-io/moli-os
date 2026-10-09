# Onboarding : Philips Hue

1. Demander l'adresse IP du pont (appli Hue ou liste des appareils de la box).
2. Il faut une clé maître (`MOLI_MASTER_KEY_FILE`) : le pilote range lui-même la clé
   d'appli et l'empreinte du certificat dans le coffre.
3. Déclarer `kind = "hue"`, `host = "<ip>"` ; `moli-os check-config`, redémarrer.
4. Appui physique : le tableau de bord dit « Appuie sur le bouton du pont Hue » ; l'humain
   appuie, le pilote redemande toutes les 2 s jusqu'à l'appui.
5. Tester : lumières, pièces et zones publiées ; allumer puis éteindre une lampe neutre
   (jamais dans une chambre), humain présent.
