mod lightbulb;
mod window_covering;
mod comelit_accessory;
mod cached_value;

use std::sync::Arc;

use futures::lock::Mutex;
use hap::accessory::HapAccessory;

pub(crate) use lightbulb::ComelitLightbulbAccessory;
pub(crate) use window_covering::ComelitWindowCoveringAccessory;
pub type AccessoryPointer = Arc<Mutex<Box<dyn HapAccessory>>>;
pub use comelit_accessory::ComelitAccessory;
pub use window_covering::WindowCoveringConfig;