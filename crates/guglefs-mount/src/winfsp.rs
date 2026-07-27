use async_trait::async_trait;
use guglefs_core::{EngineError, EngineResult, MappingConfig, MountDriver};

#[derive(Debug, Default)]
pub struct SystemMountDriver;

#[async_trait]
impl MountDriver for SystemMountDriver {
    async fn mount(&self, _config: &MappingConfig) -> EngineResult<()> {
        Err(EngineError::NotImplemented("WinFsp mount driver".into()))
    }

    async fn unmount(&self, _mount_point: &str) -> EngineResult<()> {
        Err(EngineError::NotImplemented("WinFsp unmount driver".into()))
    }
}
