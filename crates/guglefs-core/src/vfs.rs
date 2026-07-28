use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
    time::{Duration, Instant},
};

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::{
    DirectoryEntry, EngineError, EngineResult, EntryKind, FileMetadata, FileTimes, FsErrorCode,
    RemoteFileSystem,
};

const METADATA_CACHE_TTL: Duration = Duration::from_secs(3);
const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(1);
const DIRECTORY_CACHE_TTL: Duration = Duration::from_secs(2);
const READ_AHEAD_SIZE: u64 = 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 4096;

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
    content_generation: AtomicU64,
    files: RwLock<HashMap<FileHandle, Arc<OpenFile>>>,
    directories: RwLock<HashMap<DirectoryHandle, Arc<OpenDirectory>>>,
    metadata_cache: RwLock<HashMap<String, CacheEntry<CachedMetadata>>>,
    directory_cache: RwLock<HashMap<String, CacheEntry<Vec<DirectoryEntry>>>>,
}

impl<R: RemoteFileSystem + ?Sized> RemoteVfs<R> {
    pub fn new(remote: Arc<R>) -> Self {
        Self {
            remote,
            next_handle: AtomicU64::new(1),
            content_generation: AtomicU64::new(0),
            files: RwLock::new(HashMap::new()),
            directories: RwLock::new(HashMap::new()),
            metadata_cache: RwLock::new(HashMap::new()),
            directory_cache: RwLock::new(HashMap::new()),
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

    async fn metadata(&self, path: &str) -> EngineResult<FileMetadata> {
        if let Some(metadata) = self.cached_metadata(path)? {
            return metadata.into_result(path);
        }

        match self.remote.metadata(path).await {
            Ok(metadata) => {
                self.cache_metadata(path, CachedMetadata::Found(metadata.clone()))?;
                Ok(metadata)
            }
            Err(error) if error.code() == FsErrorCode::NotFound => {
                self.cache_metadata(path, CachedMetadata::NotFound)?;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    fn cached_metadata(&self, path: &str) -> EngineResult<Option<CachedMetadata>> {
        get_cached(&self.metadata_cache, path)
    }

    fn cache_metadata(&self, path: &str, metadata: CachedMetadata) -> EngineResult<()> {
        let ttl = match metadata {
            CachedMetadata::Found(_) => METADATA_CACHE_TTL,
            CachedMetadata::NotFound => NEGATIVE_CACHE_TTL,
        };
        insert_cached(&self.metadata_cache, path.into(), metadata, ttl)
    }

    fn invalidate_path(&self, path: &str) -> EngineResult<()> {
        write_lock(&self.metadata_cache)?.remove(path);
        write_lock(&self.directory_cache)?.remove(path);
        self.invalidate_parent(path)
    }

    fn invalidate_parent(&self, path: &str) -> EngineResult<()> {
        if let Some(parent) = parent_path(path) {
            write_lock(&self.directory_cache)?.remove(parent);
        }
        Ok(())
    }

    fn invalidate_subtree(&self, path: &str) -> EngineResult<()> {
        let prefix = format!("{}/", path.trim_end_matches('/'));
        write_lock(&self.metadata_cache)?
            .retain(|cached_path, _| cached_path != path && !cached_path.starts_with(&prefix));
        write_lock(&self.directory_cache)?
            .retain(|cached_path, _| cached_path != path && !cached_path.starts_with(&prefix));
        self.invalidate_parent(path)
    }

    async fn directory_entries(&self, path: &str) -> EngineResult<Vec<DirectoryEntry>> {
        if let Some(entries) = get_cached(&self.directory_cache, path)? {
            return Ok(entries);
        }

        let entries = self.remote.read_dir(path).await?;
        for entry in &entries {
            self.cache_metadata(&entry.path, CachedMetadata::Found(entry.metadata.clone()))?;
        }
        insert_cached(
            &self.directory_cache,
            path.into(),
            entries.clone(),
            DIRECTORY_CACHE_TTL,
        )?;
        Ok(entries)
    }
}

#[derive(Clone)]
enum CachedMetadata {
    Found(FileMetadata),
    NotFound,
}

impl CachedMetadata {
    fn into_result(self, path: &str) -> EngineResult<FileMetadata> {
        match self {
            Self::Found(metadata) => Ok(metadata),
            Self::NotFound => Err(EngineError::filesystem(FsErrorCode::NotFound, path)),
        }
    }
}

struct CacheEntry<T> {
    value: T,
    expires_at: Instant,
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
        let metadata = self.metadata(&path).await?;
        Ok(DirectoryEntry {
            path,
            name: name.into(),
            metadata,
        })
    }

    async fn getattr(&self, path: &str) -> EngineResult<FileMetadata> {
        self.metadata(&normalize_path(path)?).await
    }

    async fn open(&self, path: &str, options: OpenOptions) -> EngineResult<FileHandle> {
        options.validate()?;
        let path = normalize_path(path)?;
        let mut metadata = match self.metadata(&path).await {
            Ok(metadata) if metadata.kind == EntryKind::Directory => {
                return Err(EngineError::filesystem(
                    FsErrorCode::IsDirectory,
                    format!("cannot open directory as a file: {path}"),
                ));
            }
            Ok(metadata) => metadata,
            Err(error) if options.create && error.code() == FsErrorCode::NotFound => {
                self.remote.create_file(&path).await?;
                let metadata = FileMetadata {
                    kind: EntryKind::File,
                    size: 0,
                    modified: None,
                };
                self.cache_metadata(&path, CachedMetadata::Found(metadata.clone()))?;
                self.invalidate_parent(&path)?;
                metadata
            }
            Err(error) => return Err(error),
        };
        if options.truncate {
            self.remote.truncate(&path, 0).await?;
            self.content_generation.fetch_add(1, Ordering::AcqRel);
            metadata.size = 0;
            self.cache_metadata(&path, CachedMetadata::Found(metadata))?;
            self.invalidate_parent(&path)?;
        }

        let handle = FileHandle(self.allocate_handle()?);
        write_lock(&self.files)?.insert(
            handle,
            Arc::new(OpenFile {
                path,
                options,
                operation: Mutex::new(FileOperation::default()),
                dirty: AtomicBool::new(false),
                released: AtomicBool::new(false),
            }),
        );
        Ok(handle)
    }

    async fn open_dir(&self, path: &str) -> EngineResult<DirectoryHandle> {
        let path = normalize_path(path)?;
        let metadata = self.metadata(&path).await?;
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
        self.directory_entries(&directory.path).await
    }

    async fn read(&self, handle: FileHandle, offset: u64, length: u64) -> EngineResult<Vec<u8>> {
        let file = self.file(handle)?;
        let mut operation = file.operation.lock().await;
        ensure_open(file.released.load(Ordering::Acquire), handle.id())?;
        if !file.options.read {
            return Err(permission_denied("file handle is not open for reading"));
        }
        if length == 0 {
            return Ok(Vec::new());
        }
        let content_generation = self.content_generation.load(Ordering::Acquire);
        if let Some(data) = operation
            .read_cache
            .as_ref()
            .filter(|cache| cache.content_generation == content_generation)
            .and_then(|cache| cache.read(offset, length))
        {
            return Ok(data);
        }

        let fetch_length = length.max(READ_AHEAD_SIZE);
        let data = self
            .remote
            .read_range(&file.path, offset, fetch_length)
            .await?;
        let cache = ReadCache {
            offset,
            reached_end: data.len() < usize::try_from(fetch_length).unwrap_or(usize::MAX),
            data,
            content_generation,
        };
        let result = cache.read(offset, length).unwrap_or_default();
        operation.read_cache = Some(cache);
        Ok(result)
    }

    async fn write(&self, handle: FileHandle, offset: u64, data: Vec<u8>) -> EngineResult<u64> {
        let file = self.file(handle)?;
        let mut operation = file.operation.lock().await;
        ensure_open(file.released.load(Ordering::Acquire), handle.id())?;
        if !file.options.write {
            return Err(permission_denied("file handle is not open for writing"));
        }
        let cached_metadata = if file.options.append {
            let metadata = self.remote.metadata(&file.path).await?;
            let offset = metadata.size;
            self.cache_metadata(&file.path, CachedMetadata::Found(metadata.clone()))?;
            (offset, Some(CachedMetadata::Found(metadata)))
        } else {
            (offset, self.cached_metadata(&file.path)?)
        };
        let (offset, cached_metadata) = cached_metadata;
        let written = self.remote.write(&file.path, offset, data).await?;
        self.content_generation.fetch_add(1, Ordering::AcqRel);
        operation.read_cache = None;
        if let Some(CachedMetadata::Found(mut metadata)) = cached_metadata {
            metadata.size = metadata.size.max(offset.saturating_add(written));
            self.cache_metadata(&file.path, CachedMetadata::Found(metadata))?;
            self.invalidate_parent(&file.path)?;
        } else {
            self.invalidate_path(&file.path)?;
        }
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
        let path = normalize_path(path)?;
        self.remote.create_dir(&path).await?;
        self.cache_metadata(
            &path,
            CachedMetadata::Found(FileMetadata {
                kind: EntryKind::Directory,
                size: 0,
                modified: None,
            }),
        )?;
        self.invalidate_parent(&path)
    }

    async fn remove(&self, path: &str, kind: EntryKind) -> EngineResult<()> {
        let path = normalize_path(path)?;
        if path == "/" {
            return Err(invalid_argument("cannot remove the VFS root"));
        }
        let metadata = self.metadata(&path).await?;
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
            .await?;
        self.content_generation.fetch_add(1, Ordering::AcqRel);
        self.invalidate_subtree(&path)
    }

    async fn rename(&self, from: &str, to: &str) -> EngineResult<()> {
        let from = normalize_path(from)?;
        let to = normalize_path(to)?;
        self.remote.rename(&from, &to).await?;
        self.content_generation.fetch_add(1, Ordering::AcqRel);
        self.invalidate_subtree(&from)?;
        self.invalidate_subtree(&to)
    }

    async fn truncate(&self, path: &str, size: u64) -> EngineResult<()> {
        let path = normalize_path(path)?;
        let mut metadata = self.metadata(&path).await?;
        self.remote.truncate(&path, size).await?;
        self.content_generation.fetch_add(1, Ordering::AcqRel);
        metadata.size = size;
        self.cache_metadata(&path, CachedMetadata::Found(metadata))?;
        self.invalidate_parent(&path)
    }

    async fn set_times(&self, path: &str, times: FileTimes) -> EngineResult<()> {
        let path = normalize_path(path)?;
        let mut metadata = self.metadata(&path).await?;
        self.remote.set_times(&path, times.clone()).await?;
        if let Some(modified) = times.modified {
            metadata.modified = Some(modified);
        }
        self.cache_metadata(&path, CachedMetadata::Found(metadata))?;
        self.invalidate_parent(&path)
    }
}

struct OpenFile {
    path: String,
    options: OpenOptions,
    operation: Mutex<FileOperation>,
    dirty: AtomicBool,
    released: AtomicBool,
}

#[derive(Default)]
struct FileOperation {
    read_cache: Option<ReadCache>,
}

struct ReadCache {
    offset: u64,
    data: Vec<u8>,
    reached_end: bool,
    content_generation: u64,
}

impl ReadCache {
    fn read(&self, offset: u64, length: u64) -> Option<Vec<u8>> {
        let start = usize::try_from(offset.checked_sub(self.offset)?).ok()?;
        if start > self.data.len() {
            return None;
        }
        let requested = usize::try_from(length).ok()?;
        let available = self.data.len().saturating_sub(start);
        if requested > available && !self.reached_end {
            return None;
        }
        let count = requested.min(available);
        Some(self.data[start..start + count].to_vec())
    }
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

fn parent_path(path: &str) -> Option<&str> {
    if path == "/" {
        return None;
    }
    let (parent, _) = path.rsplit_once('/')?;
    Some(if parent.is_empty() { "/" } else { parent })
}

fn get_cached<T: Clone>(
    cache: &RwLock<HashMap<String, CacheEntry<T>>>,
    path: &str,
) -> EngineResult<Option<T>> {
    let now = Instant::now();
    let mut cache = write_lock(cache)?;
    match cache.get(path) {
        Some(entry) if entry.expires_at > now => Ok(Some(entry.value.clone())),
        Some(_) => {
            cache.remove(path);
            Ok(None)
        }
        None => Ok(None),
    }
}

fn insert_cached<T>(
    cache: &RwLock<HashMap<String, CacheEntry<T>>>,
    path: String,
    value: T,
    ttl: Duration,
) -> EngineResult<()> {
    let now = Instant::now();
    let mut cache = write_lock(cache)?;
    if cache.len() >= MAX_CACHE_ENTRIES {
        cache.retain(|_, entry| entry.expires_at > now);
        if cache.len() >= MAX_CACHE_ENTRIES {
            cache.clear();
        }
    }
    cache.insert(
        path,
        CacheEntry {
            value,
            expires_at: now + ttl,
        },
    );
    Ok(())
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
        metadata_reads: AtomicUsize,
        directory_reads: AtomicUsize,
        range_reads: AtomicUsize,
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
                ..Self::default()
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
            self.metadata_reads.fetch_add(1, Ordering::Relaxed);
            self.read_nodes()
                .get(path)
                .map(Self::node_metadata)
                .ok_or_else(|| EngineError::filesystem(FsErrorCode::NotFound, path))
        }

        async fn read_dir(&self, path: &str) -> EngineResult<Vec<DirectoryEntry>> {
            self.directory_reads.fetch_add(1, Ordering::Relaxed);
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
            self.range_reads.fetch_add(1, Ordering::Relaxed);
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
    async fn caches_metadata_directory_entries_and_missing_paths() {
        let remote = Arc::new(MemoryRemote::with_sample_file());
        let vfs = RemoteVfs::new(remote.clone());

        assert_eq!(vfs.getattr("/hello.txt").await.unwrap().size, 5);
        assert_eq!(vfs.getattr("/hello.txt").await.unwrap().size, 5);
        assert_eq!(remote.metadata_reads.load(Ordering::Relaxed), 1);

        let directory = vfs.open_dir("/").await.unwrap();
        assert_eq!(vfs.readdir(directory).await.unwrap().len(), 1);
        assert_eq!(vfs.readdir(directory).await.unwrap().len(), 1);
        assert_eq!(remote.directory_reads.load(Ordering::Relaxed), 1);

        assert!(vfs.getattr("/missing").await.is_err());
        assert!(vfs.getattr("/missing").await.is_err());
        assert_eq!(remote.metadata_reads.load(Ordering::Relaxed), 3);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reads_sequential_ranges_from_one_readahead_request() {
        let remote = Arc::new(MemoryRemote::with_sample_file());
        let vfs = RemoteVfs::new(remote.clone());
        let reader = vfs
            .open("/hello.txt", OpenOptions::read_only())
            .await
            .unwrap();
        let writer = vfs
            .open("/hello.txt", OpenOptions::read_write())
            .await
            .unwrap();

        assert_eq!(vfs.read(reader, 0, 2).await.unwrap(), b"he");
        assert_eq!(vfs.read(reader, 2, 2).await.unwrap(), b"ll");
        assert_eq!(vfs.read(reader, 4, 2).await.unwrap(), b"o");
        assert_eq!(remote.range_reads.load(Ordering::Relaxed), 1);

        vfs.write(writer, 0, b"H".to_vec()).await.unwrap();
        assert_eq!(vfs.read(reader, 0, 2).await.unwrap(), b"He");
        assert_eq!(remote.range_reads.load(Ordering::Relaxed), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn append_uses_the_latest_remote_file_size() {
        let remote = Arc::new(MemoryRemote::with_sample_file());
        let vfs = RemoteVfs::new(remote.clone());

        assert_eq!(vfs.getattr("/hello.txt").await.unwrap().size, 5);
        remote.write("/hello.txt", 5, b"!".to_vec()).await.unwrap();

        let handle = vfs
            .open(
                "/hello.txt",
                OpenOptions {
                    read: true,
                    write: true,
                    create: false,
                    truncate: false,
                    append: true,
                },
            )
            .await
            .unwrap();
        vfs.write(handle, 0, b"?".to_vec()).await.unwrap();

        assert_eq!(
            remote.read_range("/hello.txt", 0, 7).await.unwrap(),
            b"hello!?"
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
