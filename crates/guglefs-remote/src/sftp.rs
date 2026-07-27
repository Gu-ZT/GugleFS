use std::{
    io::SeekFrom,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, UNIX_EPOCH},
};

use async_trait::async_trait;
use guglefs_core::{
    AuthMethod, ConnectionSecrets, DirectoryEntry, EngineError, EngineResult, EntryKind,
    FileMetadata, FsErrorCode, MappingConfig, Protocol, RemoteFileSystem,
};
use russh::{client, keys::PrivateKeyWithHashAlg, Disconnect};
use russh_sftp::{
    client::{error::Error as SftpError, fs::Metadata, SftpSession},
    protocol::{FileAttributes, OpenFlags, StatusCode},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::Mutex,
};

const SSH_TIMEOUT: Duration = Duration::from_secs(30);

enum SftpAuth {
    Password(String),
    PrivateKey {
        key: String,
        passphrase: Option<String>,
    },
}

struct HostKeyVerifier {
    expected: String,
}

impl client::Handler for HostKeyVerifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(key_fingerprint(server_public_key) == self.expected)
    }
}

struct HostKeyCapture {
    fingerprint: Arc<StdMutex<Option<String>>>,
}

impl client::Handler for HostKeyCapture {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        *self
            .fingerprint
            .lock()
            .map_err(|_| russh::Error::Inconsistent)? = Some(key_fingerprint(server_public_key));
        Ok(true)
    }
}

struct SftpConnection {
    ssh: client::Handle<HostKeyVerifier>,
    sftp: SftpSession,
}

pub struct SftpFileSystem {
    host: String,
    port: u16,
    username: String,
    auth: SftpAuth,
    root: String,
    host_key_fingerprint: String,
    connection: Mutex<Option<SftpConnection>>,
}

impl std::fmt::Debug for SftpFileSystem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SftpFileSystem")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("root", &self.root)
            .field("host_key_fingerprint", &self.host_key_fingerprint)
            .finish_non_exhaustive()
    }
}

impl SftpFileSystem {
    pub fn from_config(config: &MappingConfig, secrets: ConnectionSecrets) -> EngineResult<Self> {
        if config.protocol != Protocol::Sftp {
            return Err(EngineError::InvalidConfig(
                "SFTP backend requires an SFTP mapping".into(),
            ));
        }
        let username = config
            .username
            .as_deref()
            .filter(|username| !username.trim().is_empty())
            .ok_or_else(|| EngineError::InvalidConfig("SFTP username is required".into()))?
            .to_string();
        let host_key_fingerprint = config
            .host_key_fingerprint
            .as_deref()
            .filter(|fingerprint| !fingerprint.trim().is_empty())
            .ok_or_else(|| {
                EngineError::InvalidConfig("SFTP host key fingerprint is required".into())
            })?
            .to_string();
        let auth = match &config.auth {
            AuthMethod::Password { .. } => {
                SftpAuth::Password(secrets.credential.ok_or_else(|| {
                    EngineError::InvalidConfig("SFTP password is required".into())
                })?)
            }
            AuthMethod::PrivateKey {
                key_path, key_id, ..
            } => {
                let key = if let Some(key) = secrets.private_key {
                    key
                } else if let Some(path) =
                    key_path.as_deref().filter(|path| !path.trim().is_empty())
                {
                    std::fs::read_to_string(path).map_err(|error| {
                        EngineError::InvalidConfig(format!(
                            "read SFTP private key {}: {error}",
                            path
                        ))
                    })?
                } else if key_id.is_some() {
                    return Err(EngineError::InvalidConfig(
                        "stored SFTP private key was not supplied".into(),
                    ));
                } else {
                    return Err(EngineError::InvalidConfig(
                        "SFTP private key is required".into(),
                    ));
                };
                SftpAuth::PrivateKey {
                    key,
                    passphrase: secrets.credential,
                }
            }
            AuthMethod::Anonymous => {
                return Err(EngineError::InvalidConfig(
                    "anonymous authentication is not supported by SFTP".into(),
                ));
            }
        };
        Ok(Self {
            host: config.host.clone(),
            port: config.port,
            username,
            auth,
            root: normalize_root(&config.remote_path),
            host_key_fingerprint,
            connection: Mutex::new(None),
        })
    }

    async fn open_connection(&self) -> EngineResult<SftpConnection> {
        let config = Arc::new(ssh_config());
        let verifier = HostKeyVerifier {
            expected: self.host_key_fingerprint.clone(),
        };
        let mut ssh = client::connect(config, (self.host.as_str(), self.port), verifier)
            .await
            .map_err(|error| {
                EngineError::Remote(format!(
                    "SSH connect or host key verification failed: {error}"
                ))
            })?;
        let authentication = match &self.auth {
            SftpAuth::Password(password) => ssh
                .authenticate_password(&self.username, password)
                .await
                .map_err(|error| {
                    EngineError::Remote(format!("SSH password authentication: {error}"))
                })?,
            SftpAuth::PrivateKey { key, passphrase } => {
                let key = russh::keys::decode_secret_key(key, passphrase.as_deref()).map_err(
                    |error| EngineError::InvalidConfig(format!("decode SSH private key: {error}")),
                )?;
                let hash_algorithm = ssh
                    .best_supported_rsa_hash()
                    .await
                    .map_err(|error| {
                        EngineError::Remote(format!("negotiate SSH key algorithm: {error}"))
                    })?
                    .flatten();
                ssh.authenticate_publickey(
                    &self.username,
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash_algorithm),
                )
                .await
                .map_err(|error| {
                    EngineError::Remote(format!("SSH private key authentication: {error}"))
                })?
            }
        };
        if !authentication.success() {
            return Err(EngineError::filesystem(
                FsErrorCode::PermissionDenied,
                "SSH authentication was rejected",
            ));
        }
        let channel = ssh
            .channel_open_session()
            .await
            .map_err(|error| EngineError::Remote(format!("open SSH session channel: {error}")))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|error| EngineError::Remote(format!("request SFTP subsystem: {error}")))?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|error| sftp_error("initialize session", error))?;
        sftp.set_timeout(30);
        Ok(SftpConnection { ssh, sftp })
    }

    async fn connection(
        &self,
    ) -> EngineResult<tokio::sync::MutexGuard<'_, Option<SftpConnection>>> {
        let mut connection = self.connection.lock().await;
        if connection.is_none() {
            *connection = Some(self.open_connection().await?);
        }
        Ok(connection)
    }

    fn remote_path(&self, path: &str) -> String {
        let suffix = path.trim_matches('/');
        if suffix.is_empty() {
            self.root.clone()
        } else if self.root == "/" {
            format!("/{suffix}")
        } else {
            format!("{}/{suffix}", self.root.trim_end_matches('/'))
        }
    }
}

#[async_trait]
impl RemoteFileSystem for SftpFileSystem {
    async fn connect(&self) -> EngineResult<()> {
        let remote_path = self.root.clone();
        self.connection()
            .await?
            .as_ref()
            .expect("SFTP connection initialized")
            .sftp
            .metadata(remote_path)
            .await
            .map(|_| ())
            .map_err(|error| sftp_error("read root metadata", error))
    }

    async fn disconnect(&self) -> EngineResult<()> {
        if let Some(connection) = self.connection.lock().await.take() {
            let _ = connection.sftp.close().await;
            connection
                .ssh
                .disconnect(Disconnect::ByApplication, "GugleFS unmounted", "en")
                .await
                .map_err(|error| EngineError::Remote(format!("disconnect SSH session: {error}")))?;
        }
        Ok(())
    }

    async fn metadata(&self, path: &str) -> EngineResult<FileMetadata> {
        let remote_path = self.remote_path(path);
        let connection = self.connection().await?;
        connection
            .as_ref()
            .expect("SFTP connection initialized")
            .sftp
            .metadata(remote_path)
            .await
            .map(file_metadata)
            .map_err(|error| sftp_error("read metadata", error))
    }

    async fn read_dir(&self, path: &str) -> EngineResult<Vec<DirectoryEntry>> {
        let remote_path = self.remote_path(path);
        let connection = self.connection().await?;
        let entries = connection
            .as_ref()
            .expect("SFTP connection initialized")
            .sftp
            .read_dir(remote_path)
            .await
            .map_err(|error| sftp_error("read directory", error))?;
        Ok(entries
            .map(|entry| {
                let name = entry.file_name();
                let entry_path = if path == "/" {
                    format!("/{name}")
                } else {
                    format!("{}/{name}", path.trim_end_matches('/'))
                };
                DirectoryEntry {
                    path: entry_path,
                    name,
                    metadata: file_metadata(entry.metadata()),
                }
            })
            .collect())
    }

    async fn read_range(&self, path: &str, offset: u64, length: u64) -> EngineResult<Vec<u8>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let remote_path = self.remote_path(path);
        let connection = self.connection().await?;
        let mut file = connection
            .as_ref()
            .expect("SFTP connection initialized")
            .sftp
            .open(remote_path)
            .await
            .map_err(|error| sftp_error("open file for reading", error))?;
        file.seek(SeekFrom::Start(offset))
            .await
            .map_err(|error| EngineError::Remote(format!("seek SFTP file: {error}")))?;
        let capacity = usize::try_from(length.min(1024 * 1024)).unwrap_or(1024 * 1024);
        let mut data = Vec::with_capacity(capacity);
        (&mut file)
            .take(length)
            .read_to_end(&mut data)
            .await
            .map_err(|error| EngineError::Remote(format!("read SFTP file: {error}")))?;
        file.shutdown()
            .await
            .map_err(|error| EngineError::Remote(format!("close SFTP file: {error}")))?;
        Ok(data)
    }

    async fn write(&self, path: &str, offset: u64, data: Vec<u8>) -> EngineResult<u64> {
        let remote_path = self.remote_path(path);
        let written = data.len() as u64;
        let connection = self.connection().await?;
        let mut file = connection
            .as_ref()
            .expect("SFTP connection initialized")
            .sftp
            .open_with_flags(remote_path, OpenFlags::WRITE)
            .await
            .map_err(|error| sftp_error("open file for writing", error))?;
        file.seek(SeekFrom::Start(offset))
            .await
            .map_err(|error| EngineError::Remote(format!("seek SFTP file: {error}")))?;
        file.write_all(&data)
            .await
            .map_err(|error| EngineError::Remote(format!("write SFTP file: {error}")))?;
        file.flush()
            .await
            .map_err(|error| EngineError::Remote(format!("flush SFTP file: {error}")))?;
        file.shutdown()
            .await
            .map_err(|error| EngineError::Remote(format!("close SFTP file: {error}")))?;
        Ok(written)
    }

    async fn create_file(&self, path: &str) -> EngineResult<()> {
        let remote_path = self.remote_path(path);
        let connection = self.connection().await?;
        let mut file = connection
            .as_ref()
            .expect("SFTP connection initialized")
            .sftp
            .open_with_flags(
                remote_path,
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
            )
            .await
            .map_err(|error| sftp_error("create file", error))?;
        file.shutdown()
            .await
            .map_err(|error| EngineError::Remote(format!("close new SFTP file: {error}")))
    }

    async fn create_dir(&self, path: &str) -> EngineResult<()> {
        let remote_path = self.remote_path(path);
        let connection = self.connection().await?;
        connection
            .as_ref()
            .expect("SFTP connection initialized")
            .sftp
            .create_dir(remote_path)
            .await
            .map_err(|error| sftp_error("create directory", error))
    }

    async fn remove(&self, path: &str, directory: bool) -> EngineResult<()> {
        let remote_path = self.remote_path(path);
        let connection = self.connection().await?;
        let sftp = &connection
            .as_ref()
            .expect("SFTP connection initialized")
            .sftp;
        let result = if directory {
            sftp.remove_dir(remote_path).await
        } else {
            sftp.remove_file(remote_path).await
        };
        result.map_err(|error| sftp_error("remove", error))
    }

    async fn rename(&self, from: &str, to: &str) -> EngineResult<()> {
        let from = self.remote_path(from);
        let to = self.remote_path(to);
        let connection = self.connection().await?;
        connection
            .as_ref()
            .expect("SFTP connection initialized")
            .sftp
            .rename(from, to)
            .await
            .map_err(|error| sftp_error("rename", error))
    }

    async fn truncate(&self, path: &str, size: u64) -> EngineResult<()> {
        let remote_path = self.remote_path(path);
        let connection = self.connection().await?;
        let mut attributes = FileAttributes::empty();
        attributes.size = Some(size);
        connection
            .as_ref()
            .expect("SFTP connection initialized")
            .sftp
            .set_metadata(remote_path, attributes)
            .await
            .map_err(|error| sftp_error("truncate", error))
    }
}

pub async fn inspect_host_key(host: &str, port: u16) -> EngineResult<String> {
    let fingerprint = Arc::new(StdMutex::new(None));
    let capture = HostKeyCapture {
        fingerprint: fingerprint.clone(),
    };
    let ssh = client::connect(Arc::new(ssh_config()), (host, port), capture)
        .await
        .map_err(|error| EngineError::Remote(format!("inspect SSH host key: {error}")))?;
    let _ = ssh
        .disconnect(
            Disconnect::ByApplication,
            "GugleFS host key inspection",
            "en",
        )
        .await;
    let fingerprint = fingerprint
        .lock()
        .map_err(|error| EngineError::Internal(format!("read SSH host key fingerprint: {error}")))?
        .clone();
    fingerprint.ok_or_else(|| EngineError::Remote("SSH server did not provide a host key".into()))
}

fn ssh_config() -> client::Config {
    client::Config {
        inactivity_timeout: Some(SSH_TIMEOUT),
        ..Default::default()
    }
}

fn key_fingerprint(key: &russh::keys::PublicKey) -> String {
    key.fingerprint(russh::keys::ssh_key::HashAlg::Sha256)
        .to_string()
}

fn normalize_root(path: &str) -> String {
    let path = path.trim().replace('\\', "/");
    let path = path.trim_matches('/');
    if path.is_empty() {
        "/".into()
    } else {
        format!("/{path}")
    }
}

fn file_metadata(metadata: Metadata) -> FileMetadata {
    FileMetadata {
        kind: if metadata.file_type().is_dir() {
            EntryKind::Directory
        } else {
            EntryKind::File
        },
        size: metadata.len(),
        modified: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs().to_string()),
    }
}

fn sftp_error(operation: &str, error: SftpError) -> EngineError {
    let code = match &error {
        SftpError::Status(status) => match status.status_code {
            StatusCode::NoSuchFile => FsErrorCode::NotFound,
            StatusCode::PermissionDenied => FsErrorCode::PermissionDenied,
            _ => FsErrorCode::RemoteIo,
        },
        _ => FsErrorCode::RemoteIo,
    };
    EngineError::filesystem(code, format!("SFTP {operation}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_sftp_roots() {
        assert_eq!(normalize_root("/"), "/");
        assert_eq!(normalize_root("\\home\\user\\files\\"), "/home/user/files");
    }

    #[test]
    fn maps_sftp_status_errors_to_vfs_codes() {
        let error = SftpError::Status(russh_sftp::protocol::Status {
            id: 1,
            status_code: StatusCode::NoSuchFile,
            error_message: "missing".into(),
            language_tag: "en".into(),
        });
        assert_eq!(sftp_error("metadata", error).code(), FsErrorCode::NotFound);
    }
}
