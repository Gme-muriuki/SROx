use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("http parse error: {0}")]
    HttpParse(#[from] httparse::Error),

    #[error("invalid version {0}")]
    InvalidHttpVersion(String),

    #[error("ambiguous framing")]
    AmbiguousFraming,

    #[error("request too large")]
    RequestTooLarge,

    #[error("invalid request")]
    InvalidRequest,
}
