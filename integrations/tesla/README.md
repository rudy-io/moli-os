# Voiture Tesla (API officielle)

Moli pilote la voiture de la maison par l'API officielle de Tesla (celle que l'appli
officielle utilise) : niveau de batterie, autonomie, recharge en cours, puissance de
charge, prises branchées, portes verrouillées, mode vigie, températures. La maison donne
six commandes — démarrer et stopper la charge, verrouiller et déverrouiller, mode vigie
on/off — qu'un automatisme ou l'assistant peut donner comme n'importe quelle autre.

Ce pilote n'est affilié à Tesla en rien. Chacun crée sa propre application avec son
compte développeur Tesla — la relation avec Tesla reste entre la personne et Tesla, et
le projet ne fournit ni clé ni client partagé. L'usage de l'API Fleet peut avoir un
coût selon les conditions de Tesla : à vérifier avant de créer l'application.

## Ce que Moli ne prend pas

- Ni le numéro de série (VIN) de la voiture, ni sa position, ni l'historique des
  trajets : rien de tout cela n'est lu ni gardé. Les permissions demandées ne
  contiennent pas `vehicle_location` — Tesla ne renvoie donc pas la position.
- Pas de démarrage à distance : les commandes portent sur la recharge, le verrou,
  la vigie — pas sur la marche du véhicule.
- La connexion du compte est un geste du propriétaire, depuis le tableau de bord :
  l'adresse de la page de connexion n'est servie que par la route réservée au
  propriétaire (session + code de la maison), et la valeur collée ne passe jamais
  par le journal ni la file d'approbation. L'assistant ne peut ni lire l'adresse
  ni la transmettre : les points `sign_in_url` et `sign_in` sont internes.
- Le jeton de rafraîchissement ne vit qu'au coffre chiffré : jamais dans un fichier,
  un point ou le journal.
- Le pilote ne réveille jamais la voiture lui-même : il lit quand elle répond, aux
  périodes ci-dessus, et les périodes de repos demandées par Tesla (429) sont
  respectées tout entières.

## Connexion

Le compte se connecte une fois, depuis le tableau de bord (Système, pilote tesla),
par le propriétaire seul (session + code de la maison) : le pilote affiche
l'adresse de la page de connexion de Tesla ; on s'y connecte avec son compte
Tesla, et la page finit sur l'URI de redirection enregistrée sur l'application
(l'option `redirect_uri` du pilote) avec le `code=` dans l'adresse — souvent une
page d'erreur dans le navigateur, c'est normal : c'est l'adresse qui compte. On
copie cette dernière adresse et on la colle dans le formulaire Tesla du
tableau de bord. L'adresse porte aussi un paramètre `state` qui la lie à la
page affichée : un code d'une page plus ancienne ou d'un autre navigateur est
refusé. Tout se passe chez Tesla, Moli ne voit jamais le mot de passe, et le
code collé ne passe jamais par le journal ni la file d'approbation.

En échange, Tesla donne un jeton de rafraîchissement qui vit au coffre chiffré
(`refresh_token`) : il est à usage unique et tourne à chaque échange — Moli en garde
toujours le dernier, jamais dans un fichier. Il expire au bout de trois mois : reconnecter
le compte depuis le tableau de bord (la même opération).

Le client public partagé de l'appli Tesla a été retiré par Tesla (2026) : une
application personnelle est **obligatoire**. Crée-la sur
[developer.tesla.com](https://developer.tesla.com) (permissions `openid`,
`offline_access`, `user_data`, `vehicle_device_data`, `vehicle_cmds`), enregistre-y une
URI de redirection (par ex. `https://example.com/moli-tesla` — la page n'a pas besoin
d'exister), puis dépose ses identifiants au coffre : `moli-os secrets set tesla
client_id …` puis `tesla client_secret …` (valeur par un tube, jamais au clavier).
Les deux sont nécessaires : à chaque connexion, Moli enregistre l'application dans
la région du serveur (`POST /api/1/partner_accounts` avec un jeton
`client_credentials`, idempotent) — sans cet enregistrement, tous les appels
véhicule renvoient 412 « must be registered in the current region ».
L'enregistrement exige aussi que le domaine de l'application héberge publiquement
une clé publique EC P-256 (PEM) à
`https://domaine/.well-known/appspecific/com.tesla.3p.public-key.pem` ; sans cette
clé accessible, Tesla refuse l'activation (424).
Sans `client_id`, le pilote se gare et invite à les déposer depuis le tableau de bord.

## Déconnexion

Le propriétaire déconnecte le compte à tout moment, depuis le même endroit que
la connexion (action « Déconnexion », code de la maison requis). Moli efface
alors le jeton de rafraîchissement du coffre chiffré, et le pilote revient à
l'état d'attente : le statut du compte repasse à « à connecter » et une page de
connexion fraîche s'affiche. C'est une action normale, pas une erreur : la
reprise est une nouvelle connexion, sans attente supplémentaire.

## Configuration

```toml
[[driver]]
id = "tesla"
kind = "tesla"

[driver.options]
api_host = "fleet-api.prd.eu.vn.cloud.tesla.com" # l'API, défaut Europe ; Amérique du Nord / Asie : fleet-api.prd.na.vn.cloud.tesla.com, Chine : api.tesla.cn
auth_host = "auth.tesla.com"                     # la connexion et le point de jeton (défaut) ; Chine : auth.tesla.cn
redirect_uri = "https://example.com/moli-tesla"  # OBLIGATOIRE : l'URI de redirection enregistrée sur l'application
partner_domain = "example.com"                   # domaine public des origines autorisées, hébergeant la clé publique Tesla (défaut : l'hôte de redirect_uri)
poll_charging_s = 300                            # lecture complète pendant la recharge
poll_idle_s = 600                                # éveillée et au repos
poll_asleep_s = 3600                             # endormie
```

Les hôtes sont une **liste fermée** : tout `api_host` ou `auth_host` qui n'est pas
une adresse officielle de Tesla est refusé au chargement de la configuration —
un `moli.toml` copié ne peut pas rediriger le secret du client et le jeton de
rafraîchissement vers un tiers.

## Ce que Moli voit

Un appareil par véhicule du compte, identifié par son numéro (`tesla:<id>`) ; un
appareil « Compte Tesla » porte l'état de la connexion. Le point `status` du
compte prend ces valeurs : `sign_in` (à connecter), `app_required` (identifiants
de l'application à déposer au coffre), `connecting`, `connected`, `billing_limit`
(plafond de facturation Tesla atteint), `token_not_kept` (le jeton tournant n'a
pas pu être conservé : reconnecter le compte) et `error`.

| Point | Sens |
|---|---|
| `soc` | niveau de batterie (0–100 %) |
| `range` | autonomie estimée (km ; l'API renvoie toujours des miles, même voiture en km) |
| `charging` | en recharge (vrai) ou non |
| `charge_power` | puissance de charge (W) |
| `plugged_in` | prise de charge branchée |
| `doors_locked` | portes verrouillées |
| `sentry` | mode vigie actif |
| `awake` | la voiture répond (faux quand elle est endormie) |
| `inside_temp`, `outside_temp` | températures (°C ; les °F sont convertis) |
| `command` ✎ | `start_charging`, `stop_charging`, `lock`, `unlock`, `sentry_on`, `sentry_off` |

Unités : l'API Tesla renvoie toujours les valeurs d'autonomie en miles, même pour
une voiture réglée en kilomètres — Moli les convertit systématiquement en km.
Les températures suivent les réglages de la voiture (les °F sont convertis en °C).

## Limites

- Sans connexion Internet, le pilote attend : les points gardent leurs dernières
  valeurs, l'état du compte l'indique, et la reprise se fait seule au retour.
- Une voiture endormie répond sans donner ses dernières valeurs : les points gardent
  leur dernière lecture, et `awake` indique que la voiture dort.
- `charge_power` est lue dans `charger_power` (le « Charging Power » de la
  télémétrie) : c'est le seul nom supposé de l'implémentation. Si votre réponse de
  l'API ne le porte pas, le point reste vide — tout le reste est lu tel quel.
- Tesla refuse parfois une commande sans autre explication (les voitures récentes
  exigent des commandes signées par l'appli officielle pour certains actes) : le refus
  arrive dans le journal, la maison ne réessaie pas seule.
- L'état est relu avec les périodes ci-dessus : la lecture complète
  (`vehicle_data`) n'est demandée que **pendant la recharge** (5 min) ; au repos
  (10 min) ou endormie (1 h), seule la sonde bon marché `/vehicles`
  (réveil/sommeil) est interrogée. Une voiture branchée mais qui ne charge pas
  est donc au repos. L'API compte les requêtes, Moli ne dépasse pas 60 par
  minute. Les périodes par défaut (5 min / 10 min / 1 h) sont dimensionnées
  pour rester dans le crédit mensuel de 10 $ de la Fleet API à usage personnel
  (≈ 80 requêtes de données par jour, soit ≈ 6 $/mois avec quelques commandes) ;
  les réduire rapproche les mises à jour mais fait dépasser le crédit. Si une
  voiture s'endort en cours de lecture (408), Moli la marque endormie sans rien
  changer à la session ni aux autres voitures. Si Tesla
  demande un temps d'attente (429, `Retry-After`), Moli le respecte tout entier
  avant de reprendre.
- **Vérifié sur du vrai matériel** (Model Y, depuis octobre 2026) : connexion du
  compte depuis le tableau de bord, suivi batterie/autonomie/recharge/portes/
  vigie/températures, et les commandes démarrer la charge, stopper la charge et
  verrouiller.
- **Exposées mais pas vérifiées sur la voiture** : déverrouiller et le mode
  vigie on/off. Ces commandes sont exposées et envoyées à Tesla comme les
  autres, mais leur effet sur le véhicule n'a pas été confirmé (les voitures
  récentes exigent parfois des commandes signées par l'appli officielle) : à
  tester avec prudence avant de s'y fier.
- Le cas où le compte est désactivé par sa limite de facturation (403
  `EXCEEDED_LIMIT`) a été rencontré pour de vrai : le pilote se gare et reprend
  seul quand le compte est réactivé.
