# Voiture Tesla (API officielle)

Moli pilote la voiture de la maison par l'API officielle de Tesla (celle que l'appli
officielle utilise) : niveau de batterie, autonomie, recharge en cours, puissance de
charge, prises branchées, portes verrouillées, mode vigie, températures. La maison donne
six commandes — démarrer et stopper la charge, verrouiller et déverrouiller, mode vigie
on/off — qu'un automatisme ou l'assistant peut donner comme n'importe quelle autre.

## Connexion

Le compte se connecte une fois, depuis le tableau de bord (Système, pilote tesla) : le
pilote affiche l'adresse de la page de connexion de Tesla ; on s'y connecte avec son
compte Tesla, et la page finit sur l'URI de redirection enregistrée sur l'application
(l'option `redirect_uri` du pilote) avec le `code=` dans l'adresse — souvent une page
d'erreur dans le navigateur, c'est normal : c'est l'adresse qui compte. On copie cette
dernière adresse et on la colle dans le point « Adresse de la dernière page » du
pilote. Tout se passe chez Tesla, Moli ne voit jamais le mot de passe.

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

## Configuration

```toml
[[driver]]
id = "tesla"
kind = "tesla"

[driver.options]
api_host = "fleet-api.prd.eu.vn.cloud.tesla.com" # l'API, défaut Europe ; Amérique du Nord / Asie : fleet-api.prd.na.vn.cloud.tesla.com, Chine : api.tesla.cn
auth_host = "auth.tesla.com"                     # la connexion et le point de jeton (défaut)
redirect_uri = "https://example.com/moli-tesla"  # OBLIGATOIRE : l'URI de redirection enregistrée sur l'application
partner_domain = "example.com"                   # domaine public des origines autorisées, hébergeant la clé publique Tesla (défaut : l'hôte de redirect_uri)
poll_charging_s = 60                             # interrogation pendant la recharge ou branchée
poll_idle_s = 180                                # éveillée et au repos
poll_asleep_s = 600                              # endormie
```

## Ce que Moli voit

Un appareil par véhicule du compte, identifié par son numéro (`tesla:<id>`) ; un
appareil « Compte Tesla » porte l'état de la connexion.

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

- Une voiture endormie répond sans donner ses dernières valeurs : les points gardent
  leur dernière lecture, et `awake` indique que la voiture dort.
- `charge_power` est lue dans `charger_power` (le « Charging Power » de la
  télémétrie) : c'est le seul nom supposé de l'implémentation. Si votre réponse de
  l'API ne le porte pas, le point reste vide — tout le reste est lu tel quel.
- Tesla refuse parfois une commande sans autre explication (les voitures récentes
  exigent des commandes signées par l'appli officielle pour certains actes) : le refus
  arrive dans le journal, la maison ne réessaie pas seule.
- L'état est relu avec les périodes ci-dessus (60 s en recharge, 3 minutes au repos,
  10 minutes endormie) : l'API compte les requêtes, Moli ne dépasse pas 60 par minute.
  Si Tesla demande un temps d'attente (429, `Retry-After`), Moli le respecte tout entier
  avant de reprendre.
