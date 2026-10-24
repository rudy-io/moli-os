# Onboarding : poêle à granulés Micronova (Easy Connect)

1. Demander : le poêle est-il dans une application Easy Connect (Easy Connect by MCZ, Easy
   Connect Poêle, Easy Connect Plus) ? Avec quel e-mail se connecte-t-on ? Le mot de passe, jamais.
2. Les identifiants vont au coffre, tapés par la personne :
   `read -rs V; printf %s "$V" | docker compose exec -T moli-os /moli-os secrets set micronova email`,
   puis la même chose avec `password`.
3. Déclarer `kind = "micronova"` (`id = "micronova"`) ; une autre application que Easy
   Connect : `customer_code` (Easy Connect Poêle 354925, Plus 746318) ; `moli-os
   check-config`, redémarrer.
4. Vérifier : le journal dit `micronova stoves loaded`, le poêle apparaît avec son état et
   ses températures. Ranger le poêle dans sa pièce.
5. Moli règle la consigne, la puissance, la ventilation, l'éco-stop et les programmes ; il
   n'allume ni n'éteint le poêle. Pour vérifier : baisser la consigne d'un degré, poêle en
   veille, voir l'application Easy Connect la montrer, puis la remettre.
