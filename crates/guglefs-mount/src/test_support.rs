use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use guglefs_core::{
    DirectoryEntry, EngineError, EngineResult, EntryKind, FileMetadata, FsErrorCode,
    RemoteFileSystem, RemoteVfs,
};

#[derive(Debug, Clone)]
struct MemoryNode {
    kind: EntryKind,
    data: Vec<u8>,
}

#[derive(Debug)]
struct MemoryRemote {
    nodes: RwLock<BTreeMap<String, MemoryNode>>,
}

impl Default for MemoryRemote {
    fn default() -> Self {
        Self {
            nodes: RwLock::new(BTreeMap::from([(
                "/".to_string(),
                MemoryNode {
                    kind: EntryKind::Directory,
                    data: Vec::new(),
                },
            )])),
        }
    }
}

impl MemoryRemote {
    fn metadata_for(node: &MemoryNode) -> FileMetadata {
        FileMetadata {
            kind: node.kind,
            size: node.data.len() as u64,
            modified: None,
        }
    }

    fn parent(path: &str) -> Option<&str> {
        let (parent, name) = path.rsplit_once('/')?;
        (!name.is_empty()).then_some(if parent.is_empty() { "/" } else { parent })
    }

    fn ensure_parent(nodes: &BTreeMap<String, MemoryNode>, path: &str) -> EngineResult<()> {
        let parent = Self::parent(path).ok_or_else(|| invalid_argument(path))?;
        match nodes.get(parent) {
            Some(node) if node.kind == EntryKind::Directory => Ok(()),
            Some(_) => Err(filesystem_error(FsErrorCode::NotDirectory, parent)),
            None => Err(not_found(parent)),
        }
    }

    fn create(&self, path: &str, kind: EntryKind) -> EngineResult<()> {
        let mut nodes = write_nodes(&self.nodes)?;
        Self::ensure_parent(&nodes, path)?;
        if nodes.contains_key(path) {
            return Err(filesystem_error(FsErrorCode::AlreadyExists, path));
        }
        nodes.insert(
            path.to_string(),
            MemoryNode {
                kind,
                data: Vec::new(),
            },
        );
        Ok(())
    }
}

#[async_trait]
impl RemoteFileSystem for MemoryRemote {
    async fn connect(&self) -> EngineResult<()> {
        Ok(())
    }

    async fn disconnect(&self) -> EngineResult<()> {
        Ok(())
    }

    async fn metadata(&self, path: &str) -> EngineResult<FileMetadata> {
        let nodes = read_nodes(&self.nodes)?;
        let node = nodes.get(path).ok_or_else(|| not_found(path))?;
        Ok(Self::metadata_for(node))
    }

    async fn read_dir(&self, path: &str) -> EngineResult<Vec<DirectoryEntry>> {
        let nodes = read_nodes(&self.nodes)?;
        match nodes.get(path) {
            Some(node) if node.kind == EntryKind::Directory => {}
            Some(_) => return Err(filesystem_error(FsErrorCode::NotDirectory, path)),
            None => return Err(not_found(path)),
        }
        let prefix = if path == "/" {
            "/".to_string()
        } else {
            format!("{path}/")
        };
        Ok(nodes
            .iter()
            .filter_map(|(candidate, node)| {
                let name = candidate.strip_prefix(&prefix)?;
                if name.is_empty() || name.contains('/') {
                    return None;
                }
                Some(DirectoryEntry {
                    path: candidate.clone(),
                    name: name.to_string(),
                    metadata: Self::metadata_for(node),
                })
            })
            .collect())
    }

    async fn read_range(&self, path: &str, offset: u64, length: u64) -> EngineResult<Vec<u8>> {
        let nodes = read_nodes(&self.nodes)?;
        let node = nodes.get(path).ok_or_else(|| not_found(path))?;
        if node.kind == EntryKind::Directory {
            return Err(filesystem_error(FsErrorCode::IsDirectory, path));
        }
        let start = usize::try_from(offset).map_err(|_| invalid_argument(path))?;
        let requested = usize::try_from(length).unwrap_or(usize::MAX);
        let end = start.saturating_add(requested).min(node.data.len());
        Ok(if start >= node.data.len() {
            Vec::new()
        } else {
            node.data[start..end].to_vec()
        })
    }

    async fn write(&self, path: &str, offset: u64, data: Vec<u8>) -> EngineResult<u64> {
        let mut nodes = write_nodes(&self.nodes)?;
        let node = nodes.get_mut(path).ok_or_else(|| not_found(path))?;
        if node.kind == EntryKind::Directory {
            return Err(filesystem_error(FsErrorCode::IsDirectory, path));
        }
        let start = usize::try_from(offset).map_err(|_| invalid_argument(path))?;
        let end = start
            .checked_add(data.len())
            .ok_or_else(|| invalid_argument(path))?;
        if node.data.len() < end {
            node.data.resize(end, 0);
        }
        node.data[start..end].copy_from_slice(&data);
        Ok(data.len() as u64)
    }

    async fn create_file(&self, path: &str) -> EngineResult<()> {
        self.create(path, EntryKind::File)
    }

    async fn create_dir(&self, path: &str) -> EngineResult<()> {
        self.create(path, EntryKind::Directory)
    }

    async fn remove(&self, path: &str, directory: bool) -> EngineResult<()> {
        let mut nodes = write_nodes(&self.nodes)?;
        let node = nodes.get(path).ok_or_else(|| not_found(path))?;
        let requested_kind = if directory {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        if node.kind != requested_kind {
            let code = if directory {
                FsErrorCode::NotDirectory
            } else {
                FsErrorCode::IsDirectory
            };
            return Err(filesystem_error(code, path));
        }
        let prefix = format!("{}/", path.trim_end_matches('/'));
        if directory && nodes.keys().any(|candidate| candidate.starts_with(&prefix)) {
            return Err(filesystem_error(FsErrorCode::Busy, path));
        }
        nodes.remove(path);
        Ok(())
    }

    async fn rename(&self, from: &str, to: &str) -> EngineResult<()> {
        let mut nodes = write_nodes(&self.nodes)?;
        if !nodes.contains_key(from) {
            return Err(not_found(from));
        }
        Self::ensure_parent(&nodes, to)?;
        if nodes.contains_key(to) {
            return Err(filesystem_error(FsErrorCode::AlreadyExists, to));
        }
        let prefix = format!("{}/", from.trim_end_matches('/'));
        let moved: Vec<_> = nodes
            .iter()
            .filter(|(path, _)| *path == from || path.starts_with(&prefix))
            .map(|(path, node)| (path.clone(), node.clone()))
            .collect();
        for (path, _) in &moved {
            nodes.remove(path);
        }
        for (path, node) in moved {
            let suffix = path.strip_prefix(from).unwrap_or_default();
            nodes.insert(format!("{to}{suffix}"), node);
        }
        Ok(())
    }

    async fn truncate(&self, path: &str, size: u64) -> EngineResult<()> {
        let mut nodes = write_nodes(&self.nodes)?;
        let node = nodes.get_mut(path).ok_or_else(|| not_found(path))?;
        if node.kind == EntryKind::Directory {
            return Err(filesystem_error(FsErrorCode::IsDirectory, path));
        }
        let size = usize::try_from(size).map_err(|_| invalid_argument(path))?;
        node.data.resize(size, 0);
        Ok(())
    }
}

pub(crate) fn memory_vfs() -> Arc<RemoteVfs<dyn RemoteFileSystem>> {
    let remote: Arc<dyn RemoteFileSystem> = Arc::new(MemoryRemote::default());
    Arc::new(RemoteVfs::new(remote))
}

fn read_nodes(
    nodes: &RwLock<BTreeMap<String, MemoryNode>>,
) -> EngineResult<std::sync::RwLockReadGuard<'_, BTreeMap<String, MemoryNode>>> {
    nodes
        .read()
        .map_err(|error| EngineError::Internal(error.to_string()))
}

fn write_nodes(
    nodes: &RwLock<BTreeMap<String, MemoryNode>>,
) -> EngineResult<std::sync::RwLockWriteGuard<'_, BTreeMap<String, MemoryNode>>> {
    nodes
        .write()
        .map_err(|error| EngineError::Internal(error.to_string()))
}

fn not_found(path: &str) -> EngineError {
    filesystem_error(FsErrorCode::NotFound, path)
}

fn invalid_argument(path: &str) -> EngineError {
    filesystem_error(FsErrorCode::InvalidArgument, path)
}

fn filesystem_error(code: FsErrorCode, path: &str) -> EngineError {
    EngineError::filesystem(code, path.to_string())
}
