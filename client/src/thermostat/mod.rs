// client/src/thermostat/mod.rs
mod state;
mod worker;

pub use state::{TargetHeatingCoolingState, ThermostatState};
pub use worker::{ThermostatHandle, ThermostatSink, spawn_thermostat_worker};
