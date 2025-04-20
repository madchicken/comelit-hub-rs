mod protocol;

use std::time::Duration;
use clap::Parser;
use clap_derive::Parser;
use crossterm::{event, terminal};
use crossterm::event::Event::Key;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use crate::protocol::client::{ComelitClient, ComelitClientError, ComelitOptions, ROOT_ID};

const MQTT_USER: &str = "hsrv-user";
const MQTT_PASSWORD: &str = "sf1nE9bjPc";

#[derive(Parser, Debug)]
struct Params {
    #[clap(long, default_value = "admin")]
    user: String,
    #[clap(long, default_value = "admin")]
    password: String,
    #[clap(long)]
    host: String,
    #[clap(long, default_value = "1883")]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<(), ComelitClientError> {
    // Initialize the tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env()
            .add_directive("comelit_hub_client=info".parse().unwrap()))
        .init();

    let params = Params::parse();

    let options = ComelitOptions::builder()
        .user(params.user)
        .password(params.password)
        .mqtt_user(MQTT_USER.to_string())
        .mqtt_password(MQTT_PASSWORD.to_string())
        .port(params.port)
        .host(params.host)
        .build().map_err(|e| ComelitClientError::GenericError(e.to_string()))?;
    let mut client = ComelitClient::new(options).await?;
    if let Err(e) = client.login().await {
        error!("Login failed: {}", e);
        return Err(e);
    } else {
        info!("Login successful");
    }

    println!("Press 'q' to quit, 'i' to get the hose index, '1' to subscribe to ROOT_ID, '2' to subscribe to VIP#APARTMENT, '3' to subscribe to VIP#OD#00000100.2");
    terminal::enable_raw_mode().unwrap();
    // read keyboard input
    loop {
        if event::poll(Duration::default()).unwrap() {
            if let Key(key_event) = event::read().unwrap() {
                terminal::disable_raw_mode().unwrap();
                match key_event.code {
                    event::KeyCode::Char('q') => {
                        break println!("Exiting...");
                    }
                    event::KeyCode::Char('i') => {
                        if let Ok(data) = client.fetch_index().await {
                            println!("Index {:?}", data);
                        } else {
                            error!("Info error");
                        }
                    }
                    event::KeyCode::Char('1') => {
                        if let Ok(_) = client.subscribe(ROOT_ID).await {
                            println!("Successfully subscribed to ROOT_ID");
                        } else {
                            error!("Subscribe error");
                        }
                    }
                    event::KeyCode::Char('2') => {
                        if let Ok(_) = client.subscribe("VIP#APARTMENT").await {
                            println!("Successfully subscribed to VIP#APARTMENT");
                        } else {
                            error!("Subscribe error");
                        }
                    }
                    event::KeyCode::Char('3') => {
                        if let Ok(_) = client.subscribe("VIP#OD#00000100.2").await {
                            println!("Successfully subscribed to VIP#OD#00000100.2");
                        } else {
                            error!("Subscribe error");
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
