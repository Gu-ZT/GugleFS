use std::{
    collections::HashMap,
    ffi::OsStr,
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use fuser::{
    BsdFileFlags, Config, Errno, FileAttr, FileHandle as FuseFileHandle, FileType, Filesystem,
    FopenFlags, Generation, INodeNo, LockOwner, MountOption, OpenAccMode, OpenFlags, RenameFlags,
    ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyStatfs, ReplyWrite, Request, TimeOrNow, WriteFlags,
};
use guglefs_core::{
    ConnectionSecrets, DirectoryHandle, EngineError, EngineResult, EntryKind, FileHandle,
    FileMetadata, MappingConfig, MountDriver, OpenOptions, RemoteFileSystem, RemoteVfs,
    VirtualFileSystem,
};
#[cfg(feature = "remote-backends")]
use guglefs_remote::{FtpFileSystem, SftpFileSystem, WebDavFileSystem};
use tokio::runtime::Handle;

const ATTR_TTL: Duration = Duration::from_secs(1);
const BLOCK_SIZE: u32 = 4096;
const ROOT_INODE: u64 = 1;

type DynamicVfs = RemoteVfs<dyn RemoteFileSystem>;

pub struct SystemMountDriver {
    mounts: Mutex<HashMap<PathBuf, fuser::BackgroundSession>>,
}

impl Default for SystemMountDriver {
    fn default() -> Self {
        Self {
            mounts: Mutex::new(HashMap::new()),
        }
    }
}

impl SystemMountDriver {
    pub fn unmount_all(&self) -> EngineResult<()> {
        let sessions = std::mem::take(
            &mut *self
                .mounts
                .lock()
                .map_err(|error| EngineError::Internal(error.to_string()))?,
        );
        let mut failures = Vec::new();
        for (mount_point, session) in sessions {
            if let Err(error) = session.umount_and_join() {
                failures.push(format!("{}: {error}", mount_point.display()));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(EngineError::Mount(format!(
                "failed to unmount FUSE filesystems: {}",
                failures.join("; ")
            )))
        }
    }
}

struct FuseCallbacks {
    vfs: Arc<DynamicVfs>,
    runtime: Handle,
    inodes: RwLock<InodeTable>,
}

impl FuseCallbacks {
    fn new(vfs: Arc<DynamicVfs>, runtime: Handle) -> Self {
        Self {
            vfs,
            runtime,
            inodes: RwLock::new(InodeTable::default()),
        }
    }

    fn block_on<T>(&self, future: impl Future<Output = EngineResult<T>>) -> Result<T, Errno> {
        self.runtime.block_on(future).map_err(errno)
    }

    fn path(&self, inode: INodeNo) -> Result<String, Errno> {
        self.inodes
            .read()
            .map_err(|_| Errno::EIO)?
            .path(u64::from(inode))
            .map(str::to_string)
            .ok_or(Errno::ENOENT)
    }

    fn child_path(&self, parent: INodeNo, name: &OsStr) -> Result<String, Errno> {
        let name = name.to_str().ok_or(Errno::EINVAL)?;
        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(Errno::EINVAL);
        }
        let parent = self.path(parent)?;
        Ok(if parent == "/" {
            format!("/{name}")
        } else {
            format!("{parent}/{name}")
        })
    }

    fn inode(&self, path: &str) -> Result<INodeNo, Errno> {
        self.inodes
            .write()
            .map_err(|_| Errno::EIO)
            .map(|mut inodes| INodeNo(inodes.ensure(path)))
    }

    fn entry(&self, path: &str, request: &Request, reply: ReplyEntry) {
        match self.block_on(self.vfs.getattr(path)).and_then(|metadata| {
            let inode = self.inode(path)?;
            Ok((inode, file_attr(inode, &metadata, request)))
        }) {
            Ok((_inode, attributes)) => reply.entry(&ATTR_TTL, &attributes, Generation(0)),
            Err(error) => reply.error(error),
        }
    }

    fn attributes(&self, inode: INodeNo, request: &Request) -> Result<FileAttr, Errno> {
        let path = self.path(inode)?;
        self.block_on(self.vfs.getattr(&path))
            .map(|metadata| file_attr(inode, &metadata, request))
    }

    fn open_options(flags: OpenFlags, create: bool) -> OpenOptions {
        let (read, write) = match flags.acc_mode() {
            OpenAccMode::O_RDONLY => (true, false),
            OpenAccMode::O_WRONLY => (false, true),
            OpenAccMode::O_RDWR => (true, true),
        };
        OpenOptions {
            read,
            write,
            create,
            truncate: flags.0 & libc::O_TRUNC != 0,
            append: flags.0 & libc::O_APPEND != 0,
        }
    }

    fn remove(&self, parent: INodeNo, name: &OsStr, kind: EntryKind) -> Result<(), Errno> {
        let path = self.child_path(parent, name)?;
        self.block_on(self.vfs.remove(&path, kind))?;
        self.inodes
            .write()
            .map_err(|_| Errno::EIO)?
            .remove_subtree(&path);
        Ok(())
    }
}

impl Filesystem for FuseCallbacks {
    fn lookup(&self, request: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        match self.child_path(parent, name) {
            Ok(path) => self.entry(&path, request, reply),
            Err(error) => reply.error(error),
        }
    }

    fn getattr(
        &self,
        request: &Request,
        inode: INodeNo,
        _handle: Option<FuseFileHandle>,
        reply: ReplyAttr,
    ) {
        match self.attributes(inode, request) {
            Ok(attributes) => reply.attr(&ATTR_TTL, &attributes),
            Err(error) => reply.error(error),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn setattr(
        &self,
        request: &Request,
        inode: INodeNo,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _handle: Option<FuseFileHandle>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        let result = (|| {
            let path = self.path(inode)?;
            if let Some(size) = size {
                self.block_on(self.vfs.truncate(&path, size))?;
            }
            self.attributes(inode, request)
        })();
        match result {
            Ok(attributes) => reply.attr(&ATTR_TTL, &attributes),
            Err(error) => reply.error(error),
        }
    }

    fn mkdir(
        &self,
        request: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        match self.child_path(parent, name) {
            Ok(path) => match self.block_on(self.vfs.create_dir(&path)) {
                Ok(()) => self.entry(&path, request, reply),
                Err(error) => reply.error(error),
            },
            Err(error) => reply.error(error),
        }
    }

    fn unlink(&self, _request: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        match self.remove(parent, name, EntryKind::File) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn rmdir(&self, _request: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        match self.remove(parent, name, EntryKind::Directory) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn rename(
        &self,
        _request: &Request,
        parent: INodeNo,
        name: &OsStr,
        new_parent: INodeNo,
        new_name: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        let result = (|| {
            if !flags.is_empty() {
                return Err(Errno::ENOTSUP);
            }
            let from = self.child_path(parent, name)?;
            let to = self.child_path(new_parent, new_name)?;
            self.block_on(self.vfs.rename(&from, &to))?;
            self.inodes
                .write()
                .map_err(|_| Errno::EIO)?
                .rename_subtree(&from, &to);
            Ok(())
        })();
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn open(&self, _request: &Request, inode: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let result = self
            .path(inode)
            .and_then(|path| self.block_on(self.vfs.open(&path, Self::open_options(flags, false))));
        match result {
            Ok(handle) => reply.opened(FuseFileHandle(handle.id()), FopenFlags::empty()),
            Err(error) => reply.error(error),
        }
    }

    fn create(
        &self,
        request: &Request,
        parent: INodeNo,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let result = (|| {
            let path = self.child_path(parent, name)?;
            let handle = self.block_on(
                self.vfs
                    .open(&path, Self::open_options(OpenFlags(flags), true)),
            )?;
            let metadata = match self.block_on(self.vfs.getattr(&path)) {
                Ok(metadata) => metadata,
                Err(error) => {
                    let _ = self.block_on(self.vfs.release(handle));
                    return Err(error);
                }
            };
            Ok((path, handle, metadata))
        })();
        match result {
            Ok((path, handle, metadata)) => match self.inode(&path) {
                Ok(inode) => reply.created(
                    &ATTR_TTL,
                    &file_attr(inode, &metadata, request),
                    Generation(0),
                    FuseFileHandle(handle.id()),
                    FopenFlags::empty(),
                ),
                Err(error) => {
                    let _ = self.block_on(self.vfs.release(handle));
                    reply.error(error);
                }
            },
            Err(error) => reply.error(error),
        }
    }

    fn read(
        &self,
        _request: &Request,
        _inode: INodeNo,
        handle: FuseFileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        match self.block_on(
            self.vfs
                .read(FileHandle::from_id(handle.0), offset, u64::from(size)),
        ) {
            Ok(data) => reply.data(&data),
            Err(error) => reply.error(error),
        }
    }

    fn write(
        &self,
        _request: &Request,
        _inode: INodeNo,
        handle: FuseFileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        match self.block_on(
            self.vfs
                .write(FileHandle::from_id(handle.0), offset, data.to_vec()),
        ) {
            Ok(written) => match u32::try_from(written) {
                Ok(written) => reply.written(written),
                Err(_) => reply.error(Errno::EOVERFLOW),
            },
            Err(error) => reply.error(error),
        }
    }

    fn flush(
        &self,
        _request: &Request,
        _inode: INodeNo,
        handle: FuseFileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        match self.block_on(self.vfs.flush(FileHandle::from_id(handle.0))) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn release(
        &self,
        _request: &Request,
        _inode: INodeNo,
        handle: FuseFileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        match self.block_on(self.vfs.release(FileHandle::from_id(handle.0))) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn fsync(
        &self,
        _request: &Request,
        _inode: INodeNo,
        handle: FuseFileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        match self.block_on(self.vfs.flush(FileHandle::from_id(handle.0))) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn opendir(&self, _request: &Request, inode: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        let result = self
            .path(inode)
            .and_then(|path| self.block_on(self.vfs.open_dir(&path)));
        match result {
            Ok(handle) => reply.opened(FuseFileHandle(handle.id()), FopenFlags::empty()),
            Err(error) => reply.error(error),
        }
    }

    fn readdir(
        &self,
        _request: &Request,
        inode: INodeNo,
        handle: FuseFileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let result = (|| {
            let path = self.path(inode)?;
            let parent = parent_path(&path);
            let parent_inode = self.inode(parent)?;
            let entries = self.block_on(self.vfs.readdir(DirectoryHandle::from_id(handle.0)))?;
            let mut output = Vec::with_capacity(entries.len() + 2);
            output.push((inode, FileType::Directory, ".".to_string()));
            output.push((parent_inode, FileType::Directory, "..".to_string()));
            for entry in entries {
                output.push((
                    self.inode(&entry.path)?,
                    file_type(entry.metadata.kind),
                    entry.name,
                ));
            }
            Ok(output)
        })();
        match result {
            Ok(entries) => {
                for (index, (inode, kind, name)) in
                    entries.into_iter().enumerate().skip(offset as usize)
                {
                    if reply.add(inode, (index + 1) as u64, kind, name) {
                        break;
                    }
                }
                reply.ok();
            }
            Err(error) => reply.error(error),
        }
    }

    fn releasedir(
        &self,
        _request: &Request,
        _inode: INodeNo,
        handle: FuseFileHandle,
        _flags: OpenFlags,
        reply: ReplyEmpty,
    ) {
        match self.block_on(self.vfs.release_dir(DirectoryHandle::from_id(handle.0))) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(error),
        }
    }

    fn statfs(&self, _request: &Request, _inode: INodeNo, reply: ReplyStatfs) {
        let blocks = (1_u64 << 40) / u64::from(BLOCK_SIZE);
        reply.statfs(
            blocks,
            blocks / 2,
            blocks / 2,
            1 << 30,
            1 << 29,
            BLOCK_SIZE,
            255,
            BLOCK_SIZE,
        );
    }
}

#[derive(Debug)]
struct InodeTable {
    next_inode: u64,
    paths: HashMap<u64, String>,
    inodes: HashMap<String, u64>,
}

impl Default for InodeTable {
    fn default() -> Self {
        Self {
            next_inode: ROOT_INODE + 1,
            paths: HashMap::from([(ROOT_INODE, "/".to_string())]),
            inodes: HashMap::from([("/".to_string(), ROOT_INODE)]),
        }
    }
}

impl InodeTable {
    fn path(&self, inode: u64) -> Option<&str> {
        self.paths.get(&inode).map(String::as_str)
    }

    fn ensure(&mut self, path: &str) -> u64 {
        if let Some(inode) = self.inodes.get(path) {
            return *inode;
        }
        let inode = self.next_inode;
        self.next_inode = self.next_inode.saturating_add(1);
        self.paths.insert(inode, path.to_string());
        self.inodes.insert(path.to_string(), inode);
        inode
    }

    fn remove_subtree(&mut self, path: &str) {
        let prefix = format!("{}/", path.trim_end_matches('/'));
        let removed: Vec<_> = self
            .inodes
            .iter()
            .filter(|(candidate, _)| *candidate == path || candidate.starts_with(&prefix))
            .map(|(candidate, inode)| (candidate.clone(), *inode))
            .collect();
        for (candidate, inode) in removed {
            self.inodes.remove(&candidate);
            self.paths.remove(&inode);
        }
    }

    fn rename_subtree(&mut self, from: &str, to: &str) {
        let prefix = format!("{}/", from.trim_end_matches('/'));
        let renamed: Vec<_> = self
            .inodes
            .iter()
            .filter(|(candidate, _)| *candidate == from || candidate.starts_with(&prefix))
            .map(|(candidate, inode)| (candidate.clone(), *inode))
            .collect();
        self.remove_subtree(to);
        for (old_path, inode) in renamed {
            self.inodes.remove(&old_path);
            let suffix = old_path.strip_prefix(from).unwrap_or_default();
            let new_path = format!("{to}{suffix}");
            self.paths.insert(inode, new_path.clone());
            self.inodes.insert(new_path, inode);
        }
    }
}

#[async_trait]
impl MountDriver for SystemMountDriver {
    async fn mount(&self, config: &MappingConfig, secrets: ConnectionSecrets) -> EngineResult<()> {
        prepare_mount_point(&config.mount_point)?;
        let mount_point = normalize_mount_point(&config.mount_point)?;
        if self
            .mounts
            .lock()
            .map_err(|error| EngineError::Internal(error.to_string()))?
            .contains_key(&mount_point)
        {
            return Err(EngineError::AlreadyMounted(
                mount_point.display().to_string(),
            ));
        }
        ensure_empty_mount_point(&mount_point)?;

        let remote = build_remote(config, secrets)?;
        remote.connect().await?;
        let vfs = Arc::new(RemoteVfs::new(remote));
        vfs.getattr("/").await?;
        let runtime = Handle::try_current().map_err(|error| {
            EngineError::Internal(format!("mount must run inside a Tokio runtime: {error}"))
        })?;
        let callbacks = FuseCallbacks::new(vfs, runtime);
        let mut options = Config::default();
        options.mount_options = vec![
            MountOption::FSName("GugleFS".into()),
            MountOption::Subtype("guglefs".into()),
            MountOption::RW,
            MountOption::NoDev,
            MountOption::NoSuid,
            MountOption::NoExec,
            MountOption::DefaultPermissions,
        ];
        options.n_threads = Some(4);
        options.clone_fd = cfg!(target_os = "linux");
        let session = fuser::spawn_mount(callbacks, &mount_point, &options).map_err(|error| {
            EngineError::Mount(format!(
                "FUSE mount failed at {}: {error}",
                mount_point.display()
            ))
        })?;

        let mut mounts = match self.mounts.lock() {
            Ok(mounts) => mounts,
            Err(error) => {
                let _ = session.umount_and_join();
                return Err(EngineError::Internal(error.to_string()));
            }
        };
        if mounts.contains_key(&mount_point) {
            let _ = session.umount_and_join();
            return Err(EngineError::AlreadyMounted(
                mount_point.display().to_string(),
            ));
        }
        mounts.insert(mount_point, session);
        Ok(())
    }

    async fn unmount(&self, mount_point: &str) -> EngineResult<()> {
        let mount_point = normalize_mount_point(mount_point)?;
        let session = self
            .mounts
            .lock()
            .map_err(|error| EngineError::Internal(error.to_string()))?
            .remove(&mount_point)
            .ok_or_else(|| EngineError::NotMounted(mount_point.display().to_string()))?;
        session.umount_and_join().map_err(|error| {
            EngineError::Mount(format!(
                "FUSE unmount failed at {}: {error}",
                mount_point.display()
            ))
        })
    }
}

#[cfg(feature = "remote-backends")]
fn build_remote(
    config: &MappingConfig,
    secrets: ConnectionSecrets,
) -> EngineResult<Arc<dyn RemoteFileSystem>> {
    Ok(match config.protocol {
        guglefs_core::Protocol::Ftp => {
            Arc::new(FtpFileSystem::from_config(config, secrets.credential)?)
        }
        guglefs_core::Protocol::Webdav => {
            Arc::new(WebDavFileSystem::from_config(config, secrets.credential)?)
        }
        guglefs_core::Protocol::Sftp => Arc::new(SftpFileSystem::from_config(config, secrets)?),
    })
}

#[cfg(not(feature = "remote-backends"))]
fn build_remote(
    _config: &MappingConfig,
    _secrets: ConnectionSecrets,
) -> EngineResult<Arc<dyn RemoteFileSystem>> {
    Err(EngineError::NotImplemented(
        "remote backends are disabled in this build".into(),
    ))
}

fn normalize_mount_point(value: &str) -> EngineResult<PathBuf> {
    let value = value.trim();
    let path = Path::new(value);
    if value.is_empty() || !path.is_absolute() {
        return Err(EngineError::InvalidConfig(
            "FUSE mount point must be an absolute directory path".into(),
        ));
    }
    if !path.is_dir() {
        return Err(EngineError::InvalidConfig(format!(
            "mount directory does not exist: {value}"
        )));
    }
    path.canonicalize().map_err(|error| {
        EngineError::InvalidConfig(format!("resolve mount directory {value}: {error}"))
    })
}

fn prepare_mount_point(value: &str) -> EngineResult<()> {
    let value = value.trim();
    let path = Path::new(value);
    if value.is_empty() || !path.is_absolute() {
        return Err(EngineError::InvalidConfig(
            "FUSE mount point must be an absolute directory path".into(),
        ));
    }
    if !path.exists() {
        std::fs::create_dir_all(path).map_err(|error| {
            EngineError::InvalidConfig(format!("create mount directory {value}: {error}"))
        })?;
    }
    Ok(())
}

fn ensure_empty_mount_point(path: &Path) -> EngineResult<()> {
    let mut entries = path.read_dir().map_err(|error| {
        EngineError::InvalidConfig(format!("read mount directory {}: {error}", path.display()))
    })?;
    if entries
        .next()
        .transpose()
        .map_err(|error| {
            EngineError::InvalidConfig(format!("read mount directory {}: {error}", path.display()))
        })?
        .is_some()
    {
        return Err(EngineError::InvalidConfig(format!(
            "mount directory must be empty: {}",
            path.display()
        )));
    }
    Ok(())
}

fn file_attr(inode: INodeNo, metadata: &FileMetadata, request: &Request) -> FileAttr {
    let modified = metadata
        .modified
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap_or(UNIX_EPOCH);
    FileAttr {
        ino: inode,
        size: metadata.size,
        blocks: metadata.size.div_ceil(u64::from(BLOCK_SIZE)),
        atime: modified,
        mtime: modified,
        ctime: modified,
        crtime: modified,
        kind: file_type(metadata.kind),
        perm: if metadata.kind == EntryKind::Directory {
            0o755
        } else {
            0o644
        },
        nlink: if metadata.kind == EntryKind::Directory {
            2
        } else {
            1
        },
        uid: request.uid(),
        gid: request.gid(),
        rdev: 0,
        flags: 0,
        blksize: BLOCK_SIZE,
    }
}

fn file_type(kind: EntryKind) -> FileType {
    match kind {
        EntryKind::File => FileType::RegularFile,
        EntryKind::Directory => FileType::Directory,
    }
}

fn parent_path(path: &str) -> &str {
    if path == "/" {
        return "/";
    }
    path.rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/")
}

fn errno(error: EngineError) -> Errno {
    Errno::from_i32(error.code().posix_errno())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inode_table_keeps_ids_stable_and_updates_renamed_subtrees() {
        let mut table = InodeTable::default();
        let directory = table.ensure("/docs");
        let file = table.ensure("/docs/readme.txt");

        assert_eq!(table.ensure("/docs"), directory);
        table.rename_subtree("/docs", "/archive");

        assert_eq!(table.path(directory), Some("/archive"));
        assert_eq!(table.path(file), Some("/archive/readme.txt"));
        assert_eq!(table.ensure("/archive/readme.txt"), file);
    }

    #[test]
    fn inode_table_removes_complete_subtrees() {
        let mut table = InodeTable::default();
        let directory = table.ensure("/docs");
        let file = table.ensure("/docs/readme.txt");

        table.remove_subtree("/docs");

        assert_eq!(table.path(directory), None);
        assert_eq!(table.path(file), None);
        assert_eq!(table.path(ROOT_INODE), Some("/"));
    }

    #[test]
    fn parent_paths_are_normalized() {
        assert_eq!(parent_path("/"), "/");
        assert_eq!(parent_path("/file"), "/");
        assert_eq!(parent_path("/docs/file"), "/docs");
    }
}
