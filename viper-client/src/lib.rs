mod channel;
mod client;
pub mod command;
pub mod command_response;
mod ctpp_channel;
pub mod device;
mod helper;
mod rtsp_channel;
mod stream_wrapper;

pub use client::{ICONA_BRIDGE_PORT, ViperClient};

#[cfg(test)]
mod test_helper;

use std::{fmt, fmt::Display, io};

type JSONResult<T> = Result<T, ViperError>;

#[derive(Debug)]
pub enum ViperError {
    IOError(io::Error),
    JSONError(serde_json::Error),
    Generic(String),
}

impl Display for ViperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ViperError::IOError(io_error) => write!(f, "{}", io_error),
            ViperError::JSONError(json_error) => write!(f, "{}", json_error),
            ViperError::Generic(generic_error) => write!(f, "{}", generic_error),
        }
    }
}

impl From<io::Error> for ViperError {
    fn from(error: io::Error) -> Self {
        ViperError::IOError(error)
    }
}
