use async_trait::async_trait;
use guglefs_core::{EngineError, EngineResult, RemoteFileSystem};

/// HTTPS WebDAV backend. Plain HTTP is intentionally outside the initial scope.
#[derive(Debug, Default)]
pub struct WebDavFileSystem;

#[async_trait]
impl RemoteFileSystem for WebDavFileSystem {
    async fn connect(&self) -> EngineResult<()> {
        Err(EngineError::NotImplemented("WebDAV adapter".into()))
    }

    async fn disconnect(&self) -> EngineResult<()> {
        Ok(())
    }
}
