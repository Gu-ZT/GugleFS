use std::{convert::TryFrom, future::Future, io::Cursor, pin::Pin, time::Duration};

use async_trait::async_trait;
use guglefs_core::{
    AuthMethod, DirectoryEntry, EngineError, EngineResult, EntryKind, FileMetadata, FsErrorCode,
    MappingConfig, Protocol, RemoteFileSystem,
};
use suppaftp::{
    list::{File, ListParser},
    tokio::{
        AsyncFtpStream, AsyncNativeTlsConnector, AsyncNativeTlsFtpStream, ImplAsyncFtpStream,
        TokioTlsStream,
    },
    types::FileType,
    FtpError,
};
use tokio::{io::AsyncReadExt, sync::Mutex, time};

use crate::proxy::{connect_target, system_proxy, ProxyConfig};

const FTP_CONTROL_TIMEOUT: Duration = Duration::from_secs(25);
const FTP_TRANSFER_TIMEOUT: Duration = Duration::from_secs(110);

type FtpOperation<'a, T> = Pin<Box<dyn Future<Output = EngineResult<T>> + Send + 'a>>;

enum FtpConnection {
    Plain(AsyncFtpStream),
    Secure(AsyncNativeTlsFtpStream),
}

fn add_proxy_data_stream<T>(
    stream: ImplAsyncFtpStream<T>,
    proxy: Option<&ProxyConfig>,
) -> ImplAsyncFtpStream<T>
where
    T: TokioTlsStream + Send,
{
    if let Some(proxy) = proxy.cloned() {
        stream.passive_stream_builder(move |address| {
            let proxy = proxy.clone();
            Box::pin(async move {
                proxy
                    .connect(&address.ip().to_string(), address.port())
                    .await
                    .map_err(FtpError::ConnectionError)
            })
        })
    } else {
        stream
    }
}

impl FtpConnection {
    async fn open(
        host: &str,
        port: u16,
        username: &str,
        password: &str,
        explicit_tls: bool,
        proxy: Option<&ProxyConfig>,
    ) -> EngineResult<Self> {
        let mut connection = if explicit_tls {
            let connector =
                AsyncNativeTlsConnector::from(suppaftp::async_native_tls::TlsConnector::new());
            let stream = match proxy {
                Some(proxy) => AsyncNativeTlsFtpStream::connect_with_stream(
                    connect_target(host, port, Some(proxy)).await?,
                )
                .await
                .map_err(|error| ftp_error("connect", error))?,
                None => AsyncNativeTlsFtpStream::connect((host, port))
                    .await
                    .map_err(|error| ftp_error("connect", error))?,
            };
            let stream = add_proxy_data_stream(stream, proxy);
            FtpConnection::Secure(
                stream
                    .into_secure(connector, host)
                    .await
                    .map_err(|error| ftp_error("start explicit TLS", error))?,
            )
        } else {
            let stream = match proxy {
                Some(proxy) => AsyncFtpStream::connect_with_stream(
                    connect_target(host, port, Some(proxy)).await?,
                )
                .await
                .map_err(|error| ftp_error("connect", error))?,
                None => AsyncFtpStream::connect((host, port))
                    .await
                    .map_err(|error| ftp_error("connect", error))?,
            };
            let stream = add_proxy_data_stream(stream, proxy);
            FtpConnection::Plain(stream)
        };
        connection.login(username, password).await?;
        connection.transfer_type(FileType::Binary).await?;
        Ok(connection)
    }

    async fn login(&mut self, username: &str, password: &str) -> EngineResult<()> {
        match self {
            Self::Plain(stream) => stream.login(username, password).await,
            Self::Secure(stream) => stream.login(username, password).await,
        }
        .map_err(|error| ftp_error("authenticate", error))
    }

    async fn transfer_type(&mut self, file_type: FileType) -> EngineResult<()> {
        match self {
            Self::Plain(stream) => stream.transfer_type(file_type).await,
            Self::Secure(stream) => stream.transfer_type(file_type).await,
        }
        .map_err(|error| ftp_error("select binary transfer mode", error))
    }

    async fn noop(&mut self) -> EngineResult<()> {
        match self {
            Self::Plain(stream) => stream.noop().await,
            Self::Secure(stream) => stream.noop().await,
        }
        .map_err(|error| ftp_error("keep alive", error))
    }

    async fn quit(&mut self) -> EngineResult<()> {
        match self {
            Self::Plain(stream) => stream.quit().await,
            Self::Secure(stream) => stream.quit().await,
        }
        .map_err(|error| ftp_error("disconnect", error))
    }

    async fn mlsd(&mut self, path: &str) -> Result<Vec<String>, suppaftp::FtpError> {
        match self {
            Self::Plain(stream) => stream.mlsd(Some(path)).await,
            Self::Secure(stream) => stream.mlsd(Some(path)).await,
        }
    }

    async fn mlst(&mut self, path: &str) -> Result<String, suppaftp::FtpError> {
        match self {
            Self::Plain(stream) => stream.mlst(Some(path)).await,
            Self::Secure(stream) => stream.mlst(Some(path)).await,
        }
    }

    async fn list(&mut self, path: &str) -> Result<Vec<String>, suppaftp::FtpError> {
        match self {
            Self::Plain(stream) => stream.list(Some(path)).await,
            Self::Secure(stream) => stream.list(Some(path)).await,
        }
    }

    async fn list_entries(&mut self, path: &str) -> EngineResult<Vec<File>> {
        match self.mlsd(path).await {
            Ok(lines) => lines
                .into_iter()
                .filter(|line| {
                    let line = line.to_ascii_lowercase();
                    !line.contains("type=cdir") && !line.contains("type=pdir")
                })
                .map(|line| {
                    ListParser::parse_mlsd(&line).map_err(|error| {
                        EngineError::Remote(format!("parse FTP MLSD entry for {path}: {error}"))
                    })
                })
                .collect(),
            Err(_) => self
                .list(path)
                .await
                .map_err(|error| ftp_error("list directory", error))?
                .into_iter()
                .filter(|line| !line.trim().is_empty() && !line.starts_with("total "))
                .map(|line| {
                    File::try_from(line.as_str()).map_err(|error| {
                        EngineError::Remote(format!("parse FTP LIST entry for {path}: {error}"))
                    })
                })
                .collect(),
        }
    }

    async fn metadata(&mut self, path: &str) -> EngineResult<FileMetadata> {
        if let Ok(line) = self.mlst(path).await {
            return ListParser::parse_mlst(&line)
                .map(file_metadata)
                .map_err(|error| {
                    EngineError::Remote(format!("parse FTP MLST for {path}: {error}"))
                });
        }
        let (parent, name) = split_parent(path);
        self.list_entries(parent)
            .await?
            .into_iter()
            .find(|entry| entry.name() == name)
            .map(file_metadata)
            .ok_or_else(|| {
                EngineError::filesystem(FsErrorCode::NotFound, format!("FTP path: {path}"))
            })
    }

    async fn download(&mut self, path: &str, offset: u64) -> EngineResult<Vec<u8>> {
        let offset = usize::try_from(offset)
            .map_err(|_| EngineError::InvalidConfig("FTP read offset is too large".into()))?;
        match self {
            Self::Plain(stream) => {
                stream
                    .resume_transfer(offset)
                    .await
                    .map_err(|error| ftp_error("resume download", error))?;
                let mut data_stream = stream
                    .retr_as_stream(path)
                    .await
                    .map_err(|error| ftp_error("download", error))?;
                let mut data = Vec::new();
                data_stream
                    .read_to_end(&mut data)
                    .await
                    .map_err(|error| EngineError::Remote(format!("read FTP download: {error}")))?;
                stream
                    .finalize_retr_stream(data_stream)
                    .await
                    .map_err(|error| ftp_error("finish download", error))?;
                Ok(data)
            }
            Self::Secure(stream) => {
                stream
                    .resume_transfer(offset)
                    .await
                    .map_err(|error| ftp_error("resume download", error))?;
                let mut data_stream = stream
                    .retr_as_stream(path)
                    .await
                    .map_err(|error| ftp_error("download", error))?;
                let mut data = Vec::new();
                data_stream
                    .read_to_end(&mut data)
                    .await
                    .map_err(|error| EngineError::Remote(format!("read FTPS download: {error}")))?;
                stream
                    .finalize_retr_stream(data_stream)
                    .await
                    .map_err(|error| ftp_error("finish download", error))?;
                Ok(data)
            }
        }
    }

    async fn upload(&mut self, path: &str, data: Vec<u8>) -> EngineResult<u64> {
        let mut reader = Cursor::new(data);
        match self {
            Self::Plain(stream) => stream.put_file(path, &mut reader).await,
            Self::Secure(stream) => stream.put_file(path, &mut reader).await,
        }
        .map_err(|error| ftp_error("upload", error))
    }

    async fn mkdir(&mut self, path: &str) -> EngineResult<()> {
        match self {
            Self::Plain(stream) => stream.mkdir(path).await,
            Self::Secure(stream) => stream.mkdir(path).await,
        }
        .map_err(|error| ftp_error("create directory", error))
    }

    async fn remove(&mut self, path: &str, directory: bool) -> EngineResult<()> {
        let result = match (self, directory) {
            (Self::Plain(stream), true) => stream.rmdir(path).await,
            (Self::Plain(stream), false) => stream.rm(path).await,
            (Self::Secure(stream), true) => stream.rmdir(path).await,
            (Self::Secure(stream), false) => stream.rm(path).await,
        };
        result.map_err(|error| ftp_error("remove", error))
    }

    async fn rename(&mut self, from: &str, to: &str) -> EngineResult<()> {
        match self {
            Self::Plain(stream) => stream.rename(from, to).await,
            Self::Secure(stream) => stream.rename(from, to).await,
        }
        .map_err(|error| ftp_error("rename", error))
    }
}

pub struct FtpFileSystem {
    host: String,
    port: u16,
    username: String,
    password: String,
    root: String,
    explicit_tls: bool,
    proxy: Option<ProxyConfig>,
    connection: Mutex<Option<FtpConnection>>,
}

impl std::fmt::Debug for FtpFileSystem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FtpFileSystem")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("root", &self.root)
            .field("explicit_tls", &self.explicit_tls)
            .finish_non_exhaustive()
    }
}

impl FtpFileSystem {
    pub fn from_config(config: &MappingConfig, password: Option<String>) -> EngineResult<Self> {
        if config.protocol != Protocol::Ftp {
            return Err(EngineError::InvalidConfig(
                "FTP backend requires an FTP mapping".into(),
            ));
        }
        let (username, password) = match &config.auth {
            AuthMethod::Password { .. } => (
                config
                    .username
                    .clone()
                    .ok_or_else(|| EngineError::InvalidConfig("FTP username is required".into()))?,
                password
                    .ok_or_else(|| EngineError::InvalidConfig("FTP password is required".into()))?,
            ),
            AuthMethod::Anonymous => (
                "anonymous".into(),
                password.unwrap_or_else(|| "anonymous@guglefs.local".into()),
            ),
            AuthMethod::PrivateKey { .. } | AuthMethod::SshAgent => {
                return Err(EngineError::InvalidConfig(
                    "SSH authentication is not valid for FTP".into(),
                ));
            }
        };
        let proxy = system_proxy(config)?;
        Ok(Self {
            host: config.host.clone(),
            port: config.port,
            username,
            password,
            root: normalize_root(&config.remote_path),
            explicit_tls: config.ftp_tls,
            proxy,
            connection: Mutex::new(None),
        })
    }

    async fn connection(&self) -> EngineResult<tokio::sync::MutexGuard<'_, Option<FtpConnection>>> {
        let mut connection = self.connection.lock().await;
        if connection.is_none() {
            *connection = Some(
                FtpConnection::open(
                    &self.host,
                    self.port,
                    &self.username,
                    &self.password,
                    self.explicit_tls,
                    self.proxy.as_ref(),
                )
                .await?,
            );
        }
        Ok(connection)
    }

    async fn execute<T, F>(&self, timeout: Duration, operation: F) -> EngineResult<T>
    where
        T: Send,
        F: for<'a> FnOnce(&'a mut FtpConnection) -> FtpOperation<'a, T> + Send,
    {
        let mut connection = self.connection().await?;
        let result = time::timeout(
            timeout,
            operation(connection.as_mut().expect("FTP connection initialized")),
        )
        .await
        .map_err(|_| {
            EngineError::filesystem(
                FsErrorCode::RemoteIo,
                format!(
                    "FTP operation timed out after {} seconds",
                    timeout.as_secs()
                ),
            )
        })
        .and_then(|result| result);
        reset_failed_connection(&mut connection, &result);
        result
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
impl RemoteFileSystem for FtpFileSystem {
    async fn connect(&self) -> EngineResult<()> {
        self.execute(FTP_CONTROL_TIMEOUT, |connection| {
            Box::pin(async move { connection.noop().await })
        })
        .await
    }

    async fn disconnect(&self) -> EngineResult<()> {
        if let Some(mut connection) = self.connection.lock().await.take() {
            time::timeout(FTP_CONTROL_TIMEOUT, connection.quit())
                .await
                .map_err(|_| {
                    EngineError::filesystem(FsErrorCode::RemoteIo, "FTP disconnect timed out")
                })??;
        }
        Ok(())
    }

    async fn metadata(&self, path: &str) -> EngineResult<FileMetadata> {
        let is_root = path == "/";
        let remote_path = self.remote_path(path);
        self.execute(FTP_CONTROL_TIMEOUT, move |connection| {
            Box::pin(async move {
                if is_root {
                    connection
                        .list_entries(&remote_path)
                        .await
                        .map(|_| FileMetadata {
                            kind: EntryKind::Directory,
                            size: 0,
                            modified: None,
                        })
                } else {
                    connection.metadata(&remote_path).await
                }
            })
        })
        .await
    }

    async fn read_dir(&self, path: &str) -> EngineResult<Vec<DirectoryEntry>> {
        let remote_path = self.remote_path(path);
        let entries = self
            .execute(FTP_CONTROL_TIMEOUT, move |connection| {
                Box::pin(async move { connection.list_entries(&remote_path).await })
            })
            .await?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.name() != "." && entry.name() != "..")
            .map(|entry| {
                let name = entry.name().to_string();
                let entry_path = if path == "/" {
                    format!("/{name}")
                } else {
                    format!("{}/{name}", path.trim_end_matches('/'))
                };
                DirectoryEntry {
                    path: entry_path,
                    name,
                    metadata: file_metadata(entry),
                }
            })
            .collect())
    }

    async fn read_range(&self, path: &str, offset: u64, length: u64) -> EngineResult<Vec<u8>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let remote_path = self.remote_path(path);
        let mut data = self
            .execute(FTP_TRANSFER_TIMEOUT, move |connection| {
                Box::pin(async move { connection.download(&remote_path, offset).await })
            })
            .await?;
        let length = usize::try_from(length)
            .map_err(|_| EngineError::InvalidConfig("FTP read length is too large".into()))?;
        data.truncate(length);
        Ok(data)
    }

    async fn write(&self, path: &str, offset: u64, data: Vec<u8>) -> EngineResult<u64> {
        let remote_path = self.remote_path(path);
        let written = data.len() as u64;
        self.execute(FTP_TRANSFER_TIMEOUT, move |connection| {
            Box::pin(async move {
                let mut content = connection.download(&remote_path, 0).await?;
                apply_write(&mut content, offset, &data)?;
                connection
                    .upload(&remote_path, content)
                    .await
                    .map(|_| written)
            })
        })
        .await
    }

    async fn create_file(&self, path: &str) -> EngineResult<()> {
        let remote_path = self.remote_path(path);
        self.execute(FTP_CONTROL_TIMEOUT, move |connection| {
            Box::pin(async move {
                connection
                    .upload(&remote_path, Vec::new())
                    .await
                    .map(|_| ())
            })
        })
        .await
    }

    async fn create_dir(&self, path: &str) -> EngineResult<()> {
        let remote_path = self.remote_path(path);
        self.execute(FTP_CONTROL_TIMEOUT, move |connection| {
            Box::pin(async move { connection.mkdir(&remote_path).await })
        })
        .await
    }

    async fn remove(&self, path: &str, directory: bool) -> EngineResult<()> {
        let remote_path = self.remote_path(path);
        self.execute(FTP_CONTROL_TIMEOUT, move |connection| {
            Box::pin(async move { connection.remove(&remote_path, directory).await })
        })
        .await
    }

    async fn rename(&self, from: &str, to: &str) -> EngineResult<()> {
        let from = self.remote_path(from);
        let to = self.remote_path(to);
        self.execute(FTP_CONTROL_TIMEOUT, move |connection| {
            Box::pin(async move { connection.rename(&from, &to).await })
        })
        .await
    }

    async fn truncate(&self, path: &str, size: u64) -> EngineResult<()> {
        let remote_path = self.remote_path(path);
        self.execute(FTP_TRANSFER_TIMEOUT, move |connection| {
            Box::pin(async move {
                let mut content = if size == 0 {
                    Vec::new()
                } else {
                    connection.download(&remote_path, 0).await?
                };
                let size = usize::try_from(size)
                    .map_err(|_| EngineError::InvalidConfig("FTP file size is too large".into()))?;
                content.resize(size, 0);
                connection.upload(&remote_path, content).await.map(|_| ())
            })
        })
        .await
    }
}

fn reset_failed_connection<T>(connection: &mut Option<FtpConnection>, result: &EngineResult<T>) {
    if result
        .as_ref()
        .is_err_and(|error| error.code() == FsErrorCode::RemoteIo)
    {
        *connection = None;
    }
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

fn split_parent(path: &str) -> (&str, &str) {
    let path = path.trim_end_matches('/');
    match path.rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some((parent, name)) => (parent, name),
        None => ("/", path),
    }
}

fn apply_write(content: &mut Vec<u8>, offset: u64, data: &[u8]) -> EngineResult<()> {
    let start = usize::try_from(offset)
        .map_err(|_| EngineError::InvalidConfig("FTP write offset is too large".into()))?;
    let end = start
        .checked_add(data.len())
        .ok_or_else(|| EngineError::InvalidConfig("FTP write range overflows usize".into()))?;
    if content.len() < end {
        content.resize(end, 0);
    }
    content[start..end].copy_from_slice(data);
    Ok(())
}

fn file_metadata(file: File) -> FileMetadata {
    FileMetadata {
        kind: if file.is_directory() {
            EntryKind::Directory
        } else {
            EntryKind::File
        },
        size: file.size() as u64,
        modified: None,
    }
}

fn ftp_error(operation: &str, error: suppaftp::FtpError) -> EngineError {
    EngineError::Remote(format!("FTP {operation}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_remote_roots_and_vfs_paths() {
        assert_eq!(normalize_root("/"), "/");
        assert_eq!(normalize_root("\\home\\files\\"), "/home/files");
        assert_eq!(
            split_parent("/home/files/readme.txt"),
            ("/home/files", "readme.txt")
        );
    }

    #[test]
    fn parses_machine_readable_ftp_entries() {
        let file = ListParser::parse_mlsd(
            "modify=20240102030405;size=12;type=file;unique=abc; readme.txt",
        )
        .unwrap();
        assert_eq!(file.name(), "readme.txt");
        assert_eq!(file_metadata(file).size, 12);

        let directory = ListParser::parse_mlsd("modify=20240102030405;type=dir; docs").unwrap();
        assert_eq!(file_metadata(directory).kind, EntryKind::Directory);
    }

    #[test]
    fn random_writes_preserve_existing_bytes_and_zero_fill_gaps() {
        let mut existing = b"abcdef".to_vec();
        apply_write(&mut existing, 0, b"XY").unwrap();
        assert_eq!(existing, b"XYcdef");

        apply_write(&mut existing, 8, b"Z").unwrap();
        assert_eq!(existing, b"XYcdef\0\0Z");
    }
}
