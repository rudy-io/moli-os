# Onboarding : la machine de Moli

1. Rien à demander : Moli lit `/proc` et `/sys` de la machine où il tourne (sans privilège).
2. Déclarer `kind = "host"` (options facultatives : `name`, `interface`).
3. Tester : `cpu`, `ram`, `cpu_temperature`, `net_down` ont une valeur au bout de 30 s.
4. Les disques et les conteneurs viennent d'un script côté hôte (non fourni ici, lancé chaque
   minute par cron) qui écrit `data/infra.json` : Moli ne tient pas le socket Docker (ce
   serait lui donner la main sur toute la machine).
