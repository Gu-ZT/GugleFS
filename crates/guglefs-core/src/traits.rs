use async_trait::async_trait;

use crate::{EngineResult, MappingConfig};

#[async_trait]
pub trait RemoteFileSystem: Send + Sync {
    async fn connect(&self) -> EngineResult<()>;
    async fn disconnect(&self) -> EngineResult<()>;
}

#[async_trait]
pub trait MountDriver: Send + Sync {
    async fn mount(&self, config: &MappingConfig) -> EngineResult<()>;
    async fn unmount(&self, mount_point: &str) -> EngineResult<()>;
}
