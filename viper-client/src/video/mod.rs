//! Video stream from an external unit, reverse engineered from the official
//! Comelit app (`libvipcomelit.so`). See `session` for the call sequence.

pub mod ctp;
pub mod media;
mod session;
pub mod tunnel;
pub mod unit;

pub use ctp::VideoParams;
pub(crate) use session::run;
pub use session::{CallMode, CallTarget};
pub use session::{VideoOptions, VideoStats};
pub use unit::{MediaKind, MediaPacket, SpawnedUnit, UnitConfig, UnitEvent, ViperUnit};
