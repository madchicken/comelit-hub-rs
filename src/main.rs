mod protocol;

use std::time::Duration;
use clap::Parser;
use clap_derive::Parser;
use crossterm::{event, terminal};
use crossterm::event::Event::Key;
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

    println!("Press 'q' to quit, 'l' to login, 'i' to get info");
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
                    event::KeyCode::Char('l') => {
                        if let Err(e) = client.login().await {
                            println!("Login failed: {}", e);
                        } else {
                            println!("Login successful");
                        }
                    }
                    event::KeyCode::Char('i') => {
                        if let Err(e) = client.info(ROOT_ID, 1).await {
                            println!("Info failed: {}", e);
                        } else {
                            println!("Info successful");
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
