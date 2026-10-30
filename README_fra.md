<p align="center">
  <img src="images/EH3D_BANNER.svg" alt="EH3D-SERVER banner" width="100%">
</p>

# 🧭 EH3D-SERVER

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | 🇫🇷 <b>Français</b> | <a href="README_ita.md">🇮🇹 Italiano</a> | <a href="README_deu.md">🇩🇪 Deutsch</a> | <a href="README_zho.md">🇨🇳 中文</a> | <a href="README_jpn.md">🇯🇵 日本語</a> | <a href="README_por.md">🇵🇹 Português</a></p>

### Le centre des programmes EH3D : il reçoit les travaux de DRILLPRO, de ROUTEPRO et des futurs clients, les planifie, les exécute sur la machine par son nœud et dit à tous où ils en sont

<p align="center">
  <img src="https://img.shields.io/badge/License-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Stack-Rust-DEA584.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Languages-8-00E5FF.svg" alt="Languages">
  <img src="https://img.shields.io/badge/Version-0.0.1-0f6cbd.svg" alt="Version">
  <img src="https://img.shields.io/badge/Maturity-scaffolding-FFB020.svg" alt="Maturity">
</p>

---

**Vérification d'honnêteté - ce qui tourne aujourd'hui :** un serveur Rust (axum) avec une interface REST et websocket qui garde utilisateurs, machines et travaux, planifie un travail de perçage ou de défonçage (trous et parcours, ou un fichier Excellon ou HP-GL avec sa table d'outils) avec les bibliothèques d'EH3D-DRILLPRO et d'EH3D-ROUTEPRO, montre un aperçu et l'exécute sur une machine par son EH3D-NODE-BUNGARD ou sur un simulateur, avec les dialogues de l'opérateur, un arrêt et chaque changement annoncé au fur et à mesure. 41 tests tournent sur un ordinateur, les travaux contre le simulateur. **Rien n'a encore tourné sur une vraie machine par son intermédiaire**, il ne termine pas le TLS, et parmi les clients en plus des programmes, EH3D-STUDIO existe (jamais essayé avec une vraie machine) et l'application Android et le panneau à côté de la machine n'existent pas encore.

---

> **État de la sûreté et de la sécurité :** ceci n'est pas un système de sûreté. Un travail ne démarre jamais tout seul : une personne ayant le rôle d'opérateur le démarre, un seul travail à la fois sur une machine, et la planification refuse les trous et parcours hors de la table ainsi que les outils manquants ou hors plage. Chaque requête exige un jeton (un lecteur lit, un opérateur exécute des travaux, un administrateur gère utilisateurs, jetons et machines) ; les mots de passe sont conservés sous forme de hachages Argon2, les jetons de session et de programme seulement en SHA-256, et le jeton d'un nœud n'est jamais montré par l'API. Le serveur est fait pour la maison ou un VPN vers elle : mets du HTTPS devant avant que quoi que ce soit de l'extérieur l'atteigne. L'arrêt de la machine ne dépend pas du seul serveur : le nœud s'arrête de lui-même quand son lien se tait. Le projet est indépendant : il n'est ni fabriqué, ni approuvé, ni soutenu par Bungard Elektronik.

## 📋 En un coup d'œil

| | |
|---|---|
| Version | `0.0.1` |
| Maturité | Échafaudage |
| Tests | 51 |
| Pile technique | Rust (axum, tokio) |
| Sur du matériel réel | Non : testé contre le simulateur ; rien n'a encore tourné sur une vraie machine par le serveur |

**Aller à  :** [Présentation](#-présentation) · [Architecture](#-architecture) · [Démarrage rapide](#-démarrage-rapide) · [Tests](#-tests) · [Structure](#-structure-du-dépôt) · [Liés](#-projets-liés)

## 🚦 État du projet

Chaque ligne est une partie de ce dépôt et le feu dit où elle en est aujourd'hui. Elle est écrite d'après ce que le dépôt peut montrer (tests, compilations, exécutions), pas d'après ce qui est prévu.

🟢 Fait et vérifié · 🟡 Fonctionne, avec des limites connues · 🟠 À moitié fait ou pas encore vérifié · 🔴 Pas fait ou ne fonctionne pas

| Domaine | État | Ce que cela veut dire ici |
|---|---|---|
| Logiciel | 🟡 | Un serveur Rust (axum) garde utilisateurs, machines et travaux, planifie des travaux de perçage et de défonçage avec les bibliothèques d'EH3D-DRILLPRO et d'EH3D-ROUTEPRO, montre un aperçu et exécute le travail avec des dialogues d'opérateur et un arrêt. |
| Tests | 🟢 | 51 tests réussissent (`cargo test`) : 35 dans les modules et 16 sur l'interface de bout en bout, avec des travaux sur le simulateur (11 dans `tests/api.rs`, 4 avec les vrais programmes dans `tests/programs.rs`). |
| Interface pour les clients (API) | 🟡 | Les requêtes REST et les événements websocket sont décrits dans `docs/API.md` et couverts par les tests ; seule la connexion au serveur des programmes eux-mêmes les utilise pour l'instant. |
| Clients | 🔴 | EH3D-STUDIO l'utilise (jamais essayé avec une vraie machine) ; l'application Android et le panneau tactile à côté de la machine n'existent pas encore. |
| Sécurité | 🟡 | Chaque requête exige un jeton avec l'un de trois rôles, les mots de passe sont des hachages Argon2 et les mauvais mots de passe sont ralentis ; le serveur ne termine pas le TLS (mets du HTTPS devant) et n'a pas eu de revue indépendante. |
| Matériel réel | 🟠 | Les travaux n'ont tourné que contre le simulateur : rien n'a tourné sur une vraie machine par le serveur. |
| Déploiement | 🟠 | Une unité systemd et `docs/DEPLOY.md` décrivent le service sur la CM5 ; ce dépôt n'enregistre aucun déploiement comme fait. |
| Intégration | 🟡 | Il planifie avec les mêmes bibliothèques qu'EH3D-DRILLPRO et EH3D-ROUTEPRO (leurs dossiers doivent être à côté de celui-ci) et parle au lien d'EH3D-NODE-BUNGARD, qu'il n'a atteint que par le simulateur. |
| Documentation | 🟢 | README en huit langues, la référence de l'API et le guide pour l'exécuter comme service. |

## 🎯 Présentation

* Les **travaux** : un travail de perçage (trous et outils, ou un fichier `.NCD` avec sa table d'outils) ou de défonçage (parcours et outils, ou un fichier `.PLT` avec sa table d'outils), planifié avec les bibliothèques des programmes, si bien qu'un client dessine et qu'une machine fait exactement la même chose ; refusé avec un code quand un trou est hors de la table ou qu'un outil manque.
* L'**aperçu** : le travail dans l'ordre où la machine le fait, les outils avec leur nombre, la boîte englobante, une estimation du temps et l'élément auquel chaque trou est terminé - ce dont un écran a besoin pour dessiner la carte et suivre le travail.
* L'**exécution** : un travail est prêt jusqu'à ce qu'une personne le démarre ; il tourne sur la bibliothèque des programmes (la poignée de main, l'armement, la référence, les trous, l'arrêt), attend aux dialogues de l'opérateur (changer l'outil, confirmer la broche) jusqu'à ce que quelqu'un confirme, et peut être arrêté ; la machine s'arrête drivers coupés.
* Les **machines** : pour chacune, comment l'atteindre (un EH3D-NODE-BUNGARD par le réseau, ou le simulateur), le texte de ses fichiers de configuration et les valeurs réglées dessus, de sorte que l'étalonnage d'une machine vit à un seul endroit pour tous les clients.
* La **flotte et les stations** : chaque machine appartient à une station (l'établi, la salle, l'atelier) et `GET /api/v1/fleet` répond pour toutes à la fois, groupées par station, avec ce que fait chacune (libre, occupée, en attente de l'opérateur), son travail avec la progression et le nombre de travaux prêts ; les machines travaillent en même temps, un travail sur chacune.
* Les **personnes et les programmes** : des utilisateurs avec trois rôles, des jetons pour les programmes montrés une seule fois, un ralentissement sur mauvais mot de passe et un code de mise en service sur la console pour le premier administrateur.
* Les **événements** : un websocket qui envoie chaque changement d'un travail au fur et à mesure.
* Le **service** : un binaire et un petit fichier de réglages texte, sur le port 18200 par défaut (pas celui d'ARMOR ni d'HYDRA-UMC), avec une unité systemd pour la CM5.
* **Prévu, pas encore construit** : les clients (l'application Android, le Studio, le panneau tactile à côté de la machine), HTTPS dans le serveur lui-même, et l'usage des voyants du nœud et de son tampon pour finir un travail retenu quand le serveur disparaît.

## 🧩 Rôle dans EH3D

EH3D-SERVER est le centre de la famille EH3D : les programmes lui envoient leurs travaux, il parle aux machines par leurs nœuds, et les autres clients voient et envoient les mêmes travaux par la même interface. Il utilise les bibliothèques d'[EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) et d'[EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) et parle à [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD).

## 🏛️ Architecture

<p align="center">
  <img src="images/architecture.svg" alt="Architecture de EH3D-SERVER : qui l'utilise, ce qu'il contient et de quoi il dépend" width="100%">
</p>

Comment les parties de ce dépôt s'articulent : qui l'utilise, ce qu'il contient et de quoi il dépend. L'image est dessinée à partir de `docs/architecture.json` et vérifiée par rapport au code.

## ✨ Fonctionnalités

* **Un aperçu qui est le plan :** l'aperçu et l'exécution réelle viennent de la même planification, donc ce que dessine le client est ce que fait la machine.
* **Un travail ne démarre jamais tout seul :** il est créé prêt et seul un opérateur le démarre ; un travail à la fois sur une machine (`409 machine_busy`).
* **Dialogues de l'opérateur par le réseau :** le travail s'arrête au changement d'outil et à la confirmation de la broche jusqu'à ce qu'un client confirme.
* **Événements en direct :** un websocket annonce chaque changement d'un travail au fur et à mesure, avec les résumés des travaux à la connexion.
* **Étalonnage à un seul endroit :** les fichiers de configuration et les valeurs réglées de chaque machine sont gardés sur le serveur, donc tous les clients planifient avec les mêmes chiffres.
* **Un travail qui tournait quand le serveur s'est arrêté est marqué en échec :** personne ne sait où était la machine, donc rien ne reprend tout seul.
* **Fichiers sûrs :** utilisateurs, jetons, machines et travaux sont des fichiers JSON écrits via un fichier temporaire ; un fichier endommagé est une erreur, jamais un redémarrage silencieux à zéro.
* **Mises à jour depuis GitHub :** avec `updates_dir` le serveur propose à un administrateur la vérification des dernières versions de lui-même et d'EH3D-STUDIO et leur mise à jour (`/api/v1/updates`) ; un petit assistant lancé par systemd (`deploy/eh3d-update.py`) télécharge, vérifie le SHA-256 de chaque fichier, refuse une archive aux noms hors de son dossier, remplace le dossier installé (l'ancien est gardé en `.previous`) et redémarre le service. Il a été testé avec GitHub et systemd remplacés par des doublures, jamais sur une installation réelle.
* **Le nœud de la tête de perçage par le serveur :** quand la configuration d'une machine nomme le nœud (`head_address`, `head_token`), le serveur relaie son état (`GET /api/v1/machines/:id/head/status`), sa caméra en MJPEG (`.../head/stream`, avec le jeton dans l'adresse pour un navigateur) et une mesure de la table (`POST .../head/measure`, opérateur), si bien qu'un programme sur un autre réseau l'atteint avec la même connexion. Testé avec un nœud de substitution, jamais avec un vrai.

## 🔒 Sécurité et protection

| Menace | Défense |
|---|---|
| N'importe qui sur le réseau envoyant des requêtes | toute requête sauf le contrôle de santé, la première mise en service et la connexion exige un jeton (`Authorization: Bearer`, ou `?token=` pour le websocket) ; trois rôles : le lecteur lit, l'opérateur exécute des travaux, l'administrateur gère utilisateurs, jetons et machines |
| Deviner un mot de passe | cinq mauvais mots de passe bloquent le nom, 30 s puis 60 s, 120 s et ainsi de suite tant qu'ils continuent (`429 throttled`) ; la réponse ne dit pas quelle partie était fausse, et un nom inconnu coûte autant qu'un nom connu |
| Une copie du dossier de données | les mots de passe sont des hachages Argon2id ; les jetons de session et de programme sont aléatoires et seul leur SHA-256 est gardé, donc une copie du dossier ne connecte personne ; le jeton d'un nœud n'est jamais montré par l'API |
| Prendre la main sur un serveur neuf | le premier administrateur est créé avec un code de mise en service que seule la console imprime |
| Verrouiller tout le monde dehors | le dernier administrateur ne peut être ni supprimé, ni désactivé, ni rétrogradé (`409 last_admin`) |
| Un travail qui démarre tout seul ou un second travail sur une machine | un travail est créé prêt ; seul un opérateur le démarre, et une machine exécute un travail à la fois |
| Un plan qui heurterait la table ou utiliserait un outil manquant | la planification refuse avec un code les trous et parcours hors de la table et les outils manquants ou hors plage |
| Le serveur meurt pendant que la machine bouge | le nœud s'arrête de lui-même quand son lien se tait ; le travail est marqué en échec et ne reprend jamais tout seul ; le service arrête ses travaux sur SIGINT |
| Une requête énorme | le corps est limité (`max_body_mb`, 8 Mo par défaut) |
| Un fichier de données endommagé | une erreur, jamais un démarrage silencieux à zéro |
| Un service avec trop de droits | l'unité systemd tourne sous son propre utilisateur avec `NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp` et un seul dossier inscriptible |

**Ce qui n'est pas défendu :** le serveur ne termine pas le TLS, donc jetons et mots de passe traversent le réseau en clair sauf si l'on met du HTTPS devant ; le jeton du websocket est dans l'adresse ; il n'a pas eu de revue indépendante. Signale une vulnérabilité en privé (voir [SECURITY.md](SECURITY.md)).

## 🔌 Interfaces

| Interface | Ce que c'est | Où |
|---|---|---|
| REST | `/api/v1/` : `health`, `setup`, `login`, `logout`, `session`, `account`, `users`, `tokens`, `updates`, `machines` (avec `preview` et `jobs`), `jobs` (avec `start`, `confirm`, `cancel`) ; JSON en entrée et en sortie, erreurs sous la forme `{"error","detail"}` | [docs/API.md](docs/API.md), `src/api.rs` |
| Websocket | `GET /api/v1/events?token=...` : un `hello` avec les résumés des travaux, puis `job` et `job_removed` à chaque changement | [docs/API.md](docs/API.md) |
| Vers la machine | la `connection` d'une machine est `node` (un EH3D-NODE-BUNGARD à une adresse avec son jeton) ou `demo` (le simulateur) | [docs/API.md](docs/API.md), `src/machines.rs` |
| Ligne de commande | `eh3d-server [--config FICHIER]` ; `--help` la liste | `src/main.rs` |
| Service | un binaire, port 18200 par défaut, une unité systemd | [docs/DEPLOY.md](docs/DEPLOY.md), `deploy/eh3d-server.service` |

## ⚙️ Configuration

`server.cfg` (ou le fichier donné avec `--config`) contient cinq clés facultatives ; une clé inconnue ou une valeur hors plage est une erreur au démarrage. `server.cfg.example` les explique.

| Clé | Défaut | Plage | Ce qu'elle fait |
|---|---|---|---|
| `listen` | `0.0.0.0:18200` | une adresse et un port | où le serveur écoute (18200 est son propre port) |
| `data_dir` | `data` | un dossier | où sont gardés utilisateurs, jetons, machines et travaux ; à sauvegarder |
| `session_hours` | 12 | de 1 à 720 | durée d'une connexion |
| `max_body_mb` | 8 | de 1 à 256 | la plus grande requête, en mégaoctets |
| `keep_jobs` | 200 | de 10 à 100 000 | combien de travaux terminés la liste garde |

Noms des utilisateurs et des jetons : lettres, chiffres, `.`, `_` et `-`, jusqu'à 32 caractères ; mots de passe : de 8 à 128 caractères. Les valeurs réglées sur chaque machine ne sont pas ici : elles vivent dans la fiche de la machine, au format du fichier de machine des programmes.

## 🚀 Démarrage rapide

Il faut une chaîne d'outils Rust stable, et EH3D-DRILLPRO et EH3D-ROUTEPRO à côté de ce dossier (le serveur utilise leurs bibliothèques).

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

Lance-le avec `run.bat` (Windows) ou `./run.sh` (Linux) ; il compile d'abord si besoin. Tant qu'il n'a pas d'utilisateur, la console affiche un code de mise en service : crée l'administrateur avec `POST /api/v1/setup`, connecte-toi, ajoute la machine avec l'adresse et le jeton de son nœud, et crée un jeton pour chaque programme. `server.cfg.example` liste les réglages ; `docs/DEPLOY.md` l'installe comme service sur la CM5.

## 🧪 Tests

La suite compte 51 tests (`cargo test`, exécutés sous Windows et Linux dans la CI avec COMMON, DRILLPRO et ROUTEPRO récupérés à côté du serveur) : 35 dans les modules (réglages et leurs plages, utilisateurs, rôles, jetons, le ralentissement sur mauvais mot de passe, les fichiers et le registre des travaux, la planification), 11 sur l'interface de bout en bout par le vrai routeur (`tests/api.rs`) et 5 avec les vrais programmes envoyant leurs travaux à un vrai serveur sur un vrai port, la machine étant sur le simulateur (`tests/programs.rs`). La CI exécute aussi `cargo fmt --all --check`, `cargo clippy` avec les avertissements en erreurs et `python -I tools/ci_validate.py`. Rien de tout cela n'a été testé contre un vrai nœud ou une vraie machine. L'assistant qui fait les mises à jour a 10 tests de plus qui lui sont propres (`python -I tests/test_update_helper.py`, avec GitHub et systemd remplacés par des doublures).

## 🏗️ Compilation et publication

La version se trouve dans `Cargo.toml` et dans `eh3d.project.json`, et chaque version a son entrée dans [CHANGELOG.md](CHANGELOG.md). `build.sh` / `build.bat` compilent le binaire de release (`target/release/eh3d-server`) avec un panneau en direct, et `test.sh` / `test.bat` n'exécutent que les tests ; aucun ne change la version. Aucune version n'a encore été publiée. Sur une CM5 (Linux aarch64), on le compile sur la machine elle-même ; `docs/DEPLOY.md` l'installe comme service.

## 📂 Structure du dépôt

<p align="center">
  <img src="images/repository-structure.svg" alt="Structure du dépôt EH3D-SERVER" width="100%">
</p>

<details>
<summary>Version texte de l'arborescence</summary>

```text
EH3D-SERVER/
├── src/
│   ├── api.rs                                   les routes REST et le websocket
│   ├── auth.rs                                  utilisateurs, rôles, jetons et ralentissement sur mauvais mot de passe
│   ├── plan.rs                                  de la requête aux éléments que la machine exécute
│   ├── jobs.rs state.rs                         les travaux et ce dont le serveur est fait pendant qu'il tourne
│   ├── machines.rs store.rs                     les machines, et les fichiers écrits via un fichier temporaire
│   ├── config.rs lib.rs main.rs                 le fichier de réglages, la racine de la bibliothèque et le programme
│   └── updates.rs  head.rs                      les mises à jour des services
├── tests/                                       l'interface de bout en bout sur le simulateur et les vrais programmes contre un vrai serveur
├── docs/                                        l'API, le guide pour exécuter le serveur et les données des deux images
├── deploy/                                      l'unité systemd du service
├── server.cfg.example                           le fichier de réglages avec chaque clé expliquée
├── tools/                                       contrôle de la CI et du README, le panneau des scripts et outils d'images
├── images/                                      bannière et images d'Architecture et de Structure du dépôt
├── .github/                                     workflows de CI et de publication, modèles d'issues et de pull requests
├── build.sh build.bat                           compile le binaire de release avec un panneau en direct
├── run.sh run.bat                               démarre le serveur (compile d'abord si besoin)
├── eh3d.project.json                            manifeste : version, rôle, maturité et commande de compilation
├── Cargo.toml Cargo.lock                        le paquet Rust, ses dépendances et les bibliothèques des deux programmes
├── CHANGELOG.md                                 ce qui a changé dans chaque version
├── SECURITY.md SUPPORT.md                       signaler une vulnérabilité et poser des questions
└── CONTRIBUTING.md CODE_OF_CONDUCT.md LICENSE   contribuer, code de conduite et licence
```

</details>

## 🗺️ Feuille de route et limites connues

**Limites connues aujourd'hui**

* Rien n'a tourné sur une vraie machine par le serveur : les travaux n'ont tourné que contre le simulateur et le lien avec EH3D-NODE-BUNGARD n'a pas été atteint pour de vrai.
* Le serveur ne termine pas le TLS ; mets du HTTPS devant avant que quoi que ce soit de l'extérieur l'atteigne. Il n'a pas eu de revue indépendante.
* Parmi les clients en plus des programmes, EH3D-STUDIO existe ; l'application Android et le panneau tactile n'existent pas encore.
* Il a besoin des dossiers d'EH3D-DRILLPRO et d'EH3D-ROUTEPRO à côté de lui, car il utilise leurs bibliothèques.
* Un travail qui tournait quand le serveur s'est arrêté est marqué en échec et ne reprend pas.

**Ensuite**

* Exécuter un travail entier sur une vraie machine par le serveur et le nœud.
* HTTPS dans le serveur lui-même.
* Utiliser les voyants du nœud et son tampon pour finir un travail retenu quand le serveur disparaît.
* Construire le premier client en plus des programmes.

## 🌍 Langues

Le README et les textes existent en anglais, espagnol, français, italien, allemand, chinois, japonais et portugais européen.

## 🔗 Projets liés

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) : le programme de perçage ; sa bibliothèque planifie et exécute les travaux de perçage
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) : le programme de défonçage ; sa bibliothèque planifie les travaux de défonçage
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD) : le nœud dans le boîtier de la machine avec lequel le serveur parle
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON) : les parties communes de la famille

## 📚 Documentation

* [L'API](docs/API.md) : chaque requête, le travail et la vue que dessine un client, les états et le websocket.
* [Exécuter le serveur](docs/DEPLOY.md) : la compilation, le premier démarrage, le service sur la CM5 et la sécurité du déploiement.
* [Journal des modifications](CHANGELOG.md)

## 🤝 Contribuer et communauté

* Avant une pull request, exécute `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` et `python -I tools/ci_validate.py` : tout doit réussir. Voir [CONTRIBUTING.md](CONTRIBUTING.md) et le [code de conduite](CODE_OF_CONDUCT.md).
* Le README existe en huit langues : une modification du texte se fait dans les huit fichiers, et les deux images sont dessinées à partir de `docs/architecture.json` et `docs/structure.json` par `tools/make_architecture.py` et `tools/make_structure.py`.
* Un changement de route de l'API exige son entrée dans `docs/API.md`, un test par le vrai routeur et, pour une requête qui modifie quelque chose, le rôle qui peut la faire.
* Signalez une vulnérabilité en privé : [SECURITY.md](SECURITY.md). Questions, idées et rapports : electrohobby3d@gmail.com ([SUPPORT.md](SUPPORT.md)).

## 👤 AUTEUR

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 LICENCE

Logiciel : GPL-3.0-or-later. Voir [LICENSE](LICENSE). Les conceptions matérielles de la suite utilisent la CERN OHL-S.
