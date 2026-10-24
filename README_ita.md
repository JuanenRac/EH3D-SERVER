<p align="center">
  <img src="images/EH3D_BANNER.svg" alt="EH3D-SERVER banner" width="100%">
</p>

# 🧭 EH3D-SERVER

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | <a href="README_fra.md">🇫🇷 Français</a> | 🇮🇹 <b>Italiano</b> | <a href="README_deu.md">🇩🇪 Deutsch</a> | <a href="README_zho.md">🇨🇳 中文</a> | <a href="README_jpn.md">🇯🇵 日本語</a> | <a href="README_por.md">🇵🇹 Português</a></p>

### Il centro dei programmi EH3D: riceve lavori da DRILLPRO, ROUTEPRO e dai client futuri, li pianifica, li esegue sulla macchina tramite il suo nodo e dice a tutti come procedono

<p align="center">
  <img src="https://img.shields.io/badge/License-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Stack-Rust-DEA584.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Languages-8-00E5FF.svg" alt="Languages">
  <img src="https://img.shields.io/badge/Version-0.0.1-0f6cbd.svg" alt="Version">
  <img src="https://img.shields.io/badge/Maturity-scaffolding-FFB020.svg" alt="Maturity">
</p>

---

**Verifica di onestà - cosa funziona oggi:** un server in Rust (axum) con interfaccia REST e websocket che conserva utenti, macchine e lavori, pianifica un lavoro di foratura o di fresatura (fori e percorsi, o un file Excellon o HP-GL con la sua tabella utensili) con le librerie di EH3D-DRILLPRO ed EH3D-ROUTEPRO, mostra un'anteprima, e lo esegue su una macchina tramite il suo EH3D-NODE-BUNGARD, con i dialoghi dell'operatore, un arresto e ogni cambiamento comunicato mentre avviene. 39 test girano su un computer, i lavori contro il simulatore. **Per suo tramite non è ancora girato nulla su una macchina reale**, non termina il TLS, e i client che lo useranno oltre ai programmi (l'app Android, lo Studio, il pannello accanto alla macchina) non esistono ancora.

---

> **Stato di sicurezza funzionale e informatica:** questo non è un sistema di sicurezza. Un lavoro non parte mai da solo: lo avvia una persona con il ruolo di operatore, uno alla volta per macchina, e la pianificazione rifiuta fori e percorsi fuori dal piano e utensili mancanti o fuori intervallo. Ogni richiesta richiede un token (un osservatore legge, un operatore esegue lavori, un amministratore gestisce utenti, token e macchine); le password sono conservate come hash Argon2, i token di sessione e di programma solo come SHA-256, e il token di un nodo non viene mai mostrato dall'API. Il server è per la casa o una VPN verso di essa: metti HTTPS davanti prima che arrivi qualcosa da fuori. L'arresto della macchina non dipende solo dal server: il nodo si ferma da solo quando il suo collegamento tace. Il progetto è indipendente: non è prodotto, approvato né supportato da Bungard Elektronik.

## 📋 In breve

| | |
|---|---|
| Versione | `0.0.1` |
| Maturità | Impalcatura |
| Test | 39 |
| Stack tecnologico | Rust |
| Su hardware reale | No: provato contro il simulatore; per suo tramite non è ancora girato nulla su una macchina reale |

## 🎯 Panoramica

* I **lavori**: un lavoro di foratura (fori e utensili, o un file `.NCD` con la sua tabella utensili) o di fresatura (percorsi e utensili, o un file `.PLT` con la sua tabella utensili), pianificati con le librerie dei programmi, così un client disegna e una macchina fa esattamente la stessa cosa; rifiutati con un codice quando un foro è fuori dal piano o manca un utensile.
* L'**anteprima**: il lavoro nell'ordine in cui lo fa la macchina, gli utensili con il loro numero, il rettangolo che lo contiene, una stima del tempo e il passo in cui ogni foro è finito - ciò che serve a uno schermo per disegnare la scheda e seguire il lavoro.
* L'**esecuzione**: un lavoro è pronto finché una persona non lo avvia; gira sulla libreria dei programmi (l'handshake, l'armamento, il riferimento, i fori, l'arresto), attende ai dialoghi dell'operatore (cambiare l'utensile, confermare il mandrino) finché qualcuno conferma, e può essere fermato; la macchina si ferma con i driver spenti.
* Le **macchine**: per ciascuna come raggiungerla (un EH3D-NODE-BUNGARD in rete, o il simulatore), il testo dei suoi file di configurazione e i valori messi a punto su di essa, così la calibrazione di una macchina vive in un solo posto per tutti i client.
* Le **persone e i programmi**: utenti con tre ruoli, token per i programmi mostrati una sola volta, un rallentamento con le password sbagliate e un codice di messa in servizio sulla console per il primo amministratore.
* Gli **eventi**: un websocket che invia ogni cambiamento di un lavoro mentre avviene.
* Il **servizio**: un binario e un piccolo file di testo di impostazioni, sulla porta 18200 per impostazione predefinita (non quella di ARMOR né di HYDRA-UMC), con un'unità systemd per la CM5.
* **Previsto, non ancora fatto**: i client (l'app Android, lo Studio, il pannello touch accanto alla macchina), HTTPS nel server stesso, e l'uso delle spie del nodo e del suo buffer per finire un lavoro posseduto quando il server scompare.

## 🧩 Ruolo in EH3D

EH3D-SERVER è il centro della famiglia EH3D: i programmi gli inviano i loro lavori, lui parla con le macchine tramite i loro nodi, e gli altri client vedono e inviano gli stessi lavori attraverso la stessa interfaccia. Usa le librerie di [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) ed [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) e parla con [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD).

## 📂 Struttura del repository

```text
EH3D-SERVER/
├── src/          il server: impostazioni, utenti, macchine, pianificazione, lavori, l'interfaccia e il websocket
├── tests/        l'interfaccia da un capo all'altro, con i lavori che girano sul simulatore
├── docs/         API.md e DEPLOY.md
├── deploy/       l'unità systemd
├── tools/        ci_validate.py: verifiche di manifesto, versioni, changelog e README
├── images/       banner
└── eh3d.project.json
```

## 🛠️ COMPILARE ED ESEGUIRE

Servono una toolchain Rust stabile, ed EH3D-DRILLPRO ed EH3D-ROUTEPRO accanto a questa cartella.

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

Avvialo con `run.bat` (Windows) o `./run.sh` (Linux); compila prima se serve. Finché non ha utenti la console mostra un codice di messa in servizio: crea l'amministratore con `POST /api/v1/setup`, accedi, aggiungi la macchina con l'indirizzo e il token del suo nodo, e crea un token per ogni programma. `server.cfg.example` elenca le impostazioni; `docs/DEPLOY.md` lo installa come servizio sulla CM5.

## 🔗 Progetti correlati

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO): il programma di foratura; la sua libreria pianifica ed esegue i lavori di foratura
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO): il programma di fresatura; la sua libreria pianifica i lavori di fresatura
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD): il nodo nel quadro della macchina con cui parla il server
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON): le parti comuni della famiglia

## 📚 Documentazione e community

* [L'API](docs/API.md): ogni richiesta, il lavoro e la vista che disegna un client, gli stati e il websocket.
* [Far girare il server](docs/DEPLOY.md): la compilazione, il primo avvio, il servizio sulla CM5 e la sicurezza del dispiegamento.
* [Registro delle modifiche](CHANGELOG.md)
* [Contribuire](CONTRIBUTING.md)
* [Politica di sicurezza](SECURITY.md)
* [Supporto](SUPPORT.md)

## 👤 AUTORE

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 LICENZA

Software: GPL-3.0-or-later. Vedi [LICENSE](LICENSE). I progetti hardware della suite usano la CERN OHL-S.
