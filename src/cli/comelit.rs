use async_trait::async_trait;
use clap::Parser;
use clap_derive::{Parser, Subcommand};
use comelit_hub_rs::protocol::client::{
    ComelitClient, ComelitClientError, ComelitOptions, ROOT_ID, State, StatusUpdate,
};
use comelit_hub_rs::protocol::credentials::get_secrets;
use comelit_hub_rs::protocol::out_data_messages::{ActionType, HomeDeviceData};
use comelit_hub_rs::protocol::scanner::Scanner;
use crossterm::event::Event::Key;
use crossterm::{event, terminal};
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, error, info};
use tracing_subscriber::EnvFilter;

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

#[async_trait]
impl StatusUpdate for Updater {
    async fn status_update(&self, device: &HomeDeviceData) {
        println!("Status update: {device:?}");
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
        .build()
        .map_err(|e| ComelitClientError::Generic(e.to_string()))?;
    let mut client = ComelitClient::new(options, Arc::new(Updater)).await?;
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
                            debug!("Index {:?}", data);
                        } else {
                            error!("Fetch index error");
                        }
                    }
                    event::KeyCode::Char('i') => {
                        if client.info(ROOT_ID, 2).await.is_ok() {
                            println!("Info received");
                        } else {
                            error!("Info error");
                        }
                    }
                    event::KeyCode::Char('1') => {
                        if client.subscribe(ROOT_ID).await.is_ok() {
                            println!("Successfully subscribed to ROOT_ID");
                        } else {
                            error!("Subscribe error");
                        }
                    }
                    event::KeyCode::Char('2') => {
                        if client.subscribe("VIP#APARTMENT").await.is_ok() {
                            println!("Successfully subscribed to VIP#APARTMENT");
                        } else {
                            error!("Subscribe error");
                        }
                    }
                    event::KeyCode::Char('3') => {
                        if client.subscribe("VIP#OD#00000100.2").await.is_ok() {
                            println!("Successfully subscribed to VIP#OD#00000100.2");
                        } else {
                            error!("Subscribe error");
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
                            error!("Action error");
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
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("comelit_hub_rs=info".parse().unwrap()),
        )
        .init();

    let params = Params::parse();

    match &params.command {
        Commands::Scan => {
            if let Some(host) = params.host {
                let hub = Scanner::scan_address(host.as_str())
                    .await
                    .map_err(|e| ComelitClientError::Scanner(e.to_string()))?;
                if let Some(hub) = hub {
                    info!("Found hub: {:?}", hub);
                } else {
                    info!("No hub found at {}", host);
                }
            } else {
                let hubs = Scanner::scan()
                    .await
                    .map_err(|e| ComelitClientError::Scanner(e.to_string()))?;
                for hub in hubs {
                    info!("Found hub: {:?}", hub);
                }
            }
        }
        Commands::Listen => listen(params).await?,
    }

    Ok(())
}
