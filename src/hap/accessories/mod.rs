mod lightbulb;
mod window_covering;

use std::sync::Arc;

use futures::lock::Mutex;
use hap::accessory::HapAccessory;

pub(crate) use lightbulb::ComelitLightbulbAccessory;
pub(crate) use window_covering::ComelitWindowCoveringAccessory;
pub type AccessoryPointer = Arc<Mutex<Box<dyn HapAccessory>>>;
