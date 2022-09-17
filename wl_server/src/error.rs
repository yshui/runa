use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Deserialization error: {0}")]
    Deserialization(#[from] serde::de::value::Error),
}
