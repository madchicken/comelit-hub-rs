// client/src/covering/mod.rs
mod settings;
mod state;
mod worker;

pub use settings::WindowCoveringSettings;
pub use state::{FULLY_CLOSED, FULLY_OPENED, PositionState, WindowCoveringState};
pub use worker::{
    WindowCoveringConfig, WindowCoveringHandle, WindowCoveringSink, spawn_window_covering_worker,
};
