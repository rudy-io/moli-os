# Onboarding : Shelly (Gen2 et suivants)

1. Demander : l'adresse IP de chaque Shelly (application Shelly ou box ; une réservation DHCP
   est conseillée, mais Moli suit l'appareil par son adresse MAC si la box lui en donne une
   autre), et à quoi sert chaque sortie (chauffe-eau, volet, éclairage…).
2. Une instance par Shelly : `kind = "shelly"`, `host` (et `mac` si on la connaît).
   Nommer les sorties dans l'application Shelly, ou plus tard dans Moli (étiquettes).
3. Mot de passe (conseillé une fois l'installation finie) : le poser dans l'application
   Shelly, puis le ranger par un tube : `moli-os secrets set <instance> password`.
4. `moli-os check-config`, redémarrer Moli.
5. Tester, humain présent : lire la puissance d'une sortie et du compteur, puis couper et
   remettre une sortie **neutre** (jamais une charge dont on ne connaît pas l'effet : un
   chauffe-eau sur contacteur, une pompe). Une pince d'arrivée générale doit donner une
   puissance positive quand la maison soutire, négative quand elle injecte : sinon, la
   pince est à l'envers (la retourner, ou `reverse` dans l'application Shelly).
