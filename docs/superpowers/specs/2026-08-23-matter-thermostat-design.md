# Estensione del bridge Matter ai termostati

Data: 2026-08-23
Stato: approvato per implementazione

## Contesto

Il bridge Matter (`matter/`) espone oggi luci (`OnOff`) e tapparelle
(`WindowCovering`) come endpoint bridged, seguendo un pattern consolidato:
logica di dominio protocollo-agnostica estratta in `client::<dominio>`,
riusata sia dal bridge HomeKit (`hap/`) sia dal bridge Matter, con un
`ClusterAsyncHandler` scritto a mano lato Matter (`rs-matter` non genera
moduli "hooks" per i cluster usati finora) e dispatch tramite l'enum
`BridgedEntry` in `matter/src/bridge.rs`.

Questo documento estende lo stesso pattern ai **termostati**. Il bridge HomeKit
gestisce oggi i termostati con `ThermostatWorker` (in
`hap/src/accessories/thermostat.rs`), cablato direttamente su `ComelitClient`
concreto (non generico su `ComelitClientTrait`, a differenza del worker delle
tapparelle) e privo di test propri. Il modello dati Comelit
(`ThermostatDeviceData`) riporta temperatura, umidità, modalità di controllo
(`ClimaMode`), stagione (`ThermoSeason`) e, per i modelli combo
(`ClimaThermostatDehumidifier`), stato del deumidificatore.

## Obiettivo

Un termostato esposto come endpoint Matter (cluster `Thermostat`, 0x0201),
riusando la stessa logica di riduzione stato già validata in HomeKit
(`ClimaMode`/`ThermoSeason` → Off/Heat/Cool/Auto), estratta in un modulo
condiviso `client::thermostat` come già fatto per `client::covering`.

## Ambito

**Solo il termostato base in questa iterazione.** Il servizio HomeKit
`HumidifierDehumidifier` (presente solo per i modelli
`ClimaThermostatDehumidifier`) resta fuori ambito: Matter non ha un cluster
diretto per il controllo attivo di un deumidificatore (solo
`RelativeHumidityMeasurement`, sola lettura), quindi la sua estensione andrà
affrontata con una spec separata una volta chiarito come rappresentarlo.

## Decisioni di design

Validate con l'utente durante il brainstorming:

1. **Condivisione del codice**: sì, estrarre `ThermostatWorker`/`ThermostatState`
   da `hap/` a `client::thermostat`, generalizzando il worker su
   `ComelitClientTrait` durante lo spostamento (necessario, dato che oggi è
   cablato su `ComelitClient` concreto).
2. **Persistenza**: non necessaria. A differenza delle tapparelle, Comelit
   riporta sempre temperatura e stato in tempo reale — nessuna stima di
   posizione nel tempo, nessun file di stato da persistere.
3. **Mappatura "Auto"**: il cluster Matter Thermostat intende "Auto" come
   modalità dual-setpoint con deadband (richiede la feature `AUTO_MODE`),
   semantica diversa da `ClimaMode::Auto` di Comelit (modalità
   automatica/cronoprogramma). Decisione: **non abilitare la feature
   `AUTO_MODE`**; quando Comelit riporta uno stato che HAP mapperebbe su
   `TargetHeatingCoolingState::Auto`, lo si traduce comunque su
   `SystemMode::Heat` o `SystemMode::Cool` in base alla stagione corrente —
   stessa logica già usata per gli altri casi non-Off. Questo evita di
   promettere al controller Matter un comportamento dual-setpoint che il
   termostato non supporta realmente.

## Architettura

### 1. `client/src/thermostat/` (nuovo modulo condiviso)

- `state.rs`: `ThermostatState { temperature: f32, target_temperature: f32,
  heating_cooling_state: TargetHeatingCoolingState, target_heating_cooling_state:
  TargetHeatingCoolingState }` — **solo i campi termici**; i campi
  `dehumidifier_active`/`dehumidifier_current_state` (oggi in
  `hap/src/accessories/state/thermostat.rs`) **non** entrano nel modulo
  condiviso, restano gestiti localmente da HAP. `TargetHeatingCoolingState`
  (`Off`/`Heat`/`Cool`/`Auto`) spostato invariato. `impl From<&ThermostatDeviceData>
  for ThermostatState` con la stessa logica di riduzione già presente in HAP
  (righe 17-89 di `hap/src/accessories/state/thermostat.rs`), depurata dei
  campi dehumidifier — il calcolo di `heating_cooling_state`/
  `target_heating_cooling_state` (incluso il caso `Auto`, che resta calcolato
  qui identico a oggi: la riduzione a Heat/Cool avviene solo lato Matter,
  Task per HomeKit non cambia) resta identico.
- `worker.rs`: trait `ThermostatSink { async fn update(&self, state:
  ThermostatState) }` (dyn-compatibile via `#[async_trait]`, stesso pattern di
  `WindowCoveringSink`); `ThermostatHandle` con `pub async fn
  set_target_temperature(&self, celsius: f32)`, `pub async fn
  set_hvac_mode(&self, mode: TargetHeatingCoolingState)`, `pub async fn
  mqtt_push(&self, state: ThermostatState)`, `pub async fn set_sink(&self,
  sink: Box<dyn ThermostatSink>)`, `Clone` (stesso motivo delle tapparelle:
  sia HAP che Matter hanno bisogno di più proprietari dello stesso handle) —
  **senza** `Drop`-invia-Shutdown (lezione appresa dal bug critico trovato
  nella review finale del lavoro tapparelle: un `Drop` che spegne il worker
  alla prima clone droppata è pericoloso quando l'handle è `Clone`; qui il
  worker termina naturalmente alla chiusura del canale, quando l'ultima clone
  viene droppata). `spawn_thermostat_worker<C: ComelitClientTrait +
  'static>(id: String, initial: ThermostatState, client: C) -> ThermostatHandle`.
  A differenza del worker delle tapparelle non c'è una state machine con
  stima nel tempo: il worker riceve un comando, lo inoltra al `ComelitClientTrait`
  (`set_thermostat_temperature`, `toggle_thermostat_status`,
  `set_thermostat_mode`, `set_thermostat_season`), e in caso di successo
  aggiorna lo stato locale ed esegue subito `notify_sink` (eco immediato —
  la stessa ottimizzazione già presente in HAP, commentata esplicitamente
  contro il problema di HomeKit che rilegge un valore stantio e ritenta).

### 2. `hap/src/accessories/thermostat.rs` (adattatore, ridotto)

- Usa `comelit_client_rs::thermostat::*` al posto della logica locale.
- Mantiene localmente i due campi dehumidifier (in una struct HAP-specifica
  che affianca lo stato condiviso, non dentro di esso) e la logica dei comandi
  `SetDehumidifierActive`/`SetDehumidifierThreshold`, che restano invariati
  e non passano dal modulo condiviso.
- Implementa `HapThermostatSink` scrivendo le characteristics termiche del
  servizio `Thermostat` HomeKit (quello che oggi fa la prima metà di
  `update_accessory`); la scrittura delle characteristics del servizio
  `HumidifierDehumidifier` resta locale al worker HAP, pilotata dallo stato
  dehumidifier che HAP continua a gestire per conto proprio.

### 3. `matter/src/thermostat.rs` (nuovo)

`ComelitThermostatHandler` implementa per intero il trait generato
`dm::clusters::decl::thermostat::ClusterAsyncHandler` (cluster id `0x0201`/513;
nessun modulo "hooks" scritto a mano in `rs-matter` per questo cluster, stessa
situazione di `WindowCovering`):

- **`CLUSTER`**: feature `HEATING | COOLING` (niente `AUTO_MODE`, per la
  decisione di design sopra). `ControlSequenceOfOperation` fisso su
  `CoolingAndHeating` (valore 4 — sistema capace sia di riscaldare sia di
  raffreddare, senza reheat). Attributi: `LocalTemperature`,
  `OccupiedHeatingSetpoint`, `OccupiedCoolingSetpoint`,
  `ControlSequenceOfOperation`, `SystemMode`, più i limiti
  `AbsMin/MaxHeatSetpointLimit`, `AbsMin/MaxCoolSetpointLimit` (valori
  costanti ragionevoli, es. 7°C–35°C, stesso stile delle costanti già usate
  per i limiti fisici delle tapparelle). Comando: `SetpointRaiseLower`
  (unico comando mandatory del cluster base, aggiusta il setpoint corrente
  di un delta relativo).
- **Attributi reali** (letti da uno stato condiviso aggiornato dal worker,
  stesso schema `CoveringState`/`Signal` già usato per le tapparelle):
  `local_temperature` ← `temperature`; `occupied_heating_setpoint` e
  `occupied_cooling_setpoint` **entrambi** rispecchiano `target_temperature`
  (Comelit ne ha uno solo — un controller che guarda l'uno o l'altro vede
  comunque un valore sensato, indipendentemente da quale dei due sia
  "attivo" per la modalità corrente); `system_mode` ← mappatura
  `TargetHeatingCoolingState` → `Off`/`Heat`/`Cool` (mai `Auto`, per la
  decisione sopra).
- **Comandi/scritture**:
  - scrittura `system_mode` → traduce in `set_hvac_mode` sull'handle, che a
    sua volta chiama, nell'ordine, `toggle_thermostat_status`
    (`OnThermo`/`OffThermo`), e se non-Off, `set_thermostat_season`
    (`Summer` per Cool, `Winter` per Heat) + `set_thermostat_mode(Manual)` —
    stessa sequenza già usata da `ThermostatCommand::SetHvacMode` in HAP per
    il caso non-Auto.
  - scrittura `occupied_heating_setpoint`/`occupied_cooling_setpoint` →
    `set_target_temperature` sull'handle → `set_thermostat_temperature`.
  - comando `SetpointRaiseLower` → legge il setpoint corrente dallo stato
    condiviso, applica il delta, richiama `set_target_temperature`.
- `ThermostatMatterSink` implementa `ThermostatSink`, aggiornando lo stato
  condiviso Matter (atomics) e il `Signal` di subscription, stesso schema di
  `MatterCoveringSink`. Segue la lezione della review finale del lavoro
  tapparelle: il cluster handler **deve** sovrascrivere `ClusterAsyncHandler::run`
  per drenare il `Signal` e notificare le subscription immediatamente
  (`ctx.notify_cluster_changed(...)`), non lasciare il default `pending()` —
  altrimenti gli aggiornamenti di temperatura arrivano ai controller solo al
  giro di polling della subscription, con lo stesso ritardo già identificato
  come gap per le tapparelle.

### 4. `matter/src/bridge.rs` (esteso)

```rust
pub enum BridgedEntry {
    Light(LightEntry),
    WindowCovering(CoveringEntry),
    Thermostat(ThermostatEntry),
}
```

`ThermostatEntry` è l'equivalente di `CoveringEntry`: `ep_id`, `thermostat:
ComelitThermostatHandler`, `desc`, `groups`, `bridged: BridgedInfo` (riusato
invariato). Nuova costante device type locale (`rs-matter` non la definisce,
stesso caso di `DEV_TYPE_WINDOW_COVERING`):

```rust
const DEV_TYPE_THERMOSTAT: DeviceType = DeviceType { dtype: 0x0301, drev: 1 };
```

(`drev` va verificato contro la Device Library corrente in fase di
implementazione, stesso rischio aperto già accettato per
`DEV_TYPE_WINDOW_COVERING`). `read`/`write`/`invoke`/`bump_dataver`/`run`
estesi con il terzo braccio del match, strutturalmente parallelo agli altri
due (desc → groups → bridged_device_basic_information → cluster
specifico). `run()` include anche il future di
`thermostat.run(&ctx)`, per la stessa ragione spiegata sopra
(notifica immediata, non solo dataver polling).

### 5. `matter/src/main.rs` (esteso)

Discovery estesa a `HomeDeviceData::Thermostat`, endpoint id assegnati in
ordine di scoperta insieme a luci e tapparelle (stesso ordinamento fisso già
adottato: luci, poi tapparelle, poi termostati). Nessuna sezione di settings
aggiuntiva necessaria (nessun parametro di configurazione come
opening_time/closing_time per il termostato).

## Data flow

**Comando in ingresso**: scrittura Matter (`SystemMode` o un setpoint, o
comando `SetpointRaiseLower`) → `ComelitThermostatHandler` traduce in
`handle.set_hvac_mode(...)` o `handle.set_target_temperature(...)` →
`ThermostatWorker` chiama la sequenza di metodi `ComelitClientTrait`
pertinente → in caso di successo, eco immediato dello stato locale +
`notify_sink`.

**Aggiornamento in uscita**: push MQTT Comelit → observer → `handle.mqtt_push(state)`
→ worker aggiorna lo stato interno + `notify_sink` → per Matter: stato
condiviso aggiornato + `Signal` + `ctx.notify_cluster_changed(...)` via il
`run()` override.

## Error handling

Stesso stile già in uso: errore di init di un singolo termostato loggato e
quel device saltato, senza abbattere il bridge; errori di comando (Comelit
irraggiungibile, ecc.) mappati su `rs_matter::error::Error` con `ErrorCode`
appropriato, come già fatto per `WindowCovering`.

## Testing

- `ThermostatWorker` non ha oggi test propri in HAP: si aggiungono ex novo
  nel modulo condiviso, sul modello di quelli già scritti per
  `client::covering::worker` — mapping `ThermostatDeviceData → ThermostatState`
  (incluso il caso limite dei valori Comelit non parsabili, già gestiti con
  `unwrap_or_default()` nella logica esistente), dispatch comandi verso un
  `ComelitClientTrait` finto, verifica dell'eco immediato dopo un comando
  riuscito.
- Test per `ComelitThermostatHandler`: mapping stato condiviso → attributi
  Matter letti (inclusa la duplicazione del setpoint su entrambi
  heating/cooling), scrittura `SystemMode`/setpoint → comando atteso
  sull'handle, `SetpointRaiseLower` → delta applicato correttamente.
- Verifica manuale: `cargo build --workspace`/`cargo test --workspace` su
  tutti i crate coinvolti.

## Fuori ambito

- Deumidificatore (nessun cluster Matter diretto equivalente disponibile
  oggi; da affrontare con spec separata).
- Feature Matter avanzate del cluster Thermostat: `AUTO_MODE` (per la
  decisione sopra), `SCHEDULE_CONFIGURATION`, `PRESETS`, `SETBACK`,
  `OCCUPANCY` — nessuna richiesta dal modello Comelit attuale.
- Altri tipi di device (prese, irrigazione, porte/campanelli) — restano
  fuori ambito, come già indicato nella spec delle tapparelle.

## Rischi e aperture da chiudere in implementazione

- Valore esatto di `drev` per `DEV_TYPE_THERMOSTAT` da verificare contro la
  Device Library Matter alla revisione corrente (stesso rischio già accettato
  per `DEV_TYPE_WINDOW_COVERING`, non ancora chiuso da quel lavoro).
- Valori costanti per i limiti di setpoint (`AbsMin/MaxHeatSetpointLimit`,
  `AbsMin/MaxCoolSetpointLimit`) da scegliere in fase di implementazione —
  proposta di partenza 7°C–35°C, coerente con l'intervallo tipico di un
  termostato domestico, ma non vincolante.
- Come esporre esattamente lo stato dehumidifier HAP-locale accanto allo
  stato condiviso (struct wrapper vs. campo parallelo nel worker HAP) è una
  decisione di dettaglio da fissare in fase di implementazione, non cambia
  l'interfaccia pubblica di `client::thermostat`.
