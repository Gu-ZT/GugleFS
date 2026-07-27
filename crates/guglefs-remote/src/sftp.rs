use async_trait::async_trait;
use guglefs_core::{EngineError, EngineResult, RemoteFileSystem};

#[derive(Debug, Default)]
pub struct SftpFileSystem;

#[async_trait]
impl RemoteFileSystem for SftpFileSystem {
    async fn connect(&self) -> EngineResult<()> {
        Err(EngineError::NotImplemented("SFTP adapter".into()))
    }

    async fn disconnect(&self) -> EngineResult<()> {
        Ok(())
    }
}
