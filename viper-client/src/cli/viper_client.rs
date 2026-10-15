use clap::{Parser, Subcommand};
use dotenvy::dotenv;
use std::time::Duration;
use viper_client::device::Device;
use viper_client::video::{VideoOptions, VideoParams};
use viper_client::{ICONA_BRIDGE_PORT, ViperClient, ViperError};

#[derive(Debug, Subcommand)]
enum Command {
    Info,
    OpenDoor {
        #[arg(long, default_value = None)]
        door_name: String,
    },
    OpenActuator {
        #[arg(long, default_value = None)]
        actuator_name: String,
    },
    /// Watch an external unit and record its stream until Ctrl-C or --duration
    StartVideo {
        /// Entrance or door name from the address books (see `info`)
        #[arg(long)]
        target: String,
        /// Output path without extension (.h264 / .m4v for video, -audio.alaw for audio)
        #[arg(long, default_value = "doorbell")]
        output: String,
        /// Seconds to record
        #[arg(long, default_value_t = 30)]
        duration: u64,
        /// Also record the external unit audio
        #[arg(long)]
        audio: bool,
    },
}

#[derive(Parser, Debug)]
struct Params {
    #[clap(short, long, env = "ICONA_IP")]
    ip: Option<String>,

    #[clap(short, long, env = "ICONA_PORT")]
    port: Option<u16>,

    #[clap(short, long, env = "ICONA_TOKEN")]
    token: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[tokio::main]
async fn main() -> Result<(), ViperError> {
    dotenv().ok();

    tracing_subscriber::fmt::init();

    let mut params = Params::parse();
    if params.ip.is_none() {
        if let Some((ip, port)) = ViperClient::scan().await {
            params.ip = Some(ip);
            params.port = Some(port);
        } else {
            println!("No device found");
            return Ok(());
        }
    }

    let ip = params.ip.unwrap();
    let port = params.port.unwrap_or(ICONA_BRIDGE_PORT);
    let is_up = Device::poll(ip.as_str(), port);
    if is_up {
        println!("Device is up");
        if params.token.is_none() {
            println!("Token is not provided, creating a new user");
            let mut client = ViperClient::new(ip.as_str(), port).await;
            if let Ok(token) = client.sign_up("test@gmail.com") {
                params.token = Some(token.user_token.clone());
                println!("Token is {}", token.user_token);
            } else {
                println!("Failed to sign up");
                return Ok(());
            }
        }

        println!("Connected!");
        match params.command {
            Command::Info => {
                on_connect(ip.as_str(), port, &params.token.unwrap()).await?;
            }
            Command::OpenDoor { door_name } => {
                let mut client = ViperClient::new(ip.as_str(), port).await;
                client.authorize(params.token.unwrap().as_str())?;
                let vip_reponse = client.configuration("all")?;
                println!("Opening door {door_name}");
                client.open_door(&vip_reponse.vip, door_name.as_str())?;
                client.shutdown();
            }
            Command::OpenActuator { actuator_name } => {
                let mut client = ViperClient::new(ip.as_str(), port).await;
                client.authorize(params.token.unwrap().as_str())?;
                let vip_reponse = client.configuration("all")?;
                println!("Opening actuator {actuator_name}");
                client.open_actuator(&vip_reponse.vip, actuator_name.as_str())?;
                client.shutdown();
            }
            Command::StartVideo {
                target,
                output,
                duration,
                audio,
            } => {
                let mut client = ViperClient::new(ip.as_str(), port).await;
                client.authorize(params.token.unwrap().as_str())?;
                let vip_reponse = client.configuration("all")?;
                println!("Starting video from {target}, press Ctrl-C to stop");
                let options = VideoOptions {
                    output: output.into(),
                    duration: Duration::from_secs(duration),
                    audio,
                    video: VideoParams::default(),
                };
                let stop = async {
                    let _ = tokio::signal::ctrl_c().await;
                };
                let stats = client
                    .start_video(&vip_reponse.vip, &target, options, stop)
                    .await?;
                println!("{stats:#?}");
                client.shutdown();
            }
        }
    } else {
        println!("Device is down, please check the device status");
    }
    Ok(())
}

// This is an example run purely for testing
async fn on_connect(ip: &str, port: u16, token: &str) -> Result<(), ViperError> {
    let mut client = ViperClient::new(ip, port).await;
    println!(
        "INFO: {}\n",
        serde_json::to_string_pretty(&client.info()?).unwrap()
    );
    println!(
        "UAUT: {:?}\n",
        serde_json::to_string_pretty(&client.authorize(token)?).unwrap()
    );
    let cfg = client.configuration("all")?;
    println!("UCFG: {}\n", serde_json::to_string_pretty(&cfg).unwrap());
    if let Ok(params) = client.face_recognition_params() {
        println!("FCRG: {:?}\n", params);
    } else {
        println!("Failed to get face recognition parameters");
    }

    let door_address = cfg
        .vip
        .user_parameters
        .opendoor_address_book
        .first()
        .unwrap();

    client.open_door(&cfg.vip, door_address.name.as_str())?;

    println!("Shutting down...");
    client.shutdown();

    Ok(())
}
