<p align="center">
  <img src="images/EH3D_BANNER.svg" alt="EH3D-SERVER banner" width="100%">
</p>

# 🧭 EH3D-SERVER

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | <a href="README_fra.md">🇫🇷 Français</a> | <a href="README_ita.md">🇮🇹 Italiano</a> | 🇩🇪 <b>Deutsch</b> | <a href="README_zho.md">🇨🇳 中文</a> | <a href="README_jpn.md">🇯🇵 日本語</a> | <a href="README_por.md">🇵🇹 Português</a></p>

### Das Zentrum der EH3D-Programme: Es nimmt Aufträge von DRILLPRO, ROUTEPRO und den kommenden Clients an, plant sie, führt sie über den Knoten auf der Maschine aus und sagt allen, wie es läuft

<p align="center">
  <img src="https://img.shields.io/badge/License-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Stack-Rust-DEA584.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Languages-8-00E5FF.svg" alt="Languages">
  <img src="https://img.shields.io/badge/Version-0.0.1-0f6cbd.svg" alt="Version">
  <img src="https://img.shields.io/badge/Maturity-scaffolding-FFB020.svg" alt="Maturity">
</p>

---

**Ehrlichkeitscheck - was heute tatsächlich läuft:** ein Rust-Server (axum) mit REST- und Websocket-Schnittstelle, der Benutzer, Maschinen und Aufträge verwaltet, einen Bohr- oder Fräsauftrag (Bohrungen und Bahnen oder eine Excellon- oder HP-GL-Datei mit ihrer Werkzeugtabelle) mit den Bibliotheken von EH3D-DRILLPRO und EH3D-ROUTEPRO plant, eine Vorschau zeigt und ihn über den EH3D-NODE-BUNGARD der Maschine ausführt, mit den Dialogen des Bedieners, einem Stopp und jeder Änderung live gemeldet. 39 Tests laufen auf einem Rechner, die Aufträge gegen den Simulator. **Über ihn lief noch nichts an einer echten Maschine**, er beendet kein TLS, und die Clients, die ihn außer den Programmen nutzen werden (die Android-App, das Studio, das Panel an der Maschine), gibt es noch nicht.

---

> **Stand von Sicherheit und IT-Sicherheit:** dies ist kein Sicherheitssystem. Ein Auftrag startet nie von selbst: Eine Person mit der Rolle Bediener startet ihn, einen nach dem anderen pro Maschine, und die Planung lehnt Bohrungen und Bahnen außerhalb des Tisches sowie fehlende oder außerhalb des Bereichs liegende Werkzeuge ab. Jede Anfrage braucht ein Token (ein Betrachter liest, ein Bediener führt Aufträge aus, ein Administrator verwaltet Benutzer, Tokens und Maschinen); Passwörter werden als Argon2-Hashes gespeichert, Sitzungs- und Programm-Tokens nur als SHA-256, und das Token eines Knotens zeigt die API nie. Der Server ist für das Haus oder ein VPN dorthin: Setze HTTPS davor, bevor etwas von außen herankommt. Der Stopp der Maschine hängt nicht allein vom Server ab: Der Knoten hält von selbst an, wenn seine Verbindung verstummt. Das Projekt ist unabhängig: Es wird nicht von Bungard Elektronik hergestellt, empfohlen oder unterstützt.

## 📋 Auf einen Blick

| | |
|---|---|
| Version | `0.0.1` |
| Reifegrad | Gerüst |
| Tests | 39 |
| Technologie-Stack | Rust |
| Auf echter Hardware | Nein: gegen den Simulator getestet; über den Server lief noch nichts an einer echten Maschine |

## 🎯 Überblick

* Die **Aufträge**: ein Bohrauftrag (Bohrungen und Werkzeuge oder eine `.NCD`-Datei mit Werkzeugtabelle) oder ein Fräsauftrag (Bahnen und Werkzeuge oder eine `.PLT`-Datei mit Werkzeugtabelle), geplant mit den Bibliotheken der Programme, sodass ein Client genau das zeichnet, was die Maschine tut; mit einem Code abgelehnt, wenn eine Bohrung außerhalb des Tisches liegt oder ein Werkzeug fehlt.
* Die **Vorschau**: die Arbeit in der Reihenfolge der Maschine, die Werkzeuge mit ihrer Anzahl, das umschließende Rechteck, eine Zeitschätzung und der Schritt, an dem jede Bohrung fertig ist - was ein Bildschirm braucht, um die Platine zu zeichnen und den Auftrag zu verfolgen.
* Der **Lauf**: Ein Auftrag ist bereit, bis eine Person ihn startet; er läuft auf der Bibliothek der Programme (Handshake, Scharfschalten, Referenzfahrt, Bohrungen, Stopp), wartet an den Dialogen des Bedieners (Werkzeug wechseln, Spindel bestätigen), bis jemand bestätigt, und kann gestoppt werden; die Maschine hält mit abgeschalteten Treibern an.
* Die **Maschinen**: für jede, wie man sie erreicht (ein EH3D-NODE-BUNGARD im Netzwerk oder der Simulator), der Text ihrer Konfigurationsdateien und die an ihr abgestimmten Werte, sodass die Kalibrierung einer Maschine für alle Clients an einer Stelle liegt.
* Die **Personen und Programme**: Benutzer mit drei Rollen, Tokens für Programme, die einmal gezeigt werden, eine Verlangsamung bei falschen Passwörtern und ein Einrichtungscode auf der Konsole für den ersten Administrator.
* Die **Ereignisse**: ein Websocket, der jede Änderung eines Auftrags sendet, sobald sie geschieht.
* Der **Dienst**: eine Binärdatei und eine kleine Textdatei mit Einstellungen, standardmäßig auf Port 18200 (nicht der von ARMOR oder HYDRA-UMC), mit einer systemd-Unit für den CM5.
* **Geplant, noch nicht gebaut**: die Clients (die Android-App, das Studio, das Touch-Panel an der Maschine), HTTPS im Server selbst und die Nutzung der Lampen des Knotens und seines Puffers, um einen gehaltenen Auftrag zu beenden, wenn der Server wegfällt.

## 🧩 Rolle in EH3D

EH3D-SERVER ist das Zentrum der EH3D-Familie: Die Programme schicken ihm ihre Aufträge, er spricht über deren Knoten mit den Maschinen, und die übrigen Clients sehen und senden dieselben Aufträge über dieselbe Schnittstelle. Er nutzt die Bibliotheken von [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) und [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) und spricht mit [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD).

## 📂 Repository-Struktur

```text
EH3D-SERVER/
├── src/          der Server: Einstellungen, Benutzer, Maschinen, Planung, Aufträge, die Schnittstelle und der Websocket
├── tests/        die Schnittstelle von Anfang bis Ende, mit den Aufträgen im Simulator
├── docs/         API.md und DEPLOY.md
├── deploy/       die systemd-Unit
├── tools/        ci_validate.py: Prüfungen von Manifest, Versionen, Changelog und README
├── images/       Banner
└── eh3d.project.json
```

## 🛠️ BAUEN UND AUSFÜHREN

Du brauchst eine stabile Rust-Toolchain sowie EH3D-DRILLPRO und EH3D-ROUTEPRO neben diesem Ordner.

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

Starte ihn mit `run.bat` (Windows) oder `./run.sh` (Linux); bei Bedarf wird zuerst gebaut. Solange es keinen Benutzer gibt, zeigt die Konsole einen Einrichtungscode: Lege den Administrator mit `POST /api/v1/setup` an, melde dich an, füge die Maschine mit Adresse und Token ihres Knotens hinzu und erzeuge für jedes Programm ein Token. `server.cfg.example` listet die Einstellungen; `docs/DEPLOY.md` installiert ihn als Dienst auf dem CM5.

## 🔗 Verwandte Projekte

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO): das Bohrprogramm; seine Bibliothek plant und führt die Bohraufträge aus
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO): das Fräsprogramm; seine Bibliothek plant die Fräsaufträge
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD): der Knoten im Gehäuse der Maschine, mit dem der Server spricht
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON): die gemeinsamen Teile der Familie

## 📚 Dokumentation und Community

* [Die API](docs/API.md): jede Anfrage, der Auftrag und die Ansicht, die ein Client zeichnet, die Zustände und der Websocket.
* [Den Server betreiben](docs/DEPLOY.md): der Build, der erste Start, der Dienst auf dem CM5 und die Sicherheit der Bereitstellung.
* [Änderungsprotokoll](CHANGELOG.md)
* [Mitwirken](CONTRIBUTING.md)
* [Sicherheitsrichtlinie](SECURITY.md)
* [Support](SUPPORT.md)

## 👤 AUTOR

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 LIZENZ

Software: GPL-3.0-or-later. Siehe [LICENSE](LICENSE). Die Hardware-Entwürfe der Suite verwenden die CERN OHL-S.
