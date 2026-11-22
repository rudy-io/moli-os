# Tablette murale

Une tablette Android qui ne montre que Moli : l'écran s'allume quand quelqu'un
passe devant, s'éteint quand personne n'est là, et revient seul sur Moli après
une coupure ou un redémarrage. Plus besoin d'un navigateur kiosque à côté.

Tout se règle dans Moli, **Système → Tablette murale** : la carte n'apparaît
que dans l'appli Moli sur Android (`web/src/maison/ui/WallCard.svelte`).

## Ce que fait l'appli

- **Réveil au passage** : la caméra avant est lue environ 4 fois par seconde,
  écran allumé ou éteint, par un service au premier plan
  (`modules/moli-wall/…/MotionService.kt`). Chaque image est réduite à une
  grille de 32 × 24 luminosités, comparée à la précédente puis jetée : rien
  n'est enregistré ni envoyé. Un changement de lumière qui touche toute
  l'image (lampe allumée, nuage) ne réveille pas ; quelqu'un qui passe, si.
  Cinq sensibilités, de « quelqu'un tout près » à « le moindre mouvement ».
- **Mise en veille** après 1, 2, 5, 10 ou 30 minutes sans mouvement ni toucher
  (`src/wall.ts`). Si Moli est **administrateur** de la tablette, l'écran
  s'éteint vraiment ; sinon il passe au noir (rétroéclairage allumé).
- **Moli seule** : Moli devient l'écran d'accueil, plein écran, au-dessus de
  l'écran de verrouillage, et tourne avec la tablette.

## Deux façons d'installer

### 1. Installation simple (sans remise à zéro)

1. Installer l'appli (APK) et ouvrir Moli.
2. Système → Tablette murale → activer.
3. **Autoriser** l'extinction de l'écran (écran d'Android « Activer l'application
   d'administration »).
4. **Épingler Moli** (épinglage d'écran d'Android). On en sort avec un appui
   long sur Retour et Aperçu.
5. Quand Android le propose, choisir Moli comme écran d'accueil « Toujours ».

Suffisant à la maison ; un enfant curieux peut toutefois sortir de Moli.

### 2. Installation par QR code (Moli tient la tablette)

Moli devient **propriétaire de l'appareil** (*device owner*) : pas d'écran de
verrouillage, pas de barre d'état, aucune autre appli ne s'ouvre, Moli
redémarre seule. Il faut une tablette **remise à zéro** (aucun compte).

1. Paramètres → Système → Réinitialisation → Effacer toutes les données.
2. Sur l'écran de bienvenue, toucher **6 fois** au même endroit.
3. Se connecter au Wi-Fi quand la tablette le demande, puis scanner le QR code.
4. La tablette télécharge Moli, vérifie sa signature, l'installe et la nomme
   propriétaire. Terminer l'assistant **sans compte Google**.

Le QR code contient (JSON) :

```json
{
  "android.app.extra.PROVISIONING_DEVICE_ADMIN_COMPONENT_NAME": "<paquet>/expo.modules.moliwall.WallAdminReceiver",
  "android.app.extra.PROVISIONING_DEVICE_ADMIN_PACKAGE_DOWNLOAD_LOCATION": "https://…/moli.apk",
  "android.app.extra.PROVISIONING_DEVICE_ADMIN_SIGNATURE_CHECKSUM": "<SHA-256 du certificat de signature, base64url>",
  "android.app.extra.PROVISIONING_SKIP_ENCRYPTION": true,
  "android.app.extra.PROVISIONING_LEAVE_ALL_SYSTEM_APPS_ENABLED": true
}
```

- `<paquet>` : l'identifiant de l'appli (`bundleId` de `house.json`).
- La somme de contrôle : `apksigner verify --print-certs moli.apk` donne le
  « SHA-256 digest » du certificat en hexadécimal ; le convertir en base64url
  sans `=` (`xxd -r -p | base64 | tr '+/' '-_' | tr -d '='`).
- Le lien de l'APK doit être joignable par la tablette pendant l'assistant
  (le lien de téléchargement d'un build EAS convient).

**Sortir pour l'entretien** : Système → Tablette murale → « Sortir de Moli pour
l'entretien », derrière le code de la maison. Moli rend l'écran d'accueil et
les réglages jusqu'à sa prochaine ouverture. Pour rendre la tablette tout à
fait : remise à zéro.

**WebView** : Moli s'affiche dans la WebView d'Android. Une tablette remise à
zéro sans compte garde celle de sa sortie d'usine, parfois trop ancienne :
se connecter une fois au Play Store pour mettre à jour « Android System
WebView », puis retirer le compte.

## Une tablette toujours branchée

Une batterie chargée à 100 % jour et nuit finit par gonfler. Si la tablette a
une limite de charge (Paramètres → Batterie), l'activer ; sinon, brancher son
chargeur sur une prise connectée que Moli coupe à 80 % et rallume à 40 %
(l'appli remonte le niveau de batterie de la tablette).
