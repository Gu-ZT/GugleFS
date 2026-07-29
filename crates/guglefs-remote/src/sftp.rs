use std::{
    future::Future,
    io::SeekFrom,
    path::Path,
    pin::Pin,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, UNIX_EPOCH},
};

use async_trait::async_trait;
use guglefs_core::{
    AuthMethod, ConnectionSecrets, DirectoryEntry, EngineError, EngineResult, EntryKind,
    FileMetadata, FileSystemSpace, FsErrorCode, MappingConfig, Protocol, RemoteFileSystem,
};
use russh::{
    client::{self, KeyboardInteractiveAuthResponse, Prompt},
    keys::{
        agent::{
            client::{AgentClient, AgentStream},
            AgentIdentity,
        },
        PrivateKeyWithHashAlg,
    },
    Disconnect,
};
use russh_sftp::{
    client::{error::Error as SftpError, fs::Metadata, SftpSession},
    extensions::Statvfs,
    protocol::{FileAttributes, OpenFlags, StatusCode},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::Mutex,
};

use crate::proxy::{connect_target, proxy_for_target, system_proxy, ProxyConfig};

const SSH_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const MAX_KEYBOARD_INTERACTIVE_ROUNDS: usize = 8;

enum SftpAuth {
    Password(String),
    PrivateKey {
        key: String,
        passphrase: Option<String>,
    },
    Agent,
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

type SftpOperation<'a, T> = Pin<Box<dyn Future<Output = Result<T, SftpError>> + Send + 'a>>;

pub struct SftpFileSystem {
    host: String,
    port: u16,
    username: String,
    auth: SftpAuth,
    totp_required: bool,
    totp_code: StdMutex<Option<String>>,
    root: String,
    host_key_fingerprint: String,
    proxy: Option<ProxyConfig>,
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
            AuthMethod::SshAgent => {
                if secrets.credential.is_some() || secrets.private_key.is_some() {
                    return Err(EngineError::InvalidConfig(
                        "SSH Agent authentication does not accept stored credentials".into(),
                    ));
                }
                SftpAuth::Agent
            }
            AuthMethod::Anonymous => {
                return Err(EngineError::InvalidConfig(
                    "anonymous authentication is not supported by SFTP".into(),
                ));
            }
        };
        let totp_code = secrets.totp_code.map(validate_totp_code).transpose()?;
        if config.sftp_totp_required && totp_code.is_none() {
            return Err(sftp_totp_required_error());
        }
        let proxy = system_proxy(config)?;
        Ok(Self {
            host: config.host.clone(),
            port: config.port,
            username,
            auth,
            totp_required: config.sftp_totp_required,
            totp_code: StdMutex::new(totp_code),
            root: normalize_root(&config.remote_path),
            host_key_fingerprint,
            proxy,
            connection: Mutex::new(None),
        })
    }

    async fn open_ssh(&self) -> EngineResult<client::Handle<HostKeyVerifier>> {
        let config = Arc::new(ssh_config());
        let verifier = HostKeyVerifier {
            expected: self.host_key_fingerprint.clone(),
        };
        let stream = connect_target(&self.host, self.port, self.proxy.as_ref()).await?;
        client::connect_stream(config, stream, verifier)
            .await
            .map_err(|error| {
                EngineError::Remote(format!(
                    "SSH connect or host key verification failed: {error}"
                ))
            })
    }

    async fn primary_auth(
        &self,
        ssh: &mut client::Handle<HostKeyVerifier>,
    ) -> EngineResult<client::AuthResult> {
        match &self.auth {
            SftpAuth::Password(password) => ssh
                .authenticate_password(&self.username, password)
                .await
                .map_err(|error| {
                    EngineError::Remote(format!("SSH password authentication: {error}"))
                }),
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
                })
            }
            SftpAuth::Agent => authenticate_with_agent(ssh, &self.username).await,
        }
    }

    /// 探测服务器在主认证（密码或私钥）之后是否仍要求 keyboard-interactive
    /// 二次验证。SSH 服务器以 partial_success 失败并在剩余方法中列出
    /// keyboard-interactive 时视为需要 MFA。
    pub async fn detect_mfa_requirement(&self) -> EngineResult<bool> {
        let mut ssh = self.open_ssh().await?;
        let authentication = self.primary_auth(&mut ssh).await?;
        let _ = ssh
            .disconnect(Disconnect::ByApplication, "probe complete", "")
            .await;
        Ok(match authentication {
            client::AuthResult::Success => false,
            client::AuthResult::Failure {
                remaining_methods,
                partial_success,
            } => {
                partial_success
                    && remaining_methods.contains(&russh::MethodKind::KeyboardInteractive)
            }
        })
    }

    async fn open_connection(&self) -> EngineResult<SftpConnection> {
        let mut ssh = self.open_ssh().await?;
        let authentication = self.primary_auth(&mut ssh).await?;
        let authenticated = if authentication.success() {
            true
        } else if self.totp_required {
            let totp_code = self
                .totp_code
                .lock()
                .map_err(|error| EngineError::Internal(error.to_string()))?
                .take()
                .ok_or_else(sftp_totp_required_error)?;
            let password = match &self.auth {
                SftpAuth::Password(password) => Some(password.as_str()),
                SftpAuth::PrivateKey { .. } | SftpAuth::Agent => None,
            };
            authenticate_keyboard_interactive(&mut ssh, &self.username, password, &totp_code)
                .await?
        } else {
            false
        };
        if !authenticated {
            return Err(EngineError::filesystem(
                FsErrorCode::PermissionDenied,
                "SSH authentication or MFA TOTP was rejected",
            ));
        }
        let sftp = open_sftp_session(&mut ssh).await?;
        Ok(SftpConnection { ssh, sftp })
    }

    async fn connection(
        &self,
    ) -> EngineResult<tokio::sync::MutexGuard<'_, Option<SftpConnection>>> {
        let mut connection = self.connection.lock().await;
        if connection
            .as_ref()
            .is_some_and(|connection| connection.ssh.is_closed())
        {
            if self.totp_required {
                return Err(sftp_mfa_remount_required_error());
            }
            *connection = None;
        }
        if connection.is_none() {
            *connection = Some(self.open_connection().await?);
        }
        Ok(connection)
    }

    async fn execute<T, F>(
        &self,
        operation: &str,
        retry_after_reconnect: bool,
        mut callback: F,
    ) -> EngineResult<T>
    where
        T: Send,
        F: for<'a> FnMut(&'a SftpSession) -> SftpOperation<'a, T> + Send,
    {
        let attempts = if retry_after_reconnect { 2 } else { 1 };
        for attempt in 0..attempts {
            let mut connection = self.connection().await?;
            let result = callback(
                &connection
                    .as_ref()
                    .expect("SFTP connection initialized")
                    .sftp,
            )
            .await;
            match result {
                Ok(value) => return Ok(value),
                Err(error) => {
                    let reconnect = reconnectable_sftp_error(&error);
                    if reconnect {
                        let active = connection.as_mut().expect("SFTP connection initialized");
                        if !active.ssh.is_closed() {
                            if let Ok(sftp) = open_sftp_session(&mut active.ssh).await {
                                active.sftp = sftp;
                                if attempt + 1 < attempts {
                                    continue;
                                }
                                return Err(sftp_error(operation, error));
                            }
                            return Err(sftp_error(operation, error));
                        }
                        if self.totp_required {
                            return Err(sftp_mfa_remount_required_error());
                        }
                        *connection = None;
                    }
                    if reconnect && attempt + 1 < attempts {
                        continue;
                    }
                    return Err(sftp_error(operation, error));
                }
            }
        }
        unreachable!("SFTP operations always execute at least once")
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

async fn open_sftp_session(ssh: &mut client::Handle<HostKeyVerifier>) -> EngineResult<SftpSession> {
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
    Ok(sftp)
}

fn validate_totp_code(code: String) -> EngineResult<String> {
    let code = code.trim();
    if code.len() != 6 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(EngineError::InvalidConfig(
            "SFTP MFA TOTP code must contain exactly 6 digits".into(),
        ));
    }
    Ok(code.to_string())
}

fn sftp_totp_required_error() -> EngineError {
    EngineError::filesystem(
        FsErrorCode::PermissionDenied,
        "SFTP MFA requires a current 6-digit TOTP code for manual mount",
    )
}

fn sftp_mfa_remount_required_error() -> EngineError {
    EngineError::filesystem(
        FsErrorCode::PermissionDenied,
        "SFTP MFA transport closed; manually remount with a current TOTP code",
    )
}

async fn authenticate_keyboard_interactive(
    ssh: &mut client::Handle<HostKeyVerifier>,
    username: &str,
    password: Option<&str>,
    totp_code: &str,
) -> EngineResult<bool> {
    let mut response = ssh
        .authenticate_keyboard_interactive_start(username, None)
        .await
        .map_err(|error| {
            EngineError::Remote(format!(
                "start SSH keyboard-interactive authentication: {error}"
            ))
        })?;
    for _ in 0..MAX_KEYBOARD_INTERACTIVE_ROUNDS {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                let responses = keyboard_interactive_responses(&prompts, password, totp_code)?;
                response = ssh
                    .authenticate_keyboard_interactive_respond(responses)
                    .await
                    .map_err(|error| {
                        EngineError::Remote(format!(
                            "respond to SSH keyboard-interactive authentication: {error}"
                        ))
                    })?;
            }
        }
    }
    Err(EngineError::Remote(
        "SSH keyboard-interactive authentication exceeded the prompt limit".into(),
    ))
}

fn keyboard_interactive_responses(
    prompts: &[Prompt],
    password: Option<&str>,
    totp_code: &str,
) -> EngineResult<Vec<String>> {
    prompts
        .iter()
        .map(|prompt| {
            let normalized = prompt.prompt.trim().to_lowercase();
            if prompt_matches(
                &normalized,
                &[
                    "totp",
                    "otp",
                    "one-time",
                    "token",
                    "passcode",
                    "verification code",
                    "authenticator code",
                    "验证码",
                    "动态口令",
                    "令牌",
                ],
            ) {
                return Ok(totp_code.to_string());
            }
            if prompt_matches(&normalized, &["password", "密码"]) {
                return password.map(str::to_string).ok_or_else(|| {
                    EngineError::InvalidConfig(
                        "SFTP MFA requested a password after private-key authentication".into(),
                    )
                });
            }
            if prompts.len() == 1 {
                Ok(totp_code.to_string())
            } else {
                Err(EngineError::Remote(format!(
                    "unsupported SFTP MFA authentication prompt: {}",
                    prompt.prompt.trim()
                )))
            }
        })
        .collect()
}

fn prompt_matches(prompt: &str, patterns: &[&str]) -> bool {
    patterns.iter().any(|pattern| prompt.contains(pattern))
}

#[async_trait]
impl RemoteFileSystem for SftpFileSystem {
    async fn connect(&self) -> EngineResult<()> {
        let remote_path = self.root.clone();
        self.execute("read root metadata", true, move |sftp| {
            let remote_path = remote_path.clone();
            Box::pin(async move { sftp.metadata(remote_path).await.map(|_| ()) })
        })
        .await
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
        self.execute("read metadata", true, move |sftp| {
            let remote_path = remote_path.clone();
            Box::pin(async move { sftp.metadata(remote_path).await.map(file_metadata) })
        })
        .await
    }

    async fn filesystem_space(&self, path: &str) -> EngineResult<Option<FileSystemSpace>> {
        let remote_path = self.remote_path(path);
        self.execute("read filesystem space", true, move |sftp| {
            let remote_path = remote_path.clone();
            Box::pin(async move {
                sftp.fs_info(remote_path)
                    .await
                    .map(|info| info.map(statvfs_space))
            })
        })
        .await
    }

    async fn read_dir(&self, path: &str) -> EngineResult<Vec<DirectoryEntry>> {
        let remote_path = self.remote_path(path);
        let vfs_path = path.to_string();
        self.execute("read directory", true, move |sftp| {
            let remote_path = remote_path.clone();
            let vfs_path = vfs_path.clone();
            Box::pin(async move {
                let entries = sftp.read_dir(remote_path).await?;
                Ok(entries
                    .map(|entry| {
                        let name = entry.file_name();
                        let entry_path = if vfs_path == "/" {
                            format!("/{name}")
                        } else {
                            format!("{}/{name}", vfs_path.trim_end_matches('/'))
                        };
                        DirectoryEntry {
                            path: entry_path,
                            name,
                            metadata: file_metadata(entry.metadata()),
                        }
                    })
                    .collect())
            })
        })
        .await
    }

    async fn read_range(&self, path: &str, offset: u64, length: u64) -> EngineResult<Vec<u8>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let remote_path = self.remote_path(path);
        self.execute("read file", true, move |sftp| {
            let remote_path = remote_path.clone();
            Box::pin(async move {
                let mut file = sftp.open(remote_path).await?;
                file.seek(SeekFrom::Start(offset))
                    .await
                    .map_err(SftpError::from)?;
                let capacity = usize::try_from(length.min(1024 * 1024)).unwrap_or(1024 * 1024);
                let mut data = Vec::with_capacity(capacity);
                (&mut file)
                    .take(length)
                    .read_to_end(&mut data)
                    .await
                    .map_err(SftpError::from)?;
                file.shutdown().await.map_err(SftpError::from)?;
                Ok(data)
            })
        })
        .await
    }

    async fn write(&self, path: &str, offset: u64, data: Vec<u8>) -> EngineResult<u64> {
        let remote_path = self.remote_path(path);
        let written = data.len() as u64;
        self.execute("write file", true, move |sftp| {
            let remote_path = remote_path.clone();
            let data = data.clone();
            Box::pin(async move {
                let mut file = sftp
                    .open_with_flags(remote_path.clone(), OpenFlags::WRITE)
                    .await?;
                file.seek(SeekFrom::Start(offset))
                    .await
                    .map_err(SftpError::from)?;
                file.write_all(&data).await.map_err(SftpError::from)?;
                file.flush().await.map_err(SftpError::from)?;
                file.shutdown().await.map_err(SftpError::from)?;

                let mut verification = sftp.open(remote_path).await?;
                verification
                    .seek(SeekFrom::Start(offset))
                    .await
                    .map_err(SftpError::from)?;
                let mut persisted = vec![0; data.len()];
                verification
                    .read_exact(&mut persisted)
                    .await
                    .map_err(SftpError::from)?;
                verification.shutdown().await.map_err(SftpError::from)?;
                if persisted != data {
                    return Err(SftpError::UnexpectedBehavior(format!(
                        "SFTP write verification failed at offset {offset}: expected {} bytes",
                        data.len()
                    )));
                }
                Ok(written)
            })
        })
        .await
    }

    async fn create_file(&self, path: &str) -> EngineResult<()> {
        let remote_path = self.remote_path(path);
        self.execute("create file", false, move |sftp| {
            let remote_path = remote_path.clone();
            Box::pin(async move {
                let mut file = sftp
                    .open_with_flags(
                        remote_path,
                        OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                    )
                    .await?;
                file.shutdown().await.map_err(SftpError::from)
            })
        })
        .await
    }

    async fn create_dir(&self, path: &str) -> EngineResult<()> {
        let remote_path = self.remote_path(path);
        self.execute("create directory", false, move |sftp| {
            let remote_path = remote_path.clone();
            Box::pin(async move { sftp.create_dir(remote_path).await })
        })
        .await
    }

    async fn remove(&self, path: &str, directory: bool) -> EngineResult<()> {
        let remote_path = self.remote_path(path);
        self.execute("remove", false, move |sftp| {
            let remote_path = remote_path.clone();
            Box::pin(async move {
                if directory {
                    sftp.remove_dir(remote_path).await
                } else {
                    sftp.remove_file(remote_path).await
                }
            })
        })
        .await
    }

    async fn rename(&self, from: &str, to: &str) -> EngineResult<()> {
        let from = self.remote_path(from);
        let to = self.remote_path(to);
        self.execute("rename", false, move |sftp| {
            let from = from.clone();
            let to = to.clone();
            Box::pin(async move { sftp.rename(from, to).await })
        })
        .await
    }

    async fn truncate(&self, path: &str, size: u64) -> EngineResult<()> {
        let remote_path = self.remote_path(path);
        self.execute("truncate", true, move |sftp| {
            let remote_path = remote_path.clone();
            Box::pin(async move {
                let mut attributes = FileAttributes::empty();
                attributes.size = Some(size);
                sftp.set_metadata(&remote_path, attributes).await?;
                let actual_size = sftp.metadata(remote_path).await?.size.unwrap_or_default();
                if actual_size != size {
                    return Err(SftpError::UnexpectedBehavior(format!(
                        "SFTP truncate verification failed: expected {size} bytes, got {actual_size}"
                    )));
                }
                Ok(())
            })
        })
        .await
    }
}

pub async fn inspect_host_key(
    host: &str,
    port: u16,
    ignore_system_proxy: bool,
) -> EngineResult<String> {
    let fingerprint = Arc::new(StdMutex::new(None));
    let capture = HostKeyCapture {
        fingerprint: fingerprint.clone(),
    };
    let proxy = proxy_for_target(Protocol::Sftp, host, port, ignore_system_proxy)?;
    let stream = connect_target(host, port, proxy.as_ref()).await?;
    let ssh = client::connect_stream(Arc::new(ssh_config()), stream, capture)
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

pub fn known_host_fingerprints(path: &Path, host: &str, port: u16) -> EngineResult<Vec<String>> {
    let mut fingerprints = russh::keys::known_hosts::known_host_keys_path(host, port, path)
        .map_err(|error| EngineError::InvalidConfig(format!("read OpenSSH known_hosts: {error}")))?
        .into_iter()
        .map(|(_, key)| key_fingerprint(&key))
        .collect::<Vec<_>>();
    fingerprints.sort_unstable();
    fingerprints.dedup();
    Ok(fingerprints)
}

type DynamicAgentClient = AgentClient<Box<dyn AgentStream + Send + Unpin + 'static>>;

async fn authenticate_with_agent(
    ssh: &mut client::Handle<HostKeyVerifier>,
    username: &str,
) -> EngineResult<client::AuthResult> {
    let mut agent = connect_ssh_agent().await?;
    let identities = agent
        .request_identities()
        .await
        .map_err(|error| EngineError::Remote(format!("read identities from SSH Agent: {error}")))?;
    if identities.is_empty() {
        return Err(EngineError::filesystem(
            FsErrorCode::PermissionDenied,
            "SSH Agent has no identities",
        ));
    }
    let hash_algorithm = ssh
        .best_supported_rsa_hash()
        .await
        .map_err(|error| EngineError::Remote(format!("negotiate SSH key algorithm: {error}")))?
        .flatten();
    let mut last_failure = None;
    for identity in identities {
        let authentication = match identity {
            AgentIdentity::PublicKey { key, .. } => {
                ssh.authenticate_publickey_with(username, key, hash_algorithm, &mut agent)
                    .await
            }
            AgentIdentity::Certificate { certificate, .. } => {
                ssh.authenticate_certificate_with(username, certificate, hash_algorithm, &mut agent)
                    .await
            }
        }
        .map_err(|error| EngineError::Remote(format!("SSH Agent authentication: {error}")))?;
        if authentication.success() {
            return Ok(authentication);
        }
        last_failure = Some(authentication);
    }
    last_failure.ok_or_else(|| {
        EngineError::filesystem(
            FsErrorCode::PermissionDenied,
            "SSH Agent authentication was rejected",
        )
    })
}

#[cfg(unix)]
async fn connect_ssh_agent() -> EngineResult<DynamicAgentClient> {
    AgentClient::connect_env()
        .await
        .map(AgentClient::dynamic)
        .map_err(|error| EngineError::Remote(format!("connect SSH Agent: {error}")))
}

#[cfg(windows)]
async fn connect_ssh_agent() -> EngineResult<DynamicAgentClient> {
    use std::{ffi::OsString, time::Duration};

    const OPENSSH_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";
    let mut pipes = std::env::var_os("SSH_AUTH_SOCK")
        .filter(|path| !path.is_empty())
        .into_iter()
        .collect::<Vec<OsString>>();
    if !pipes
        .iter()
        .any(|path| path == std::ffi::OsStr::new(OPENSSH_AGENT_PIPE))
    {
        pipes.push(OPENSSH_AGENT_PIPE.into());
    }
    for pipe in pipes {
        if let Ok(Ok(agent)) = tokio::time::timeout(
            Duration::from_secs(2),
            AgentClient::connect_named_pipe(pipe),
        )
        .await
        {
            return Ok(agent.dynamic());
        }
    }
    if let Ok(agent) = AgentClient::connect_pageant().await {
        return Ok(agent.dynamic());
    }
    Err(EngineError::Remote(
        "cannot connect to Windows OpenSSH Agent or Pageant".into(),
    ))
}

fn ssh_config() -> client::Config {
    client::Config {
        keepalive_interval: Some(SSH_KEEPALIVE_INTERVAL),
        keepalive_max: 3,
        nodelay: true,
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
        created: None,
        accessed: metadata
            .accessed()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs()),
        modified: metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs()),
    }
}

fn statvfs_space(info: Statvfs) -> FileSystemSpace {
    let block_size = info.fragment_size.max(1);
    FileSystemSpace {
        total_bytes: info.blocks.saturating_mul(block_size),
        available_bytes: info.blocks_avail.saturating_mul(block_size),
        total_files: Some(info.inodes),
        available_files: Some(info.inodes_avail),
        block_size: u32::try_from(block_size).unwrap_or(u32::MAX),
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

fn reconnectable_sftp_error(error: &SftpError) -> bool {
    matches!(
        error,
        SftpError::IO(_)
            | SftpError::Timeout
            | SftpError::UnexpectedPacket
            | SftpError::UnexpectedBehavior(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt(value: &str) -> Prompt {
        Prompt {
            prompt: value.into(),
            echo: false,
        }
    }

    #[test]
    fn normalizes_sftp_roots() {
        assert_eq!(normalize_root("/"), "/");
        assert_eq!(normalize_root("\\home\\user\\files\\"), "/home/user/files");
    }

    #[test]
    fn maps_statvfs_capacity_and_inode_counts() {
        let space = statvfs_space(Statvfs {
            block_size: 8192,
            fragment_size: 4096,
            blocks: 100,
            blocks_free: 40,
            blocks_avail: 30,
            inodes: 50,
            inodes_free: 20,
            inodes_avail: 15,
            fs_id: 1,
            flags: 0,
            name_max: 255,
        });

        assert_eq!(space.total_bytes, 409_600);
        assert_eq!(space.available_bytes, 122_880);
        assert_eq!(space.total_files, Some(50));
        assert_eq!(space.available_files, Some(15));
        assert_eq!(space.block_size, 4096);
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

    #[test]
    fn reconnects_only_for_transport_and_session_errors() {
        assert!(reconnectable_sftp_error(&SftpError::UnexpectedBehavior(
            "session closed".into()
        )));
        assert!(reconnectable_sftp_error(&SftpError::Timeout));
        assert!(!reconnectable_sftp_error(&SftpError::Limited(
            "packet limit".into()
        )));
        assert!(!reconnectable_sftp_error(&SftpError::Status(
            russh_sftp::protocol::Status {
                id: 1,
                status_code: StatusCode::NoSuchFile,
                error_message: "missing".into(),
                language_tag: "en".into(),
            }
        )));
    }

    #[test]
    fn keeps_idle_ssh_sessions_alive() {
        let config = ssh_config();
        assert_eq!(config.inactivity_timeout, None);
        assert_eq!(config.keepalive_interval, Some(SSH_KEEPALIVE_INTERVAL));
        assert_eq!(config.keepalive_max, 3);
    }

    #[test]
    fn imports_plain_and_hashed_openssh_known_hosts_entries() {
        let directory =
            std::env::temp_dir().join(format!("guglefs-known-hosts-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("known_hosts");
        std::fs::write(
            &path,
            concat!(
                "[localhost]:13265 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ\n",
                "|1|O33ESRMWPVkMYIwJ1Uw+n877jTo=|nuuC5vEqXlEZ/8BXQR7m619W6Ak= ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILIG2T/B0l0gaqj3puu510tu9N1OkQ4znY3LYuEm5zCF\n",
            ),
        )
        .unwrap();

        let plain = known_host_fingerprints(&path, "localhost", 13265).unwrap();
        let hashed = known_host_fingerprints(&path, "example.com", 22).unwrap();

        assert_eq!(plain.len(), 1);
        assert_eq!(hashed.len(), 1);
        assert_ne!(plain, hashed);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn accepts_only_six_digit_totp_codes() {
        assert_eq!(validate_totp_code(" 123456 ".into()).unwrap(), "123456");
        assert!(validate_totp_code("12345".into()).is_err());
        assert!(validate_totp_code("1234567".into()).is_err());
        assert!(validate_totp_code("12345a".into()).is_err());
    }

    #[test]
    fn sends_totp_to_one_time_password_prompts() {
        let responses = keyboard_interactive_responses(
            &[prompt("One-time password:")],
            Some("account-password"),
            "123456",
        )
        .unwrap();

        assert_eq!(responses, ["123456"]);
    }

    #[test]
    fn sends_account_password_to_password_prompts() {
        let responses = keyboard_interactive_responses(
            &[prompt("Password:")],
            Some("account-password"),
            "123456",
        )
        .unwrap();

        assert_eq!(responses, ["account-password"]);
    }

    #[test]
    fn treats_a_single_unknown_mfa_prompt_as_totp() {
        let responses = keyboard_interactive_responses(&[prompt("Code:")], None, "123456").unwrap();

        assert_eq!(responses, ["123456"]);
    }
}
