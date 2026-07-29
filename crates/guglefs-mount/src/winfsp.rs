use std::{
    collections::HashMap,
    future::Future,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
};

use async_trait::async_trait;
use guglefs_core::{
    ConnectionSecrets, DirectoryEntry, DirectoryHandle, EngineError, EngineResult, EntryKind,
    FileHandle, FileMetadata, FileSystemSpace, FsErrorCode, MappingConfig, MountDriver,
    OpenOptions, RemoteFileSystem, RemoteVfs, ResilientRemoteFileSystem, VirtualFileSystem,
};
use guglefs_remote::{FtpFileSystem, SftpFileSystem, WebDavFileSystem};
use tokio::runtime::Handle;
use widestring::{U16CStr, U16CString};
use winfsp_wrs::{
    CleanupFlags, CreateFileInfo, CreateOptions, DirInfo, FileAccessRights, FileAttributes,
    FileInfo, FileSystem, FileSystemInterface, OperationGuardStrategy, Params, SecurityDescriptor,
    VolumeInfo, WriteMode, NTSTATUS, STATUS_ACCESS_DENIED, STATUS_DIRECTORY_NOT_EMPTY,
    STATUS_FILE_IS_A_DIRECTORY, STATUS_INVALID_HANDLE, STATUS_INVALID_PARAMETER,
    STATUS_IO_DEVICE_ERROR, STATUS_NAME_TOO_LONG, STATUS_NOT_A_DIRECTORY, STATUS_NOT_SUPPORTED,
    STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_INVALID, STATUS_OBJECT_NAME_NOT_FOUND,
};

type DynamicVfs = RemoteVfs<dyn RemoteFileSystem>;

pub struct SystemMountDriver {
    mounts: Mutex<HashMap<String, MountedFileSystem>>,
}

struct MountedFileSystem {
    file_system: FileSystem,
    restore_empty_directory: bool,
    root_prefetch: tokio::task::JoinHandle<()>,
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
        let mounts = std::mem::take(
            &mut *self
                .mounts
                .lock()
                .map_err(|error| EngineError::Internal(error.to_string()))?,
        );
        let mut failures = Vec::new();
        for (mount_point, mounted) in mounts {
            mounted.root_prefetch.abort();
            mounted.file_system.stop();
            if let Err(error) =
                restore_mount_directory(&mount_point, mounted.restore_empty_directory)
            {
                failures.push(error.to_string());
            }
        }
        if !failures.is_empty() {
            return Err(EngineError::Mount(failures.join("; ")));
        }
        Ok(())
    }
}

struct MountContext {
    vfs: Arc<DynamicVfs>,
    runtime: Handle,
}

#[derive(Clone, Copy)]
enum HandleKind {
    File(FileHandle),
    Directory(DirectoryHandle),
}

struct WinHandle {
    mount: Arc<MountContext>,
    path: RwLock<String>,
    kind: EntryKind,
    handle: HandleKind,
    writable: bool,
    delete_requested: AtomicBool,
    directory_snapshot: Mutex<Option<Arc<Vec<DirectoryEntry>>>>,
}

struct MountCallbacks {
    mount: Arc<MountContext>,
}

impl MountContext {
    fn block_on<T>(&self, future: impl Future<Output = EngineResult<T>>) -> Result<T, NTSTATUS> {
        self.runtime.block_on(future).map_err(ntstatus)
    }

    fn metadata(&self, path: &str) -> Result<FileMetadata, NTSTATUS> {
        self.block_on(self.vfs.getattr(path))
    }

    fn file_info(&self, path: &str) -> Result<FileInfo, NTSTATUS> {
        let metadata = self.metadata(path)?;
        Ok(file_info(path, &metadata))
    }

    fn directory_entries(&self, path: &str) -> Result<Vec<guglefs_core::DirectoryEntry>, NTSTATUS> {
        let handle = self.block_on(self.vfs.open_dir(path))?;
        let entries = self.block_on(self.vfs.readdir(handle));
        let release = self.block_on(self.vfs.release_dir(handle));
        match (entries, release) {
            (Ok(entries), Ok(())) => Ok(entries),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    }

    fn resolve_existing_path(&self, requested: &str) -> Result<String, NTSTATUS> {
        if requested == "/" {
            return Ok("/".into());
        }
        match self.metadata(requested) {
            Ok(_) => return Ok(requested.into()),
            Err(status) if status == STATUS_OBJECT_NAME_NOT_FOUND => {}
            Err(status) => return Err(status),
        }

        let mut resolved = "/".to_string();
        for component in requested.trim_start_matches('/').split('/') {
            let entries = self.directory_entries(&resolved)?;
            let name =
                matching_windows_name(component, &entries)?.ok_or(STATUS_OBJECT_NAME_NOT_FOUND)?;
            resolved = join_vfs_path(&resolved, name);
        }
        Ok(resolved)
    }

    fn resolve_new_path(&self, requested: &str) -> Result<String, NTSTATUS> {
        let (parent, name) = split_vfs_path(requested).ok_or(STATUS_OBJECT_NAME_INVALID)?;
        let resolved_parent = self.resolve_existing_path(parent)?;
        let entries = self.directory_entries(&resolved_parent)?;
        if matching_windows_name(name, &entries)?.is_some() {
            return Err(STATUS_OBJECT_NAME_COLLISION);
        }
        Ok(join_vfs_path(&resolved_parent, name))
    }

    fn resolve_rename_target(
        &self,
        requested: &str,
        source: &str,
        replace_if_exists: bool,
    ) -> Result<String, NTSTATUS> {
        let (parent, name) = split_vfs_path(requested).ok_or(STATUS_OBJECT_NAME_INVALID)?;
        let resolved_parent = self.resolve_existing_path(parent)?;
        let entries = self.directory_entries(&resolved_parent)?;
        let Some(existing_name) = matching_windows_name(name, &entries)? else {
            return Ok(join_vfs_path(&resolved_parent, name));
        };
        let existing = join_vfs_path(&resolved_parent, existing_name);
        if existing == source {
            return Ok(join_vfs_path(&resolved_parent, name));
        }
        if !replace_if_exists {
            return Err(STATUS_OBJECT_NAME_COLLISION);
        }
        Ok(existing)
    }
}

impl MountCallbacks {
    fn path(file_name: &U16CStr) -> Result<String, NTSTATUS> {
        let value = file_name
            .to_string()
            .map_err(|_| STATUS_OBJECT_NAME_INVALID)?
            .replace('\\', "/");
        let value = value.trim_matches('/');
        let path = if value.is_empty() {
            "/".into()
        } else {
            format!("/{value}")
        };
        validate_windows_path(&path)?;
        Ok(path)
    }

    fn context_path(file_context: &WinHandle) -> Result<String, NTSTATUS> {
        file_context
            .path
            .read()
            .map(|path| path.clone())
            .map_err(|_| STATUS_IO_DEVICE_ERROR)
    }

    fn access_is_writable(access: FileAccessRights) -> bool {
        access.is(FileAccessRights::FILE_WRITE_DATA)
            || access.is(FileAccessRights::FILE_APPEND_DATA)
            || access.is(FileAccessRights::FILE_WRITE_ATTRIBUTES)
            || access.is(FileAccessRights::DELETE)
    }

    fn access_is_readable(access: FileAccessRights) -> bool {
        access.is(FileAccessRights::FILE_READ_DATA)
            || access.is(FileAccessRights::FILE_LIST_DIRECTORY)
            || access.is(FileAccessRights::FILE_READ_ATTRIBUTES)
    }

    fn open_existing(
        &self,
        requested_path: String,
        create_options: CreateOptions,
        granted_access: FileAccessRights,
    ) -> Result<(Arc<WinHandle>, FileInfo), NTSTATUS> {
        let path = self.mount.resolve_existing_path(&requested_path)?;
        let writable = Self::access_is_writable(granted_access);
        let readable = Self::access_is_readable(granted_access) || !writable;
        let metadata = self.mount.metadata(&path)?;
        let kind = existing_entry_kind(&metadata, create_options)?;
        let handle = match kind {
            EntryKind::Directory => {
                let handle = self.mount.block_on(self.mount.vfs.open_dir(&path))?;
                HandleKind::Directory(handle)
            }
            EntryKind::File => {
                let handle = self.mount.block_on(self.mount.vfs.open(
                    &path,
                    OpenOptions {
                        read: readable,
                        write: writable,
                        create: false,
                        truncate: false,
                        append: false,
                    },
                ))?;
                HandleKind::File(handle)
            }
        };
        let info = file_info(&path, &metadata);
        Ok((
            Arc::new(WinHandle {
                mount: self.mount.clone(),
                path: RwLock::new(path),
                kind,
                handle,
                writable,
                delete_requested: AtomicBool::new(false),
                directory_snapshot: Mutex::new(None),
            }),
            info,
        ))
    }

    fn create_new(
        &self,
        requested_path: String,
        create_info: CreateFileInfo,
    ) -> Result<(Arc<WinHandle>, FileInfo), NTSTATUS> {
        let path = self.mount.resolve_new_path(&requested_path)?;
        let directory = create_info
            .create_options
            .is(CreateOptions::FILE_DIRECTORY_FILE);
        let handle = if directory {
            self.mount.block_on(self.mount.vfs.create_dir(&path))?;
            let directory_handle = self.mount.block_on(self.mount.vfs.open_dir(&path))?;
            HandleKind::Directory(directory_handle)
        } else {
            let file_handle = self.mount.block_on(self.mount.vfs.open(
                &path,
                OpenOptions {
                    read: true,
                    write: true,
                    create: true,
                    truncate: false,
                    append: false,
                },
            ))?;
            HandleKind::File(file_handle)
        };
        let metadata = self.mount.metadata(&path)?;
        let info = file_info(&path, &metadata);
        Ok((
            Arc::new(WinHandle {
                mount: self.mount.clone(),
                path: RwLock::new(path),
                kind: if directory {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                },
                handle,
                writable: true,
                delete_requested: AtomicBool::new(false),
                directory_snapshot: Mutex::new(None),
            }),
            info,
        ))
    }

    fn release(&self, handle: Arc<WinHandle>) {
        let result = match handle.handle {
            HandleKind::File(file) => self.mount.block_on(self.mount.vfs.release(file)),
            HandleKind::Directory(directory) => {
                self.mount.block_on(self.mount.vfs.release_dir(directory))
            }
        };
        let _ = result;
    }
}

impl FileSystemInterface for MountCallbacks {
    type FileContext = Arc<WinHandle>;

    const GET_VOLUME_INFO_DEFINED: bool = true;
    const CREATE_DEFINED: bool = true;
    const OPEN_DEFINED: bool = true;
    const OVERWRITE_DEFINED: bool = true;
    const CLEANUP_DEFINED: bool = true;
    const CLOSE_DEFINED: bool = true;
    const READ_DEFINED: bool = true;
    const WRITE_DEFINED: bool = true;
    const FLUSH_DEFINED: bool = true;
    const GET_FILE_INFO_DEFINED: bool = true;
    const SET_BASIC_INFO_DEFINED: bool = true;
    const SET_FILE_SIZE_DEFINED: bool = true;
    const CAN_DELETE_DEFINED: bool = true;
    const RENAME_DEFINED: bool = true;
    const READ_DIRECTORY_DEFINED: bool = true;
    const SET_DELETE_DEFINED: bool = true;

    fn get_volume_info(&self) -> Result<VolumeInfo, NTSTATUS> {
        let label = U16CString::from_str("GugleFS").map_err(|_| STATUS_INVALID_PARAMETER)?;
        let space = self
            .mount
            .block_on(self.mount.vfs.filesystem_space("/"))?
            .unwrap_or(FileSystemSpace {
                total_bytes: 1 << 40,
                available_bytes: 1 << 39,
                total_files: None,
                available_files: None,
                block_size: 4096,
            });
        VolumeInfo::new(space.total_bytes, space.available_bytes, label.as_ustr())
            .map_err(|_| STATUS_INVALID_PARAMETER)
    }

    fn create(
        &self,
        file_name: &U16CStr,
        create_file_info: CreateFileInfo,
        _security_descriptor: SecurityDescriptor,
    ) -> Result<(Self::FileContext, FileInfo), NTSTATUS> {
        self.create_new(Self::path(file_name)?, create_file_info)
    }

    fn open(
        &self,
        file_name: &U16CStr,
        create_options: CreateOptions,
        granted_access: FileAccessRights,
    ) -> Result<(Self::FileContext, FileInfo), NTSTATUS> {
        self.open_existing(Self::path(file_name)?, create_options, granted_access)
    }

    fn overwrite(
        &self,
        file_context: Self::FileContext,
        _file_attributes: FileAttributes,
        _replace_file_attributes: bool,
        _allocation_size: u64,
    ) -> Result<FileInfo, NTSTATUS> {
        if file_context.kind == EntryKind::Directory {
            return Err(STATUS_FILE_IS_A_DIRECTORY);
        }
        let path = Self::context_path(&file_context)?;
        self.mount.block_on(self.mount.vfs.truncate(&path, 0))?;
        file_context.mount.file_info(&path)
    }

    fn cleanup(
        &self,
        file_context: Self::FileContext,
        _file_name: Option<&U16CStr>,
        flags: CleanupFlags,
    ) {
        if flags.is(CleanupFlags::DELETE) || file_context.delete_requested.load(Ordering::Acquire) {
            if let Ok(path) = Self::context_path(&file_context) {
                let _ = file_context
                    .mount
                    .block_on(file_context.mount.vfs.remove(&path, file_context.kind));
            }
        }
    }

    fn close(&self, file_context: Self::FileContext) {
        self.release(file_context);
    }

    fn read(
        &self,
        file_context: Self::FileContext,
        buffer: &mut [u8],
        offset: u64,
    ) -> Result<usize, NTSTATUS> {
        let HandleKind::File(handle) = file_context.handle else {
            return Err(STATUS_FILE_IS_A_DIRECTORY);
        };
        let bytes = file_context.mount.block_on(file_context.mount.vfs.read(
            handle,
            offset,
            buffer.len() as u64,
        ))?;
        buffer[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }

    fn write(
        &self,
        file_context: Self::FileContext,
        buffer: &[u8],
        mode: WriteMode,
    ) -> Result<(usize, FileInfo), NTSTATUS> {
        if !file_context.writable {
            return Err(STATUS_ACCESS_DENIED);
        }
        let HandleKind::File(handle) = file_context.handle else {
            return Err(STATUS_FILE_IS_A_DIRECTORY);
        };
        let path = Self::context_path(&file_context)?;
        let (offset, data) = match mode {
            WriteMode::Normal { offset } => (offset, buffer.to_vec()),
            WriteMode::ConstrainedIO { offset } => {
                let size = file_context.mount.metadata(&path)?.size;
                if offset >= size {
                    return Ok((0, file_context.mount.file_info(&path)?));
                }
                let available = usize::try_from(size - offset).unwrap_or(usize::MAX);
                (offset, buffer[..buffer.len().min(available)].to_vec())
            }
            WriteMode::WriteToEOF => (file_context.mount.metadata(&path)?.size, buffer.to_vec()),
        };
        if data.is_empty() {
            return Ok((0, file_context.mount.file_info(&path)?));
        }
        let written = file_context
            .mount
            .block_on(file_context.mount.vfs.write(handle, offset, data))?;
        Ok((written as usize, file_context.mount.file_info(&path)?))
    }

    fn flush(&self, file_context: Self::FileContext) -> Result<FileInfo, NTSTATUS> {
        if let HandleKind::File(handle) = file_context.handle {
            file_context
                .mount
                .block_on(file_context.mount.vfs.flush(handle))?;
        }
        let path = Self::context_path(&file_context)?;
        file_context.mount.file_info(&path)
    }

    fn get_file_info(&self, file_context: Self::FileContext) -> Result<FileInfo, NTSTATUS> {
        let path = Self::context_path(&file_context)?;
        file_context.mount.file_info(&path)
    }

    fn set_basic_info(
        &self,
        file_context: Self::FileContext,
        _file_attributes: FileAttributes,
        _creation_time: u64,
        _last_access_time: u64,
        _last_write_time: u64,
        _change_time: u64,
    ) -> Result<FileInfo, NTSTATUS> {
        let path = Self::context_path(&file_context)?;
        file_context.mount.file_info(&path)
    }

    fn set_file_size(
        &self,
        file_context: Self::FileContext,
        new_size: u64,
        set_allocation_size: bool,
    ) -> Result<FileInfo, NTSTATUS> {
        if file_context.kind == EntryKind::Directory {
            return Err(STATUS_FILE_IS_A_DIRECTORY);
        }
        let path = Self::context_path(&file_context)?;
        // WinFsp reports allocation-size requests separately from EOF changes.
        // The remote backends have no portable preallocation operation, so an
        // allocation request must not pad or truncate the visible file.
        if !set_allocation_size {
            file_context
                .mount
                .block_on(file_context.mount.vfs.truncate(&path, new_size))?;
        }
        file_context.mount.file_info(&path)
    }

    fn can_delete(
        &self,
        file_context: Self::FileContext,
        _file_name: &U16CStr,
    ) -> Result<(), NTSTATUS> {
        if file_context.kind == EntryKind::Directory {
            let directory = file_context.mount.block_on(
                file_context
                    .mount
                    .vfs
                    .open_dir(&Self::context_path(&file_context)?),
            )?;
            let entries = file_context
                .mount
                .block_on(file_context.mount.vfs.readdir(directory));
            let _ = file_context
                .mount
                .block_on(file_context.mount.vfs.release_dir(directory));
            if !entries?.is_empty() {
                return Err(STATUS_DIRECTORY_NOT_EMPTY);
            }
        }
        Ok(())
    }

    fn rename(
        &self,
        file_context: Self::FileContext,
        _file_name: &U16CStr,
        new_file_name: &U16CStr,
        replace_if_exists: bool,
    ) -> Result<(), NTSTATUS> {
        let source = Self::context_path(&file_context)?;
        let target = file_context.mount.resolve_rename_target(
            &Self::path(new_file_name)?,
            &source,
            replace_if_exists,
        )?;
        file_context
            .mount
            .block_on(file_context.mount.vfs.rename(&source, &target))?;
        *file_context
            .path
            .write()
            .map_err(|_| STATUS_IO_DEVICE_ERROR)? = target;
        Ok(())
    }

    fn read_directory(
        &self,
        file_context: Self::FileContext,
        marker: Option<&U16CStr>,
        mut add_dir_info: impl FnMut(DirInfo) -> bool,
    ) -> Result<(), NTSTATUS> {
        let HandleKind::Directory(handle) = file_context.handle else {
            return Err(STATUS_NOT_A_DIRECTORY);
        };
        let entries = {
            let mut snapshot = file_context
                .directory_snapshot
                .lock()
                .map_err(|_| STATUS_IO_DEVICE_ERROR)?;
            if marker.is_none() || snapshot.is_none() {
                let mut entries = file_context
                    .mount
                    .block_on(file_context.mount.vfs.readdir(handle))?;
                entries.retain(|entry| validate_windows_component(&entry.name).is_ok());
                entries.sort_by(|left, right| {
                    windows_name_key(&left.name)
                        .cmp(&windows_name_key(&right.name))
                        .then_with(|| left.name.cmp(&right.name))
                });
                entries.dedup_by(|left, right| {
                    windows_name_key(&left.name) == windows_name_key(&right.name)
                });
                *snapshot = Some(Arc::new(entries));
            }
            Arc::clone(snapshot.as_ref().expect("directory snapshot initialized"))
        };
        let marker = marker.map(|value| windows_name_key(&value.to_string_lossy()));
        for entry in entries.iter() {
            if marker
                .as_deref()
                .is_some_and(|value| windows_name_key(&entry.name).as_str() <= value)
            {
                continue;
            }
            if !add_dir_info(DirInfo::from_str(
                file_info(&entry.path, &entry.metadata),
                &entry.name,
            )) {
                break;
            }
        }
        Ok(())
    }

    fn set_delete(
        &self,
        file_context: Self::FileContext,
        _file_name: &U16CStr,
        delete_file: bool,
    ) -> Result<(), NTSTATUS> {
        file_context
            .delete_requested
            .store(delete_file, Ordering::Release);
        Ok(())
    }
}

#[async_trait]
impl MountDriver for SystemMountDriver {
    async fn mount(&self, config: &MappingConfig, secrets: ConnectionSecrets) -> EngineResult<()> {
        let mount_point = normalize_mount_point(&config.mount_point)?;
        if self
            .mounts
            .lock()
            .map_err(|error| EngineError::Internal(error.to_string()))?
            .contains_key(&mount_point)
        {
            return Err(EngineError::AlreadyMounted(mount_point));
        }
        let backend: Arc<dyn RemoteFileSystem> = match config.protocol {
            guglefs_core::Protocol::Ftp => {
                Arc::new(FtpFileSystem::from_config(config, secrets.credential)?)
            }
            guglefs_core::Protocol::Webdav => {
                Arc::new(WebDavFileSystem::from_config(config, secrets.credential)?)
            }
            guglefs_core::Protocol::Sftp => Arc::new(SftpFileSystem::from_config(config, secrets)?),
        };
        let remote: Arc<dyn RemoteFileSystem> = Arc::new(ResilientRemoteFileSystem::new(backend));
        remote.connect().await?;
        let vfs = Arc::new(RemoteVfs::new(remote));
        vfs.getattr("/").await?;
        let runtime = Handle::try_current().map_err(|error| {
            EngineError::Internal(format!("mount must run inside a Tokio runtime: {error}"))
        })?;
        let context = Arc::new(MountContext { vfs, runtime });
        winfsp_wrs::init().map_err(|error| {
            EngineError::Mount(format!(
                "WinFsp is not installed or its DLL could not be loaded: {error}"
            ))
        })?;
        let mount_point_wide = U16CString::from_str(&mount_point)
            .map_err(|_| EngineError::InvalidConfig("mount point is not valid UTF-16".into()))?;
        let restore_empty_directory = prepare_directory_mount_point(&mount_point)?;
        let mut volume_params = winfsp_wrs::VolumeParams::default();
        volume_params
            .set_case_sensitive_search(false)
            .set_case_preserved_names(true)
            .set_unicode_on_disk(true)
            .set_pass_query_directory_pattern(true)
            .set_supports_posix_unlink_rename(true);
        let params = Params {
            volume_params,
            guard_strategy: OperationGuardStrategy::Fine,
        };
        let file_system = FileSystem::start(
            params,
            Some(&mount_point_wide),
            MountCallbacks {
                mount: Arc::clone(&context),
            },
        )
        .map_err(|status| {
            let _ = restore_mount_directory(&mount_point, restore_empty_directory);
            if status == STATUS_OBJECT_NAME_COLLISION {
                EngineError::Mount(format!(
                    "WinFsp mount point is occupied: {mount_point} (0x{status:08X})"
                ))
            } else {
                EngineError::Mount(format!("WinFsp mount failed: 0x{status:08X}"))
            }
        })?;
        let root_vfs = Arc::clone(&context.vfs);
        let root_prefetch = context.runtime.spawn(async move {
            if let Ok(handle) = root_vfs.open_dir("/").await {
                let _ = root_vfs.readdir(handle).await;
                let _ = root_vfs.release_dir(handle).await;
            }
        });

        let mut mounts = match self.mounts.lock() {
            Ok(mounts) => mounts,
            Err(error) => {
                root_prefetch.abort();
                file_system.stop();
                let _ = restore_mount_directory(&mount_point, restore_empty_directory);
                return Err(EngineError::Internal(error.to_string()));
            }
        };
        if mounts.contains_key(&mount_point) {
            root_prefetch.abort();
            file_system.stop();
            let _ = restore_mount_directory(&mount_point, restore_empty_directory);
            return Err(EngineError::AlreadyMounted(mount_point));
        }
        mounts.insert(
            mount_point,
            MountedFileSystem {
                file_system,
                restore_empty_directory,
                root_prefetch,
            },
        );
        Ok(())
    }

    async fn unmount(&self, mount_point: &str) -> EngineResult<()> {
        let mount_point = normalize_mount_point(mount_point)?;
        let mounted = self
            .mounts
            .lock()
            .map_err(|error| EngineError::Internal(error.to_string()))?
            .remove(&mount_point)
            .ok_or_else(|| EngineError::NotMounted(mount_point.clone()))?;
        mounted.root_prefetch.abort();
        mounted.file_system.stop();
        restore_mount_directory(&mount_point, mounted.restore_empty_directory)?;
        Ok(())
    }
}

fn normalize_mount_point(value: &str) -> EngineResult<String> {
    let value = value.trim().replace('/', "\\");
    let bytes = value.as_bytes();
    if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Ok(format!("{}:", (bytes[0] as char).to_ascii_uppercase()));
    }
    if bytes.len() == 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\' {
        return Ok(format!("{}:", (bytes[0] as char).to_ascii_uppercase()));
    }
    let path = Path::new(&value);
    if !path.is_absolute() {
        return Err(EngineError::InvalidConfig(
            "Windows mount point must be a drive letter or an absolute directory path".into(),
        ));
    }
    if let Some(parent) = path.parent() {
        if !parent.is_dir() {
            return Err(EngineError::InvalidConfig(format!(
                "mount directory parent does not exist: {}",
                parent.display()
            )));
        }
    }
    Ok(value.trim_end_matches('\\').to_string())
}

fn prepare_directory_mount_point(mount_point: &str) -> EngineResult<bool> {
    if is_drive_mount_point(mount_point) {
        return Ok(false);
    }
    let path = Path::new(mount_point);
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(EngineError::InvalidConfig(format!(
                "inspect mount directory {mount_point}: {error}"
            )))
        }
    };
    if is_reparse_point(&metadata) {
        if stale_winfsp_mount_point(path) {
            std::fs::remove_dir(path).map_err(|error| {
                EngineError::Mount(format!(
                    "remove stale WinFsp mount point {mount_point}: {error}"
                ))
            })?;
            return Ok(false);
        }
        return Err(EngineError::AlreadyMounted(format!(
            "mount point is an active or unknown reparse point: {mount_point}"
        )));
    }
    if !metadata.is_dir() {
        return Err(EngineError::InvalidConfig(format!(
            "mount point is not a directory: {mount_point}"
        )));
    }
    let mut entries = std::fs::read_dir(path).map_err(|error| {
        EngineError::InvalidConfig(format!("read mount directory {mount_point}: {error}"))
    })?;
    if entries
        .next()
        .transpose()
        .map_err(|error| {
            EngineError::InvalidConfig(format!("read mount directory {mount_point}: {error}"))
        })?
        .is_some()
    {
        return Err(EngineError::InvalidConfig(format!(
            "mount directory must be empty: {mount_point}"
        )));
    }
    std::fs::remove_dir(path).map_err(|error| {
        EngineError::Mount(format!("prepare mount directory {mount_point}: {error}"))
    })?;
    Ok(true)
}

fn restore_mount_directory(mount_point: &str, restore: bool) -> EngineResult<()> {
    if !restore {
        return Ok(());
    }
    match std::fs::create_dir(mount_point) {
        Ok(()) => Ok(()),
        Err(error)
            if error.kind() == std::io::ErrorKind::AlreadyExists
                && Path::new(mount_point).is_dir() =>
        {
            Ok(())
        }
        Err(error) => Err(EngineError::Mount(format!(
            "restore mount directory {mount_point}: {error}"
        ))),
    }
}

fn is_drive_mount_point(mount_point: &str) -> bool {
    let bytes = mount_point.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn stale_winfsp_mount_point(path: &Path) -> bool {
    if std::fs::metadata(path).is_ok() {
        return false;
    }
    std::fs::read_link(path).is_ok_and(|target| {
        let target = target.to_string_lossy().to_ascii_lowercase();
        target.contains("winfsp") || target.contains("volume{")
    })
}

fn validate_windows_path(path: &str) -> Result<(), NTSTATUS> {
    if path.encode_utf16().count() > 32_767 {
        return Err(STATUS_NAME_TOO_LONG);
    }
    for component in path.trim_start_matches('/').split('/') {
        if !component.is_empty() {
            validate_windows_component(component)?;
        }
    }
    Ok(())
}

fn validate_windows_component(component: &str) -> Result<(), NTSTATUS> {
    if component.encode_utf16().count() > 255 {
        return Err(STATUS_NAME_TOO_LONG);
    }
    if component.is_empty()
        || matches!(component, "." | "..")
        || component.ends_with([' ', '.'])
        || component
            .chars()
            .any(|character| character <= '\u{1f}' || "<>:\"/\\|?*".contains(character))
    {
        return Err(STATUS_OBJECT_NAME_INVALID);
    }
    let base_name = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    let reserved = matches!(base_name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || base_name
            .strip_prefix("COM")
            .or_else(|| base_name.strip_prefix("LPT"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'));
    if reserved {
        return Err(STATUS_OBJECT_NAME_INVALID);
    }
    Ok(())
}

fn windows_name_key(name: &str) -> String {
    name.to_lowercase()
}

fn matching_windows_name<'a>(
    requested: &str,
    entries: &'a [guglefs_core::DirectoryEntry],
) -> Result<Option<&'a str>, NTSTATUS> {
    if let Some(exact) = entries.iter().find(|entry| entry.name == requested) {
        return Ok(Some(exact.name.as_str()));
    }
    let requested_key = windows_name_key(requested);
    let mut matches = entries
        .iter()
        .filter(|entry| windows_name_key(&entry.name) == requested_key);
    let Some(first) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(STATUS_OBJECT_NAME_COLLISION);
    }
    Ok(Some(first.name.as_str()))
}

fn split_vfs_path(path: &str) -> Option<(&str, &str)> {
    let path = path.trim_end_matches('/');
    let (parent, name) = path.rsplit_once('/')?;
    (!name.is_empty()).then_some((if parent.is_empty() { "/" } else { parent }, name))
}

fn join_vfs_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", parent.trim_end_matches('/'))
    }
}

fn file_info(path: &str, metadata: &FileMetadata) -> FileInfo {
    let mut attributes = if metadata.kind == EntryKind::Directory {
        FileAttributes::DIRECTORY
    } else {
        FileAttributes::ARCHIVE
    };
    if path
        .rsplit('/')
        .next()
        .is_some_and(|name| name.starts_with('.') && name.len() > 1)
    {
        attributes |= FileAttributes::HIDDEN;
    }
    let creation = filetime_from_unix(metadata.created);
    let access = filetime_from_unix(metadata.accessed);
    let write = filetime_from_unix(metadata.modified);
    let change = write;
    FileInfo::default()
        .set_file_attributes(attributes)
        .set_file_size(metadata.size)
        .set_allocation_size(metadata.size)
        .set_creation_time(creation)
        .set_last_access_time(access)
        .set_last_write_time(write)
        .set_change_time(change)
        .to_owned()
}

fn filetime_from_unix(seconds: Option<u64>) -> u64 {
    seconds
        .and_then(|value| value.checked_add(11_644_473_600))
        .and_then(|value| value.checked_mul(10_000_000))
        .unwrap_or(0)
}

fn existing_entry_kind(
    metadata: &FileMetadata,
    create_options: CreateOptions,
) -> Result<EntryKind, NTSTATUS> {
    match metadata.kind {
        EntryKind::Directory if create_options.is(CreateOptions::FILE_NON_DIRECTORY_FILE) => {
            Err(STATUS_FILE_IS_A_DIRECTORY)
        }
        EntryKind::File if create_options.is(CreateOptions::FILE_DIRECTORY_FILE) => {
            Err(STATUS_NOT_A_DIRECTORY)
        }
        kind => Ok(kind),
    }
}

fn ntstatus(error: EngineError) -> NTSTATUS {
    #[cfg(debug_assertions)]
    eprintln!("WinFsp operation failed: {error}");

    match error.code() {
        FsErrorCode::NotFound => STATUS_OBJECT_NAME_NOT_FOUND,
        FsErrorCode::AlreadyExists => STATUS_OBJECT_NAME_COLLISION,
        FsErrorCode::PermissionDenied => STATUS_ACCESS_DENIED,
        FsErrorCode::NotDirectory => STATUS_NOT_A_DIRECTORY,
        FsErrorCode::IsDirectory => STATUS_FILE_IS_A_DIRECTORY,
        FsErrorCode::InvalidHandle => STATUS_INVALID_HANDLE,
        FsErrorCode::InvalidArgument | FsErrorCode::AlreadyMounted | FsErrorCode::NotMounted => {
            STATUS_INVALID_PARAMETER
        }
        FsErrorCode::Unsupported => STATUS_NOT_SUPPORTED,
        FsErrorCode::Busy
        | FsErrorCode::RemoteIo
        | FsErrorCode::MountIo
        | FsErrorCode::Internal => STATUS_IO_DEVICE_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::memory_vfs;
    use guglefs_core::DirectoryEntry;
    use std::{
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temporary_mount_point(test_name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "guglefs-{test_name}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn metadata(kind: EntryKind) -> FileMetadata {
        FileMetadata {
            kind,
            size: 0,
            created: None,
            accessed: None,
            modified: None,
        }
    }

    fn entry(name: &str) -> DirectoryEntry {
        DirectoryEntry {
            path: format!("/{name}"),
            name: name.into(),
            metadata: metadata(EntryKind::File),
        }
    }

    fn create_info(create_options: CreateOptions) -> CreateFileInfo {
        CreateFileInfo {
            create_options,
            granted_access: FileAccessRights::FILE_ALL_ACCESS,
            file_attributes: FileAttributes::NORMAL,
            allocation_size: 0,
        }
    }

    fn wide(value: &str) -> U16CString {
        U16CString::from_str(value).unwrap()
    }

    #[test]
    fn memory_backend_exercises_winfsp_file_operations() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mount = Arc::new(MountContext {
            vfs: memory_vfs(),
            runtime: runtime.handle().clone(),
        });
        let callbacks = MountCallbacks { mount };

        let (directory, _) = callbacks
            .create_new(
                "/docs".into(),
                create_info(CreateOptions::FILE_DIRECTORY_FILE),
            )
            .unwrap();
        let (file, _) = callbacks
            .create_new(
                "/docs/hello.txt".into(),
                create_info(CreateOptions::FILE_NON_DIRECTORY_FILE),
            )
            .unwrap();

        assert_eq!(
            callbacks
                .write(file.clone(), b"hello", WriteMode::Normal { offset: 0 })
                .unwrap()
                .0,
            5
        );
        callbacks.flush(file.clone()).unwrap();
        let mut buffer = [0_u8; 16];
        assert_eq!(callbacks.read(file.clone(), &mut buffer, 0).unwrap(), 5);
        assert_eq!(&buffer[..5], b"hello");

        let mut names = Vec::new();
        callbacks
            .read_directory(directory.clone(), None, |entry| {
                let length = entry
                    .file_name
                    .iter()
                    .position(|character| *character == 0)
                    .unwrap_or(entry.file_name.len());
                names.push(String::from_utf16(&entry.file_name[..length]).unwrap());
                true
            })
            .unwrap();
        assert_eq!(names, ["hello.txt"]);

        let old_name = wide("\\docs\\hello.txt");
        let new_name = wide("\\docs\\greeting.txt");
        callbacks
            .rename(
                file.clone(),
                old_name.as_ucstr(),
                new_name.as_ucstr(),
                false,
            )
            .unwrap();
        callbacks.set_file_size(file.clone(), 16, true).unwrap();
        let mut allocated = [0_u8; 16];
        assert_eq!(callbacks.read(file.clone(), &mut allocated, 0).unwrap(), 5);
        assert_eq!(&allocated[..5], b"hello");
        assert_eq!(
            callbacks
                .write(file.clone(), b"XYZ", WriteMode::ConstrainedIO { offset: 4 },)
                .unwrap()
                .0,
            1
        );
        assert_eq!(
            callbacks
                .write(
                    file.clone(),
                    b"ignored",
                    WriteMode::ConstrainedIO { offset: 16 },
                )
                .unwrap()
                .0,
            0
        );
        callbacks.set_file_size(file.clone(), 4, false).unwrap();
        let mut truncated = [0_u8; 8];
        assert_eq!(callbacks.read(file.clone(), &mut truncated, 0).unwrap(), 4);
        assert_eq!(&truncated[..4], b"hell");

        callbacks
            .set_delete(file.clone(), new_name.as_ucstr(), true)
            .unwrap();
        callbacks.cleanup(
            file.clone(),
            Some(new_name.as_ucstr()),
            CleanupFlags::DELETE,
        );
        callbacks.close(file);

        names.clear();
        callbacks
            .read_directory(directory.clone(), None, |entry| {
                let length = entry
                    .file_name
                    .iter()
                    .position(|character| *character == 0)
                    .unwrap_or(entry.file_name.len());
                names.push(String::from_utf16(&entry.file_name[..length]).unwrap());
                true
            })
            .unwrap();
        assert!(names.is_empty());
        callbacks.close(directory);

        assert_eq!(
            callbacks
                .mount
                .block_on(callbacks.mount.vfs.getattr("/missing")),
            Err(STATUS_OBJECT_NAME_NOT_FOUND)
        );
    }

    #[test]
    fn overwrites_unicode_jar_without_changing_payload_bytes() {
        const FILE_SIZE: usize = 851_287;
        const WRITE_CHUNK: usize = 64 * 1024;
        const READ_CHUNK: usize = 37 * 1024;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mount = Arc::new(MountContext {
            vfs: memory_vfs(),
            runtime: runtime.handle().clone(),
        });
        let callbacks = MountCallbacks { mount };
        let (file, _) = callbacks
            .create_new(
                "/ResourcePackGenerator-Bukkit【资源包】.jar".into(),
                create_info(CreateOptions::FILE_NON_DIRECTORY_FILE),
            )
            .unwrap();

        let old = vec![0xa5; FILE_SIZE + 128 * 1024];
        callbacks
            .write(file.clone(), &old, WriteMode::Normal { offset: 0 })
            .unwrap();
        callbacks
            .overwrite(
                file.clone(),
                FileAttributes::NORMAL,
                false,
                FILE_SIZE as u64,
            )
            .unwrap();
        callbacks
            .set_file_size(file.clone(), FILE_SIZE as u64, true)
            .unwrap();

        let expected: Vec<u8> = (0..FILE_SIZE)
            .map(|index| ((index * 31 + index / 251) % 256) as u8)
            .collect();
        for (index, chunk) in expected.chunks(WRITE_CHUNK).enumerate() {
            let offset = (index * WRITE_CHUNK) as u64;
            assert_eq!(
                callbacks
                    .write(file.clone(), chunk, WriteMode::Normal { offset })
                    .unwrap()
                    .0,
                chunk.len()
            );
        }
        callbacks
            .set_file_size(file.clone(), FILE_SIZE as u64, false)
            .unwrap();
        callbacks.flush(file.clone()).unwrap();

        let mut actual = Vec::with_capacity(FILE_SIZE);
        let mut offset = 0_u64;
        while actual.len() < FILE_SIZE {
            let mut chunk = vec![0; READ_CHUNK.min(FILE_SIZE - actual.len())];
            let read = callbacks.read(file.clone(), &mut chunk, offset).unwrap();
            assert_ne!(read, 0);
            actual.extend_from_slice(&chunk[..read]);
            offset += read as u64;
        }
        assert_eq!(actual, expected);
        callbacks.close(file);
    }

    #[test]
    fn directory_paging_reuses_a_stable_handle_snapshot() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let mount = Arc::new(MountContext {
            vfs: memory_vfs(),
            runtime: runtime.handle().clone(),
        });
        let callbacks = MountCallbacks { mount };
        let (directory, _) = callbacks
            .create_new(
                "/docs".into(),
                create_info(CreateOptions::FILE_DIRECTORY_FILE),
            )
            .unwrap();
        let (first_file, _) = callbacks
            .create_new(
                "/docs/a.txt".into(),
                create_info(CreateOptions::FILE_NON_DIRECTORY_FILE),
            )
            .unwrap();

        let mut first_page = Vec::new();
        callbacks
            .read_directory(directory.clone(), None, |entry| {
                let length = entry
                    .file_name
                    .iter()
                    .position(|character| *character == 0)
                    .unwrap_or(entry.file_name.len());
                first_page.push(String::from_utf16(&entry.file_name[..length]).unwrap());
                false
            })
            .unwrap();
        assert_eq!(first_page, ["a.txt"]);

        let (second_file, _) = callbacks
            .create_new(
                "/docs/z.txt".into(),
                create_info(CreateOptions::FILE_NON_DIRECTORY_FILE),
            )
            .unwrap();
        let marker = wide("a.txt");
        let mut continuation = Vec::new();
        callbacks
            .read_directory(directory.clone(), Some(marker.as_ucstr()), |entry| {
                let length = entry
                    .file_name
                    .iter()
                    .position(|character| *character == 0)
                    .unwrap_or(entry.file_name.len());
                continuation.push(String::from_utf16(&entry.file_name[..length]).unwrap());
                true
            })
            .unwrap();
        assert!(continuation.is_empty());

        let mut restarted = Vec::new();
        callbacks
            .read_directory(directory.clone(), None, |entry| {
                let length = entry
                    .file_name
                    .iter()
                    .position(|character| *character == 0)
                    .unwrap_or(entry.file_name.len());
                restarted.push(String::from_utf16(&entry.file_name[..length]).unwrap());
                true
            })
            .unwrap();
        assert_eq!(restarted, ["a.txt", "z.txt"]);

        callbacks.close(first_file);
        callbacks.close(second_file);
        callbacks.close(directory);
    }

    #[test]
    fn open_kind_uses_metadata_without_a_type_constraint() {
        let options = CreateOptions(0);

        assert_eq!(
            existing_entry_kind(&metadata(EntryKind::Directory), options),
            Ok(EntryKind::Directory)
        );
        assert_eq!(
            existing_entry_kind(&metadata(EntryKind::File), options),
            Ok(EntryKind::File)
        );
    }

    #[test]
    fn open_kind_enforces_explicit_type_constraints() {
        assert_eq!(
            existing_entry_kind(
                &metadata(EntryKind::Directory),
                CreateOptions::FILE_NON_DIRECTORY_FILE,
            ),
            Err(STATUS_FILE_IS_A_DIRECTORY)
        );
        assert_eq!(
            existing_entry_kind(
                &metadata(EntryKind::File),
                CreateOptions::FILE_DIRECTORY_FILE,
            ),
            Err(STATUS_NOT_A_DIRECTORY)
        );
    }

    #[test]
    fn normalizes_drive_root_mount_points() {
        assert_eq!(normalize_mount_point("z:").unwrap(), "Z:");
        assert_eq!(normalize_mount_point("z:\\").unwrap(), "Z:");
        assert_eq!(normalize_mount_point("Z:/").unwrap(), "Z:");
    }

    #[test]
    fn validates_windows_file_name_components() {
        for valid in ["readme.txt", "中文 文档.txt", ".gitignore", "COM10.txt"] {
            assert_eq!(validate_windows_component(valid), Ok(()), "{valid}");
        }
        for invalid in [
            "CON",
            "con.txt",
            "LPT1.log",
            "bad:name",
            "trailing.",
            "trailing ",
            ".",
            "..",
        ] {
            assert_eq!(
                validate_windows_component(invalid),
                Err(STATUS_OBJECT_NAME_INVALID),
                "{invalid}"
            );
        }
        assert_eq!(
            validate_windows_component(&"x".repeat(256)),
            Err(STATUS_NAME_TOO_LONG)
        );
    }

    #[test]
    fn resolves_names_case_insensitively_but_prefers_exact_matches() {
        let entries = vec![entry("Readme.txt"), entry("README.TXT")];

        assert_eq!(
            matching_windows_name("Readme.txt", &entries),
            Ok(Some("Readme.txt"))
        );
        assert_eq!(
            matching_windows_name("readme.txt", &entries),
            Err(STATUS_OBJECT_NAME_COLLISION)
        );
        assert_eq!(matching_windows_name("other.txt", &entries), Ok(None));
    }

    #[test]
    fn projects_basic_windows_attributes_from_remote_names() {
        let hidden = file_info("/.settings", &metadata(EntryKind::File)).file_attributes();
        assert!(hidden.is(FileAttributes::HIDDEN));
        assert!(hidden.is(FileAttributes::ARCHIVE));

        let directory = file_info("/docs", &metadata(EntryKind::Directory)).file_attributes();
        assert!(directory.is(FileAttributes::DIRECTORY));
        assert!(!directory.is(FileAttributes::HIDDEN));
    }

    #[test]
    fn projects_remote_timestamps_to_windows_filetime() {
        let metadata = FileMetadata {
            kind: EntryKind::File,
            size: 5,
            created: Some(1_700_000_000),
            accessed: Some(1_700_000_100),
            modified: Some(1_700_000_200),
        };

        let info = file_info("/file.txt", &metadata);
        assert_eq!(info.creation_time(), 133_444_736_000_000_000);
        assert_eq!(info.last_access_time(), 133_444_737_000_000_000);
        assert_eq!(info.last_write_time(), 133_444_738_000_000_000);
        assert_eq!(info.change_time(), info.last_write_time());
        assert_eq!(filetime_from_unix(None), 0);
    }

    #[test]
    fn temporarily_removes_and_restores_an_empty_mount_directory() {
        let mount_point = temporary_mount_point("empty-directory");
        std::fs::create_dir(&mount_point).unwrap();
        let mount_point = mount_point.to_string_lossy().into_owned();

        assert!(prepare_directory_mount_point(&mount_point).unwrap());
        assert!(!Path::new(&mount_point).exists());
        restore_mount_directory(&mount_point, true).unwrap();
        assert!(Path::new(&mount_point).is_dir());

        std::fs::remove_dir(mount_point).unwrap();
    }

    #[test]
    fn rejects_a_non_empty_mount_directory() {
        let mount_point = temporary_mount_point("non-empty-directory");
        std::fs::create_dir(&mount_point).unwrap();
        std::fs::write(mount_point.join("keep.txt"), b"keep").unwrap();
        let mount_point_text = mount_point.to_string_lossy().into_owned();

        let error = prepare_directory_mount_point(&mount_point_text).unwrap_err();
        assert!(error.to_string().contains("must be empty"));
        assert!(mount_point.join("keep.txt").is_file());

        std::fs::remove_file(mount_point.join("keep.txt")).unwrap();
        std::fs::remove_dir(mount_point).unwrap();
    }
}
