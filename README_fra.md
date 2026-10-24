<p align="center">
  <img src="images/EH3D_BANNER.svg" alt="EH3D-SERVER banner" width="100%">
</p>

# 🧭 EH3D-SERVER

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | 🇫🇷 <b>Français</b> | <a href="README_ita.md">🇮🇹 Italiano</a> | <a href="README_deu.md">🇩🇪 Deutsch</a> | <a href="README_zho.md">🇨🇳 中文</a> | <a href="README_jpn.md">🇯🇵 日本語</a> | <a href="README_por.md">🇵🇹 Português</a></p>

### Le centre des programmes EH3D : il reçoit les travaux de DRILLPRO, ROUTEPRO et des clients à venir, les planifie, les exécute sur la machine par son nœud et dit à tous comment ils avancent

<p align="center">
  <img src="https://img.shields.io/badge/License-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Stack-Rust-DEA584.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Languages-8-00E5FF.svg" alt="Languages">
  <img src="https://img.shields.io/badge/Version-0.0.1-0f6cbd.svg" alt="Version">
  <img src="https://img.shields.io/badge/Maturity-scaffolding-FFB020.svg" alt="Maturity">
</p>

---

**Vérification d'honnêteté - ce qui tourne aujourd'hui :** un serveur Rust (axum) avec une interface REST et websocket qui garde utilisateurs, machines et travaux, planifie un travail de perçage ou de fraisage (trous et trajectoires, ou un fichier Excellon ou HP-GL avec sa table d'outils) avec les bibliothèques d'EH3D-DRILLPRO et d'EH3D-ROUTEPRO, montre un aperçu, et l'exécute sur une machine par son EH3D-NODE-BUNGARD, avec les dialogues de l'opérateur, un arrêt et chaque changement annoncé quand il arrive. 39 tests s'exécutent sur un ordinateur, les travaux contre le simulateur. **Rien n'a encore tourné sur une machine réelle par son intermédiaire**, il ne termine pas le TLS, et les clients qui l'utiliseront en plus des programmes (l'appli Android, le Studio, le panneau à côté de la machine) n'existent pas encore.

---

> **État de la sûreté et de la sécurité :** ceci n'est pas un système de sécurité. Un travail ne démarre jamais seul : une personne ayant le rôle d'opérateur le démarre, un seul à la fois par machine, et la planification refuse les trous et trajectoires hors de la table et les outils manquants ou hors plage. Chaque requête demande un jeton (un observateur lit, un opérateur exécute des travaux, un administrateur gère utilisateurs, jetons et machines) ; les mots de passe sont gardés en hachages Argon2, les jetons de session et de programme seulement en SHA-256, et le jeton d'un nœud n'est jamais montré par l'API. Le serveur est pour la maison ou un VPN vers elle : mettez du HTTPS devant avant que quoi que ce soit de l'extérieur l'atteigne. L'arrêt de la machine ne dépend pas du seul serveur : le nœud s'arrête de lui-même quand sa liaison se tait. Le projet est indépendant : il n'est ni fabriqué, ni approuvé, ni soutenu par Bungard Elektronik.

## 📋 En un coup d'œil

| | |
|---|---|
| Version | `0.0.1` |
| Maturité | Échafaudage |
| Tests | 39 |
| Pile technique | Rust |
| Sur du matériel réel | Non : testé contre le simulateur ; rien n'a encore tourné sur une machine réelle par le serveur |

## 🎯 Vue d'ensemble

* Les **travaux** : un travail de perçage (trous et outils, ou un fichier `.NCD` avec sa table d'outils) ou de fraisage (trajectoires et outils, ou un fichier `.PLT` avec sa table d'outils), planifiés avec les bibliothèques des programmes, si bien qu'un client dessine et qu'une machine fait exactement la même chose ; refusés avec un code quand un trou est hors de la table ou qu'un outil manque.
* L'**aperçu** : le travail dans l'ordre où la machine le fait, les outils avec leur nombre, le rectangle qui l'englobe, une estimation du temps et l'étape à laquelle chaque trou est terminé - ce dont un écran a besoin pour dessiner la carte et suivre le travail.
* L'**exécution** : un travail est prêt jusqu'à ce qu'une personne le démarre ; il tourne sur la bibliothèque des programmes (la poignée de main, l'armement, la référence, les trous, l'arrêt), attend aux dialogues de l'opérateur (changer l'outil, confirmer la broche) jusqu'à ce que quelqu'un confirme, et peut être arrêté ; la machine s'arrête avec les drivers coupés.
* Les **machines** : pour chacune comment l'atteindre (un EH3D-NODE-BUNGARD par le réseau, ou le simulateur), le texte de ses fichiers de configuration et les valeurs réglées sur elle, si bien que l'étalonnage d'une machine vit à un seul endroit pour tous les clients.
* Les **personnes et les programmes** : des utilisateurs à trois rôles, des jetons pour les programmes montrés une seule fois, un ralentissement sur les mots de passe erronés et un code de mise en service sur la console pour le premier administrateur.
* Les **événements** : un websocket qui envoie chaque changement d'un travail au moment où il arrive.
* Le **service** : un binaire et un petit fichier texte de réglages, sur le port 18200 par défaut (ni celui d'ARMOR ni celui d'HYDRA-UMC), avec une unité systemd pour la CM5.
* **Prévu, pas encore fait** : les clients (l'appli Android, le Studio, le panneau tactile à côté de la machine), le HTTPS dans le serveur lui-même, et l'usage des voyants du nœud et de son tampon pour finir un travail détenu quand le serveur disparaît.

## 🧩 Rôle dans EH3D

EH3D-SERVER est le centre de la famille EH3D : les programmes lui envoient leurs travaux, il parle aux machines par leurs nœuds, et les autres clients voient et envoient les mêmes travaux par la même interface. Il utilise les bibliothèques d'[EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) et d'[EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) et parle à [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD).

## 📂 Structure du dépôt

```text
EH3D-SERVER/
├── src/          le serveur : réglages, utilisateurs, machines, planification, travaux, l'interface et le websocket
├── tests/        l'interface de bout en bout, avec les travaux tournant sur le simulateur
├── docs/         API.md et DEPLOY.md
├── deploy/       l'unité systemd
├── tools/        ci_validate.py : vérifications du manifeste, des versions, du changelog et des README
├── images/       bannière
└── eh3d.project.json
```

## 🛠️ COMPILER ET EXÉCUTER

Il faut une chaîne d'outils Rust stable, et EH3D-DRILLPRO et EH3D-ROUTEPRO à côté de ce dossier.

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

Lancez-le avec `run.bat` (Windows) ou `./run.sh` (Linux) ; il se compile d'abord si nécessaire. Tant qu'il n'a pas d'utilisateur, la console montre un code de mise en service : créez l'administrateur avec `POST /api/v1/setup`, connectez-vous, ajoutez la machine avec l'adresse et le jeton de son nœud, et créez un jeton pour chaque programme. `server.cfg.example` liste les réglages ; `docs/DEPLOY.md` l'installe comme service sur la CM5.

## 🔗 Projets liés

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO): le programme de perçage ; sa bibliothèque planifie et exécute les travaux de perçage
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO): le programme de fraisage ; sa bibliothèque planifie les travaux de fraisage
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD): le nœud dans le coffret de la machine auquel parle le serveur
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON): les parties communes de la famille

## 📚 Documentation et communauté

* [L'API](docs/API.md): chaque requête, le travail et la vue qu'un client dessine, les états et le websocket.
* [Faire tourner le serveur](docs/DEPLOY.md): la compilation, le premier démarrage, le service sur la CM5 et la sécurité du déploiement.
* [Journal des modifications](CHANGELOG.md)
* [Contribuer](CONTRIBUTING.md)
* [Politique de sécurité](SECURITY.md)
* [Assistance](SUPPORT.md)

## 👤 AUTEUR

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 LICENCE

Logiciel : GPL-3.0-or-later. Voir [LICENSE](LICENSE). Les conceptions matérielles de la suite utilisent la CERN OHL-S.
