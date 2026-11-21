# Onboarding : Solcast

1. Sur <https://solcast.com>, compte « Home User » (gratuit) : déclarer chaque toiture (orientation,
   inclinaison, puissance crête). Noter l'identifiant de chacune (« Resource Id », de la forme
   `abcd-1234-ef56-7890`) et la clé d'API (« Your API Key »).
2. Secret, rangé par un tube : `printf %s "$CLE" | moli-os secrets set <instance> api_key`.
3. Déclarer `kind = "solcast"` avec `sites = ["<id 1>", "<id 2>"]` ; `calls_per_day` si le compte
   en permet plus que 10 ; `moli-os check-config`, redémarrer.
4. Tester : l'appareil « Prévision solaire » montre aujourd'hui, demain, ce qui reste, et
   `calls_left` baisse d'un par toiture à chaque mise à jour.
5. Venant de Home Assistant : désactiver l'intégration Solcast de HA (ou réduire ses appels), le quota
   est partagé par le compte.
