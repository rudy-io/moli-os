# Onboarding : télé Philips

1. Demander : l'adresse IP de la télé (fixe : elle sert d'identité) et son adresse MAC (pour
   l'allumer par Wake-on-LAN).
2. Identifiants d'appairage JointSpace (`username`, `password`) : le pilote ne sait pas
   appairer ; procédure non documentée ici. L'humain les range par un tube :
   `moli-os secrets set <instance> username`, puis `password`. Il faut une clé maître.
3. Déclarer `kind = "philips"`, `host`, `mac` ; `moli-os check-config`, redémarrer.
4. Tester, humain présent : baisser puis remonter le volume ; en veille profonde, la télé est
   hors ligne (`power = false`), c'est normal.
5. Applis et « ce qui se joue » : Cast marche seul. Pour ouvrir des applis, reprendre
   l'appairage Android TV Remote de HA s'il existe :
   `cat …/.storage/androidtv_remote_cert.pem | docker exec -i moli-os /moli-os secrets set <instance> atv_cert`,
   idem `androidtv_remote_key.pem` → `atv_key`, puis redémarrer.
