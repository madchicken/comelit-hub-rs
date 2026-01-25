use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use comelit_hub_rs::{
    ActionType, ComelitClientError, DeviceStatus, HomeDeviceData, LightDeviceData, ROOT_ID, State,
    StatusUpdate,
};
use crossterm::event::Event::Key;
use crossterm::{event, terminal};

use crate::{Params, utils::create_client};

#[derive(Default)]
struct Updater {
    index: Arc<Mutex<HashMap<String, HomeDeviceData>>>,
}

#[async_trait]
impl StatusUpdate for Updater {
    async fn status_update(&self, device: &HomeDeviceData) {
        println!("Status update: {device:?}");
        if let Ok(mut guard) = self.index.lock() {
            let device = guard.get_mut(&device.id()).unwrap();
            if let HomeDeviceData::Light(light) = device {
                light.status = light.status.clone();
                light.power_status = light.power_status.clone();
            }
        }
    }
}

impl Updater {
    pub fn get_device(&self, id: &str) -> Option<HomeDeviceData> {
        if let Ok(guard) = self.index.lock() {
            guard.get(id).cloned()
        } else {
            None
        }
    }
}

pub async fn listen(params: Params) -> Result<(), ComelitClientError> {
    let updater = Arc::new(Updater::default());
    let client = create_client(params, Some(updater.clone())).await?;
    if let Err(e) = client.login(State::Disconnected).await {
        println!("Login failed: {}", e);
        return Err(e);
    } else {
        println!("Login successful");
    }

    let index = client.fetch_index(1).await?;
    if let Ok(mut guard) = updater.index.lock() {
        for (id, device) in index.clone().into_iter() {
            guard.insert(id, device.clone());
        }
    }
    client.subscribe(ROOT_ID).await?;
    println!("Subscribed to index updates");
    let lights: Vec<LightDeviceData> = index
        .into_iter()
        .filter_map(|(_, device)| match device {
            HomeDeviceData::Light(l) => Some(l),
            _ => None,
        })
        .collect();

    println!("Press 'q' to quit");
    println!("Press 'f' to fetch the house index");
    println!("Press 'l' to list lights");
    println!("Press 'c' to send action to VIP#OD#00000100.2");
    println!("Press 'd' to send action to VIP#APARTMENT");

    terminal::enable_raw_mode().unwrap();
    // read keyboard input
    loop {
        #[allow(clippy::collapsible_if)]
        if event::poll(Duration::default()).unwrap() {
            if let Key(key_event) = event::read().unwrap() {
                terminal::disable_raw_mode().unwrap();
                match key_event.code {
                    event::KeyCode::Char('q') => {
                        break println!("Exiting...");
                    }
                    event::KeyCode::Char('f') => {
                        if let Ok(data) = client.fetch_index(1).await {
                            println!("Index {:?}", data);
                        } else {
                            println!("Fetch index error");
                        }
                    }
                    event::KeyCode::Char('l') => {
                        let lights: Vec<LightDeviceData> = updater
                            .index
                            .lock()
                            .unwrap()
                            .clone()
                            .into_iter()
                            .filter_map(|(_, device)| match device {
                                HomeDeviceData::Light(l) => Some(l),
                                _ => None,
                            })
                            .collect();
                        for (i, l) in lights.iter().enumerate() {
                            println!(
                                "{i} - Light {}, status: {:?}",
                                l.description.clone().unwrap_or_default(),
                                l.status
                            );
                        }
                    }
                    event::KeyCode::Char('c') => {
                        if client
                            .send_action("VIP#OD#00000100.2", ActionType::Set, 1)
                            .await
                            .is_ok()
                        {
                            println!("Successfully sent action to VIP#OD#00000100.2");
                        } else {
                            println!("Action error");
                        }
                    }
                    event::KeyCode::Char('d') => {
                        if client
                            .send_action("VIP#APARTMENT", ActionType::Set, 1)
                            .await
                            .is_ok()
                        {
                            println!("Successfully set action to VIP#APARTMENT");
                        } else {
                            println!("Action error");
                        }
                    }
                    event::KeyCode::Char(c) => {
                        let number = c.to_digit(10);
                        if let Some(number) = number {
                            if let Some(light) = lights.get(number as usize) {
                                if let Some(device) = updater.get_device(&light.id)
                                    && let HomeDeviceData::Light(light_data) = device
                                {
                                    let on = light_data.status.clone().unwrap_or_default()
                                        == DeviceStatus::On;
                                    println!(
                                        "Turning {} {}",
                                        light_data.description.unwrap_or_default(),
                                        if on { "on" } else { "off" }
                                    );
                                    client
                                        .toggle_device_status(light_data.id.as_str(), !on)
                                        .await?;
                                }
                            }
                        }
                    }
                    _ => {}
                }
                terminal::enable_raw_mode().unwrap();
            }
        }
    }
    terminal::disable_raw_mode().unwrap();
    client.disconnect().await?;
    Ok(())
}
