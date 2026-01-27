mod accessories;
mod bridge;
mod logging;
mod settings;

use std::process::exit;

pub use bridge::start_bridge;

use anyhow::Result;
use clap::Parser;
use clap_derive::Parser;
use logging::{LogConfig, LogGuard, RotationPeriod};
use settings::Settings;
use tracing::{info, warn};

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

    // Logging options
    /// Directory for log files (if not set, logs to stdout)
    #[clap(long)]
    log_dir: Option<String>,
    /// Prefix for log file names (default: "comelit-hub")
    #[clap(long, default_value = "comelit-hub")]
    log_prefix: String,
    /// Log rotation period: minutely, hourly, daily, never (default: daily)
    #[clap(long, default_value = "daily")]
    log_rotation: String,
    /// Maximum number of log files to keep, 0 for unlimited (default: 7)
    #[clap(long, default_value = "7")]
    max_log_files: usize,
    /// Also output logs to console when file logging is enabled
    #[clap(long)]
    log_to_console: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let params = Params::parse();

    // Set up logging based on whether a log directory is provided
    let _log_guard = setup_logging(&params)?;

    let settings = if let Some(path) = params.settings {
        if let Ok(read_to_string) = std::fs::read_to_string(path) {
            serde_json::from_str(&read_to_string)?
        } else {
            warn!("Failed to read settings file, using default settings");
            Settings::default()
        }
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

    info!("Bridge ended");
    exit(0); // force exit
}

fn setup_logging(params: &Params) -> Result<LogGuard> {
    match &params.log_dir {
        Some(log_dir) => {
            // Parse rotation period
            let rotation: RotationPeriod = params
                .log_rotation
                .parse()
                .map_err(|e: String| anyhow::anyhow!(e))?;

            let config = LogConfig {
                log_dir: log_dir.clone(),
                log_prefix: params.log_prefix.clone(),
                rotation,
                max_log_files: params.max_log_files,
            };

            // Create the log directory if it doesn't exist
            std::fs::create_dir_all(log_dir)?;

            if params.log_to_console {
                Ok(logging::setup_dual_logging(config)?)
            } else {
                Ok(logging::setup_file_logging(config)?)
            }
        }
        None => {
            // No log directory specified, log to console only
            Ok(logging::setup_console_logging())
        }
    }
}
