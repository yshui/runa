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
