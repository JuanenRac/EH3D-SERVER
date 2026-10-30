<p align="center">
  <img src="images/EH3D_BANNER.svg" alt="EH3D-SERVER banner" width="100%">
</p>

# 🧭 EH3D-SERVER

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | <a href="README_fra.md">🇫🇷 Français</a> | <a href="README_ita.md">🇮🇹 Italiano</a> | 🇩🇪 <b>Deutsch</b> | <a href="README_zho.md">🇨🇳 中文</a> | <a href="README_jpn.md">🇯🇵 日本語</a> | <a href="README_por.md">🇵🇹 Português</a></p>

### Das Zentrum der EH3D-Programme: Es nimmt Aufträge von DRILLPRO, ROUTEPRO und den künftigen Clients an, plant sie, führt sie über den Knoten auf der Maschine aus und berichtet allen, wie sie laufen

<p align="center">
  <img src="https://img.shields.io/badge/License-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Stack-Rust-DEA584.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Languages-8-00E5FF.svg" alt="Languages">
  <img src="https://img.shields.io/badge/Version-0.0.1-0f6cbd.svg" alt="Version">
  <img src="https://img.shields.io/badge/Maturity-scaffolding-FFB020.svg" alt="Maturity">
</p>

---

**Ehrlichkeitscheck - was heute tatsächlich läuft:** ein Rust-Server (axum) mit REST- und Websocket-Schnittstelle, der Benutzer, Maschinen und Aufträge verwaltet, einen Bohr- oder Fräsauftrag (Bohrungen und Pfade oder eine Excellon- oder HP-GL-Datei mit ihrer Werkzeugtabelle) mit den Bibliotheken von EH3D-DRILLPRO und EH3D-ROUTEPRO plant, eine Vorschau zeigt und ihn über den EH3D-NODE-BUNGARD oder in einem Simulator auf einer Maschine ausführt, mit den Dialogen des Bedieners, einem Stopp und jeder Änderung, die sofort gemeldet wird. 41 Tests laufen auf einem Computer, die Aufträge gegen den Simulator. **Über ihn lief noch nichts auf einer echten Maschine**, er beendet kein TLS, und die Clients, die ihn neben den Programmen nutzen sollen (die Android-App, das Studio, das Panel an der Maschine), gibt es noch nicht.

---

> **Stand von Sicherheit und IT-Sicherheit:** dies ist kein Sicherheitssystem (Safety). Ein Auftrag startet nie von selbst: Eine Person mit der Rolle Bediener startet ihn, jeweils ein Auftrag pro Maschine, und die Planung weist Bohrungen und Pfade außerhalb des Tisches sowie fehlende oder außerhalb des Bereichs liegende Werkzeuge ab. Jede Anfrage braucht ein Token (ein Betrachter liest, ein Bediener führt Aufträge aus, ein Administrator verwaltet Benutzer, Tokens und Maschinen); Passwörter werden als Argon2-Hashes gespeichert, Sitzungs- und Programm-Tokens nur als SHA-256, und das Token eines Knotens zeigt die API nie. Der Server ist für das Haus oder ein VPN dorthin: Setze HTTPS davor, bevor etwas von außen ihn erreicht. Der Stopp der Maschine hängt nicht allein vom Server ab: Der Knoten stoppt von selbst, wenn seine Verbindung still wird. Das Projekt ist unabhängig: Es wird nicht von Bungard Elektronik hergestellt, empfohlen oder unterstützt.

## 📋 Auf einen Blick

| | |
|---|---|
| Version | `0.0.1` |
| Reifegrad | Gerüst |
| Tests | 47 |
| Technologie-Stack | Rust (axum, tokio) |
| Auf echter Hardware | Nein: gegen den Simulator getestet; über den Server lief noch nichts auf einer echten Maschine |

**Springe zu:** [Überblick](#-überblick) · [Architektur](#-architektur) · [Schnellstart](#-schnellstart) · [Tests](#-tests) · [Struktur](#-struktur-des-repositorys) · [Verwandte](#-verwandte-projekte)

## 🚦 Projektstatus

Jede Zeile ist ein Teil dieses Repositorys, und die Ampel sagt, wie weit er heute ist. Sie beruht auf dem, was das Repository zeigen kann (Tests, Builds, Läufe), nicht auf dem, was geplant ist.

🟢 Fertig und überprüft · 🟡 Funktioniert, mit bekannten Grenzen · 🟠 Teilweise fertig oder noch nicht überprüft · 🔴 Nicht gemacht oder funktioniert nicht

| Bereich | Stand | Was das hier bedeutet |
|---|---|---|
| Software | 🟡 | Ein Rust-Server (axum) verwaltet Benutzer, Maschinen und Aufträge, plant Bohr- und Fräsaufträge mit den Bibliotheken von EH3D-DRILLPRO und EH3D-ROUTEPRO, zeigt eine Vorschau und führt den Auftrag mit Bedienerdialogen und einem Stopp aus. |
| Tests | 🟢 | 47 Tests bestehen (`cargo test`): 29 in den Modulen und 14 an der Schnittstelle von Ende zu Ende, mit Aufträgen im Simulator (8 in `tests/api.rs`, 4 mit den echten Programmen in `tests/programs.rs`). |
| Schnittstelle für Clients (API) | 🟡 | Die REST-Anfragen und die Websocket-Ereignisse sind in `docs/API.md` beschrieben und von den Tests abgedeckt; bisher nutzt sie nur die Serververbindung der Programme selbst. |
| Clients | 🔴 | Die Android-App, das Studio und das Touch-Panel an der Maschine gibt es noch nicht. |
| Sicherheit | 🟡 | Jede Anfrage braucht ein Token mit einer von drei Rollen, Passwörter sind Argon2-Hashes und falsche Passwörter werden ausgebremst; der Server beendet kein TLS (setze HTTPS davor) und wurde nicht unabhängig geprüft. |
| Echte Hardware | 🟠 | Die Aufträge liefen nur gegen den Simulator: Über den Server lief nichts auf einer echten Maschine. |
| Bereitstellung | 🟠 | Eine systemd-Unit und `docs/DEPLOY.md` beschreiben den Dienst auf der CM5; dieses Repository verzeichnet keine Bereitstellung als erledigt. |
| Integration | 🟡 | Er plant mit denselben Bibliotheken wie EH3D-DRILLPRO und EH3D-ROUTEPRO (deren Ordner müssen neben diesem liegen) und spricht mit der Verbindung von EH3D-NODE-BUNGARD, die er nur über den Simulator erreicht hat. |
| Dokumentation | 🟢 | README in acht Sprachen, die API-Referenz und die Anleitung, ihn als Dienst zu betreiben. |

## 🎯 Überblick

* Die **Aufträge**: ein Bohrauftrag (Bohrungen und Werkzeuge oder eine `.NCD`-Datei mit ihrer Werkzeugtabelle) oder ein Fräsauftrag (Pfade und Werkzeuge oder eine `.PLT`-Datei mit ihrer Werkzeugtabelle), mit den Bibliotheken der Programme geplant, sodass ein Client zeichnet und die Maschine genau dasselbe tut; mit einem Code abgewiesen, wenn eine Bohrung außerhalb des Tisches liegt oder ein Werkzeug fehlt.
* Die **Vorschau**: die Arbeit in der Reihenfolge, in der die Maschine sie ausführt, die Werkzeuge mit ihrer Anzahl, der Hüllquader, eine Zeitschätzung und das Element, bei dem jede Bohrung fertig ist - was ein Bildschirm braucht, um die Platine zu zeichnen und den Auftrag zu verfolgen.
* Der **Lauf**: Ein Auftrag ist bereit, bis eine Person ihn startet; er läuft auf der Bibliothek der Programme (Handshake, Scharfschalten, Referenzfahrt, Bohrungen, Stopp), wartet an den Dialogen des Bedieners (Werkzeug wechseln, Spindel bestätigen), bis jemand bestätigt, und kann gestoppt werden; die Maschine stoppt mit abgeschalteten Treibern.
* Die **Maschinen**: zu jeder, wie man sie erreicht (ein EH3D-NODE-BUNGARD über das Netz oder der Simulator), der Text ihrer Konfigurationsdateien und die an ihr abgestimmten Werte, sodass die Kalibrierung einer Maschine für alle Clients an einer Stelle liegt.
* Die **Flotte und die Stationen**: jede Maschine gehört zu einer Station (die Werkbank, der Raum, die Werkstatt), und `GET /api/v1/fleet` antwortet für alle auf einmal, nach Stationen gruppiert, mit dem, was jede tut (frei, beschäftigt, wartet auf den Bediener), ihrem Auftrag mit dem Fortschritt und der Zahl der bereiten Aufträge; die Maschinen laufen gleichzeitig, ein Auftrag auf jeder.
* Die **Menschen und die Programme**: Benutzer mit drei Rollen, Tokens für Programme, die nur einmal angezeigt werden, eine Bremse bei falschen Passwörtern und ein Einrichtungscode auf der Konsole für den ersten Administrator.
* Die **Ereignisse**: ein Websocket, der jede Änderung eines Auftrags sofort sendet.
* Der **Dienst**: eine Binärdatei und eine kleine Einstellungsdatei, standardmäßig auf Port 18200 (nicht der von ARMOR oder HYDRA-UMC), mit einer systemd-Unit für die CM5.
* **Geplant, noch nicht gebaut**: die Clients (die Android-App, das Studio, das Panel mit Touchscreen an der Maschine), HTTPS im Server selbst und die Nutzung der Lampen und des Puffers des Knotens, um einen festgehaltenen Auftrag zu beenden, wenn der Server wegfällt.

## 🧩 Rolle in EH3D

EH3D-SERVER ist das Zentrum der EH3D-Familie: Die Programme schicken ihm ihre Aufträge, er spricht über deren Knoten mit den Maschinen, und die anderen Clients sehen und senden dieselben Aufträge über dieselbe Schnittstelle. Er nutzt die Bibliotheken von [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) und [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) und spricht mit [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD).

## 🏛️ Architektur

<p align="center">
  <img src="images/architecture.svg" alt="Architektur von EH3D-SERVER: wer es nutzt, was es enthält und wovon es abhängt" width="100%">
</p>

Wie die Teile dieses Repositorys zusammenpassen: wer es nutzt, was es enthält und wovon es abhängt. Das Bild wird aus `docs/architecture.json` gezeichnet und gegen den Code geprüft.

## ✨ Funktionen

* **Eine Vorschau, die der Plan ist:** Vorschau und echter Lauf stammen aus derselben Planung, sodass ein Client zeichnet, was die Maschine tut.
* **Ein Auftrag startet nie von selbst:** Er wird bereit angelegt, und nur ein Bediener startet ihn; ein Auftrag pro Maschine (`409 machine_busy`).
* **Bedienerdialoge über das Netz:** Der Auftrag pausiert beim Werkzeugwechsel und bei der Spindelbestätigung, bis ein Client bestätigt.
* **Ereignisse live:** Ein Websocket meldet jede Änderung eines Auftrags sofort, beim Verbinden mit den Zusammenfassungen der Aufträge.
* **Kalibrierung an einer Stelle:** Die Konfigurationsdateien und abgestimmten Werte jeder Maschine liegen auf dem Server, sodass alle Clients mit denselben Zahlen planen.
* **Ein Auftrag, der beim Stopp des Servers lief, wird als fehlgeschlagen markiert:** Niemand weiß, wo die Maschine war, daher setzt nichts von selbst fort.
* **Sichere Dateien:** Benutzer, Tokens, Maschinen und Aufträge sind JSON-Dateien, die über eine temporäre Datei geschrieben werden; eine beschädigte Datei ist ein Fehler, nie ein stiller Neustart bei null.
* **Aktualisierungen von GitHub:** mit `updates_dir` bietet der Server einem Administrator die Prüfung der neuesten Releases von sich selbst und von EH3D-STUDIO und deren Aktualisierung an (`/api/v1/updates`); ein kleiner, von systemd gestarteter Helfer (`deploy/eh3d-update.py`) lädt herunter, prüft den SHA-256 jeder Datei, lehnt ein Archiv mit Namen außerhalb seines Ordners ab, ersetzt den installierten Ordner (der alte bleibt als `.previous`) und startet den Dienst neu. Getestet mit durch Attrappen ersetzten GitHub und systemd, nie an einer echten Installation.

## 🔒 Sicherheit und Schutz

| Bedrohung | Abwehr |
|---|---|
| Jeder im Netz, der Anfragen sendet | jede Anfrage außer Gesundheitsprüfung, Ersteinrichtung und Anmeldung braucht ein Token (`Authorization: Bearer`, beim Websocket `?token=`); drei Rollen: Betrachter liest, Bediener führt Aufträge aus, Administrator verwaltet Benutzer, Tokens und Maschinen |
| Ein Passwort erraten | fünf falsche Passwörter sperren den Namen, 30 s und dann 60 s, 120 s und so weiter, solange sie anhalten (`429 throttled`); die Antwort sagt nicht, welcher Teil falsch war, und ein unbekannter Name kostet so viel wie ein bekannter |
| Eine Kopie des Datenordners | Passwörter sind Argon2id-Hashes; Sitzungs- und Programm-Tokens sind zufällig und nur ihr SHA-256 wird gespeichert, sodass eine Kopie des Ordners niemanden anmeldet; das Token eines Knotens zeigt die API nie |
| Übernahme eines frischen Servers | Der erste Administrator wird mit einem Einrichtungscode angelegt, den nur die Konsole ausgibt |
| Alle aussperren | Der letzte Administrator kann nicht gelöscht, deaktiviert oder herabgestuft werden (`409 last_admin`) |
| Ein Auftrag, der von selbst startet, oder ein zweiter Auftrag auf einer Maschine | Ein Auftrag wird bereit angelegt; nur ein Bediener startet ihn, und eine Maschine führt einen Auftrag zur Zeit aus |
| Ein Plan, der den Tisch träfe oder ein fehlendes Werkzeug nutzte | Die Planung weist Bohrungen und Pfade außerhalb des Tisches sowie fehlende oder außerhalb des Bereichs liegende Werkzeuge mit einem Code ab |
| Der Server stirbt, während die Maschine fährt | Der Knoten stoppt von selbst, wenn seine Verbindung still wird; der Auftrag wird als fehlgeschlagen markiert und setzt nie von selbst fort; der Dienst stoppt seine Aufträge bei SIGINT |
| Eine riesige Anfrage | Der Rumpf ist begrenzt (`max_body_mb`, standardmäßig 8 MB) |
| Eine beschädigte Datendatei | ein Fehler, nie ein stiller Start bei null |
| Ein Dienst mit zu vielen Rechten | Die systemd-Unit läuft unter eigenem Benutzer mit `NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp` und einem einzigen beschreibbaren Ordner |

**Was nicht abgewehrt wird:** Der Server beendet kein TLS, daher laufen Tokens und Passwörter im Klartext durch das Netz, wenn man kein HTTPS davorsetzt; das Token des Websockets steht in der Adresse; er wurde nicht unabhängig geprüft. Melde eine Schwachstelle privat (siehe [SECURITY.md](SECURITY.md)).

## 🔌 Schnittstellen

| Schnittstelle | Was es ist | Wo |
|---|---|---|
| REST | `/api/v1/`: `health`, `setup`, `login`, `logout`, `session`, `account`, `users`, `tokens`, `updates`, `machines` (mit `preview` und `jobs`), `jobs` (mit `start`, `confirm`, `cancel`); JSON hinein und heraus, Fehler als `{"error","detail"}` | [docs/API.md](docs/API.md), `src/api.rs` |
| Websocket | `GET /api/v1/events?token=...`: ein `hello` mit den Zusammenfassungen der Aufträge, danach `job` und `job_removed` bei jeder Änderung | [docs/API.md](docs/API.md) |
| Zur Maschine | die `connection` einer Maschine ist `node` (ein EH3D-NODE-BUNGARD unter einer Adresse mit seinem Token) oder `demo` (der Simulator) | [docs/API.md](docs/API.md), `src/machines.rs` |
| Kommandozeile | `eh3d-server [--config DATEI]`; `--help` listet es auf | `src/main.rs` |
| Dienst | eine Binärdatei, standardmäßig Port 18200, eine systemd-Unit | [docs/DEPLOY.md](docs/DEPLOY.md), `deploy/eh3d-server.service` |

## ⚙️ Konfiguration

`server.cfg` (oder die mit `--config` angegebene Datei) enthält fünf optionale Schlüssel; ein unbekannter Schlüssel oder ein Wert außerhalb des Bereichs ist beim Start ein Fehler. `server.cfg.example` erklärt sie.

| Schlüssel | Standard | Bereich | Was er bewirkt |
|---|---|---|---|
| `listen` | `0.0.0.0:18200` | eine Adresse und ein Port | wo der Server lauscht (18200 ist sein eigener Port) |
| `data_dir` | `data` | ein Ordner | wo Benutzer, Tokens, Maschinen und Aufträge liegen; sichern |
| `session_hours` | 12 | 1 bis 720 | wie lange eine Anmeldung gilt |
| `max_body_mb` | 8 | 1 bis 256 | die größte Anfrage in Megabyte |
| `keep_jobs` | 200 | 10 bis 100.000 | wie viele beendete Aufträge die Liste behält |

Namen von Benutzern und Tokens: Buchstaben, Ziffern, `.`, `_` und `-`, bis 32 Zeichen; Passwörter: 8 bis 128 Zeichen. Die an jeder Maschine abgestimmten Werte stehen nicht hier: Sie liegen im Datensatz der Maschine, im Format der Maschinendatei der Programme.

## 🚀 Schnellstart

Du brauchst eine stabile Rust-Toolchain sowie EH3D-DRILLPRO und EH3D-ROUTEPRO neben diesem Ordner (der Server nutzt ihre Bibliotheken).

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

Starte ihn mit `run.bat` (Windows) oder `./run.sh` (Linux); bei Bedarf baut er zuerst. Solange es keinen Benutzer gibt, zeigt die Konsole einen Einrichtungscode: Lege den Administrator mit `POST /api/v1/setup` an, melde dich an, füge die Maschine mit Adresse und Token ihres Knotens hinzu und erzeuge für jedes Programm ein Token. `server.cfg.example` listet die Einstellungen auf; `docs/DEPLOY.md` installiert ihn als Dienst auf der CM5.

## 🧪 Tests

Die Suite hat 47 Tests (`cargo test`, in der CI unter Windows und Linux mit COMMON, DRILLPRO und ROUTEPRO neben dem Server ausgeführt): 29 in den Modulen (Einstellungen und ihre Bereiche, Benutzer, Rollen, Tokens, die Bremse bei falschen Passwörtern, die Dateien und das Auftragsbuch, die Planung), 8 an der Schnittstelle von Ende zu Ende über den echten Router (`tests/api.rs`) und 5 mit den echten Programmen, die ihre Aufträge an einen echten Server auf einem echten Port schicken, mit der Maschine im Simulator (`tests/programs.rs`). Die CI führt außerdem `cargo fmt --all --check`, `cargo clippy` mit Warnungen als Fehlern und `python -I tools/ci_validate.py` aus. Nichts davon wurde gegen einen echten Knoten oder eine echte Maschine getestet. Der Helfer, der die Aktualisierungen macht, hat 10 weitere eigene Tests (`python -I tests/test_update_helper.py`, mit durch Attrappen ersetzten GitHub und systemd).

## 🏗️ Build und Veröffentlichung

Die Version steht in `Cargo.toml` und in `eh3d.project.json`, und jede Version hat ihren Eintrag in [CHANGELOG.md](CHANGELOG.md). `build.sh` / `build.bat` bauen die Release-Binärdatei (`target/release/eh3d-server`) mit einem Live-Panel, und `test.sh` / `test.bat` führen nur die Tests aus; keines ändert die Version. Es wurde noch kein Release veröffentlicht. Auf einer CM5 (aarch64-Linux) baut man ihn auf der Maschine selbst; `docs/DEPLOY.md` installiert ihn als Dienst.

## 📂 Struktur des Repositorys

<p align="center">
  <img src="images/repository-structure.svg" alt="Struktur des Repositorys EH3D-SERVER" width="100%">
</p>

<details>
<summary>Textversion des Baums</summary>

```text
EH3D-SERVER/
├── src/
│   ├── api.rs                                   die REST-Routen und der Websocket
│   ├── auth.rs                                  Benutzer, Rollen, Tokens und die Bremse bei falschen Passwörtern
│   ├── plan.rs                                  von der Anfrage zu den Elementen, die die Maschine ausführt
│   ├── jobs.rs state.rs                         die Aufträge und woraus der Server im Betrieb besteht
│   ├── machines.rs store.rs                     die Maschinen und die über eine temporäre Datei geschriebenen Dateien
│   ├── config.rs lib.rs main.rs                 die Einstellungsdatei, die Bibliothekswurzel und das Programm
│   └── updates.rs                               die Aktualisierungen der Dienste
├── tests/                                       die Schnittstelle von Ende zu Ende im Simulator und die echten Programme gegen einen echten Server
├── docs/                                        die API, die Anleitung zum Betrieb des Servers und die Daten der beiden Bilder
├── deploy/                                      die systemd-Unit des Dienstes
├── server.cfg.example                           die Einstellungsdatei mit jeder erklärten Taste
├── tools/                                       CI-Prüfung, README-Prüfung, das Panel der Skripte und die Bildwerkzeuge
├── images/                                      Banner und die Bilder von Architektur und Repository-Struktur
├── .github/                                     CI- und Release-Workflows, Vorlagen für Issues und Pull Requests
├── build.sh build.bat                           baut die Release-Binärdatei mit einem Live-Panel
├── run.sh run.bat                               startet den Server (baut bei Bedarf zuerst)
├── eh3d.project.json                            Manifest: Version, Rolle, Reifegrad und Build-Befehl
├── Cargo.toml Cargo.lock                        das Rust-Paket, seine Abhängigkeiten und die Bibliotheken der beiden Programme
├── CHANGELOG.md                                 was sich in jeder Version geändert hat
├── SECURITY.md SUPPORT.md                       Schwachstellen melden und Fragen stellen
└── CONTRIBUTING.md CODE_OF_CONDUCT.md LICENSE   Mitwirken, Verhaltenskodex und Lizenz
```

</details>

## 🗺️ Roadmap und bekannte Grenzen

**Bekannte Grenzen heute**

* Über den Server lief nichts auf einer echten Maschine: Die Aufträge liefen nur gegen den Simulator, und die Verbindung zum EH3D-NODE-BUNGARD wurde nie echt erreicht.
* Der Server beendet kein TLS; setze HTTPS davor, bevor etwas von außen ihn erreicht. Er wurde nicht unabhängig geprüft.
* Die Clients, die ihn neben den Programmen nutzen sollen (die Android-App, das Studio, das Touch-Panel), gibt es noch nicht.
* Er braucht die Ordner von EH3D-DRILLPRO und EH3D-ROUTEPRO neben sich, weil er ihre Bibliotheken nutzt.
* Ein Auftrag, der beim Stopp des Servers lief, wird als fehlgeschlagen markiert und setzt nicht fort.

**Als Nächstes**

* Einen ganzen Auftrag über Server und Knoten auf einer echten Maschine ausführen.
* HTTPS im Server selbst.
* Die Lampen und den Puffer des Knotens nutzen, um einen festgehaltenen Auftrag zu beenden, wenn der Server wegfällt.
* Den ersten Client neben den Programmen bauen.

## 🌍 Sprachen

README und Texte gibt es auf Englisch, Spanisch, Französisch, Italienisch, Deutsch, Chinesisch, Japanisch und europäischem Portugiesisch.

## 🔗 Verwandte Projekte

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO): das Bohrprogramm; seine Bibliothek plant und führt die Bohraufträge aus
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO): das Fräsprogramm; seine Bibliothek plant die Fräsaufträge
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD): der Knoten im Gehäuse der Maschine, mit dem der Server spricht
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON): die gemeinsamen Teile der Familie

## 📚 Dokumentation

* [Die API](docs/API.md): jede Anfrage, der Auftrag und die Ansicht, die ein Client zeichnet, die Zustände und der Websocket.
* [Den Server betreiben](docs/DEPLOY.md): der Build, der erste Start, der Dienst auf der CM5 und die Sicherheit der Bereitstellung.
* [Änderungsprotokoll](CHANGELOG.md)

## 🤝 Mitwirken und Community

* Führe vor einem Pull Request `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` und `python -I tools/ci_validate.py` aus: alles muss bestehen. Siehe [CONTRIBUTING.md](CONTRIBUTING.md) und den [Verhaltenskodex](CODE_OF_CONDUCT.md).
* Das README gibt es in acht Sprachen: Eine Textänderung wird in allen acht Dateien gemacht, und die beiden Bilder werden aus `docs/architecture.json` und `docs/structure.json` mit `tools/make_architecture.py` und `tools/make_structure.py` gezeichnet.
* Eine Änderung an einer Route der API braucht ihren Eintrag in `docs/API.md`, einen Test über den echten Router und, bei einer Anfrage, die etwas ändert, die Rolle, die sie ausführen darf.
* Melde eine Schwachstelle privat: [SECURITY.md](SECURITY.md). Fragen, Ideen und Meldungen: electrohobby3d@gmail.com ([SUPPORT.md](SUPPORT.md)).

## 👤 AUTOR

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 LIZENZ

Software: GPL-3.0-or-later. Siehe [LICENSE](LICENSE). Die Hardware-Entwürfe der Suite verwenden die CERN OHL-S.
