mod accessories;
mod bridge;
mod settings;

pub use bridge::start_bridge;

use anyhow::Result;
use clap::Parser;
use clap_derive::Parser;
use settings::Settings;
use signal_hook::{consts::SIGHUP, iterator::Signals};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
pub struct Params {
    /// User name for the Comelit Bridge (default: "admin")
    #[clap(long, default_value = "admin")]
    user: String,
    /// Password for the Comelit Bridge (default: "admin")
    #[clap(long, default_value = "admin")]
    password: String,
    /// Hostname or IP address of the Comelit Bridge (if not set, it will scan the network to find it)
    #[clap(long)]
    host: Option<String>,
    /// Port number for the Comelit Bridge (default: 1883)
    #[clap(long, default_value = "1883")]
    port: Option<u16>,
    /// Settings file path for the Comelit Bridge (if not set, it will use default settings)
    #[clap(long)]
    settings: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize the tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let params = Params::parse();

    let settings = if let Some(path) = params.settings {
        serde_json::from_str(&std::fs::read_to_string(path)?)?
    } else {
        Settings::default()
    };

    let mut signals = Signals::new([SIGHUP])?;
    std::thread::spawn(move || {
        for _ in signals.forever() {
            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::from_default_env())
                .init();
            info!("Reopening log files");
        }
    });

    start_bridge(
        params.user.as_str(),
        params.password.as_str(),
        params.host,
        params.port,
        settings,
    )
    .await?;

    Ok(())
}
