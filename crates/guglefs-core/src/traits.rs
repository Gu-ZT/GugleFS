use async_trait::async_trait;

use crate::{EngineError, EngineResult, MappingConfig};

#[derive(Default)]
pub struct ConnectionSecrets {
    pub credential: Option<String>,
    pub private_key: Option<String>,
    pub totp_code: Option<String>,
}

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
    pub created: Option<u64>,
    pub accessed: Option<u64>,
    pub modified: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileSystemSpace {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub total_files: Option<u64>,
    pub available_files: Option<u64>,
    pub block_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    pub path: String,
    pub name: String,
    pub metadata: FileMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryPage {
    pub entries: Vec<DirectoryEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileTimes {
    pub accessed: Option<u64>,
    pub modified: Option<u64>,
}

#[async_trait]
pub trait RemoteFileSystem: Send + Sync {
    async fn connect(&self) -> EngineResult<()>;
    async fn disconnect(&self) -> EngineResult<()>;

    async fn metadata(&self, _path: &str) -> EngineResult<FileMetadata> {
        Err(EngineError::NotImplemented("remote metadata".into()))
    }

    async fn filesystem_space(&self, _path: &str) -> EngineResult<Option<FileSystemSpace>> {
        Ok(None)
    }

    async fn read_dir(&self, _path: &str) -> EngineResult<Vec<DirectoryEntry>> {
        Err(EngineError::NotImplemented(
            "remote directory listing".into(),
        ))
    }

    async fn read_dir_page(
        &self,
        path: &str,
        cursor: Option<&str>,
        _max_entries: usize,
    ) -> EngineResult<DirectoryPage> {
        if cursor.is_some() {
            return Err(EngineError::InvalidConfig(
                "remote directory cursor is no longer valid".into(),
            ));
        }
        Ok(DirectoryPage {
            entries: self.read_dir(path).await?,
            next_cursor: None,
        })
    }

    async fn close_dir_cursor(&self, _cursor: &str) -> EngineResult<()> {
        Ok(())
    }

    async fn read_range(&self, _path: &str, _offset: u64, _length: u64) -> EngineResult<Vec<u8>> {
        Err(EngineError::NotImplemented("remote read".into()))
    }

    async fn write(&self, _path: &str, _offset: u64, _data: Vec<u8>) -> EngineResult<u64> {
        Err(EngineError::NotImplemented("remote write".into()))
    }

    async fn create_file(&self, path: &str) -> EngineResult<()> {
        self.write(path, 0, Vec::new()).await.map(|_| ())
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

    async fn truncate(&self, path: &str, size: u64) -> EngineResult<()> {
        if size == 0 {
            self.write(path, 0, Vec::new()).await.map(|_| ())
        } else {
            Err(EngineError::NotImplemented("remote truncate".into()))
        }
    }

    async fn set_times(&self, _path: &str, _times: FileTimes) -> EngineResult<()> {
        Err(EngineError::NotImplemented("remote timestamps".into()))
    }

    async fn flush(&self, _path: &str) -> EngineResult<()> {
        Ok(())
    }
}

#[async_trait]
pub trait MountDriver: Send + Sync {
    async fn mount(&self, config: &MappingConfig, secrets: ConnectionSecrets) -> EngineResult<()>;
    async fn unmount(&self, mount_point: &str) -> EngineResult<()>;
}
