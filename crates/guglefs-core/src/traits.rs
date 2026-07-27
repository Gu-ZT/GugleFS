use async_trait::async_trait;

use crate::{EngineError, EngineResult, MappingConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntryKind {
    #[default]
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMetadata {
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub path: String,
    pub name: String,
    pub metadata: FileMetadata,
}

#[async_trait]
pub trait RemoteFileSystem: Send + Sync {
    async fn connect(&self) -> EngineResult<()>;
    async fn disconnect(&self) -> EngineResult<()>;

    async fn metadata(&self, _path: &str) -> EngineResult<FileMetadata> {
        Err(EngineError::NotImplemented("remote metadata".into()))
    }

    async fn read_dir(&self, _path: &str) -> EngineResult<Vec<DirectoryEntry>> {
        Err(EngineError::NotImplemented(
            "remote directory listing".into(),
        ))
    }

    async fn read_range(&self, _path: &str, _offset: u64, _length: u64) -> EngineResult<Vec<u8>> {
        Err(EngineError::NotImplemented("remote read".into()))
    }

    async fn write(&self, _path: &str, _offset: u64, _data: Vec<u8>) -> EngineResult<u64> {
        Err(EngineError::NotImplemented("remote write".into()))
    }

    async fn create_dir(&self, _path: &str) -> EngineResult<()> {
        Err(EngineError::NotImplemented(
            "remote directory creation".into(),
        ))
    }

    async fn remove(&self, _path: &str, _directory: bool) -> EngineResult<()> {
        Err(EngineError::NotImplemented("remote remove".into()))
    }

    async fn rename(&self, _from: &str, _to: &str) -> EngineResult<()> {
        Err(EngineError::NotImplemented("remote rename".into()))
    }
}

#[async_trait]
pub trait MountDriver: Send + Sync {
    async fn mount(&self, config: &MappingConfig) -> EngineResult<()>;
    async fn unmount(&self, mount_point: &str) -> EngineResult<()>;
}
