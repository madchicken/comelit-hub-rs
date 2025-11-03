use anyhow::Result;
use clap::Parser;
use clap_derive::Parser;
use hap::start_bridge;
use tracing_subscriber::EnvFilter;

mod cli;
mod hap;
mod protocol;

#[derive(Parser, Debug)]
pub struct Params {
    #[clap(long)]
    user: Option<String>,
    #[clap(long)]
    password: Option<String>,
    #[clap(long)]
    host: Option<String>,
    #[clap(long)]
    port: Option<u16>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize the tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("comelit_hub_rs=info".parse().unwrap()),
        )
        .init();

    let params = Params::parse();

    start_bridge(params).await?;

    Ok(())
}
