# Proxmox VE

Pilote natif `crates/moli-proxmox`, **lecture seule** : le nœud, ses conteneurs (LXC) et machines
virtuelles, ses stockages, la dernière sauvegarde et ses disques.

> **Éprouvé** sur un Proxmox VE 9.2 (un nœud, six conteneurs LXC, un stockage `local-lvm`, un
> partage de sauvegarde, un SSD NVMe) : `/nodes`, `/status`, `/lxc`, `/storage`, `/tasks`,
> `/disks/list`, `/apt/update` et la configuration d'un conteneur ont répondu comme prévu ; les
> fixtures sont ces réponses, anonymisées (numéro de série, empreinte, noms des conteneurs).
> **Pas éprouvé** : une machine virtuelle (`/qemu`), un cluster de plusieurs nœuds, une panne réelle
> (fixtures `*-faulty` reconstituées : ce nœud est sain).

- **Transport** : l'API REST de Proxmox VE, **HTTPS** (8006 par défaut), certificat **épinglé au
  premier contact** (empreinte rangée dans le coffre sous `cert`, avant que le jeton parte). Chaque
  appel est un `GET` avec l'en-tête `Authorization: PVEAPIToken=<utilisateur>@<royaume>!<nom>=<secret>` :
  pas de session, pas de mot de passe, rien dans une adresse.
- **Jeton** : un jeton d'API d'un utilisateur au rôle **`PVEAuditor`** sur `/` (il voit tout, ne peut
  rien changer). L'identifiant (`token_id`) va dans `moli.toml`, le secret dans le coffre
  (`token_secret`).
- **Lecture** toutes les 60 s (`poll_secs`, de 5 à 3600) : statut du nœud, conteneurs, machines
  virtuelles, stockages, tâches `vzdump`. Les disques (`/disks/list`) toutes les 10 min, les mises à
  jour en attente (`/apt/update`, le cache de Proxmox) toutes les heures. La configuration d'un invité
  n'est lue que s'il est arrêté (`onboot` dit s'il devrait tourner), 32 au plus par lecture. Réponses
  bornées : 4 Mio, et 256 entrées par liste (conteneurs, machines virtuelles, stockages, disques,
  tâches). Une partie qui ne répond pas (droit
  manquant, délai) est laissée de côté, pas le reste : seul `/status` dit si le nœud répond.
- **Appareils** :
  - `node` : en marche (`up`), processeur, mémoire (part, utilisée, totale), disque système, durée
    d'allumage, mises à jour en attente, version, `problem` / `detail` ;
  - `lxc_<vmid>`, `qemu_<vmid>` : en marche, processeur, mémoire, durée d'allumage, disque (conteneurs
    seulement : Proxmox ne voit pas l'intérieur d'une machine virtuelle), `problem` / `detail` ;
  - `storage_<id>` : occupation, libre, taille, contenu (stockages actifs) ;
  - `backup` : dernier résultat, dernière exécution (UTC), durée, heures depuis la dernière réussie, en cours
    (les tâches sont lues avec `source=all` : celle qui tourne aussi) ;
  - `disk_<n>` : santé SMART, vie restante, taille, type. Nommés par leur place, jamais par leur numéro de série.
- **Alertes** (`problem` binaire sens `alert`, `detail` texte) : nœud injoignable ; invité arrêté alors
  qu'il démarre avec le serveur ; mémoire d'un conteneur qui tourne, disque d'un conteneur, disque système
  ou stockage au-dessus de `full_percent` (90) ; mémoire d'une machine virtuelle au-dessus de
  `vm_full_percent`, seulement s'il est donné (Proxmox compte le cache disque de l'invité comme occupé :
  une VM Linux paraît toujours pleine) ; dernière sauvegarde en échec, ou aucune réussie depuis
  `backup_hours` (26) ; SMART en échec ou moins de 10 % de vie. La page « Parc » les liste, un
  automatisme peut s'y accrocher.
- **Une phrase même quand tout va bien** : sans problème, `detail` dit comment va la partie, pour que
  chaque carte de « Parc » se lise d'un coup d'œil (#289) : nœud « en marche depuis 12 j · mémoire 30 % ·
  disque système 8 % · 2 mises à jour en attente », conteneur « en marche depuis 3 j · mémoire 35 % ·
  disque 41 % », machine virtuelle « en marche depuis 2 h · 4 Gio de mémoire » (sa taille, pas sa part, qui
  compte le cache), invité « arrêté », stockage « 10 % de 68 Gio, 61 Gio libres », sauvegarde « réussie il
  y a 3 h · 10 min » ou « en cours depuis 4 min », disque « SMART PASSED · 97 % de vie ». Un problème garde
  ses propres mots. La phrase s'en tient aux valeurs qui bougent lentement (pas le processeur, la mémoire
  à 5 % près) pour ne pas remplir l'historique d'une ligne par minute.
- **Un nœud injoignable n'arrête pas le pilote** : l'appareil `node` reste, `up` faux, `problem` vrai ;
  les autres appareils passent hors ligne sans leur ancien verdict.
- **Jeton refusé** (401, 403) : avertissement visible, nouvel essai dans 15 min seulement.
  **Certificat changé** : attente d'une personne, `moli-os secrets forget <instance> cert`.
- **Pas encore** : la température (absente de l'API Proxmox), les ordres (démarrer, arrêter,
  redémarrer un invité : un rôle plus large et la garde), plusieurs nœuds dans une instance (une
  instance = un nœud), Proxmox Backup Server.
