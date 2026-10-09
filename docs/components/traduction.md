# Traduire Moli

Une maison parle **une** langue : celle choisie à l'installation (`data/settings.json`), sinon
`[server] language` de `moli.toml` (défaut `fr`). Le serveur (erreurs, notices, assistant) et le tableau
de bord la parlent tous les deux.

## Les catalogues

`locales/<langue>/<zone>.json`, à la racine du dépôt, partagés par le serveur (crate `moli-i18n`, embarqués
à la compilation) et le tableau de bord (`web/src/lib/i18n.svelte.js`).

- Un objet plat. Chaque clé commence par le nom de sa zone : `installation.titre` dans `installation.json`.
  Après la zone, des minuscules ASCII, chiffres, `_` et `.` pour regrouper : `energie.carte.titre`.
- Paramètres : `{nom}`, remplis par `t('zone.cle', { nom })`. Les mêmes paramètres dans chaque langue.
- Pluriel : `zone.cle_one` et `zone.cle_other`, choisis par le paramètre `count`.
- Le français est la référence et le repli : une clé absente ailleurs s'affiche en français, une clé absente
  partout s'affiche telle quelle (visible, donc trouvée).
- **Le français reproduit mot pour mot le texte d'origine** (apostrophes typographiques, espaces, majuscules) :
  traduire ne change rien pour une maison française.

## Dans le tableau de bord

```js
import { t, locale } from '../../lib/i18n.svelte.js';   // chemin relatif depuis le fichier
```

- Dans le balisage : `{t('salon.titre')}`, `title={t('salon.volume')}`.
- **Jamais `t()` au niveau du module ni dans une constante calculée une seule fois** : la langue arrive
  avec la session, après le premier rendu. Dans un composant : `$derived(t(...))` ou un appel dans le
  balisage ou une fonction. Dans un module JS : `t()` à l'intérieur des fonctions ; une table de libellés
  devient une table de **clés**, traduites à l'usage.
- Nombres et dates : `toLocaleString(locale(), …)`, jamais `'fr-FR'` en dur.
- Pas de `{@html}` pour du texte traduit : couper la phrase autrement (une commande sur sa propre ligne,
  par exemple, comme l'écran Bienvenue).
- Ne se traduisent pas : identifiants, valeurs envoyées à l'API ou stockées (`chaud`, `froid`, un mode de la
  maison…), noms donnés par la maison (pièces, appareils), unités (`°C`, `kWh`, `W`), classes CSS, URL.
  Pour **afficher** une valeur, une clé : `t('energie.periode.' + id)`, avec la valeur brute en repli.

## Vérifier

```sh
node scripts/i18n-check.mjs            # clés identiques dans chaque langue, aucun texte en dur dans web/src
node scripts/i18n-check.mjs --report   # le décompte par fichier
```

Une exception (texte qui doit rester tel quel, comme le nom d'une langue écrit dans cette langue) va dans
`scripts/i18n-allow.txt`, avec sa raison.

## Ajouter une langue

Copier `locales/fr/` vers `locales/<code>/`, traduire chaque valeur (jamais les clés), lancer le contrôle.
La langue apparaît d'elle-même dans le choix de l'écran Bienvenue.
