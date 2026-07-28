mod error;
mod manager;
mod model;
mod persistence;
mod traits;
mod vfs;

pub use error::{EngineError, EngineResult, FsErrorCode};
pub use manager::MappingManager;
pub use model::{
    AuthMethod, MappingConfig, MappingRuntime, MappingState, Protocol, WebDavAuthMethod,
};
pub use persistence::{ConfigDocument, CONFIG_SCHEMA_VERSION};
pub use traits::{
    ConnectionSecrets, DirectoryEntry, EntryKind, FileMetadata, FileTimes, MountDriver,
    RemoteFileSystem,
};
pub use vfs::{DirectoryHandle, FileHandle, OpenOptions, RemoteVfs, VirtualFileSystem};
