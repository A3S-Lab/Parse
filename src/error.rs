use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("{0}")]
    InvalidRequest(String),
    #[error("{0}")]
    UnsupportedType(String),
    #[error("document needs OCR")]
    NeedsOcr,
    #[error("{0}")]
    Unavailable(String),
}
