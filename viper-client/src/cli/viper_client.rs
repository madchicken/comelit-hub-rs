use clap::Parser;
use dotenvy::dotenv;
use viper_client::device::Device;
use viper_client::{ICONA_BRIDGE_PORT, ViperClient, ViperError};

#[derive(Parser, Debug)]
struct Params {
    #[clap(short, long, env = "ICONA_IP")]
    ip: Option<String>,

    #[clap(short, long, env = "ICONA_PORT")]
    port: Option<u16>,

    #[clap(short, long, env = "ICONA_TOKEN")]
    token: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), ViperError> {
    dotenv().ok();

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
            let mut client = ViperClient::new(ip.as_str(), port);
            if let Ok(token) = client.sign_up("test@gmail.com") {
                params.token = Some(token.user_token.clone());
                println!("Token is {}", token.user_token);
            } else {
                println!("Failed to sign up");
                return Ok(());
            }
        }

        println!("Connected!");
        on_connect(ip.as_str(), port, &params.token.unwrap())?;
    } else {
        println!("Device is down, please check the device status");
    }
    Ok(())
}

// This is an example run purely for testing
fn on_connect(ip: &str, port: u16, token: &str) -> Result<(), ViperError> {
    let mut client = ViperClient::new(ip, port);
    println!(
        "INFO: {}\n",
        serde_json::to_string_pretty(&client.info()?).unwrap()
    );
    println!(
        "UAUT: {:?}\n",
        serde_json::to_string_pretty(&client.authorize(token)?).unwrap()
    );
    println!(
        "UCFG: {}\n",
        serde_json::to_string_pretty(&client.configuration("all")?).unwrap()
    );
    if let Ok(params) = client.face_recognition_params() {
        println!("FCRG: {:?}\n", params);
    } else {
        println!("Failed to get face recognition parameters");
    }

    println!("Shutting down...");
    client.shutdown();

    Ok(())
}
