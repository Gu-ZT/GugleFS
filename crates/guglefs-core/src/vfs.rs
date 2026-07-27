use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
};

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::{
    DirectoryEntry, EngineError, EngineResult, EntryKind, FileMetadata, FileTimes, FsErrorCode,
    RemoteFileSystem,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileHandle(u64);

impl FileHandle {
    pub const fn id(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DirectoryHandle(u64);

impl DirectoryHandle {
    pub const fn id(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenOptions {
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub truncate: bool,
    pub append: bool,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self {
            read: true,
            write: false,
            create: false,
            truncate: false,
            append: false,
        }
    }
}

impl OpenOptions {
    pub const fn read_only() -> Self {
        Self {
            read: true,
            write: false,
            create: false,
            truncate: false,
            append: false,
        }
    }

    pub const fn read_write() -> Self {
        Self {
            read: true,
            write: true,
            create: false,
            truncate: false,
            append: false,
        }
    }

    fn validate(self) -> EngineResult<()> {
        if !self.read && !self.write {
            return Err(invalid_argument("open requires read or write access"));
        }
        if (self.create || self.truncate || self.append) && !self.write {
            return Err(invalid_argument(
                "create, truncate, and append require write access",
            ));
        }
        Ok(())
    }
}

#[async_trait]
pub trait VirtualFileSystem: Send + Sync {
    async fn lookup(&self, parent: &str, name: &str) -> EngineResult<DirectoryEntry>;
    async fn getattr(&self, path: &str) -> EngineResult<FileMetadata>;
    async fn open(&self, path: &str, options: OpenOptions) -> EngineResult<FileHandle>;
    async fn open_dir(&self, path: &str) -> EngineResult<DirectoryHandle>;
    async fn readdir(&self, handle: DirectoryHandle) -> EngineResult<Vec<DirectoryEntry>>;
    async fn read(&self, handle: FileHandle, offset: u64, length: u64) -> EngineResult<Vec<u8>>;
    async fn write(&self, handle: FileHandle, offset: u64, data: Vec<u8>) -> EngineResult<u64>;
    async fn flush(&self, handle: FileHandle) -> EngineResult<()>;
    async fn release(&self, handle: FileHandle) -> EngineResult<()>;
    async fn release_dir(&self, handle: DirectoryHandle) -> EngineResult<()>;
    async fn create_dir(&self, path: &str) -> EngineResult<()>;
    async fn remove(&self, path: &str, kind: EntryKind) -> EngineResult<()>;
    async fn rename(&self, from: &str, to: &str) -> EngineResult<()>;
    async fn truncate(&self, path: &str, size: u64) -> EngineResult<()>;
    async fn set_times(&self, path: &str, times: FileTimes) -> EngineResult<()>;
}

pub struct RemoteVfs<R: RemoteFileSystem + ?Sized> {
    remote: Arc<R>,
    next_handle: AtomicU64,
    files: RwLock<HashMap<FileHandle, Arc<OpenFile>>>,
    directories: RwLock<HashMap<DirectoryHandle, Arc<OpenDirectory>>>,
}

impl<R: RemoteFileSystem + ?Sized> RemoteVfs<R> {
    pub fn new(remote: Arc<R>) -> Self {
        Self {
            remote,
            next_handle: AtomicU64::new(1),
            files: RwLock::new(HashMap::new()),
            directories: RwLock::new(HashMap::new()),
        }
    }

    pub fn remote(&self) -> &Arc<R> {
        &self.remote
    }

    pub fn open_file_count(&self) -> EngineResult<usize> {
        Ok(read_lock(&self.files)?.len())
    }

    pub fn open_directory_count(&self) -> EngineResult<usize> {
        Ok(read_lock(&self.directories)?.len())
    }

    fn allocate_handle(&self) -> EngineResult<u64> {
        self.next_handle
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| EngineError::Internal("VFS handle space exhausted".into()))
    }

    fn file(&self, handle: FileHandle) -> EngineResult<Arc<OpenFile>> {
        read_lock(&self.files)?
            .get(&handle)
            .cloned()
            .ok_or_else(|| invalid_handle(handle.id()))
    }

    fn directory(&self, handle: DirectoryHandle) -> EngineResult<Arc<OpenDirectory>> {
        read_lock(&self.directories)?
            .get(&handle)
            .cloned()
            .ok_or_else(|| invalid_handle(handle.id()))
    }
}

#[async_trait]
impl<R: RemoteFileSystem + ?Sized> VirtualFileSystem for RemoteVfs<R> {
    async fn lookup(&self, parent: &str, name: &str) -> EngineResult<DirectoryEntry> {
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', '\0']) {
            return Err(invalid_argument("invalid child name"));
        }
        let parent = normalize_path(parent)?;
        let path = if parent == "/" {
            format!("/{name}")
        } else {
            format!("{parent}/{name}")
        };
        let metadata = self.remote.metadata(&path).await?;
        Ok(DirectoryEntry {
            path,
            name: name.into(),
            metadata,
        })
    }

    async fn getattr(&self, path: &str) -> EngineResult<FileMetadata> {
        self.remote.metadata(&normalize_path(path)?).await
    }

    async fn open(&self, path: &str, options: OpenOptions) -> EngineResult<FileHandle> {
        options.validate()?;
        let path = normalize_path(path)?;
        match self.remote.metadata(&path).await {
            Ok(metadata) if metadata.kind == EntryKind::Directory => {
                return Err(EngineError::filesystem(
                    FsErrorCode::IsDirectory,
                    format!("cannot open directory as a file: {path}"),
                ));
            }
            Ok(_) => {}
            Err(error) if options.create && error.code() == FsErrorCode::NotFound => {
                self.remote.create_file(&path).await?;
            }
            Err(error) => return Err(error),
        }
        if options.truncate {
            self.remote.truncate(&path, 0).await?;
        }

        let handle = FileHandle(self.allocate_handle()?);
        write_lock(&self.files)?.insert(
            handle,
            Arc::new(OpenFile {
                path,
                options,
                operation: Mutex::new(()),
                dirty: AtomicBool::new(false),
                released: AtomicBool::new(false),
            }),
        );
        Ok(handle)
    }

    async fn open_dir(&self, path: &str) -> EngineResult<DirectoryHandle> {
        let path = normalize_path(path)?;
        let metadata = self.remote.metadata(&path).await?;
        if metadata.kind != EntryKind::Directory {
            return Err(EngineError::filesystem(
                FsErrorCode::NotDirectory,
                format!("not a directory: {path}"),
            ));
        }

        let handle = DirectoryHandle(self.allocate_handle()?);
        write_lock(&self.directories)?.insert(
            handle,
            Arc::new(OpenDirectory {
                path,
                operation: Mutex::new(()),
                released: AtomicBool::new(false),
            }),
        );
        Ok(handle)
    }

    async fn readdir(&self, handle: DirectoryHandle) -> EngineResult<Vec<DirectoryEntry>> {
        let directory = self.directory(handle)?;
        let _operation = directory.operation.lock().await;
        ensure_open(directory.released.load(Ordering::Acquire), handle.id())?;
        self.remote.read_dir(&directory.path).await
    }

    async fn read(&self, handle: FileHandle, offset: u64, length: u64) -> EngineResult<Vec<u8>> {
        let file = self.file(handle)?;
        let _operation = file.operation.lock().await;
        ensure_open(file.released.load(Ordering::Acquire), handle.id())?;
        if !file.options.read {
            return Err(permission_denied("file handle is not open for reading"));
        }
        self.remote.read_range(&file.path, offset, length).await
    }

    async fn write(&self, handle: FileHandle, offset: u64, data: Vec<u8>) -> EngineResult<u64> {
        let file = self.file(handle)?;
        let _operation = file.operation.lock().await;
        ensure_open(file.released.load(Ordering::Acquire), handle.id())?;
        if !file.options.write {
            return Err(permission_denied("file handle is not open for writing"));
        }
        let offset = if file.options.append {
            self.remote.metadata(&file.path).await?.size
        } else {
            offset
        };
        let written = self.remote.write(&file.path, offset, data).await?;
        file.dirty.store(true, Ordering::Release);
        Ok(written)
    }

    async fn flush(&self, handle: FileHandle) -> EngineResult<()> {
        let file = self.file(handle)?;
        let _operation = file.operation.lock().await;
        ensure_open(file.released.load(Ordering::Acquire), handle.id())?;
        flush_file(self.remote.as_ref(), &file).await
    }

    async fn release(&self, handle: FileHandle) -> EngineResult<()> {
        let file = self.file(handle)?;
        let _operation = file.operation.lock().await;
        ensure_open(file.released.load(Ordering::Acquire), handle.id())?;
        let flush_result = flush_file(self.remote.as_ref(), &file).await;
        file.released.store(true, Ordering::Release);
        write_lock(&self.files)?.remove(&handle);
        flush_result
    }

    async fn release_dir(&self, handle: DirectoryHandle) -> EngineResult<()> {
        let directory = self.directory(handle)?;
        let _operation = directory.operation.lock().await;
        ensure_open(directory.released.load(Ordering::Acquire), handle.id())?;
        directory.released.store(true, Ordering::Release);
        write_lock(&self.directories)?.remove(&handle);
        Ok(())
    }

    async fn create_dir(&self, path: &str) -> EngineResult<()> {
        self.remote.create_dir(&normalize_path(path)?).await
    }

    async fn remove(&self, path: &str, kind: EntryKind) -> EngineResult<()> {
        let path = normalize_path(path)?;
        if path == "/" {
            return Err(invalid_argument("cannot remove the VFS root"));
        }
        let metadata = self.remote.metadata(&path).await?;
        if metadata.kind != kind {
            let code = if kind == EntryKind::Directory {
                FsErrorCode::NotDirectory
            } else {
                FsErrorCode::IsDirectory
            };
            return Err(EngineError::filesystem(
                code,
                format!("entry kind does not match remove request: {path}"),
            ));
        }
        self.remote
            .remove(&path, kind == EntryKind::Directory)
            .await
    }

    async fn rename(&self, from: &str, to: &str) -> EngineResult<()> {
        self.remote
            .rename(&normalize_path(from)?, &normalize_path(to)?)
            .await
    }

    async fn truncate(&self, path: &str, size: u64) -> EngineResult<()> {
        self.remote.truncate(&normalize_path(path)?, size).await
    }

    async fn set_times(&self, path: &str, times: FileTimes) -> EngineResult<()> {
        self.remote.set_times(&normalize_path(path)?, times).await
    }
}

struct OpenFile {
    path: String,
    options: OpenOptions,
    operation: Mutex<()>,
    dirty: AtomicBool,
    released: AtomicBool,
}

struct OpenDirectory {
    path: String,
    operation: Mutex<()>,
    released: AtomicBool,
}

async fn flush_file<R: RemoteFileSystem + ?Sized>(remote: &R, file: &OpenFile) -> EngineResult<()> {
    if file.dirty.load(Ordering::Acquire) {
        remote.flush(&file.path).await?;
        file.dirty.store(false, Ordering::Release);
    }
    Ok(())
}

fn normalize_path(path: &str) -> EngineResult<String> {
    if !path.starts_with('/') || path.contains('\0') {
        return Err(invalid_argument("VFS paths must be absolute"));
    }
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => return Err(invalid_argument("VFS paths cannot contain '..'")),
            value => segments.push(value),
        }
    }
    if segments.is_empty() {
        Ok("/".into())
    } else {
        Ok(format!("/{}", segments.join("/")))
    }
}

fn read_lock<T>(lock: &RwLock<T>) -> EngineResult<RwLockReadGuard<'_, T>> {
    lock.read()
        .map_err(|error| EngineError::Internal(error.to_string()))
}

fn write_lock<T>(lock: &RwLock<T>) -> EngineResult<RwLockWriteGuard<'_, T>> {
    lock.write()
        .map_err(|error| EngineError::Internal(error.to_string()))
}

fn ensure_open(released: bool, id: u64) -> EngineResult<()> {
    if released {
        Err(invalid_handle(id))
    } else {
        Ok(())
    }
}

fn invalid_argument(message: impl Into<String>) -> EngineError {
    EngineError::filesystem(FsErrorCode::InvalidArgument, message)
}

fn invalid_handle(id: u64) -> EngineError {
    EngineError::filesystem(FsErrorCode::InvalidHandle, format!("invalid handle: {id}"))
}

fn permission_denied(message: impl Into<String>) -> EngineError {
    EngineError::filesystem(FsErrorCode::PermissionDenied, message)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        sync::{
            atomic::{AtomicUsize, Ordering},
            RwLock,
        },
    };

    use super::*;

    #[derive(Debug, Clone)]
    struct Node {
        kind: EntryKind,
        data: Vec<u8>,
        modified: Option<String>,
    }

    #[derive(Debug, Default)]
    struct MemoryRemote {
        nodes: RwLock<BTreeMap<String, Node>>,
        flushes: AtomicUsize,
    }

    impl MemoryRemote {
        fn with_sample_file() -> Self {
            let mut nodes = BTreeMap::new();
            nodes.insert(
                "/".into(),
                Node {
                    kind: EntryKind::Directory,
                    data: Vec::new(),
                    modified: None,
                },
            );
            nodes.insert(
                "/hello.txt".into(),
                Node {
                    kind: EntryKind::File,
                    data: b"hello".to_vec(),
                    modified: None,
                },
            );
            Self {
                nodes: RwLock::new(nodes),
                flushes: AtomicUsize::new(0),
            }
        }

        fn read_nodes(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, Node>> {
            self.nodes.read().unwrap()
        }

        fn write_nodes(&self) -> std::sync::RwLockWriteGuard<'_, BTreeMap<String, Node>> {
            self.nodes.write().unwrap()
        }

        fn node_metadata(node: &Node) -> FileMetadata {
            FileMetadata {
                kind: node.kind,
                size: node.data.len() as u64,
                modified: node.modified.clone(),
            }
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
            self.read_nodes()
                .get(path)
                .map(Self::node_metadata)
                .ok_or_else(|| EngineError::filesystem(FsErrorCode::NotFound, path))
        }

        async fn read_dir(&self, path: &str) -> EngineResult<Vec<DirectoryEntry>> {
            let prefix = if path == "/" {
                "/".to_string()
            } else {
                format!("{path}/")
            };
            let mut entries = Vec::new();
            for (entry_path, node) in self.read_nodes().iter() {
                let Some(name) = entry_path.strip_prefix(&prefix) else {
                    continue;
                };
                if name.is_empty() || name.contains('/') {
                    continue;
                }
                entries.push(DirectoryEntry {
                    path: entry_path.clone(),
                    name: name.into(),
                    metadata: Self::node_metadata(node),
                });
            }
            Ok(entries)
        }

        async fn read_range(&self, path: &str, offset: u64, length: u64) -> EngineResult<Vec<u8>> {
            let node = self
                .read_nodes()
                .get(path)
                .cloned()
                .ok_or_else(|| EngineError::filesystem(FsErrorCode::NotFound, path))?;
            if node.kind == EntryKind::Directory {
                return Err(EngineError::filesystem(FsErrorCode::IsDirectory, path));
            }
            let start = usize::try_from(offset).unwrap();
            let end = start.saturating_add(usize::try_from(length).unwrap());
            Ok(node
                .data
                .get(start..end.min(node.data.len()))
                .unwrap_or_default()
                .to_vec())
        }

        async fn write(&self, path: &str, offset: u64, data: Vec<u8>) -> EngineResult<u64> {
            let mut nodes = self.write_nodes();
            let node = nodes
                .get_mut(path)
                .ok_or_else(|| EngineError::filesystem(FsErrorCode::NotFound, path))?;
            if node.kind == EntryKind::Directory {
                return Err(EngineError::filesystem(FsErrorCode::IsDirectory, path));
            }
            let offset = usize::try_from(offset).unwrap();
            let end = offset.saturating_add(data.len());
            if node.data.len() < end {
                node.data.resize(end, 0);
            }
            node.data[offset..end].copy_from_slice(&data);
            Ok(data.len() as u64)
        }

        async fn create_file(&self, path: &str) -> EngineResult<()> {
            let mut nodes = self.write_nodes();
            if nodes.contains_key(path) {
                return Err(EngineError::filesystem(FsErrorCode::AlreadyExists, path));
            }
            nodes.insert(
                path.into(),
                Node {
                    kind: EntryKind::File,
                    data: Vec::new(),
                    modified: None,
                },
            );
            Ok(())
        }

        async fn create_dir(&self, path: &str) -> EngineResult<()> {
            let mut nodes = self.write_nodes();
            if nodes.contains_key(path) {
                return Err(EngineError::filesystem(FsErrorCode::AlreadyExists, path));
            }
            nodes.insert(
                path.into(),
                Node {
                    kind: EntryKind::Directory,
                    data: Vec::new(),
                    modified: None,
                },
            );
            Ok(())
        }

        async fn remove(&self, path: &str, directory: bool) -> EngineResult<()> {
            let mut nodes = self.write_nodes();
            let node = nodes
                .get(path)
                .ok_or_else(|| EngineError::filesystem(FsErrorCode::NotFound, path))?;
            if (node.kind == EntryKind::Directory) != directory {
                return Err(EngineError::filesystem(FsErrorCode::InvalidArgument, path));
            }
            nodes.remove(path);
            Ok(())
        }

        async fn rename(&self, from: &str, to: &str) -> EngineResult<()> {
            let mut nodes = self.write_nodes();
            let node = nodes
                .remove(from)
                .ok_or_else(|| EngineError::filesystem(FsErrorCode::NotFound, from))?;
            nodes.insert(to.into(), node);
            Ok(())
        }

        async fn truncate(&self, path: &str, size: u64) -> EngineResult<()> {
            let mut nodes = self.write_nodes();
            let node = nodes
                .get_mut(path)
                .ok_or_else(|| EngineError::filesystem(FsErrorCode::NotFound, path))?;
            if node.kind == EntryKind::Directory {
                return Err(EngineError::filesystem(FsErrorCode::IsDirectory, path));
            }
            node.data.resize(usize::try_from(size).unwrap(), 0);
            Ok(())
        }

        async fn set_times(&self, path: &str, times: FileTimes) -> EngineResult<()> {
            let mut nodes = self.write_nodes();
            let node = nodes
                .get_mut(path)
                .ok_or_else(|| EngineError::filesystem(FsErrorCode::NotFound, path))?;
            node.modified = times.modified;
            Ok(())
        }

        async fn flush(&self, _path: &str) -> EngineResult<()> {
            self.flushes.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn file_handles_serialize_writes_and_close_cleanly() {
        let remote = Arc::new(MemoryRemote::with_sample_file());
        let vfs = RemoteVfs::new(remote.clone());

        let entry = vfs.lookup("/", "hello.txt").await.unwrap();
        assert_eq!(entry.metadata.size, 5);
        let handle = vfs
            .open("/hello.txt", OpenOptions::read_write())
            .await
            .unwrap();
        assert_eq!(vfs.read(handle, 0, 5).await.unwrap(), b"hello");
        assert_eq!(vfs.write(handle, 5, b" world".to_vec()).await.unwrap(), 6);
        vfs.flush(handle).await.unwrap();
        assert_eq!(remote.flushes.load(Ordering::Relaxed), 1);
        vfs.release(handle).await.unwrap();
        assert_eq!(vfs.open_file_count().unwrap(), 0);
        assert!(
            matches!(vfs.read(handle, 0, 1).await, Err(error) if error.code() == FsErrorCode::InvalidHandle)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn directory_and_mutation_operations_use_distinct_handles() {
        let remote = Arc::new(MemoryRemote::with_sample_file());
        let vfs = RemoteVfs::new(remote);

        let directory = vfs.open_dir("/").await.unwrap();
        assert_eq!(vfs.readdir(directory).await.unwrap()[0].name, "hello.txt");
        vfs.release_dir(directory).await.unwrap();
        assert!(
            matches!(vfs.readdir(directory).await, Err(error) if error.code() == FsErrorCode::InvalidHandle)
        );

        vfs.create_dir("/docs").await.unwrap();
        let file = vfs
            .open(
                "/docs/readme.txt",
                OpenOptions {
                    read: true,
                    write: true,
                    create: true,
                    truncate: false,
                    append: false,
                },
            )
            .await
            .unwrap();
        vfs.write(file, 0, b"draft".to_vec()).await.unwrap();
        vfs.release(file).await.unwrap();
        vfs.truncate("/docs/readme.txt", 3).await.unwrap();
        vfs.set_times(
            "/docs/readme.txt",
            FileTimes {
                accessed: None,
                modified: Some("now".into()),
            },
        )
        .await
        .unwrap();
        vfs.rename("/docs/readme.txt", "/docs/README")
            .await
            .unwrap();
        assert_eq!(vfs.getattr("/docs/README").await.unwrap().size, 3);
        vfs.remove("/docs/README", EntryKind::File).await.unwrap();
        assert!(
            matches!(vfs.getattr("/docs/README").await, Err(error) if error.code() == FsErrorCode::NotFound)
        );
        assert!(
            matches!(vfs.remove("/", EntryKind::Directory).await, Err(error) if error.code() == FsErrorCode::InvalidArgument)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_invalid_paths_and_access_modes() {
        let vfs = RemoteVfs::new(Arc::new(MemoryRemote::with_sample_file()));
        assert!(
            matches!(vfs.getattr("../hello.txt").await, Err(error) if error.code() == FsErrorCode::InvalidArgument)
        );
        assert!(
            matches!(vfs.open("/hello.txt", OpenOptions { read: false, write: false, ..OpenOptions::default() }).await, Err(error) if error.code() == FsErrorCode::InvalidArgument)
        );
        let read_handle = vfs
            .open("/hello.txt", OpenOptions::read_only())
            .await
            .unwrap();
        assert!(
            matches!(vfs.write(read_handle, 0, vec![1]).await, Err(error) if error.code() == FsErrorCode::PermissionDenied)
        );
    }
}
