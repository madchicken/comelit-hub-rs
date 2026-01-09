mod accessories;
mod bridge;
mod logging;
mod settings;

pub use bridge::start_bridge;

use anyhow::Result;
use clap::Parser;
use clap_derive::Parser;
use logging::{ReopenableFile, ReopenableFileHandle, setup_sighup_handler};
use settings::Settings;
use tracing::warn;
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
    /// Log file path (if not set, logs to stdout)
    #[clap(long)]
    log_file: Option<String>,
    /// Error log file path (if not set, errors go to stderr)
    #[clap(long)]
    error_log_file: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let params = Params::parse();

    // Set up logging based on whether file paths are provided
    let (log_handle, err_handle) = setup_logging(&params)?;

    // Set up SIGHUP handler for log rotation
    let _signal_thread = setup_sighup_handler(log_handle, err_handle);

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

    Ok(())
}

fn setup_logging(
    params: &Params,
) -> Result<(Option<ReopenableFileHandle>, Option<ReopenableFileHandle>)> {
    match (&params.log_file, &params.error_log_file) {
        (Some(log_path), Some(err_path)) => {
            // Both log and error files specified
            let log_file = ReopenableFile::new(log_path)?;
            let err_file = ReopenableFile::new(err_path)?;
            let log_handle = ReopenableFileHandle::new(log_file);
            let err_handle = ReopenableFileHandle::new(err_file);

            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::from_default_env())
                .with_writer(log_handle.clone())
                .with_ansi(false)
                .init();

            Ok((Some(log_handle), Some(err_handle)))
        }
        (Some(log_path), None) => {
            // Only log file specified, errors also go to log file
            let log_file = ReopenableFile::new(log_path)?;
            let log_handle = ReopenableFileHandle::new(log_file);

            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::from_default_env())
                .with_writer(log_handle.clone())
                .with_ansi(false)
                .init();

            Ok((Some(log_handle), None))
        }
        (None, _) => {
            // No log file specified, log to stdout/stderr
            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::from_default_env())
                .init();

            Ok((None, None))
        }
    }
}
