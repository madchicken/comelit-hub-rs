use anyhow::Result;
use clap::Parser;
use clap_derive::Parser;
use comelit_hub_rs::{hap::start_bridge, settings::Settings};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
pub struct Params {
    #[clap(long)]
    user: String,
    #[clap(long)]
    password: String,
    #[clap(long)]
    host: Option<String>,
    #[clap(long)]
    port: Option<u16>,
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
