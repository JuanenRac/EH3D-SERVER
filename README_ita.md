<p align="center">
  <img src="images/EH3D_BANNER.svg" alt="EH3D-SERVER banner" width="100%">
</p>

# <img src="images/brand/png/icon-64.png" alt="" width="40" height="40" align="absmiddle"> EH3D-SERVER

<p align="center"><a href="README.md">🇺🇸 English</a> | <a href="README_spa.md">🇪🇸 Español</a> | <a href="README_fra.md">🇫🇷 Français</a> | 🇮🇹 <b>Italiano</b> | <a href="README_deu.md">🇩🇪 Deutsch</a> | <a href="README_zho.md">🇨🇳 中文</a> | <a href="README_jpn.md">🇯🇵 日本語</a> | <a href="README_por.md">🇵🇹 Português</a></p>

### Il centro dei programmi EH3D: riceve i lavori da DRILLPRO, ROUTEPRO e dai client futuri, li pianifica, li esegue sulla macchina tramite il suo nodo e racconta a tutti come procedono

<p align="center">
  <img src="https://img.shields.io/badge/License-GPL%203.0--or--later-blue.svg" alt="GPL 3.0 or later">
  <img src="https://img.shields.io/badge/Stack-Rust-DEA584.svg" alt="Rust">
  <img src="https://img.shields.io/badge/Languages-8-00E5FF.svg" alt="Languages">
  <img src="https://img.shields.io/badge/Version-0.1.4-0f6cbd.svg" alt="Version">
  <img src="https://img.shields.io/badge/Maturity-functional-4CAF50.svg" alt="Maturity">
</p>

---

**Verifica di onestà - cosa funziona oggi:** un server Rust (axum) con interfaccia REST e websocket che tiene utenti, macchine e lavori, pianifica un lavoro di foratura o di fresatura (fori e percorsi, oppure un file Excellon o HP-GL con la sua tabella utensili) con le librerie di EH3D-DRILLPRO ed EH3D-ROUTEPRO, mostra un'anteprima e lo esegue su una macchina tramite il suo EH3D-NODE-BUNGARD o su un simulatore, con i dialoghi dell'operatore, uno stop e ogni cambiamento comunicato man mano. 41 test girano su un computer, i lavori contro il simulatore. **Finora nulla è stato eseguito su una macchina reale attraverso di esso**, non termina il TLS, e dei client oltre ai programmi, EH3D-STUDIO esiste (mai provato con una macchina reale) e l'app Android e il pannello accanto alla macchina non esistono ancora. È anche il server di ogni applicazione di EH3D, con quattro servizi comuni (un archivio di progetti, cataloghi condivisi, plugin di macchina con una coda di lavori e un flusso di eventi, e un login unico); girano solo nei test e nessuna applicazione li usa ancora.

---

> **Stato di sicurezza funzionale e informatica:** questo non è un sistema di sicurezza funzionale. Un lavoro non parte mai da solo: lo avvia una persona con il ruolo di operatore, un lavoro alla volta su una macchina, e la pianificazione rifiuta fori e percorsi fuori dal tavolo e utensili mancanti o fuori intervallo. Ogni richiesta richiede un token (un osservatore legge, un operatore esegue lavori, un amministratore gestisce utenti, token e macchine); le password sono conservate come hash Argon2, i token di sessione e di programma solo come SHA-256, e il token di un nodo non viene mai mostrato dall'API. Il server è pensato per la casa o una VPN verso di essa: metti HTTPS davanti prima che qualcosa dall'esterno lo raggiunga. L'arresto della macchina non dipende dal solo server: il nodo si ferma da solo quando il suo collegamento tace. Il progetto è indipendente: non è prodotto, approvato né supportato da Bungard Elektronik.

## 📋 In breve

| | |
|---|---|
| Versione | `0.1.4` |
| Maturità | Funzionale |
| Test | 347 |
| Stack tecnologico | Rust (axum, tokio) |
| Su hardware reale | No: provato contro il simulatore; finora nulla è stato eseguito su una macchina reale tramite il server |

**Vai a:** [Panoramica](#-panoramica) · [Architettura](#-architettura) · [Avvio rapido](#-avvio-rapido) · [Test](#-test) · [Struttura](#-struttura-del-repository) · [Correlati](#-progetti-correlati)

## 🚦 Stato del progetto

Ogni riga è una parte di questo repository e il semaforo dice a che punto è oggi. È scritta in base a ciò che il repository può mostrare (test, compilazioni, esecuzioni), non a ciò che è previsto.

🟢 Fatto e verificato · 🟡 Funziona, con limiti noti · 🟠 Fatto a metà o non ancora verificato · 🔴 Non fatto o non funzionante

| Area | Stato | Cosa significa qui |
|---|---|---|
| Software | 🟡 | Un server Rust (axum) tiene utenti, macchine e lavori, pianifica lavori di foratura e fresatura con le librerie di EH3D-DRILLPRO ed EH3D-ROUTEPRO, mostra un'anteprima ed esegue il lavoro con dialoghi dell'operatore e uno stop. |
| Test | 🟡 | Passano 347 test (`cargo test`): 191 nei moduli e 156 sull'interfaccia end to end, con lavori sul simulatore (41 in `tests/api.rs`, 7 con i programmi reali in `tests/programs.rs`). |
| Interfaccia per i client (API) | 🟡 | Le richieste REST e gli eventi websocket sono descritti in `docs/API.md` (le rotte anche nel contratto OpenAPI `docs/openapi.yaml`) e coperti dai test; finora li usa solo la connessione al server dei programmi stessi. |
| Servizi comuni | 🟠 | L'archivio di progetti, i cataloghi condivisi, i plugin di macchina con la loro coda e i loro eventi e il login unico girano nei test su un computer; nessuna applicazione li usa ancora, nulla è girato su una macchina reale attraverso la coda e i cataloghi non sono stati caricati con i dati reali dei loro programmi. |
| Client | 🔴 | EH3D-STUDIO lo usa (mai provato con una macchina reale); l'app Android e il pannello touch accanto alla macchina non esistono ancora. |
| Sicurezza | 🟡 | Ogni richiesta richiede un token con uno di tre ruoli, le password sono hash Argon2 e le password sbagliate vengono rallentate; il server non termina il TLS (metti HTTPS davanti) e non ha avuto una revisione indipendente. |
| Hardware reale | 🟠 | I lavori sono stati eseguiti solo contro il simulatore: nulla è stato eseguito su una macchina reale tramite il server. |
| Distribuzione | 🟠 | Un'unità systemd e `docs/DEPLOY.md` descrivono il servizio sulla CM5; questo repository non registra alcuna distribuzione come fatta. |
| Integrazione | 🟠 | Pianifica con le stesse librerie di EH3D-DRILLPRO ed EH3D-ROUTEPRO (le loro cartelle devono stare accanto a questa) e parla con il collegamento di EH3D-NODE-BUNGARD, raggiunto solo tramite il simulatore. |
| Documentazione | 🟡 | README in otto lingue, il riferimento dell'API e la guida per eseguirlo come servizio. |

## 🎯 Panoramica

* I **lavori**: un lavoro di foratura (fori e utensili, o un file `.NCD` con la sua tabella utensili) o di fresatura (percorsi e utensili, o un file `.PLT` con la sua tabella utensili), pianificato con le librerie dei programmi, così un client disegna e la macchina fa esattamente la stessa cosa; rifiutato con un codice quando un foro è fuori dal tavolo o manca un utensile.
* **Lavori G-code liberi:** un lavoro `kind: gcode` porta il testo di un programma; il server lo ripulisce (senza commenti né comandi di impostazioni del controllore), giudica `S` e `F` rispetto al mandrino della macchina e lo esegue riga per riga sul simulatore o su un controllore GRBL su una porta seriale del server (`connection.kind = grbl`, una compilazione con `--features serial`). Il lavoro può essere messo in pausa (`pause`), rilasciato (`resume`) e fermato (`cancel`), e uno fermato riprende dalla riga raggiunta: i codici in vigore vengono restituiti e il lavoro attende l'operatore prima del primo movimento. Provato solo sul simulatore, mai su un vero controllore GRBL.
* L'**anteprima**: il lavoro nell'ordine in cui lo fa la macchina, gli utensili con il loro numero, il riquadro, una stima del tempo e l'elemento a cui ogni foro è finito - ciò che serve a uno schermo per disegnare la scheda e seguire il lavoro.
* L'**esecuzione**: un lavoro è pronto finché una persona lo avvia; gira sulla libreria dei programmi (handshake, armamento, riferimento, fori, stop), attende ai dialoghi dell'operatore (cambia l'utensile, conferma il mandrino) finché qualcuno conferma, e può essere fermato; la macchina si ferma con i driver spenti.
* Le **macchine**: per ognuna, come raggiungerla (un EH3D-NODE-BUNGARD via rete, o il simulatore), il testo dei suoi file di configurazione e i valori tarati su di essa, così la calibrazione di una macchina vive in un solo posto per tutti i client.
* La **flotta e le stazioni**: ogni macchina appartiene a una stazione (il banco, la sala, l'officina) e `GET /api/v1/fleet` risponde per tutte insieme, raggruppate per stazione, con ciò che fa ciascuna (libera, occupata, in attesa dell'operatore), il suo lavoro con l'avanzamento e il numero di lavori pronti; le macchine lavorano insieme, un lavoro su ciascuna.
* Le **persone e i programmi**: utenti con tre ruoli, token per i programmi mostrati una sola volta, un rallentamento sulle password sbagliate e un codice di configurazione sulla console per il primo amministratore.
* Gli **eventi**: un websocket che invia ogni cambiamento di un lavoro man mano che avviene.
* Il **servizio**: un binario e un piccolo file di impostazioni di testo, sulla porta 18200 per impostazione predefinita (non quella di ARMOR né di HYDRA-UMC), con un'unità systemd per la CM5.
* **Previsto, non ancora costruito**: i client (l'app Android, lo Studio, il pannello touch accanto alla macchina), HTTPS nel server stesso e l'uso dei LED del nodo e del suo buffer per finire un lavoro trattenuto quando il server sparisce.
* **Una prova a secco del piano esatto e controlli di buon senso nella vista di ogni lavoro:** i passi di un lavoro di foratura o fresatura vengono seguiti su carta, come li legge l'unità di controllo, e la vista porta `dry_run` (fin dove arriva ogni asse dai finecorsa, dove finisce il piano, i passi e i secondi) e `warnings` (un'unità probabilmente sbagliata, un lavoro più grande del piano, una scala diversa da 1). Un piano che porta un asse fuori dalla corsa del file della macchina viene rifiutato con `outside_course`, o solo elencato quando la taratura della macchina dice `dry_run = warn`; `dry_run_margin_mm`, `sanity_tiny_mm` e `sanity_far_mm` sono chiavi della stessa taratura. La misura e il riepilogo vengono da `eh3d-summary` in EH3D-COMMON, così ogni programma dice la stessa cosa.
* **I quattro servizi comuni, per ogni applicazione di EH3D:** un **archivio di progetti** (file con tutte le versioni conservate e verificate, `If-Match` perché due persone non si sovrascrivano mai, un blocco di modifica, importazione ed esportazione del contenitore `.eh3dproj`), **cataloghi condivisi** (materiali, utensili, profili di macchina, librerie e base dei componenti, con ricerca ed `ETag`), **plugin di macchina** (ogni tipo di macchina dietro un'unica interfaccia, una coda di lavori con cronologia, un flusso di eventi del server e notifiche) e **un solo login** (l'applicazione e il ruolo come dati della sessione, sessioni che si possono elencare e chiudere, una politica per password e blocco). Vedi [docs/SERVICES.md](docs/SERVICES.md).

## 🧩 Ruolo in EH3D

EH3D-SERVER è il centro della famiglia EH3D: i programmi gli inviano i loro lavori, parla con le macchine tramite i loro nodi, e gli altri client vedono e inviano gli stessi lavori attraverso la stessa interfaccia. Usa le librerie di [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO) ed [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO) e parla con [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD).

## 🏛️ Architettura

<p align="center">
  <img src="images/architecture.svg" alt="Architettura di EH3D-SERVER: chi lo usa, che cosa contiene e da che cosa dipende" width="100%">
</p>

Come si incastrano le parti di questo repository: chi lo usa, che cosa contiene e da che cosa dipende. L'immagine è disegnata da `docs/architecture.json` e verificata rispetto al codice.

## ✨ Funzionalità

* **Un'anteprima che è il piano:** l'anteprima e l'esecuzione reale vengono dalla stessa pianificazione, quindi ciò che il client disegna è ciò che fa la macchina.
* **Un lavoro non parte mai da solo:** viene creato pronto e solo un operatore lo avvia; un lavoro alla volta su una macchina (`409 machine_busy`).
* **Dialoghi dell'operatore via rete:** il lavoro si ferma al cambio utensile e alla conferma del mandrino finché un client conferma.
* **Eventi dal vivo:** un websocket comunica ogni cambiamento di un lavoro man mano che avviene, con i riepiloghi dei lavori alla connessione.
* **Calibrazione in un solo posto:** i file di configurazione e i valori tarati di ogni macchina sono tenuti sul server, quindi tutti i client pianificano con gli stessi numeri.
* **Un lavoro in corso quando il server si è fermato è segnato come fallito:** nessuno sa dov'era la macchina, quindi nulla riprende da solo.
* **File durevoli:** utenti, token, macchine e lavori sono file JSON scritti tramite un file temporaneo che viene scaricato sul disco prima di sostituire il vecchio, e poi si scarica la cartella; il file sostituito viene conservato una volta come `<nome>.bak`. Un file danneggiato resta un errore, mai un riavvio silenzioso da zero, ma se la copia è buona l'errore la nomina e dice come recuperare (copiarla sopra il file); non viene mai caricata da sola.
* **Aggiornamenti da GitHub:** con `updates_dir` il server offre a un amministratore il controllo delle ultime release di sé stesso e di EH3D-STUDIO e il loro aggiornamento (`/api/v1/updates`); un piccolo helper avviato da systemd (`deploy/eh3d-update.py`) scarica, verifica lo SHA-256 di ogni file, rifiuta un archivio con nomi fuori dalla sua cartella, sostituisce la cartella installata (la precedente resta come `.previous`) e riavvia il servizio. Se esiste il file `/etc/eh3d-update.signers`, verifica anche la firma OpenSSH (`.sig`) di ogni file e rifiuta un pacchetto che ne è privo; si sostituisce da solo con la copia che arriva nella release. Attivarlo gli fa quindi rifiutare le release pubblicate senza `.sig`. È stato provato con GitHub e systemd sostituiti da simulazioni, mai su un'installazione reale. Un amministratore può anche ripristinare la versione lasciata dall'ultimo aggiornamento e svuotare la cronologia degli aggiornamenti.
* **Esercizio:** SIGTERM (`systemctl stop`) ferma le macchine in funzione e poi il server, in ordine; l'unità systemd è rinforzata (`UMask=0077`, gruppo `dialout`, nessuna cartella personale, nessun nuovo privilegio); `GET /api/v1/health` dà la versione, la revisione dell'interfaccia e `ok` o `degraded` (un disco pieno o in sola lettura, o un panic che ha avvelenato un lock, che ora il server supera), e di più a chi ha eseguito l'accesso; un timer notturno copia la cartella dei dati (`deploy/eh3d-backup.py`); `deploy/Caddyfile.example` e `docs/DEPLOY.md` danno HTTPS con Caddy. Questi file sono provati come file, mai sulla CM5.
* **Il nodo della testa di foratura tramite il server:** quando la configurazione di una macchina nomina il nodo (`head_address`, `head_token`), il server inoltra il suo stato (`GET /api/v1/machines/:id/head/status`), la sua telecamera come MJPEG (`.../head/stream`, con il token nell'indirizzo per un browser) e una misura del tavolo (`POST .../head/measure`, operatore), così un programma su un'altra rete lo raggiunge con lo stesso accesso. Provato con un nodo simulato, mai con uno reale.
* **Il controllo qualità con la telecamera della testa:** il server accende l'anello di LED, scatta una foto dal nodo, la giudica con l'analizzatore dell'officina (il profilo di ogni macchina è conservato dal server), fa dire all'anello com'è andata (verde, o rosso lampeggiante) e conserva l'ultimo controllo perché lo schermo accanto alla macchina e i telefoni lo mostrino: `GET/POST .../head/light`, `GET .../head/snapshot`, `POST/GET .../head/inspect`, `GET/PUT .../head/inspect/profile` e `POST .../head/inspect/calibrate`.
* **Il mandrino della macchina:** `GET` e `PUT /api/v1/machines/:id/spindle` dicono quale mandrino porta la macchina (`original` o `vfd`) e i suoi limiti (giri, avanzamento di taglio, avanzamento di affondo, punta più grossa); un lavoro che chiede di più viene rifiutato con `spindle_limit` (l'interruttore di sicurezza `guard` è `reject` per impostazione predefinita; con `warn` il lavoro passa con i giri ridotti al massimo del mandrino e ogni eccesso elencato in `spindle_warnings`).
* **Un archivio di progetti con versioni:** un file non viene mai sovrascritto, ogni scrittura è una nuova versione con il suo SHA-256, una scrittura su un file esistente deve dire da quale versione parte (`If-Match`), i byte sono verificati contro il loro hash a ogni lettura e i nomi che potrebbero uscire dalla cartella o che sono di Windows vengono rifiutati.
* **Macchine dietro plugin:** le macchine Bungard sono il plugin `bungard` (stesse macchine, stessi lavori, stesse vecchie rotte) e un plugin `demo` senza hardware serve ai test; una coda generica conserva la cronologia di ogni lavoro e un flusso di eventi (`GET /api/v1/events/stream`) permette a un client di riconnettersi con `Last-Event-ID` senza perdere nulla.
* **Un login per ogni applicazione:** il login dice per quale applicazione è, ogni persona vede e chiude le proprie sessioni aperte e un amministratore quelle di chiunque, password e blocco seguono le impostazioni, e l'audit conserva login, rifiuti e chiusure, mai un segreto; `GET /api/v1/auth/policy` mostra allo Studio gli interruttori di sicurezza.

## 🔒 Sicurezza e protezione

| Minaccia | Difesa |
|---|---|
| Chiunque sulla rete invii richieste | ogni richiesta tranne il controllo di salute, la prima configurazione e il login richiede un token (`Authorization: Bearer`, o `?ticket=` per il websocket); tre ruoli: l'osservatore legge, l'operatore esegue lavori, l'amministratore gestisce utenti, token e macchine |
| Indovinare una password | cinque password sbagliate bloccano il nome, 30 s e poi 60 s, 120 s e così via finché continuano (`429 throttled`); la risposta non dice quale parte era sbagliata, e un nome sconosciuto costa quanto uno noto |
| Una copia della cartella dati | le password sono hash Argon2id; i token di sessione e di programma sono casuali e se ne conserva solo lo SHA-256, quindi una copia della cartella non fa entrare nessuno; il token di un nodo non viene mai mostrato dall'API |
| Prendere il controllo di un server nuovo | il primo amministratore si crea con un codice di configurazione che stampa solo la console |
| Chiudere fuori tutti | l'ultimo amministratore non può essere eliminato, disattivato né declassato (`409 last_admin`) |
| Un lavoro che parte da solo o un secondo lavoro su una macchina | un lavoro viene creato pronto; solo un operatore lo avvia, e una macchina esegue un lavoro alla volta |
| Un piano che colpirebbe il tavolo o userebbe un utensile mancante | la pianificazione rifiuta con un codice fori e percorsi fuori dal tavolo e utensili mancanti o fuori intervallo |
| Il server muore mentre la macchina si muove | il nodo si ferma da solo quando il suo collegamento tace; il lavoro è segnato come fallito e non riprende da solo; il servizio ferma i suoi lavori con SIGINT |
| Una richiesta enorme | il corpo è limitato (`max_body_mb`, 8 MB per impostazione predefinita) |
| Non sapere chi ha fatto cosa | una riga di audit per ogni login (anche quelli falliti), lavoro, arresto e modifica, sulla console e in `audit.jsonl`; un lavoro dice chi lo ha avviato e chi lo ha fermato; nessun segreto viene scritto |
| Un client che scrive il proprio indirizzo nell'intestazione del proxy | `X-Forwarded-For` è creduto solo con `trust_forwarded_for` attivo e solo su una connessione da 127.0.0.1; disattivo per impostazione predefinita |
| Un token di programma dimenticato su un portatile prestato | i token possono scadere (`days`, `token_default_days`) e l'elenco mostra quando ciascuno è stato usato l'ultima volta |
| Un thread del server va in panico | con `exit_on_panic` (attivo) i lavori in corso vengono fermati e il server esce perché systemd lo riavvii |
| Un file di dati danneggiato | un errore, mai un avvio silenzioso da zero |
| Un servizio con troppi diritti | l'unità systemd gira con un utente proprio con `NoNewPrivileges`, `ProtectSystem=strict`, `PrivateTmp` e una sola cartella scrivibile |
| Un lavoro che chiede al mandrino più di quanto dà | con un mandrino scelto per la macchina, la pianificazione rifiuta (`422 spindle_limit`) giri o avanzamenti oltre i suoi limiti, sia alla creazione sia all'avvio; l'interruttore è `reject` a meno che un amministratore imposti `warn` |

**Ciò che non è difeso:** il server non termina il TLS, quindi token e password attraversano la rete in chiaro a meno che si metta HTTPS davanti; il token del websocket è nell'indirizzo; non ha avuto una revisione indipendente. Segnala una vulnerabilità in privato (vedi [SECURITY.md](SECURITY.md)).

## 🔌 Interfacce

| Interfaccia | Che cos'è | Dove |
|---|---|---|
| REST | `/api/v1/`: `health`, `setup`, `login`, `logout`, `session`, `account`, `users`, `tokens`, `updates`, `machines` (con `preview` e `jobs`), `jobs` (con `start`, `confirm`, `cancel`); JSON in entrata e in uscita, errori come `{"error","detail"}` | [docs/API.md](docs/API.md), `src/api.rs` |
| Websocket | `GET /api/v1/events?ticket=...`: un `hello` con i riepiloghi dei lavori, poi `job` e `job_removed` a ogni cambiamento | [docs/API.md](docs/API.md) |
| Verso la macchina | la `connection` di una macchina è `node` (un EH3D-NODE-BUNGARD a un indirizzo con il suo token) o `demo` (il simulatore) | [docs/API.md](docs/API.md), `src/machines.rs` |
| Riga di comando | `eh3d-server [--config FILE]`; `--help` la elenca | `src/main.rs` |
| Servizio | un binario, porta 18200 per impostazione predefinita, un'unità systemd | [docs/DEPLOY.md](docs/DEPLOY.md), `deploy/eh3d-server.service` |
| Servizi comuni | `/api/v1/apps/{app}/projects`, `catalogs`, `plugins`, `work`, `notifications`, `auth`; `GET /api/v1/events/stream` invia eventi del server | [docs/SERVICES.md](docs/SERVICES.md), [docs/openapi.yaml](docs/openapi.yaml) |

## ⚙️ Configurazione

`server.cfg` (o il file indicato con `--config`) contiene 71 chiavi facoltative; una chiave sconosciuta o un valore fuori intervallo è un errore all'avvio. `server.cfg.example` le spiega.

| Chiave | Predefinito | Intervallo | Cosa fa |
|---|---|---|---|
| `listen` | `0.0.0.0:18200` | un indirizzo e una porta | dove il server ascolta (18200 è la sua porta) |
| `data_dir` | `data` | una cartella | dove si conservano utenti, token, macchine e lavori; fanne un backup |
| `session_hours` | 12 | da 1 a 720 | quanto dura un login |
| `max_body_mb` | 8 | da 1 a 256 | la richiesta più grande, in megabyte |
| `keep_jobs` | 200 | da 10 a 100.000 | quanti lavori finiti conserva l'elenco |
| `exit_on_panic` | `true` | true o false | quando un thread va in panico: ferma le macchine ed esce perché il servizio riparta (false: scrive solo il panico) |
| `trust_forwarded_for` | `false` | true o false | prende l'indirizzo del client da `X-Forwarded-For`, solo su una connessione da 127.0.0.1 (il proxy locale) |
| `persist_sessions` | `false` | true o false | conserva i login su disco (solo l'hash di ogni token) così un riavvio non disconnette nessuno |
| `token_default_days` | 0 | da 0 a 3650 | giorni di validità di un token di programma quando non è indicato nessuno (0: non scade) |
| `events_ping_s` | 30 | da 1 a 3600 | secondi tra i ping del websocket degli eventi |
| `events_idle_s` | 90 | da 2 a 86400 | secondi senza alcun segnale (un pong conta) prima di chiudere quel websocket |
| `audit_file` | `true` | true o false | scrive l'audit (chi ha fatto cosa) in `audit.jsonl` oltre che sulla console |
| `job_log` | `true` | true o false | scrive una riga all'inizio e una alla fine di ogni lavoro in `eh3d-jobs.log` |
| `log_max_kb` | 1024 | da 16 a 1048576 | kilobyte oltre i quali audit e registro dei lavori vengono messi da parte (si tiene una copia precedente) |

I limiti e gli interruttori dei servizi comuni (l'archivio, i cataloghi, la coda, il flusso di eventi, i login, le password) sono chiavi dello stesso file, ciascuna spiegata in `server.cfg.example`; `GET /api/v1/auth/policy` mostra a un amministratore gli interruttori di sicurezza.

Nomi di utenti e token: lettere, cifre, `.`, `_` e `-`, fino a 32 caratteri; password: da 8 a 128 caratteri. I valori tarati su ogni macchina non sono qui: stanno nella scheda della macchina, nel formato del file macchina dei programmi.

## 🚀 Avvio rapido

Serve una toolchain Rust stabile, ed EH3D-DRILLPRO ed EH3D-ROUTEPRO accanto a questa cartella (il server usa le loro librerie).

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
python -I tools/ci_validate.py
```

Avvialo con `run.bat` (Windows) o `./run.sh` (Linux); compila prima se serve. Finché non ha utenti, la console mostra un codice di configurazione: crea l'amministratore con `POST /api/v1/setup`, accedi, aggiungi la macchina con l'indirizzo e il token del suo nodo e crea un token per ogni programma. `server.cfg.example` elenca le impostazioni; `docs/DEPLOY.md` lo installa come servizio sulla CM5.

## 🧪 Test

La suite ha 347 test (`cargo test`, eseguiti su Windows e Linux nella CI con COMMON, DRILLPRO e ROUTEPRO scaricati accanto al server): 191 nei moduli (impostazioni e loro intervalli, utenti, ruoli, token, il rallentamento sulle password sbagliate, i file e il registro dei lavori, la pianificazione), 41 sull'interfaccia end to end attraverso il vero router (`tests/api.rs`), 2 che mantengono gli esempi di risposte letti dai client (`tests/examples.rs`) e 7 con i programmi reali che inviano i loro lavori a un vero server su una vera porta, con la macchina sul simulatore (`tests/programs.rs`). Gli altri file di `tests/` ne aggiungono 106 (i quattro servizi comuni con i loro documenti, artefatti, attività di calcolo, condivisione, eventi, catalogo degli errori, rotte del documento dell'API, un arresto da segnale e un panic). La CI esegue anche `cargo fmt --all --check`, `cargo clippy` con gli avvisi come errori e `python -I tools/ci_validate.py`. Nulla di questo è stato provato contro un nodo o una macchina reali. L'helper che fa gli aggiornamenti ha altri 21 test propri (`python -I tests/test_update_helper.py`, con GitHub e systemd sostituiti da simulazioni). Altri due file di test verificano che un panic che avvelena un lock non fermi il server (`tests/poison.rs`) e che il binario reale si fermi con SIGTERM (`tests/sigterm.rs`, su Linux); la copia di sicurezza e i file della distribuzione hanno 7 e 8 test propri (`tests/test_backup.py`, `tests/test_deploy_files.py`). Altri due file di test verificano il websocket degli eventi su un socket reale (`tests/events.rs`, 3 test) e il catalogo dei codici di errore rispetto al codice del server (`tests/errors.rs`, 3 test). Il contratto delle rotte è verificato rispetto al router da `tests/openapi.rs` (6 test) e da `python -I tools/check_openapi.py`, che la CI esegue anch'essa.

## 🏗️ Compilazione e rilascio

La versione sta in `Cargo.toml` e in `eh3d.project.json`, e ogni versione ha la sua voce in [CHANGELOG.md](CHANGELOG.md). `build.sh` / `build.bat` compilano il binario di release (`target/release/eh3d-server`) con un pannello dal vivo, e `test.sh` / `test.bat` eseguono solo i test; nessuno cambia la versione. Non è ancora stata pubblicata alcuna versione. Su una CM5 (Linux aarch64) si compila sulla macchina stessa; `docs/DEPLOY.md` lo installa come servizio.

## 📂 Struttura del repository

<p align="center">
  <img src="images/repository-structure.svg" alt="Struttura del repository EH3D-SERVER" width="100%">
</p>

<details>
<summary>Versione testuale dell'albero</summary>

```text
EH3D-SERVER/
├── src/
│   ├── api.rs                                   le route REST e il websocket
│   ├── auth.rs                                  utenti, ruoli, token e rallentamento sulle password sbagliate
│   ├── plan.rs                                  dalla richiesta agli elementi che la macchina esegue
│   ├── jobs.rs state.rs                         i lavori e di cosa è fatto il server mentre gira
│   ├── audit.rs errors.rs panic.rs              audit, registro, codici di errore, uscita in panico
│   ├── machines.rs store.rs                     le macchine e i file scritti tramite uno temporaneo
│   ├── config.rs lib.rs main.rs                 il file delle impostazioni, la radice della libreria e il programma
│   ├── updates.rs  head.rs                      gli aggiornamenti dei servizi
│   ├── inspect.rs                               il controllo qualità con la telecamera della testa
│   ├── gcode.rs gcode_run.rs                    G-code libero: pulizia, lettura ed esecuzione sul simulatore o su GRBL
│   ├── spindle.rs                               il mandrino di una macchina, i suoi limiti e l'interruttore di sicurezza
│   ├── pstore.rs api_pstore.rs                  l'archivio dei progetti: versioni, blocchi, contenitore
│   ├── catalogs.rs api_catalogs.rs              i cataloghi condivisi: materiali, utensili, librerie
│   ├── plugins.rs work.rs feed.rs api_work.rs   plugin di macchina, coda dei lavori, eventi, notifiche
│   └── apps.rs api_auth.rs                      le applicazioni e il login unico per tutte
├── tests/                                       l'interfaccia end to end sul simulatore e i programmi reali contro un vero server
├── docs/                                        l'API, il suo contratto OpenAPI, la guida per eseguire il server e i dati delle due immagini
├── deploy/                                      le unità systemd e gli assistenti del servizio
├── server.cfg.example                           il file delle impostazioni con ogni chiave spiegata
├── tools/                                       controlli di CI e README, OpenAPI, il pannello degli script e strumenti per le immagini
├── images/                                      banner e le immagini di Architettura e Struttura del repository
├── .github/                                     workflow di CI e di rilascio, modelli di issue e pull request
├── build.sh build.bat                           compila il binario di release con un pannello dal vivo
├── run.sh run.bat                               avvia il server (compila prima se serve)
├── eh3d.project.json                            manifesto: versione, ruolo, maturità e comando di compilazione
├── Cargo.toml Cargo.lock                        il pacchetto Rust, le sue dipendenze e le librerie dei due programmi
├── CHANGELOG.md                                 cosa è cambiato in ogni versione
├── SECURITY.md SUPPORT.md                       come segnalare una vulnerabilità e dove chiedere
└── CONTRIBUTING.md CODE_OF_CONDUCT.md LICENSE   come contribuire, codice di condotta e licenza
```

</details>

## 🗺️ Roadmap e limiti noti

**Limiti noti oggi**

* Nulla è stato eseguito su una macchina reale attraverso il server: i lavori hanno girato solo contro il simulatore e il collegamento con EH3D-NODE-BUNGARD non è stato raggiunto per davvero.
* Il server non termina il TLS; metti HTTPS davanti prima che qualcosa dall'esterno lo raggiunga. Non ha avuto una revisione indipendente.
* Dei client oltre ai programmi, EH3D-STUDIO esiste; l'app Android e il pannello touch non esistono ancora.
* Servono le cartelle di EH3D-DRILLPRO ed EH3D-ROUTEPRO accanto, perché usa le loro librerie.
* Un lavoro in corso quando il server si è fermato è segnato come fallito e non riprende.
* Nessuna applicazione usa ancora i servizi comuni (archivio di progetti, cataloghi, coda generica, login unico): sono descritti e testati su un computer, non provati con un client reale né con una macchina reale.

**Prossimi passi**

* Eseguire un lavoro intero su una macchina reale tramite il server e il nodo.
* HTTPS nel server stesso.
* Usare i LED del nodo e il suo buffer per finire un lavoro trattenuto quando il server sparisce.
* Costruire il primo client oltre ai programmi.

## 🌍 Lingue

Il README e i testi esistono in inglese, spagnolo, francese, italiano, tedesco, cinese, giapponese e portoghese europeo.

## 🔗 Progetti correlati

* [EH3D-DRILLPRO](https://github.com/JuanenRac/EH3D-DRILLPRO): il programma di foratura; la sua libreria pianifica ed esegue i lavori di foratura
* [EH3D-ROUTEPRO](https://github.com/JuanenRac/EH3D-ROUTEPRO): il programma di fresatura; la sua libreria pianifica i lavori di fresatura
* [EH3D-NODE-BUNGARD](https://github.com/JuanenRac/EH3D-NODE-BUNGARD): il nodo nella scatola della macchina con cui parla il server
* [EH3D-COMMON](https://github.com/JuanenRac/EH3D-COMMON): le parti comuni della famiglia

## 📚 Documentazione

* [L'API](docs/API.md): ogni richiesta, il lavoro e la vista che disegna un client, gli stati e il websocket.
* [Le rotte](docs/openapi.yaml): il contratto OpenAPI di ogni rotta (ruolo, ambito, schemi, esempi e codici di errore), con la sua tabella in [docs/ROUTES.md](docs/ROUTES.md).
* [I servizi comuni](docs/SERVICES.md): l'archivio di progetti, i cataloghi condivisi, i plugin di macchina con la coda e gli eventi e il login unico, ciascuno con la sua pagina ([PROJECT_STORE.md](docs/PROJECT_STORE.md), [CATALOGS.md](docs/CATALOGS.md), [MACHINE_PLUGINS.md](docs/MACHINE_PLUGINS.md), [AUTH.md](docs/AUTH.md)).
* [L'esecuzione del progetto, l'audit e gli interruttori di sicurezza](docs/DESIGN_RUN.md): eseguire il programma di progettazione installato su una scheda di un progetto, con il rapporto conservato come file del progetto, e ciò che l'amministratore legge e cambia a server in funzione ([AUTH.md](docs/AUTH.md)).
* [Eseguire il server](docs/DEPLOY.md): la compilazione, il primo avvio, il servizio sulla CM5 e la sicurezza della distribuzione.
* [Registro delle modifiche](CHANGELOG.md)

## 🤝 Contribuire e comunità

* Prima di una pull request esegui `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` e `python -I tools/ci_validate.py`: tutto deve passare. Vedi [CONTRIBUTING.md](CONTRIBUTING.md) e il [codice di condotta](CODE_OF_CONDUCT.md).
* Il README esiste in otto lingue: una modifica al testo si fa in tutti e otto i file, e le due immagini sono disegnate da `docs/architecture.json` e `docs/structure.json` con `tools/make_architecture.py` e `tools/make_structure.py`.
* Una modifica a una route dell'API richiede la sua voce in `docs/API.md`, un test attraverso il vero router e, per una richiesta che cambia qualcosa, il ruolo che può farla. Serve anche la sua operazione nel documento OpenAPI: esegui `python tools/make_openapi.py`, che scrive `docs/openapi.yaml`, `docs/openapi.json` e `docs/ROUTES.md`.
* Segnala una vulnerabilità in privato: [SECURITY.md](SECURITY.md). Domande, idee e segnalazioni: electrohobby3d@gmail.com ([SUPPORT.md](SUPPORT.md)).

## 👤 AUTORE

**JuanenRac (Electro Hobby 3D)** · electrohobby3d@gmail.com

## 📜 LICENZA

Software: GPL-3.0-or-later. Vedi [LICENSE](LICENSE). I progetti hardware della suite usano la CERN OHL-S.
