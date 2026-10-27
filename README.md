<div align="center">
  <img src="https://raw.githubusercontent.com/madchicken/homebridge-comelit-hub/master/images/comelit.png" alt="Comelit Logo" width="120" />
  <h1>comelit-hub-rs</h1>
  <p>Bridge HomeKit per impianti domotici Comelit HUB, scritto in Rust.</p>

  [![Build](https://github.com/madchicken/comelit-hub-rs/actions/workflows/rust.yml/badge.svg)](https://github.com/madchicken/comelit-hub-rs/actions/workflows/rust.yml)
  [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
</div>

---

## Panoramica

`comelit-hub-rs` è un bridge che espone i dispositivi di un impianto **Comelit HUB** come accessori **Apple HomeKit** tramite il protocollo HAP (HomeKit Accessory Protocol). Comunica con il concentratore Comelit via **MQTT** e permette di controllare luci, tapparelle, termostati, citofoni e cancelli direttamente dall'app Casa di iOS/macOS.

Con un videocitofono **Comelit ViP** il campanello diventa un videocitofono HomeKit completo: notifica con l'immagine di chi suona, diretta video con audio e risposta a voce dal microfono dell'iPhone.

### Dispositivi supportati

| Dispositivo | Accessorio HomeKit |
|---|---|
| 💡 Luci | Lampadina on/off |
| 🪟 Tapparelle / veneziane | Window Covering con posizione |
| 🌡️ Termostati / deumidificatori | Termostato |
| 🚪 Cancelli / porte | Porta |
| 🔔 Citofono | Campanello |
| 📹 Videocitofono ViP | Videocitofono: anteprima, diretta video e audio, parlare al posto esterno |

---

## Architettura

Il workspace Rust è composto da cinque crate:

```
comelit-hub-rs/
├── client/        # comelit-client-rs — libreria MQTT + CLI comelit-hub-cli
├── hap/           # comelit-hub-hap   — bridge HAP (binario principale)
├── matter/        # comelit-matter    — bridge Matter (sperimentale)
├── tui/           # comelit-hub-tui   — interfaccia TUI di diagnostica
└── viper-client/  # viper-client      — client del videocitofono ViP + CLI
```

Il bridge si connette al concentratore Comelit, recupera l'indice dei dispositivi, crea gli accessori HAP corrispondenti e rimane in ascolto degli aggiornamenti MQTT per sincronizzare lo stato in tempo reale. Il videocitofono passa invece dal protocollo ViP (porta 64100), descritto in [`viper-client/README.md`](viper-client/README.md).

---

## Prerequisiti

- Concentratore **Comelit HUB** (modello 20003150 o compatibile) raggiungibile in rete locale
- **Apple Home** (iOS 16+ / macOS 13+) per l'associazione HomeKit; per usare il videocitofono fuori casa serve un hub Casa (Apple TV, HomePod o iPad)
- Per il videocitofono: un impianto **Comelit ViP** raggiungibile in rete locale e un utente ViP dedicato al bridge (vedi [Videocitofono](#videocitofono))

Solo per la compilazione da sorgenti:

- **Rust** 1.88+ (edition 2024)
- **cmake** e un compilatore C, per libopus e libfdk-aac che vengono compilate nel binario
  - macOS: `xcode-select --install` e `brew install cmake`
  - Debian / Ubuntu: `sudo apt install build-essential cmake`

---

## Installazione

### Pacchetti precompilati

Ogni [release](https://github.com/madchicken/comelit-hub-rs/releases) contiene:

- `comelit-hub-hap-<versione>-amd64.deb` per Linux x86_64 (Debian, Ubuntu)
- `comelit-hub-hap-<versione>-macos.pkg` per macOS su Apple Silicon

I pacchetti copiano il binario, gli script, l'unità systemd o il LaunchDaemon e una configurazione di esempio, ma non creano le cartelle di lavoro e non avviano il servizio. Dopo l'installazione:

**Linux**
```bash
sudo dpkg -i comelit-hub-hap-<versione>-amd64.deb
sudo mkdir -p /var/lib/comelit-hub-hap /var/log/comelit-hub-hap
sudo systemctl daemon-reload
sudo systemctl enable --now comelit-hub-hap
```

**macOS**
```bash
sudo installer -pkg comelit-hub-hap-<versione>-macos.pkg -target /
sudo mkdir -p /var/lib/comelit-hub-hap /var/log/comelit-hub-hap
sudo launchctl load /Library/LaunchDaemons/com.comelit.hub.hap.plist
```

Prima di avviare il servizio conviene impostare credenziali e configurazione (vedi [Configurazione](#configurazione)).

### Da sorgenti

```bash
git clone https://github.com/madchicken/comelit-hub-rs.git
cd comelit-hub-rs
cargo build --release -p comelit-hub-hap
```

Il binario compilato si trova in `target/release/comelit-hub-hap`.

Per un Mac con una versione di macOS più vecchia di quella su cui si compila va indicata la versione minima, per esempio `MACOSX_DEPLOYMENT_TARGET=15.0 cargo build --release -p comelit-hub-hap`. Per un Mac Intel da un Mac Apple Silicon si aggiunge `--target x86_64-apple-darwin` (dopo `rustup target add x86_64-apple-darwin`): il binario finisce in `target/x86_64-apple-darwin/release/`.

### Script di installazione (Linux / macOS)

Dopo la compilazione, lo script installa il binario, i file di configurazione e lo script `comelit-hub-ctl`, registra il servizio di sistema e lo avvia.

```bash
cd hap/installer
sudo ./install.sh
```

Lo script prende il binario da `target/release`; per una build di debug si usa `sudo BIN_TYPE=debug ./install.sh`. Se `/etc/comelit-hub-hap` contiene già una configurazione, viene mantenuta: lo script si può rilanciare per aggiornare il binario.

**Linux**: installa e abilita un'unità **systemd**:
```
/etc/systemd/system/comelit-hub-hap.service
```

**macOS**: installa un **LaunchDaemon**:
```
/Library/LaunchDaemons/com.comelit.hub.hap.plist
```

In entrambi i casi il servizio gira come root con cartella di lavoro `/var/lib/comelit-hub-hap`, dove sono salvate le associazioni HomeKit, e scrive i log in `/var/log/comelit-hub-hap`.

Per disinstallare:
```bash
sudo ./uninstall.sh
```

La disinstallazione rimuove binario, servizio e configurazione, chiede se cancellare i log e lascia `/var/lib/comelit-hub-hap`: reinstallando, gli accessori restano associati a Casa.

### Windows (sperimentale)

`hap/installer/windows/install.ps1` registra il bridge come servizio con [NSSM](https://nssm.cc/), a partire da un `comelit-hub-hap.exe` nella cartella corrente. Lo script non è testato e non passa credenziali né file di configurazione al servizio: vanno aggiunti con `nssm set ComelitHubHAP AppParameters ...`, usando le opzioni di [Avvio manuale](#avvio-manuale).

---

## Configurazione

### Variabili d'ambiente (`/etc/comelit-hub-hap/comelit-hub-hap.env`)

Lette dallo script di avvio del servizio:

```env
COMELIT_USER=admin
COMELIT_PASSWORD=admin
COMELIT_CONFIG=/etc/comelit-hub-hap/comelit-hub-hap-config.json
RUST_LOG=info

# Log
COMELIT_LOG_DIR=/var/log/comelit-hub-hap
COMELIT_LOG_PREFIX=comelit-hub
COMELIT_LOG_ROTATION=daily   # minutely | hourly | daily | never
COMELIT_MAX_LOG_FILES=7
```

Il servizio cerca il concentratore in rete: l'indirizzo non si imposta da qui.

### File di configurazione (`comelit-hub-hap-config.json`)

```json
{
  "pairing_code": [1, 1, 1, 2, 2, 3, 3, 3],
  "mount_lights": true,
  "mount_window_covering": true,
  "mount_thermo": true,
  "mount_doors": true,
  "mount_doorbells": false,
  "window_covering": {
    "opening_time": 35,
    "closing_time": 35
  },
  "door": {
    "opening_closing_time": 60,
    "opened_time": 60
  },
  "prometheus_url": null,
  "prometheus_token": null,
  "doorbell_video": {
    "enabled": false,
    "viper_token": null,
    "viper_host": null,
    "viper_port": 64100,
    "entrance": null,
    "ring_timeout_secs": 60,
    "snapshot_max_age_secs": 5,
    "video_bitrate_kbps": 1000
  }
}
```

La sezione `doorbell_video` è facoltativa: se manca, il videocitofono è disattivato.

| Chiave | Descrizione |
|---|---|
| `pairing_code` | Codice di 8 cifre per l'associazione HomeKit |
| `mount_*` | Abilita/disabilita la registrazione per categoria di dispositivi |
| `window_covering.opening_time` | Tempo in secondi per aprire completamente una tapparella |
| `window_covering.closing_time` | Tempo in secondi per chiudere completamente una tapparella |
| `door.opening_closing_time` | Durata del ciclo apertura/chiusura cancello (secondi) |
| `door.opened_time` | Tempo che il cancello rimane aperto prima di richiudersi (secondi) |
| `prometheus_url` | URL di un server Prometheus da cui la pagina dei grafici della web UI legge le serie storiche (opzionale) |
| `prometheus_token` | Token di accesso a `prometheus_url`, se richiesto (opzionale) |
| `doorbell_video.enabled` | Collega il campanello al videocitofono ViP: lo squillo arriva dal sistema ViP invece che dai messaggi MQTT dell'hub (richiede `mount_doorbells`) |
| `doorbell_video.viper_token` | Token utente ViP, obbligatorio se `enabled`. Deve essere un utente dedicato al bridge (vedi [Videocitofono](#videocitofono)) |
| `doorbell_video.viper_host` | Indirizzo del gateway ViP; se assente viene cercato in rete |
| `doorbell_video.viper_port` | Porta del gateway ViP |
| `doorbell_video.entrance` | Nome dell'ingresso ViP da inquadrare quando nessuno suona |
| `doorbell_video.ring_timeout_secs` | Dopo quanti secondi di squillo senza risposta il bridge chiude il proprio ramo della chiamata |
| `doorbell_video.snapshot_max_age_secs` | Per quanti secondi riusare l'anteprima della telecamera. L'app Casa ne chiede una ogni 10 secondi mentre mostra il campanello: la telecamera esterna resta accesa e si spegne poco dopo la chiusura dell'app. Uno squillo ne cattura sempre una nuova |
| `doorbell_video.video_bitrate_kbps` | Bitrate in kbit/s chiesto al posto esterno quando non c'è una diretta HomeKit: è la qualità delle anteprime. Durante una diretta il posto esterno usa il bitrate chiesto da HomeKit (da 132 a 299 kbit/s), perché HomeKit blocca il video se riceve di più. La telecamera manda sempre 320x240 |

---

## Videocitofono

Con `mount_doorbells` e `doorbell_video.enabled` il campanello diventa un videocitofono HomeKit, pubblicato come accessorio a sé (iOS non accetta videocitofoni dentro un bridge): va aggiunto in Casa separatamente, con lo stesso codice del bridge. Il log lo ricorda all'avvio con `Pair doorbell ... using pin code ...`.

### Utente ViP dedicato

Il bridge si registra nell'impianto ViP con il token di un proprio utente. Il token di un'app già in uso non va bene: le chiamate smetterebbero di arrivare a quell'app. Il nuovo utente si crea con:

```bash
viper-client --ip <indirizzo del gateway ViP> --token <token di un utente esistente> \
  activate --email <email> --description comelit-hub-hap
```

Il comando stampa il token del nuovo utente da mettere in `doorbell_video.viper_token`.

### Cosa fa

- **Squillo**: il campanello suona sui dispositivi Apple, con l'anteprima di chi è al posto esterno nella notifica.
- **Diretta**: il video H.264 del posto esterno arriva all'iPhone cifrato con SRTP, senza transcodifica né `ffmpeg`. Mentre suona il video viene dalla chiamata in corso, altrimenti dall'ingresso `entrance`.
- **Audio**: l'audio del posto esterno (G.711 A-law a 8 kHz) viene convertito in AAC-ELD, il formato che chiedono gli hub Casa, o in Opus se l'iPhone lo chiede direttamente.
- **Risposta**: aprendo la diretta mentre qualcuno suona, per esempio toccando la notifica, il bridge risponde alla chiamata, perché il posto esterno manda l'audio solo a chiamata risposta. Gli altri dispositivi smettono di squillare e l'app Comelit mostra "chiamata risposta da altro dispositivo". Chiudendo la diretta il bridge riaggancia. Le anteprime non rispondono mai.
- **Parlare**: toccando il microfono nella diretta la voce dell'iPhone arriva all'altoparlante del posto esterno, sia durante uno squillo sia guardando l'ingresso.

libfdk-aac e libopus sono compilate nel binario: non servono librerie di sistema.

---

## Avvio manuale

```bash
comelit-hub-hap \
  --user admin \
  --password admin \
  --host 192.168.1.100 \
  --settings /etc/comelit-hub-hap/comelit-hub-hap-config.json
```

Se `--host` viene omesso, il bridge esegue una scansione automatica della rete locale per trovare il concentratore Comelit. Le associazioni HomeKit vengono salvate nella cartella corrente.

### Opzioni complete

```
--user <USER>               Utente Comelit Bridge [default: admin]
--password <PASSWORD>       Password Comelit Bridge [default: admin]
--host <HOST>               IP o hostname del concentratore
--port <PORT>               Porta MQTT [default: 1883]
--settings <PATH>           Percorso del file di configurazione JSON
--log-dir <DIR>             Directory per i file di log (senza, log su console)
--log-prefix <PREFIX>       Prefisso dei file di log [default: comelit-hub]
--log-rotation <PERIOD>     Rotazione: minutely | hourly | daily | never [default: daily]
--max-log-files <N>         Numero massimo di file di log (0 = illimitato) [default: 7]
--log-to-console            Stampa i log anche su console (con --log-dir)
--web-port <PORT>           Porta della web UI [default: 8080]
```

La web UI è sempre attiva: `--web-enabled` esiste ma non permette di spegnerla.

---

## Associazione con HomeKit

Al primo avvio il bridge stampa nel log il codice di associazione:

```
Pair your Comelit Bridge using pin code 111-22-333
```

Nell'app Casa si aggiunge un accessorio inserendo il codice, oppure inquadrando il QR code della web UI. Il codice di default è `111-22-333` (configurabile tramite `pairing_code` nel JSON). Il videocitofono si aggiunge a parte con lo stesso codice.

---

## Web UI e metriche

Il bridge espone una **web UI** sulla porta `8080` (configurabile):

| Percorso | Contenuto |
|---|---|
| `/` | Dashboard: connessione al concentratore, dispositivi, QR code per l'associazione |
| `/devices` | Dispositivi registrati e loro stato |
| `/charts` | Grafici storici (con `prometheus_url`) |
| `/health` | Controllo di salute |
| `/metrics` | Metriche in formato **Prometheus** |
| `/api/status` | Stato in JSON |

---

## CLI (`comelit-hub-cli`)

Il crate `client` include una CLI per interagire direttamente con il concentratore Comelit senza HomeKit (`cargo build --release -p comelit-client-rs`):

```bash
# Scansione della rete
comelit-hub-cli scan

# Informazioni su un dispositivo
comelit-hub-cli --host 192.168.1.100 info --id DOM#BL#20.1

# Elenco delle luci e accensione/spegnimento
comelit-hub-cli --host 192.168.1.100 lights list
comelit-hub-cli --host 192.168.1.100 lights toggle --id DOM#LT#1.1 --toggle 1

# Ascolta gli aggiornamenti in tempo reale
comelit-hub-cli --host 192.168.1.100 listen
```

## CLI del videocitofono (`viper-client`)

Il crate `viper-client` include una CLI per il sistema ViP (`cargo build --release -p viper-client`). Indirizzo e token si passano con `--ip` e `--token` o con le variabili `ICONA_IP` e `ICONA_TOKEN`:

```bash
viper-client info                               # configurazione dell'utente e rubrica
viper-client activate --email <email>           # crea un utente con un proprio token
viper-client open-door --door-name <porta>      # apre una porta della rubrica
viper-client start-video --target <ingresso>    # registra il video di un ingresso (--audio anche l'audio)
viper-client listen-video                       # aspetta uno squillo e ne registra il video
```

---

## Gestione del servizio (`comelit-hub-ctl`)

Lo script `comelit-hub-ctl` è installato in `/usr/local/bin` e fornisce comandi di gestione uniformi su Linux e macOS (`start`, `stop` e `restart` vanno lanciati con `sudo`):

```bash
sudo comelit-hub-ctl start
sudo comelit-hub-ctl stop
sudo comelit-hub-ctl restart
comelit-hub-ctl status
comelit-hub-ctl logs           # ultime 50 righe del log corrente
comelit-hub-ctl logs -f        # segue il log
comelit-hub-ctl logs -n 200    # ultime 200 righe
comelit-hub-ctl list-logs      # file di log presenti
```

`sudo comelit-hub-ctl reset`, solo su Linux, cancella le associazioni HomeKit del bridge (`/var/lib/comelit-hub-hap/data`): dopo il riavvio gli accessori vanno aggiunti di nuovo in Casa.

---

## Sviluppo

```bash
# Compilazione debug
cargo build

# Test
cargo test

# Test con log visibili
RUST_LOG=debug cargo test -- --nocapture

# Interfaccia di diagnostica nel terminale
cargo run -p comelit-hub-tui
```

---

## Licenza

MIT — vedi [LICENSE](LICENSE).
