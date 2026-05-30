<div align="center">
  <img src="https://raw.githubusercontent.com/madchicken/homebridge-comelit-hub/master/images/comelit.png" alt="Comelit Logo" width="120" />
  <h1>comelit-hub-rs</h1>
  <p>Bridge HomeKit per impianti domotici Comelit HUB, scritto in Rust.</p>

  [![Build](https://github.com/madchicken/comelit-hub-rs/actions/workflows/build.yml/badge.svg)](https://github.com/madchicken/comelit-hub-rs/actions)
  [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
</div>

---

## Panoramica

`comelit-hub-rs` è un bridge che espone i dispositivi di un impianto **Comelit HUB** come accessori **Apple HomeKit** tramite il protocollo HAP (HomeKit Accessory Protocol). Comunica con il concentratore Comelit via **MQTT** e permette di controllare luci, tapparelle, termostati, citofoni e cancelli direttamente dall'app Casa di iOS/macOS.

### Dispositivi supportati

| Dispositivo | Accessorio HomeKit |
|---|---|
| 💡 Luci | Lampadina on/off |
| 🪟 Tapparelle / veneziane | Window Covering con posizione |
| 🌡️ Termostati / deumidificatori | Termostato |
| 🚪 Cancelli / porte | Porta |
| 🔔 Citofono | Campanello |

---

## Architettura

Il workspace Rust è composto da quattro crate:

```
comelit-hub-rs/
├── client/        # comelit-client-rs — libreria MQTT + CLI
├── hap/           # comelit-hub-hap   — bridge HAP (binario principale)
├── tui/           # interfaccia TUI di diagnostica
└── viper-client/  # client per dispositivi Viper
```

Il bridge si connette al concentratore Comelit, recupera l'indice dei dispositivi, crea gli accessori HAP corrispondenti e rimane in ascolto degli aggiornamenti MQTT per sincronizzare lo stato in tempo reale.

---

## Prerequisiti

- Concentratore **Comelit HUB** (modello 20003150 o compatibile) raggiungibile in rete locale
- **Rust** 1.80+ (edition 2024) — solo per compilazione da sorgenti
- **Apple Home** (iOS 16+ / macOS 13+) per l'associazione HomeKit

---

## Installazione

### Da sorgenti

```bash
git clone https://github.com/madchicken/comelit-hub-rs.git
cd comelit-hub-rs
cargo build --release -p comelit-hub-hap
```

Il binario compilato si trova in `target/release/comelit-hub-hap`.

### Script di installazione (Linux / macOS)

Lo script installa il binario, i file di configurazione e registra il servizio di sistema.

```bash
cd hap/installer
sudo ./install.sh
```

**Linux** — installa e abilita un'unità **systemd**:
```
/etc/systemd/system/comelit-hub-hap.service
```

**macOS** — installa un **LaunchDaemon**:
```
/Library/LaunchDaemons/com.comelit.hub.hap.plist
```

Per disinstallare:
```bash
sudo ./uninstall.sh
```

---

## Configurazione

### Variabili d'ambiente (`/etc/comelit-hub-hap/comelit-hub-hap.env`)

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
  "prometheus_token": null
}
```

| Chiave | Descrizione |
|---|---|
| `pairing_code` | Codice di 8 cifre per l'associazione HomeKit |
| `mount_*` | Abilita/disabilita la registrazione per categoria di dispositivi |
| `window_covering.opening_time` | Tempo in secondi per aprire completamente una tapparella |
| `window_covering.closing_time` | Tempo in secondi per chiudere completamente una tapparella |
| `door.opening_closing_time` | Durata del ciclo apertura/chiusura cancello (secondi) |
| `door.opened_time` | Tempo che il cancello rimane aperto prima di richiudersi (secondi) |
| `prometheus_url` | URL del push gateway Prometheus (opzionale) |

---

## Avvio manuale

```bash
comelit-hub-hap \
  --user admin \
  --password admin \
  --host 192.168.1.100 \
  --settings /etc/comelit-hub-hap/comelit-hub-hap-config.json
```

Se `--host` viene omesso, il bridge esegue una scansione automatica della rete locale per trovare il concentratore Comelit.

### Opzioni complete

```
--user <USER>               Utente Comelit Bridge [default: admin]
--password <PASSWORD>       Password Comelit Bridge [default: admin]
--host <HOST>               IP o hostname del concentratore
--port <PORT>               Porta MQTT [default: 1883]
--settings <PATH>           Percorso del file di configurazione JSON
--log-dir <DIR>             Directory per i file di log
--log-prefix <PREFIX>       Prefisso dei file di log [default: comelit-hub]
--log-rotation <PERIOD>     Rotazione: minutely | hourly | daily | never [default: daily]
--max-log-files <N>         Numero massimo di file di log (0 = illimitato) [default: 7]
--log-to-console            Stampa i log anche su console (con --log-dir)
--web-enabled               Abilita la web UI [default: true]
--web-port <PORT>           Porta della web UI [default: 8080]
```

---

## Associazione con HomeKit

Al primo avvio il bridge stampa un **QR code** nel log. Inquadralo con l'app Casa per completare l'associazione:

```
Pair your Comelit Bridge using pin code 111-22-333
```

Il codice di default è `111-22-333` (configurabile tramite `pairing_code` nel JSON).

---

## Web UI e metriche

Il bridge espone una **web UI** sulla porta `8080` (configurabile) con:

- Stato della connessione al concentratore
- Lista dei dispositivi registrati e loro stato
- QR code per l'associazione HomeKit
- Endpoint `/metrics` in formato **Prometheus**

---

## CLI (`comelit-hub-cli`)

Il crate `client` include una CLI per interagire direttamente con il concentratore Comelit senza HomeKit:

```bash
# Scansione della rete
comelit-hub-cli scan

# Informazioni su un dispositivo
comelit-hub-cli device-info --id DOM#BL#20.1

# Accendi/spegni una luce
comelit-hub-cli lights --id DOM#LT#1.1 --toggle 1

# Ascolta gli aggiornamenti in tempo reale
comelit-hub-cli listen
```

---

## Gestione del servizio (`comelit-hub-ctl`)

Lo script `comelit-hub-ctl` è installato in `/usr/local/bin` e fornisce comandi di gestione uniformi su Linux e macOS:

```bash
comelit-hub-ctl start
comelit-hub-ctl stop
comelit-hub-ctl restart
comelit-hub-ctl status
comelit-hub-ctl logs
```

---

## Sviluppo

```bash
# Compilazione debug
cargo build

# Test
cargo test

# Test con log visibili
RUST_LOG=debug cargo test -- --nocapture
```

---

## Licenza

MIT — vedi [LICENSE](LICENSE).
