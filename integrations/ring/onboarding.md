# Onboarding : Ring

Prérequis :

- une sonnette/caméra Ring avec **l'offre Ring base** active (indispensable pour l'API partenaire) ;
- un compte développeur Ring (`developer.amazon.com/ring/console/apps`) ;
- une URL publique HTTPS vers cette instance (par exemple via tunnel Cloudflare).

## 1. Créer l'application partenaire

1. Sur le portail développeur Ring, créer une application (catégorie *Developer* ou *Partner*).
2. Copier les trois valeurs affichées **une seule fois** : `client_id`, `client_secret` et la
   **HMAC key**.
3. Déclarer les URLs de l'application :
   - **Token Exchange URL** : `https://<votre-domaine>/api/ring/link`
   - **Account Link URL** : `https://<votre-domaine>/api/ring/linked`
   - **Webhook URL** : une URL qui répond 200 (Moli n'utilise pas les webhooks, les événements
     sont lus par sonde).

## 2. Ranger les clés dans le coffre

Par tube, jamais au clavier ni à l'écran :

```
printf %s "<client_id>"    | moli-os secrets set ring client_id
printf %s "<client_secret>" | moli-os secrets set ring client_secret
printf %s "<hmac_key>"     | moli-os secrets set ring hmac_key
```

## 3. Déclarer le pilote

```toml
[[driver]]
id = "ring"
kind = "ring"
```

`moli-os check-config`, puis redémarrer. Le pilote attend les clés (message « compte Ring non lié »
jusqu'au lien) ; l'identifiant matériel (`hardware_id`) est généré et rangé tout seul.

## 4. Lier le compte

1. Dans l'appli Ring : *Paramètres de l'appareil → Intégrations (Connections) → Moli* → lier le
   compte, en autorisant au moins **caméras** (lecture).
2. Ring envoie le code au backend, puis redirige le navigateur vers
   `https://<votre-domaine>/api/ring/linked?nonce=…&time=…`.
3. Ouvrir le tableau de bord et se connecter (code PIN) si ce n'est pas déjà fait, puis recharger
   la page du lien. La page passe à « Ring lié » : c'est le signal de succès.

## 5. Vérifier

- Les appareils Ring apparaissent dans le tableau de bord (lecture seule) ;
- une sonnette actionnée passe `doorbell` à vrai ; un passage devant une caméra passe `person` /
  `vehicule` / `animal` ;
- une image s'affiche (`/api/devices/<id>/snapshot`).

## Pièges connus

- Le code d'autorisation est à **usage unique** et expire à 60 s : en cas d'échec, relancer le lien
  depuis l'appli Ring (la paire précédente est remplacée, la plus récente gagne).
- L'API peut répondre **406** depuis des adresses IP de centre de données/cloud, même avec les
  en-têtes corrects : si Moli tourne sur un VPS, contacter le support Ring avec l'IP égrenante
  statique.
- Derrière Cloudflare Access : laisser `/api/ring/link` joignable par le backend Ring (sans
  authentification) ; la sécurité tient au code à usage unique + `client_secret`, et la confirmation
  exige de toute façon un code PIN.
