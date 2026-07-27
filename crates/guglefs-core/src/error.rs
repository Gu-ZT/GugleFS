use thiserror::Error;

pub type EngineResult<T> = Result<T, EngineError>;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("mapping not found: {0}")]
    MappingNotFound(String),
    #[error("mapping is already mounted: {0}")]
    AlreadyMounted(String),
    #[error("mapping is not mounted: {0}")]
    NotMounted(String),
    #[error("feature is not implemented yet: {0}")]
    NotImplemented(String),
    #[error("remote backend error: {0}")]
    Remote(String),
    #[error("mount driver error: {0}")]
    Mount(String),
    #[error("internal engine error: {0}")]
    Internal(String),
}
