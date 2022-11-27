use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Deserialization error: {0}")]
    Deserialization(#[from] wl_io::traits::de::Error),
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

impl Error {
    pub fn custom<E>(e: E) -> Self
    where
        E: wl_protocol::ProtocolError + Send + Sync + 'static,
    {
        Self::Custom {
            object_id_and_code: e.wayland_error(),
            fatal:              e.fatal(),
            source:             Box::new(e),
        }
    }
}
impl wl_protocol::ProtocolError for Error {
    fn wayland_error(&self) -> Option<(u32, u32)> {
        use wl_protocol::wayland::wl_display::v1::enums::Error::*;
        match self {
            Self::Deserialization(_) => Some((1, InvalidMethod as u32)),
            Self::IdExists(_) => Some((1, InvalidObject as u32)),
            Self::UnknownGlobal(_) => Some((1, InvalidObject as u32)),
            Self::UnknownObject(_) => Some((1, InvalidObject as u32)),
            Self::InvalidObject(_) => Some((1, InvalidObject as u32)),
            Self::UnknownError(_) => Some((1, Implementation as u32)),
            Self::UnknownFatalError(_) => Some((1, Implementation as u32)),
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
