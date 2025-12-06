mod cached_value;
mod comelit_accessory;
mod dehumidifier;
mod lightbulb;
mod state;
mod thermostat;
mod window_covering;

use std::sync::Arc;

use futures::lock::Mutex;
use hap::accessory::HapAccessory;

pub(crate) use lightbulb::ComelitLightbulbAccessory;
pub(crate) use window_covering::ComelitWindowCoveringAccessory;
pub(crate) type AccessoryPointer = Arc<Mutex<Box<dyn HapAccessory>>>;
pub(crate) use comelit_accessory::ComelitAccessory;
pub(crate) use dehumidifier::ComelitDehumidifierAccessory;
pub(crate) use thermostat::ComelitThermostatAccessory;
pub(crate) use window_covering::WindowCoveringConfig;
