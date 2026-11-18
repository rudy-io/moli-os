# Onboarding : Apple TV

1. Trouver l'adresse de l'Apple TV (Réglages › Réseau, ou l'annonce mDNS
   `_companion-link._tcp`) et lui donner une adresse fixe dans la box : elle sert à la joindre.
2. Déclarer l'instance (il faut une clé maître : l'appairage va dans le coffre) :

   ```toml
   [[driver]]
   id = "appletv"
   kind = "appletv"
   [driver.options]
   host = "192.168.1.x"
   # name = "Moli"   # le nom sous lequel l'Apple TV liste Moli
   ```

   `moli-os check-config`. **Quelqu'un devant la télé** ouvre d'abord Réglages ›
   Télécommandes et appareils › App Télécommande et appareils (le code y apparaît ; il ne
   s'affiche pas toujours de lui-même), et prépare la commande ci-dessous. Puis redémarrer Moli.
3. L'Apple TV affiche un code à 4 chiffres ; le statut du pilote dit la commande. L'humain le
   donne par un tube, **dans la minute** (l'Apple TV abandonne la demande avant une minute) :
   `read -rs C && printf '%s' "$C" | docker exec -i moli-os /moli-os secrets set appletv pin; unset C`
   Le pilote l'utilise une fois et l'efface. Trop tard ou échec (la raison est dans le
   statut) : redémarrer Moli, un nouveau code s'affiche.
4. Vérifier : l'appareil « Apple TV » apparaît, `power` dit si elle dort, `apps` liste ses
   applis. Essayer une touche (`key` = `CursorRight`) humain présent.
5. Page Salon : étiqueter l'appareil dans « Salon » **et** renseigner
   `"salon": { "tv": "<id de l'appareil>" }` dans `data/home.json`.
6. Désappairer : Réglages › Télécommandes et appareils de l'Apple TV, retirer « Moli », puis
   `moli-os secrets forget appletv credentials` et redémarrer.
