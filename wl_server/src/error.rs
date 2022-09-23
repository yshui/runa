use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Deserialization error: {0}")]
    Deserialization(#[from] serde::de::value::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Uncategorized error: {0}")]
    Custom(#[from] anyhow::Error),
}

impl From<wl_common::Infallible> for Error {
    fn from(f: wl_common::Infallible) -> Self {
        match f {}
    }
}
