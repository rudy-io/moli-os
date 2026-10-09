# Onboarding : présence

1. Demander qui doit être suivi, et le réseau de la maison (`192.168.1.0/24`).
2. Pour chaque téléphone, l'adresse Wi-Fi : iPhone, Réglages › Wi-Fi › (i) du réseau de la
   maison › « Adresse Wi-Fi » (laisser « Adresse privée » sur « Fixe ») ; Android,
   Paramètres › Wi-Fi › le réseau › « Adresse MAC de l'appareil ».
3. Déclarer `kind = "presence"`, `network`, et un `[[driver.options.person]]` par personne
   (`id`, `name`, `macs`). `moli-os check-config`, redémarrer.
4. Tester : couper le Wi-Fi du téléphone, la personne passe « absente » après le délai ;
   le rallumer, elle revient en moins d'une minute.
