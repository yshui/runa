use thiserror::Error;
use runa_wayland_protocols::wayland::wl_display::v1 as wl_display;

use crate::objects::DISPLAY_ID;

/// Converting Rust errors to Wayland error events
pub trait ProtocolError: std::error::Error + Send + Sync + 'static {
    /// Returns an object id and a wayland error code associated with this
    /// error, in that order. If not None, an error event will be sent to
    /// the client.
    fn wayland_error(&self) -> Option<(u32, u32)>;
    /// Returns whether this error is fatal. If true, the client should be
    /// disconnected.
    fn fatal(&self) -> bool;
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Deserialization error: {0}")]
    Deserialization(#[from] runa_io::traits::de::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("NewId {0} sent by the client is already in use")]
    IdExists(u32),
    #[error("Object {0} is invalid for this operation")]
    InvalidObject(u32),
    #[error("Unknown Global: {0}")]
    UnknownGlobal(u32),
    #[error("Unknown object: {0}")]
    UnknownObject(u32),
    #[error("An unknown error occurred: {0}")]
    UnknownError(&'static str),
    #[error("An unknown fatal error occurred: {0}")]
    UnknownFatalError(&'static str),
    #[error("{source}")]
    Custom {
        #[source]
        source:             Box<dyn std::error::Error + Send + Sync + 'static>,
        object_id_and_code: Option<(u32, u32)>,
        fatal:              bool,
    },
}

impl From<std::convert::Infallible> for Error {
    fn from(f: std::convert::Infallible) -> Self {
        match f {}
    }
}

impl From<Box<dyn std::error::Error + Send + Sync + 'static>> for Error {
    fn from(e: Box<dyn std::error::Error + Send + Sync + 'static>) -> Self {
        Self::Custom {
            object_id_and_code: Some((DISPLAY_ID, wl_display::enums::Error::Implementation as u32)),
            fatal:              true,
            source:             e,
        }
    }
}

impl Error {
    pub fn custom<E>(e: E) -> Self
    where
        E: ProtocolError + Send + Sync + 'static,
    {
        Self::Custom {
            object_id_and_code: e.wayland_error(),
            fatal:              e.fatal(),
            source:             Box::new(e),
        }
    }
}
impl ProtocolError for Error {
    fn wayland_error(&self) -> Option<(u32, u32)> {
        use runa_wayland_protocols::wayland::wl_display::v1::enums::Error::*;
        match self {
            Self::Deserialization(_) => Some((DISPLAY_ID, InvalidMethod as u32)),
            Self::IdExists(_) => Some((DISPLAY_ID, InvalidObject as u32)),
            Self::UnknownGlobal(_) => Some((DISPLAY_ID, InvalidObject as u32)),
            Self::UnknownObject(_) => Some((DISPLAY_ID, InvalidObject as u32)),
            Self::InvalidObject(_) => Some((DISPLAY_ID, InvalidObject as u32)),
            Self::UnknownError(_) => Some((DISPLAY_ID, Implementation as u32)),
            Self::UnknownFatalError(_) => Some((DISPLAY_ID, Implementation as u32)),
            Self::Custom {
                object_id_and_code, ..
            } => *object_id_and_code,
            Self::Io(_) => None, /* We had an I/O error communicating with the client, so we
                                  * better not send anything further */
        }
    }

    fn fatal(&self) -> bool {
        match self {
            Self::Deserialization(_) |
            Self::Io(_) |
            Self::IdExists(_) |
            Self::UnknownGlobal(_) |
            Self::UnknownObject(_) |
            Self::InvalidObject(_) |
            Self::UnknownFatalError(_) => true,
            Self::UnknownError(_) => false,
            Self::Custom { fatal, .. } => *fatal,
        }
    }
}
