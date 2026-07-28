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
    ConnectionSecrets, DirectoryHandle, EngineError, EngineResult, EntryKind, FileHandle,
    FileMetadata, FsErrorCode, MappingConfig, MountDriver, OpenOptions, RemoteFileSystem,
    RemoteVfs, VirtualFileSystem,
};
use guglefs_remote::{FtpFileSystem, SftpFileSystem, WebDavFileSystem};
use tokio::runtime::Handle;
use widestring::{U16CStr, U16CString};
use winfsp_wrs::{
    filetime_now, CleanupFlags, CreateFileInfo, CreateOptions, DirInfo, FileAccessRights,
    FileAttributes, FileInfo, FileSystem, FileSystemInterface, OperationGuardStrategy, Params,
    SecurityDescriptor, VolumeInfo, WriteMode, NTSTATUS, STATUS_ACCESS_DENIED,
    STATUS_DIRECTORY_NOT_EMPTY, STATUS_FILE_IS_A_DIRECTORY, STATUS_INVALID_HANDLE,
    STATUS_INVALID_PARAMETER, STATUS_IO_DEVICE_ERROR, STATUS_NOT_A_DIRECTORY, STATUS_NOT_SUPPORTED,
    STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_NOT_FOUND,
};

type DynamicVfs = RemoteVfs<dyn RemoteFileSystem>;

pub struct SystemMountDriver {
    mounts: Mutex<HashMap<String, FileSystem>>,
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
        for file_system in mounts.into_values() {
            file_system.stop();
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
        Ok(file_info(&metadata))
    }
}

impl MountCallbacks {
    fn path(file_name: &U16CStr) -> Result<String, NTSTATUS> {
        let value = file_name.to_string_lossy().replace('\\', "/");
        let value = value.trim_matches('/');
        if value.is_empty() {
            Ok("/".into())
        } else {
            Ok(format!("/{value}"))
        }
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
        path: String,
        create_options: CreateOptions,
        granted_access: FileAccessRights,
    ) -> Result<(Arc<WinHandle>, FileInfo), NTSTATUS> {
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
        Ok((
            Arc::new(WinHandle {
                mount: self.mount.clone(),
                path: RwLock::new(path),
                kind,
                handle,
                writable,
                delete_requested: AtomicBool::new(false),
            }),
            file_info(&metadata),
        ))
    }

    fn create_new(
        &self,
        path: String,
        create_info: CreateFileInfo,
        security_descriptor: SecurityDescriptor,
    ) -> Result<(Arc<WinHandle>, FileInfo), NTSTATUS> {
        let _ = security_descriptor;
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
            }),
            file_info(&metadata),
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
        VolumeInfo::new(1 << 40, 1 << 39, label.as_ustr()).map_err(|_| STATUS_INVALID_PARAMETER)
    }

    fn create(
        &self,
        file_name: &U16CStr,
        create_file_info: CreateFileInfo,
        security_descriptor: SecurityDescriptor,
    ) -> Result<(Self::FileContext, FileInfo), NTSTATUS> {
        self.create_new(
            Self::path(file_name)?,
            create_file_info,
            security_descriptor,
        )
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
        let offset = match mode {
            WriteMode::Normal { offset } | WriteMode::ConstrainedIO { offset } => offset,
            WriteMode::WriteToEOF => file_context.mount.metadata(&path)?.size,
        };
        let written = file_context.mount.block_on(file_context.mount.vfs.write(
            handle,
            offset,
            buffer.to_vec(),
        ))?;
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
        _set_allocation_size: bool,
    ) -> Result<FileInfo, NTSTATUS> {
        if file_context.kind == EntryKind::Directory {
            return Err(STATUS_FILE_IS_A_DIRECTORY);
        }
        let path = Self::context_path(&file_context)?;
        file_context
            .mount
            .block_on(file_context.mount.vfs.truncate(&path, new_size))?;
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
        _replace_if_exists: bool,
    ) -> Result<(), NTSTATUS> {
        let target = Self::path(new_file_name)?;
        let source = Self::context_path(&file_context)?;
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
        let mut entries = file_context
            .mount
            .block_on(file_context.mount.vfs.readdir(handle))?;
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        let marker = marker.map(|value| value.to_string_lossy());
        for entry in entries {
            if marker
                .as_deref()
                .is_some_and(|value| entry.name.as_str() <= value)
            {
                continue;
            }
            if !add_dir_info(DirInfo::from_str(file_info(&entry.metadata), &entry.name)) {
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
        let remote: Arc<dyn RemoteFileSystem> = match config.protocol {
            guglefs_core::Protocol::Ftp => {
                Arc::new(FtpFileSystem::from_config(config, secrets.credential)?)
            }
            guglefs_core::Protocol::Webdav => {
                Arc::new(WebDavFileSystem::from_config(config, secrets.credential)?)
            }
            guglefs_core::Protocol::Sftp => Arc::new(SftpFileSystem::from_config(config, secrets)?),
        };
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
        let mut volume_params = winfsp_wrs::VolumeParams::default();
        volume_params
            .set_case_sensitive_search(true)
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
            MountCallbacks { mount: context },
        )
        .map_err(|status| EngineError::Mount(format!("WinFsp mount failed: 0x{status:08X}")))?;
        let mut mounts = match self.mounts.lock() {
            Ok(mounts) => mounts,
            Err(error) => {
                file_system.stop();
                return Err(EngineError::Internal(error.to_string()));
            }
        };
        if mounts.contains_key(&mount_point) {
            file_system.stop();
            return Err(EngineError::AlreadyMounted(mount_point));
        }
        mounts.insert(mount_point, file_system);
        Ok(())
    }

    async fn unmount(&self, mount_point: &str) -> EngineResult<()> {
        let mount_point = normalize_mount_point(mount_point)?;
        let file_system = self
            .mounts
            .lock()
            .map_err(|error| EngineError::Internal(error.to_string()))?
            .remove(&mount_point)
            .ok_or_else(|| EngineError::NotMounted(mount_point.clone()))?;
        file_system.stop();
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
        return Ok(format!("{}:\\", (bytes[0] as char).to_ascii_uppercase()));
    }
    let path = Path::new(&value);
    if !path.is_absolute() {
        return Err(EngineError::InvalidConfig(
            "Windows mount point must be a drive letter or an absolute directory path".into(),
        ));
    }
    if !path.is_dir() {
        return Err(EngineError::InvalidConfig(format!(
            "mount directory does not exist: {value}"
        )));
    }
    Ok(value.trim_end_matches('\\').to_string())
}

fn file_info(metadata: &FileMetadata) -> FileInfo {
    let attributes = if metadata.kind == EntryKind::Directory {
        FileAttributes::DIRECTORY
    } else {
        FileAttributes::NORMAL
    };
    FileInfo::default()
        .set_file_attributes(attributes)
        .set_file_size(metadata.size)
        .set_allocation_size(metadata.size)
        .set_time(filetime_now())
        .to_owned()
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

    fn metadata(kind: EntryKind) -> FileMetadata {
        FileMetadata {
            kind,
            size: 0,
            modified: None,
        }
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
}
