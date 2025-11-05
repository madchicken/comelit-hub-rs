mod lightbulb;

use std::sync::Arc;

use futures::lock::Mutex;
use hap::accessory::HapAccessory;

pub(crate) use lightbulb::ComelitLightbulbAccessory;
pub type AccessoryPointer = Arc<Mutex<Box<dyn HapAccessory>>>;
