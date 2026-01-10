use std::time::Duration;

use comelit_hub_rs::{ComelitClientError, Scanner};

use crate::Params;

pub async fn scan(params: Params) -> Result<(), ComelitClientError> {
    if let Some(host) = params.host {
        let hub = Scanner::scan_address(host.as_str(), Some(Duration::from_secs(5)))
            .await
            .map_err(|e| ComelitClientError::Scanner(e.to_string()))?;
        if let Some(hub) = hub {
            println!("Found hub: {:?}", hub);
        } else {
            println!("No hub found at {}", host);
        }
    } else {
        let hubs = Scanner::scan(Some(Duration::from_secs(5)))
            .await
            .map_err(|e| ComelitClientError::Scanner(e.to_string()))?;
        for hub in hubs {
            println!("Found hub: {:?}", hub);
        }
    }
    Ok(())
}
