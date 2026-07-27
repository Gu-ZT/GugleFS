use async_trait::async_trait;
use guglefs_core::{EngineError, EngineResult, RemoteFileSystem};

#[derive(Debug, Default)]
pub struct FtpFileSystem;

#[async_trait]
impl RemoteFileSystem for FtpFileSystem {
    async fn connect(&self) -> EngineResult<()> {
        Err(EngineError::NotImplemented("FTP adapter".into()))
    }

    async fn disconnect(&self) -> EngineResult<()> {
        Ok(())
    }
}
