# Estensione del bridge Matter alle tapparelle (window covering)

Data: 2026-08-23
Stato: approvato per implementazione

## Contesto

Il bridge Matter (`matter/`) espone oggi solo le luci Comelit come endpoint
bridged, con cluster `OnOff`. È un prototipo funzionante ma limitato: usa
credenziali di test `rs-matter` (`TEST_DEV_ATT`/`TEST_DEV_COMM`/`TEST_DEV_DET`)
e non ha equivalenti per gli altri tipi di device già supportati dal bridge
HomeKit (`hap/`) — termostati, tapparelle, prese, irrigazione.

Questo documento definisce l'estensione del bridge Matter alle **tapparelle**,
primo passo di allineamento con `hap/`. Il modello dati Comelit per le
tapparelle (`WindowCoveringDeviceData`) riporta solo uno stato discreto
(`Stopped` / `GoingUp` / `GoingDown`), non una posizione percentuale assoluta.
Il bridge HAP colma questa lacuna con un worker (`WindowCoveringWorker` in
`hap/src/accessories/window_covering.rs`) che stima la posizione nel tempo
usando tempi di apertura/chiusura configurati, con persistenza su file tra
riavvii. Questa logica non è specifica di HomeKit — dipende solo dal trait
`ComelitClientTrait` — e va estratta ed estesa a Matter.

## Obiettivo

Un unico bridge Matter (stesso nodo/pairing già usato per le luci) che espone
anche le tapparelle scoperte sull'hub Comelit, con posizione percentuale
stimata coerente con quella già mostrata da HomeKit, riusando la stessa
logica di dominio invece di duplicarla.

## Decisioni di design

Queste decisioni sono state validate con l'utente durante il brainstorming:

1. **Stima della posizione**: sì, replicare la logica del worker HAP
   (percentuale stimata nel tempo), non solo comandi Apri/Chiudi/Stop.
2. **Condivisione del codice**: la state machine (`WindowCoveringWorker`,
   `WindowCoveringState`, `PositionState`) si sposta da `hap/` a `client/`
   (`comelit-client-rs`), protocollo-agnostica; `hap/` e `matter/` la riusano
   ciascuno con il proprio "sink" finale.
3. **Persistenza**: sì, riusare la stessa persistenza su file (stessa
   convenzione `FileStorage::current_dir()`), spostata insieme alla state
   machine.
4. **Ambito del bridge**: un unico bridge Matter con endpoint eterogenei
   (luci + tapparelle, tutte quelle scoperte), non un bridge separato.
5. **Configurazione tempi apertura/chiusura**: `matter/` legge lo stesso file
   di settings JSON usato da `hap/` (stesso formato, stesso file se
   l'utente lo punta a entrambi), estraendone solo la sezione
   `window_covering` — senza dipendere dal crate `hap/`.
6. **Dispatch endpoint eterogenei**: `enum BridgedEntry { Light(LightEntry),
   WindowCovering(CoveringEntry) }` con `match` statico in
   `ComelitBridgeHandler`, niente `dyn` dispatch (evita di dover boxare i
   future di `AsyncHandler`, che è un async-trait).

## Architettura

### 1. `client/src/covering/` (nuovo modulo condiviso)

Contenuto spostato meccanicamente da `hap/src/accessories/window_covering.rs`
e `hap/src/accessories/state/window_covering.rs`, senza riscritture della
logica:

- `WindowCoveringState`, `PositionState`, `FULLY_OPENED`/`FULLY_CLOSED`
- `WorkerCommand`, `WindowCoveringWorker<C: ComelitClientTrait>` con tutti i
  suoi metodi (`handle_move_to`, `handle_status_update`, `update_position`,
  `finalize_position`, `finalize_position_with_target`)
- `WindowCoveringConfig` (opening_time/closing_time come `Duration`)
- `WindowCoveringSettings` (i due campi grezzi in secondi, `Serialize`/
  `Deserialize`, con lo stesso default 35s/35s)
- `from_storage`/`save` per la persistenza su file

L'unico punto che nel codice originale è HomeKit-specifico è l'ultimo passo
del worker, oggi chiamato `update_accessory`. Diventa un trait:

```rust
#[async_trait]
pub trait WindowCoveringSink: Send + Sync {
    async fn update(&self, state: WindowCoveringState);
}
```

`WindowCoveringWorker` tiene un `Arc<dyn WindowCoveringSink>` (o è generico su
`S: WindowCoveringSink`, da decidere in fase di implementazione in base a
cosa richiede meno boilerplate) e lo invoca al posto della vecchia chiamata
diretta alle characteristics HAP.

I test esistenti (`test_move_to_open`, `test_external_movement`,
`test_move_to_close`, `test_no_action_when_same_position`,
`test_no_spurious_move_when_target_equals_current`,
`test_reaches_target_and_stops`) si spostano con il codice, adattati solo per
usare un `WindowCoveringSink` finto al posto delle assertion dirette su
characteristics HAP — la logica sotto test non cambia.

### 2. `hap/src/accessories/window_covering.rs` (adattatore, ridotto)

- `ComelitWindowCoveringAccessory::new` costruisce il worker importandolo da
  `comelit_client_rs::covering::*` invece di usare la copia locale.
- Implementa `WindowCoveringSink` scrivendo le characteristics HAP (quello
  che oggi fa `update_accessory`).
- Nessun cambio di comportamento visibile: stesso set di test, stesso
  comportamento runtime.

### 3. `matter/src/covering.rs` (nuovo)

`rs-matter` non ha, per `WindowCovering`, un modulo "hooks" scritto a mano
come `dm::clusters::app::on_off::OnOffHooks`. Genera dall'IDL Matter
(`controller-clusters-V1.5.1.0.matter`, cluster id `0x0102`/258) il trait
completo `dm::clusters::decl::window_covering::ClusterAsyncHandler` (~25
metodi: attributi lift/tilt, `OperationalStatus`, comandi `UpOrOpen`/
`DownOrClose`/`StopMotion`/`GoToLiftValue`/`GoToLiftPercentage`/
`GoToTiltValue`/`GoToTiltPercentage`) e un `HandlerAsyncAdaptor<T>` che
implementa `AsyncHandler` generico sopra quel trait.

`ComelitCoveringHandler` implementa `ClusterAsyncHandler` per intero:

- **Attributi reali** (letti da uno stato condiviso aggiornato dal worker):
  `current_position_lift_percentage`, `current_position_lift_percent_100_ths`,
  `target_position_lift_percent_100_ths`, `operational_status`.
- **Attributi costanti/non pertinenti**: nessun supporto tilt (i metodi tilt
  ritornano valori di default/non-nulli coerenti con l'assenza della feature
  `Tilt` nel `FeatureMap`), `physical_closed_limit_lift` e limiti installati
  fissi (0 / 10000, coerenti con `Percent100ths`), `end_product_type` fisso
  su `RollerShutter` o `Unknown` (da confermare in implementazione in base a
  cosa Comelit espone in `ObjectSubtype`), `mode`/`safety_status` con valori
  di default statici.
- **Comandi**: `handle_up_or_open`, `handle_down_or_close`,
  `handle_stop_motion`, `handle_go_to_lift_percentage` traducono in
  `WorkerCommand::MoveTo` inviato su un channel `mpsc` (stesso pattern del
  `MqttCommand` in `light.rs`). `GoToLiftValue`/tilt non supportati:
  ritornano un errore Matter appropriato (`ErrorCode::InvalidCommand` o
  equivalente, da allineare a come `on_off` gestisce comandi non
  applicabili).

`ComelitCoveringHandler` implementa anche `WindowCoveringSink`: alla
notifica del worker aggiorna lo stato condiviso (stessa forma di
`LightState`: campi atomici/`RwLock` più un `Signal` per svegliare le
subscription Matter, mirror di `LightState::signal`).

`FeatureMap` dichiara solo la feature `PositionAwareLift` (nessun `Tilt`,
nessun `AbsolutePosition` se non necessaria).

### 4. `matter/src/bridge.rs` (generalizzato)

```rust
enum BridgedEntry {
    Light(LightEntry),
    WindowCovering(CoveringEntry),
}
```

`CoveringEntry` è l'equivalente di `LightEntry` per le tapparelle: `ep_id`,
`window_covering: HandlerAsyncAdaptor<ComelitCoveringHandler>`, `desc`,
`groups`, `bridged: BridgedInfo` (riusato invariato — non dipende dal tipo di
device).

`ComelitBridgeHandler` passa da `lights: Vec<LightEntry>` a
`entries: Vec<BridgedEntry>`. `read`/`write`/`invoke`/`bump_dataver` fanno
`match` sulla variante per instradare verso il cluster handler giusto.

Nuova costante device type (non ancora presente in `rs-matter`, va definita
localmente come già fa il crate per `DEV_TYPE_ON_OFF_LIGHT`):

```rust
const DEV_TYPE_WINDOW_COVERING: DeviceType = DeviceType { dtype: 0x0202, drev: 1 };
static COVERING_DEVICE_TYPES: [DeviceType; 2] =
    [DEV_TYPE_WINDOW_COVERING, DEV_TYPE_BRIDGED_NODE];
```

(la revisione `drev` va verificata contro la Device Library Matter corrente
in fase di implementazione, seguendo lo stesso stile di commento che
`rs-matter` usa per gli altri device type).

`BridgeMetadata::new` costruisce la lista cluster/endpoint per ogni variante
di `BridgedEntry` (oggi costruisce solo per luci).

### 5. `matter/src/main.rs` (discovery e avvio estesi)

- Step 3 (discovery): oltre a `HomeDeviceData::Light`, filtra anche
  `HomeDeviceData::WindowCovering`. Endpoint id assegnati in ordine di
  scoperta su **tutti** i device insieme (ordinamento stabile per id, come
  oggi solo per le luci) — luci e tapparelle si intercalano naturalmente
  nella numerazione degli endpoint, non è un problema per Matter.
- Nuovo flag CLI `--settings <path>` (mirror di `hap/src/main.rs::Params`),
  opzionale. Se assente: `WindowCoveringSettings::default()` (35s/35s). Se
  presente: deserializza **solo** `{ window_covering: WindowCoveringSettings
  }` dal file JSON — una struct minimale locale a `matter/`, non importa
  `hap::settings::Settings`. `serde` ignora automaticamente gli altri campi
  del file (`pairing_code`, `mount_*`, `prometheus_*`), quindi lo stesso file
  usato da HAP funziona senza modifiche.
- Persistenza: `covering::WindowCoveringState::from_storage`/`save` (mosse in
  `client/`), stessa convenzione `FileStorage::current_dir()` di HAP.
- `run_matter` costruisce `Vec<BridgedEntry>` invece di `Vec<LightEntry>`,
  mescolando le due varianti in base al tipo scoperto.

## Data flow

**Comando in ingresso** (da un controller Matter):
`UpOrOpen`/`DownOrClose`/`StopMotion`/`GoToLiftPercentage` → invoke su
`ComelitCoveringHandler` → `WorkerCommand::MoveTo` sul channel mpsc →
`WindowCoveringWorker::handle_move_to` (logica invariata) → azione Comelit
(`ActionType::SetBlindPosition` o equivalente) → `client.send_action(...)`.

**Aggiornamento in uscita** (da push MQTT Comelit):
push → `ComelitObserver` → `WindowCoveringWorker::handle_status_update` →
timer di stima posizione (`update_position`, invariato) →
`WindowCoveringSink::update` → per Matter: stato condiviso aggiornato +
`Signal` → gli attributi Matter riflettono il nuovo stato, le subscription
attive notificano il change.

## Error handling

Stesso stile già in uso nel bridge Matter: un errore nell'inizializzazione di
un singolo device viene loggato e quel device viene saltato, senza abbattere
l'intero bridge (coerente con il comportamento già presente per le luci).
Errori di comando (es. comando non applicabile, timeout worker) vengono
mappati su `rs_matter::error::Error` con un `ErrorCode` appropriato, seguendo
la stessa convenzione già usata per gli invoke `OnOff` falliti.

## Testing

- I test unitari esistenti del worker (in `hap/src/accessories/window_covering.rs`,
  sezione `#[cfg(test)]`) si spostano in `client/` con il codice, adattati
  solo nel punto di iniezione del `WindowCoveringSink` finto — la logica
  sotto test (state machine, stima posizione, `FakeComelitClient`) resta
  identica. Questo garantisce che lo spostamento non introduca regressioni
  in `hap/`.
- Nuovi test unitari per `ComelitCoveringHandler` in `matter/`: mapping
  stato condiviso → attributi Matter letti, e invoke comando → `WorkerCommand`
  atteso sul channel.
- Verifica manuale: `cargo check -p comelit-hub-matter` e
  `cargo check -p comelit-client-rs` (oltre a `cargo test`) su entrambi i
  crate. Non è previsto un ambiente Matter reale in CI per questo cambiamento
  — nessun test end-to-end con un controller Matter reale è nell'ambito di
  questa spec.

## Fuori ambito

- Tilt (le tapparelle Comelit non lo supportano; il cluster Matter lo rende
  disponibile ma non viene esposto).
- Altri tipi di device (termostati, prese, irrigazione, porte/campanelli) —
  seguiranno con spec separate una volta consolidato questo pattern.
- Certificazione Matter / credenziali di produzione (`TEST_DEV_ATT` ecc.
  restano invariate, fuori ambito di questa estensione).
- Bridge Matter separato per le tapparelle (scartato in fase di design).

## Rischi e aperture da chiudere in implementazione

- Valore esatto di `drev` per `DEV_TYPE_WINDOW_COVERING` da verificare contro
  la Device Library Matter alla revisione corrente.
- Scelta tra `Arc<dyn WindowCoveringSink>` e generico `S: WindowCoveringSink`
  per `WindowCoveringWorker` — decisione locale in fase di implementazione,
  in base a quale richiede meno boilerplate nei due call site (`hap/` e
  `matter/`).
- Mapping esatto di `end_product_type` verso i valori Comelit disponibili in
  `ObjectSubtype` (se assente un mapping sensato, usare `Unknown`).
