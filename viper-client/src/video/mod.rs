//! Video stream from an external unit, reverse engineered from the official
//! Comelit app (`libvipcomelit.so`). See `session` for the call sequence.

pub mod ctp;
pub mod media;
mod session;
pub mod tunnel;

pub use ctp::VideoParams;
pub(crate) use session::{CallTarget, run};
pub use session::{VideoOptions, VideoStats};
