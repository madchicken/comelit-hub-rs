use comelit_hub_rs::{ComelitClientError, DeviceStatus, HomeDeviceData, State};

use crate::{Params, utils::create_client};

pub async fn toggle_light(params: Params, id: &str, toggle: &u8) -> Result<(), ComelitClientError> {
    let client = create_client(params, None).await?;
    if let Err(e) = client.login(State::Disconnected).await {
        println!("Login failed: {}", e);
        return Err(e);
    } else {
        println!("Login successful");
    }
    client.toggle_device_status(id, *toggle > 0).await?;
    println!("Device {} status toggled", id);
    Ok(())
}

pub async fn list_lights(params: Params) -> Result<(), ComelitClientError> {
    let client = create_client(params, None).await?;
    if let Err(e) = client.login(State::Disconnected).await {
        println!("Login failed: {}", e);
        return Err(e);
    } else {
        println!("Login successful");
    }
    let devices = client.fetch_index(1).await?;
    for (id, device_data) in devices {
        if let HomeDeviceData::Light(light) = device_data {
            println!(
                "Light '{}' ({}) status: {}",
                light.description.unwrap_or("Unknown".to_string()),
                id,
                if light.status.unwrap_or_default() == DeviceStatus::On {
                    "on"
                } else {
                    "off"
                }
            );
        }
    }
    Ok(())
}
