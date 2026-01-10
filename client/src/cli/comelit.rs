mod commands;
mod utils;

use clap::Parser;
use clap_derive::{Parser, Subcommand};
use comelit_hub_rs::ComelitClientError;

use crate::commands::listen;

#[derive(Subcommand, Debug, Clone)]
enum SubCommands {
    Toggle {
        #[arg(long)]
        id: String,
        #[arg(long, default_value = "1")]
        toggle: u8,
    },
    List,
}

#[derive(Subcommand, Debug, Default, Clone)]
enum Commands {
    Scan,
    #[default]
    Listen,
    Info {
        #[arg(long)]
        id: String,
        #[arg(long, short, default_value = "1")]
        level: Option<u8>,
    },
    Lights {
        #[command(subcommand)]
        command: SubCommands,
    },
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

#[tokio::main]
async fn main() -> Result<(), ComelitClientError> {
    let params = Params::parse();

    match &params.command.clone() {
        Commands::Scan => commands::scan(params).await?,
        Commands::Listen => listen(params).await?,
        Commands::Info { id, level } => commands::get_device_info(params, id, level).await?,
        Commands::Lights { command } => match command {
            SubCommands::Toggle { id, toggle } => {
                commands::toggle_light(params, id, toggle).await?
            }
            SubCommands::List => commands::list_lights(params).await?,
        },
    }

    Ok(())
}
