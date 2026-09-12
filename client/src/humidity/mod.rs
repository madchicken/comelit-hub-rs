// client/src/humidity/mod.rs
mod state;
mod worker;

pub use state::HumidityState;
pub use worker::{HumidityHandle, HumiditySink, spawn_humidity_worker};
