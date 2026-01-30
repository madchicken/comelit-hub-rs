use comelit_client_rs::{ComelitClientError, State};
use serde_json::Value;

use crate::{Params, utils::create_client};

pub async fn get_device_info(
    params: Params,
    id: &str,
    level: &Option<u8>,
) -> Result<(), ComelitClientError> {
    let client = create_client(params, None).await?;
    if let Err(e) = client.login(State::Disconnected).await {
        println!("Login failed: {}", e);
        return Err(e);
    } else {
        println!("Login successful");
    }
    let info = client.info::<Value>(id, level.unwrap_or(1)).await?;
    println!(
        "Device info: {}",
        serde_json::to_string_pretty(&info).unwrap()
    );
    Ok(())
}
