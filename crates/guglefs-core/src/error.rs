use thiserror::Error;

pub type EngineResult<T> = Result<T, EngineError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsErrorCode {
    InvalidArgument,
    NotFound,
    AlreadyExists,
    PermissionDenied,
    NotDirectory,
    IsDirectory,
    InvalidHandle,
    Busy,
    AlreadyMounted,
    NotMounted,
    Unsupported,
    RemoteIo,
    MountIo,
    Internal,
}

impl FsErrorCode {
    pub const fn posix_errno(self) -> i32 {
        match self {
            Self::InvalidArgument => 22,
            Self::NotFound => 2,
            Self::AlreadyExists => 17,
            Self::PermissionDenied => 13,
            Self::NotDirectory => 20,
            Self::IsDirectory => 21,
            Self::InvalidHandle => 9,
            Self::Busy | Self::AlreadyMounted => 16,
            Self::NotMounted => 19,
            Self::Unsupported => {
                #[cfg(target_os = "macos")]
                {
                    45
                }
                #[cfg(not(target_os = "macos"))]
                {
                    95
                }
            }
            Self::RemoteIo | Self::MountIo | Self::Internal => 5,
        }
    }

    pub const fn windows_error_code(self) -> u32 {
        match self {
            Self::InvalidArgument => 87,
            Self::NotFound => 2,
            Self::AlreadyExists => 80,
            Self::PermissionDenied => 5,
            Self::NotDirectory => 267,
            Self::IsDirectory => 87,
            Self::InvalidHandle => 6,
            Self::Busy | Self::AlreadyMounted => 170,
            Self::NotMounted => 21,
            Self::Unsupported => 50,
            Self::RemoteIo | Self::MountIo => 1117,
            Self::Internal => 1359,
        }
    }
}

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
    #[error("filesystem error ({code:?}): {message}")]
    Filesystem { code: FsErrorCode, message: String },
}

impl EngineError {
    pub fn filesystem(code: FsErrorCode, message: impl Into<String>) -> Self {
        Self::Filesystem {
            code,
            message: message.into(),
        }
    }

    pub const fn code(&self) -> FsErrorCode {
        match self {
            Self::InvalidConfig(_) => FsErrorCode::InvalidArgument,
            Self::MappingNotFound(_) => FsErrorCode::NotFound,
            Self::AlreadyMounted(_) => FsErrorCode::AlreadyMounted,
            Self::NotMounted(_) => FsErrorCode::NotMounted,
            Self::NotImplemented(_) => FsErrorCode::Unsupported,
            Self::Remote(_) => FsErrorCode::RemoteIo,
            Self::Mount(_) => FsErrorCode::MountIo,
            Self::Internal(_) => FsErrorCode::Internal,
            Self::Filesystem { code, .. } => *code,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filesystem_codes_have_platform_mappings() {
        assert_eq!(FsErrorCode::NotFound.posix_errno(), 2);
        assert_eq!(FsErrorCode::PermissionDenied.windows_error_code(), 5);
        assert_eq!(
            EngineError::filesystem(FsErrorCode::Busy, "locked").code(),
            FsErrorCode::Busy
        );
    }
}
