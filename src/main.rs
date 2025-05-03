mod protocol;
mod js;

use std::time::Duration;
use clap::Parser;
use clap_derive::{Parser, Subcommand};
use crossterm::{event, terminal};
use crossterm::event::Event::Key;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use crate::protocol::client::{ComelitClient, ComelitClientError, ComelitOptions, State, StatusUpdate, ROOT_ID};
use crate::protocol::credentials::get_secrets;
use crate::protocol::out_data_messages::{ActionType, HomeDeviceData};
use crate::protocol::scanner::Scanner;

#[derive(Subcommand, Debug, Default)]
enum Commands {
    Scan,
    #[default]
    Listen,
}

#[derive(Parser, Debug)]
struct Params {
    #[clap(long)]
    user: Option<String>,
    #[clap(long)]
    password: Option<String>,
    #[clap(long)]
    host: Option<String>,
    #[clap(long)]
    port: Option<u16>,

    #[command(subcommand)]
    command: Commands,
}

struct Updater;

impl StatusUpdate for Updater {
    fn status_update(&self, device: &HomeDeviceData) {
        println!("Status update: {:?}", device);
    }
}

async fn listen(params: Params) -> Result<(), ComelitClientError> {
    let (mqtt_user, mqtt_password) = get_secrets();
    let options = ComelitOptions::builder()
        .user(params.user)
        .password(params.password)
        .mqtt_user(mqtt_user)
        .mqtt_password(mqtt_password)
        .port(params.port)
        .host(params.host)
        .build().map_err(|e| ComelitClientError::GenericError(e.to_string()))?;
    let mut client = ComelitClient::new(options, Box::new(Updater)).await?;
    if let Err(e) = client.login(State::Disconnected).await {
        error!("Login failed: {}", e);
        return Err(e);
    } else {
        info!("Login successful");
    }

    println!("Press 'q' to quit");
    println!("Press 'f' to fetch the house index");
    println!("Press 'i' to fetch the info about ROOT_ID");
    println!("Press '1' to subscribe to ROOT_ID");
    println!("Press '2' to subscribe to VIP#APARTMENT");
    println!("Press '3' to subscribe to VIP#OD#00000100.2");
    println!("Press 'c' to send action to VIP#OD#00000100.2");
    println!("Press 'd' to send action to VIP#APARTMENT");

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
                    event::KeyCode::Char('f') => {
                        if let Ok(data) = client.fetch_index().await {
                            println!("Index {:?}", data);
                        } else {
                            error!("Fetch index error");
                        }
                    }
                    event::KeyCode::Char('i') => {
                        if let Ok(_) = client.info(ROOT_ID, 2).await {
                            println!("Info received");
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
                    event::KeyCode::Char('c') => {
                        if let Ok(_) = client.send_action("VIP#OD#00000100.2", ActionType::Set, 1).await {
                            println!("Successfully sent action to VIP#OD#00000100.2");
                        } else {
                            error!("Action error");
                        }
                    }
                    event::KeyCode::Char('d') => {
                        if let Ok(_) = client.send_action("VIP#APARTMENT", ActionType::Set, 1).await {
                            println!("Successfully set action to VIP#APARTMENT");
                        } else {
                            error!("Action error");
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
#[tokio::main]
async fn main() -> Result<(), ComelitClientError> {
    // Initialize the tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env()
            .add_directive("comelit_hub_rs=info".parse().unwrap()))
        .init();

    let params = Params::parse();

    match &params.command {
        Commands::Scan => {
            if let Some(host) = params.host {
                let hub = Scanner::scan_address(host.as_str()).await.map_err(|e| ComelitClientError::ScannerError(e.to_string()))?;
                if let Some(hub) = hub {
                    println!("Found hub: {:?}", hub);
                } else {
                    println!("No hub found at {}", host);
                }
            } else {
                let hubs = Scanner::scan().await.map_err(|e| ComelitClientError::ScannerError(e.to_string()))?;
                for hub in hubs {
                    println!("Found hub: {:?}", hub);
                }
            }
        }
        Commands::Listen => listen(params).await?,
    }


    Ok(())
}
