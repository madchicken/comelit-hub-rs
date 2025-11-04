use anyhow::Result;
use clap::Parser;
use clap_derive::Parser;
use comelit_hub_rs::hap::start_bridge;
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
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize the tracing subscriber
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive("comelit_hub_rs=info".parse()?),
        )
        .init();

    let params = Params::parse();

    start_bridge(params.user.as_str(), params.password.as_str(), params.host, params.port).await?;

    Ok(())
}
