use async_trait::async_trait;
use diqwest::WithDigestAuth;
use guglefs_core::{
    AuthMethod, DirectoryEntry, EngineError, EngineResult, EntryKind, FileMetadata, FsErrorCode,
    MappingConfig, Protocol, RemoteFileSystem, WebDavAuthMethod,
};
use percent_encoding::percent_decode_str;
use quick_xml::{events::Event, Reader};
use reqwest::{
    header::{self, HeaderValue},
    Client, Identity, Method, Proxy, RequestBuilder, Response, StatusCode, Url,
};
use std::{fs, future::Future, path::Path, pin::Pin, sync::Arc, time::Duration};
use tokio::sync::Mutex;

use crate::proxy::{system_proxy, ProxyConfig};

#[derive(Clone)]
pub struct WebDavFileSystem {
    client: Client,
    base_url: Url,
    authentication: WebDavAuthentication,
    write_lock: Arc<Mutex<()>>,
}

#[derive(Clone)]
enum WebDavAuthentication {
    Basic { username: String, password: String },
    Digest { username: String, password: String },
    Bearer(String),
    None,
}

impl WebDavFileSystem {
    pub fn new(
        base_url: impl AsRef<str>,
        username: Option<String>,
        password: Option<String>,
    ) -> EngineResult<Self> {
        let base_url = Url::parse(base_url.as_ref())
            .map_err(|error| EngineError::InvalidConfig(format!("invalid WebDAV URL: {error}")))?;
        if base_url.scheme() != "https" || base_url.host_str().is_none() {
            return Err(EngineError::InvalidConfig(
                "WebDAV requires an HTTPS URL with a host".into(),
            ));
        }
        let authentication = match (username, password) {
            (Some(username), Some(password)) => WebDavAuthentication::Basic { username, password },
            _ => WebDavAuthentication::None,
        };
        Self::new_with_proxy(base_url, authentication, None, None)
    }

    fn new_with_proxy(
        base_url: impl AsRef<str>,
        authentication: WebDavAuthentication,
        proxy: Option<ProxyConfig>,
        client_certificate_path: Option<&Path>,
    ) -> EngineResult<Self> {
        let base_url = Url::parse(base_url.as_ref())
            .map_err(|error| EngineError::InvalidConfig(format!("invalid WebDAV URL: {error}")))?;
        if base_url.scheme() != "https" || base_url.host_str().is_none() {
            return Err(EngineError::InvalidConfig(
                "WebDAV requires an HTTPS URL with a host".into(),
            ));
        }
        let mut client = Client::builder().timeout(Duration::from_secs(30)).redirect(
            reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    return attempt.stop();
                }
                if attempt
                    .previous()
                    .last()
                    .is_some_and(|previous| !same_origin(previous, attempt.url()))
                {
                    return attempt.stop();
                }
                attempt.follow()
            }),
        );
        client = match proxy {
            Some(proxy) => client.proxy(Proxy::all(proxy.url().as_str()).map_err(|error| {
                EngineError::InvalidConfig(format!("invalid WebDAV proxy: {error}"))
            })?),
            None => client.no_proxy(),
        };
        if let Some(path) = client_certificate_path {
            let pem = fs::read(path).map_err(|error| {
                EngineError::InvalidConfig(format!(
                    "read WebDAV client certificate PEM bundle: {error}"
                ))
            })?;
            let identity = Identity::from_pem(&pem).map_err(|error| {
                EngineError::InvalidConfig(format!(
                    "parse WebDAV client certificate PEM bundle: {error}"
                ))
            })?;
            client = client.identity(identity);
        }
        let client = client
            .build()
            .map_err(|error| EngineError::Internal(format!("create WebDAV client: {error}")))?;
        Ok(Self {
            client,
            base_url,
            authentication,
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    pub fn from_config(config: &MappingConfig, password: Option<String>) -> EngineResult<Self> {
        if config.protocol != Protocol::Webdav {
            return Err(EngineError::InvalidConfig(
                "WebDAV backend requires a WebDAV mapping".into(),
            ));
        }
        let host = config.host.trim();
        let authority_host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        let mut url = Url::parse(&format!("https://{authority_host}:{}/", config.port))
            .map_err(|error| EngineError::InvalidConfig(format!("invalid WebDAV host: {error}")))?;
        let remote_path = format!("/{}", config.remote_path.trim_matches('/'));
        url.set_path(if remote_path == "/" {
            "/"
        } else {
            remote_path.as_str()
        });
        let authentication = match (&config.webdav_auth, &config.auth) {
            (WebDavAuthMethod::Basic, AuthMethod::Password { .. }) => WebDavAuthentication::Basic {
                username: required_username(config)?,
                password: password.ok_or_else(|| {
                    EngineError::InvalidConfig("WebDAV Basic password is required".into())
                })?,
            },
            (WebDavAuthMethod::Digest, AuthMethod::Password { .. }) => {
                WebDavAuthentication::Digest {
                    username: required_username(config)?,
                    password: password.ok_or_else(|| {
                        EngineError::InvalidConfig("WebDAV Digest password is required".into())
                    })?,
                }
            }
            (WebDavAuthMethod::Bearer, AuthMethod::Password { .. }) => {
                WebDavAuthentication::Bearer(password.ok_or_else(|| {
                    EngineError::InvalidConfig("WebDAV Bearer token is required".into())
                })?)
            }
            (WebDavAuthMethod::ClientCertificate, AuthMethod::Anonymous)
            | (WebDavAuthMethod::Anonymous, AuthMethod::Anonymous) => WebDavAuthentication::None,
            _ => {
                return Err(EngineError::InvalidConfig(
                    "WebDAV authentication configuration is inconsistent".into(),
                ));
            }
        };
        let client_certificate_path = match config.webdav_auth {
            WebDavAuthMethod::ClientCertificate => Some(
                config
                    .webdav_client_certificate_path
                    .as_deref()
                    .filter(|path| !path.trim().is_empty())
                    .map(Path::new)
                    .ok_or_else(|| {
                        EngineError::InvalidConfig(
                            "WebDAV client certificate PEM bundle is required".into(),
                        )
                    })?,
            ),
            _ => None,
        };
        let proxy = system_proxy(config)?;
        Self::new_with_proxy(url.as_str(), authentication, proxy, client_certificate_path)
    }

    fn endpoint(&self, path: &str) -> Url {
        let mut url = self.base_url.clone();
        let base_path = url.path().trim_end_matches('/');
        let suffix = path.trim_matches('/');
        let joined = if suffix.is_empty() {
            format!("{base_path}/")
        } else if base_path.is_empty() {
            format!("/{suffix}")
        } else {
            format!("{base_path}/{suffix}")
        };
        url.set_path(&joined);
        url
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let request = self.client.request(method, self.endpoint(path));
        match &self.authentication {
            WebDavAuthentication::Basic { username, password } => {
                request.basic_auth(username, Some(password))
            }
            WebDavAuthentication::Bearer(token) => request.bearer_auth(token),
            WebDavAuthentication::Digest { .. } | WebDavAuthentication::None => request,
        }
    }

    async fn send(&self, request: RequestBuilder, operation: &str) -> EngineResult<Response> {
        match &self.authentication {
            WebDavAuthentication::Digest { username, password } => request
                .send_with_digest_auth(username, password)
                .await
                .map_err(|error| {
                    EngineError::Remote(format!("WebDAV {operation} request: {error}"))
                }),
            _ => request.send().await.map_err(|error| {
                EngineError::Remote(format!("WebDAV {operation} request: {error}"))
            }),
        }
    }

    fn vfs_path(&self, href: &str) -> Option<String> {
        let resource_path = decode_url_path(href)?;
        let base_path = decode_url_path(self.base_url.path())?;
        if resource_path == base_path {
            return Some("/".into());
        }
        let prefix = if base_path == "/" {
            "/".to_string()
        } else {
            format!("{base_path}/")
        };
        resource_path
            .strip_prefix(&prefix)
            .map(|suffix| format!("/{suffix}"))
    }

    async fn finish(response: reqwest::Response, operation: &str) -> EngineResult<()> {
        if response.status().is_success() {
            return Ok(());
        }
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(status_error(status, operation, &body))
    }

    async fn propfind(&self, path: &str, depth: &str) -> EngineResult<Vec<Resource>> {
        let request = self
            .request(Method::from_bytes(b"PROPFIND").expect("valid method"), path)
            .header("Depth", depth)
            .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
            .body(PROPFIND_BODY);
        let response = self.send(request, "PROPFIND").await?;
        if response.status() != StatusCode::MULTI_STATUS {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(status_error(status, "PROPFIND", &body));
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| EngineError::Remote(format!("read PROPFIND response: {error}")))?;
        parse_propfind(&body)
    }

    async fn resource(&self, path: &str) -> EngineResult<Resource> {
        self.propfind(path, "0")
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| EngineError::Remote("WebDAV returned no resource metadata".into()))
    }

    async fn read_current_version(&self, path: &str, resource: &Resource) -> EngineResult<Vec<u8>> {
        if resource.metadata.size == 0 {
            return Ok(Vec::new());
        }
        let request = apply_write_condition(self.request(Method::GET, path), resource);
        let response = self.send(request, "GET").await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(status_error(status, "read current version", &body));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| EngineError::Remote(format!("read WebDAV GET response: {error}")))
    }

    async fn put_current_version(
        &self,
        path: &str,
        data: Vec<u8>,
        resource: &Resource,
    ) -> EngineResult<u64> {
        let length = data.len() as u64;
        let request = apply_write_condition(self.request(Method::PUT, path), resource).body(data);
        let response = self.send(request, "PUT").await?;
        Self::finish(response, "conditional write").await?;
        Ok(length)
    }

    async fn put(&self, path: &str, data: Vec<u8>) -> EngineResult<u64> {
        let length = data.len() as u64;
        let request = self.request(Method::PUT, path).body(data);
        let response = self.send(request, "PUT").await?;
        Self::finish(response, "write").await?;
        Ok(length)
    }

    fn copy_resource<'a>(
        &'a self,
        from: &'a str,
        to: &'a str,
        metadata: FileMetadata,
    ) -> Pin<Box<dyn Future<Output = EngineResult<()>> + Send + 'a>> {
        Box::pin(async move {
            match metadata.kind {
                EntryKind::File => {
                    let content = self.read_range(from, 0, metadata.size).await?;
                    self.put(to, content).await?;
                }
                EntryKind::Directory => {
                    self.create_dir(to).await?;
                    for entry in self.read_dir(from).await? {
                        let target = format!(
                            "{}/{}",
                            to.trim_end_matches('/'),
                            entry.name.trim_matches('/')
                        );
                        self.copy_resource(&entry.path, &target, entry.metadata)
                            .await?;
                    }
                }
            }
            Ok(())
        })
    }

    async fn rename_manually(
        &self,
        from: &str,
        to: &str,
        metadata: FileMetadata,
    ) -> EngineResult<()> {
        let directory = metadata.kind == EntryKind::Directory;
        self.copy_resource(from, to, metadata).await?;
        let request = self.request(Method::DELETE, from);
        let response = self.send(request, "DELETE").await?;
        Self::finish(
            response,
            if directory {
                "remove renamed directory"
            } else {
                "remove renamed file"
            },
        )
        .await
    }
}

#[async_trait]
impl RemoteFileSystem for WebDavFileSystem {
    async fn connect(&self) -> EngineResult<()> {
        let request = self.request(Method::OPTIONS, "");
        let response = self.send(request, "OPTIONS").await?;
        Self::finish(response, "connect").await
    }

    async fn disconnect(&self) -> EngineResult<()> {
        Ok(())
    }

    async fn metadata(&self, path: &str) -> EngineResult<FileMetadata> {
        self.resource(path).await.map(|resource| resource.metadata)
    }

    async fn read_dir(&self, path: &str) -> EngineResult<Vec<DirectoryEntry>> {
        let requested = normalize_path(path);
        Ok(self
            .propfind(path, "1")
            .await?
            .into_iter()
            .filter_map(|resource| {
                let resource_path = self.vfs_path(&resource.href)?;
                if resource_path == requested {
                    return None;
                }
                let name = resource_path
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string();
                Some(DirectoryEntry {
                    path: resource_path,
                    name,
                    metadata: resource.metadata,
                })
            })
            .collect())
    }

    async fn read_range(&self, path: &str, offset: u64, length: u64) -> EngineResult<Vec<u8>> {
        if length == 0 {
            return Ok(Vec::new());
        }
        let end = offset
            .checked_add(length - 1)
            .ok_or_else(|| EngineError::InvalidConfig("read range overflows u64".into()))?;
        let request = self
            .request(Method::GET, path)
            .header(header::RANGE, format!("bytes={offset}-{end}"));
        let response = self.send(request, "GET").await?;
        let status = response.status();
        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            return Ok(Vec::new());
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(status_error(status, "GET", &body));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|error| EngineError::Remote(format!("read WebDAV GET response: {error}")))?;
        if status == StatusCode::PARTIAL_CONTENT {
            return Ok(
                bytes[..bytes.len().min(usize::try_from(length).map_err(|_| {
                    EngineError::InvalidConfig("read range does not fit the platform usize".into())
                })?)]
                    .to_vec(),
            );
        }
        let start = usize::try_from(offset)
            .map_err(|_| EngineError::InvalidConfig("read range offset is too large".into()))?;
        let requested_length = usize::try_from(length)
            .map_err(|_| EngineError::InvalidConfig("read range length is too large".into()))?;
        if start >= bytes.len() {
            return Ok(Vec::new());
        }
        Ok(bytes[start..bytes.len().min(start.saturating_add(requested_length))].to_vec())
    }

    async fn write(&self, path: &str, offset: u64, data: Vec<u8>) -> EngineResult<u64> {
        if data.is_empty() {
            return Ok(0);
        }
        let _guard = self.write_lock.lock().await;
        let resource = self.resource(path).await?;
        if resource.metadata.kind == EntryKind::Directory {
            return Err(EngineError::filesystem(
                FsErrorCode::IsDirectory,
                format!("cannot write a directory: {path}"),
            ));
        }
        let existing = self.read_current_version(path, &resource).await?;
        let start = usize::try_from(offset)
            .map_err(|_| EngineError::InvalidConfig("write offset is too large".into()))?;
        let end = start
            .checked_add(data.len())
            .ok_or_else(|| EngineError::InvalidConfig("write range overflows usize".into()))?;
        let mut content = existing;
        if content.len() < end {
            content.resize(end, 0);
        }
        content[start..end].copy_from_slice(&data);
        let written = data.len() as u64;
        self.put_current_version(path, content, &resource).await?;
        Ok(written)
    }

    async fn create_file(&self, path: &str) -> EngineResult<()> {
        let request = self
            .request(Method::PUT, path)
            .header("If-None-Match", "*")
            .body(Vec::new());
        let response = self.send(request, "create file").await?;
        Self::finish(response, "create file").await
    }

    async fn create_dir(&self, path: &str) -> EngineResult<()> {
        let method = Method::from_bytes(b"MKCOL").expect("valid method");
        let request = self.request(method, path);
        let response = self.send(request, "MKCOL").await?;
        Self::finish(response, "create directory").await
    }

    async fn remove(&self, path: &str, _directory: bool) -> EngineResult<()> {
        let request = self.request(Method::DELETE, path);
        let response = self.send(request, "DELETE").await?;
        Self::finish(response, "remove").await
    }

    async fn rename(&self, from: &str, to: &str) -> EngineResult<()> {
        let destination = self.endpoint(to).to_string();
        let method = Method::from_bytes(b"MOVE").expect("valid method");
        let request = self
            .request(method, from)
            .header("Destination", destination)
            .header("Overwrite", "T");
        let response = self.send(request, "MOVE").await?;
        if response.status().is_success() {
            return Ok(());
        }

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let move_error = status_error(status, "rename", &body);
        if !move_fallback_allowed(status) {
            return Err(move_error);
        }

        match self.metadata(from).await {
            Ok(metadata) => match self.metadata(to).await {
                Err(error) if error.code() == FsErrorCode::NotFound => {
                    self.rename_manually(from, to, metadata).await
                }
                _ => Err(move_error),
            },
            Err(error) if error.code() == FsErrorCode::NotFound => match self.metadata(to).await {
                Ok(_) => Ok(()),
                Err(_) => Err(move_error),
            },
            Err(_) => Err(move_error),
        }
    }

    async fn truncate(&self, path: &str, size: u64) -> EngineResult<()> {
        let _guard = self.write_lock.lock().await;
        let resource = self.resource(path).await?;
        if resource.metadata.kind == EntryKind::Directory {
            return Err(EngineError::filesystem(
                FsErrorCode::IsDirectory,
                format!("cannot truncate a directory: {path}"),
            ));
        }
        if resource.metadata.size == size {
            return Ok(());
        }
        let new_size = usize::try_from(size)
            .map_err(|_| EngineError::InvalidConfig("truncate size is too large".into()))?;
        let mut content = if size == 0 {
            Vec::new()
        } else {
            self.read_current_version(path, &resource).await?
        };
        content.resize(new_size, 0);
        self.put_current_version(path, content, &resource).await?;
        Ok(())
    }
}

#[derive(Debug)]
struct Resource {
    href: String,
    metadata: FileMetadata,
    etag: Option<String>,
}

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8" ?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:resourcetype />
    <d:getcontentlength />
    <d:getlastmodified />
    <d:getetag />
  </d:prop>
</d:propfind>"#;

fn required_username(config: &MappingConfig) -> EngineResult<String> {
    config
        .username
        .as_deref()
        .map(str::trim)
        .filter(|username| !username.is_empty())
        .map(str::to_string)
        .ok_or_else(|| EngineError::InvalidConfig("WebDAV username is required".into()))
}

fn parse_propfind(body: &[u8]) -> EngineResult<Vec<Resource>> {
    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut resources = Vec::new();
    let mut current: Option<ParsedResource> = None;
    let mut field = Field::None;

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(element)) => match element.local_name().as_ref() {
                b"response" => current = Some(ParsedResource::default()),
                b"href" => field = Field::Href,
                b"getcontentlength" => field = Field::Size,
                b"getlastmodified" => field = Field::Modified,
                b"getetag" => field = Field::Etag,
                b"resourcetype" => field = Field::ResourceType,
                _ => {}
            },
            Ok(Event::Empty(element)) if element.local_name().as_ref() == b"collection" => {
                if let Some(resource) = current.as_mut() {
                    resource.kind = EntryKind::Directory;
                }
            }
            Ok(Event::Text(text)) => {
                if let Some(resource) = current.as_mut() {
                    let value = text.unescape().map_err(|error| {
                        EngineError::Remote(format!("parse WebDAV XML: {error}"))
                    })?;
                    match field {
                        Field::Href => resource.href = value.into_owned(),
                        Field::Size => resource.size = value.parse().unwrap_or_default(),
                        Field::Modified => resource.modified = Some(value.into_owned()),
                        Field::Etag => resource.etag = Some(value.into_owned()),
                        Field::None | Field::ResourceType => {}
                    }
                }
            }
            Ok(Event::End(element)) => match element.local_name().as_ref() {
                b"response" => {
                    if let Some(resource) = current.take() {
                        resources.push(Resource {
                            href: resource.href,
                            metadata: FileMetadata {
                                kind: resource.kind,
                                size: resource.size,
                                modified: resource.modified,
                            },
                            etag: resource.etag,
                        });
                    }
                    field = Field::None;
                }
                _ => field = Field::None,
            },
            Ok(Event::Eof) => break,
            Err(error) => return Err(EngineError::Remote(format!("parse WebDAV XML: {error}"))),
            _ => {}
        }
        buffer.clear();
    }
    Ok(resources)
}

#[derive(Debug, Default)]
struct ParsedResource {
    href: String,
    kind: EntryKind,
    size: u64,
    modified: Option<String>,
    etag: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum Field {
    None,
    Href,
    Size,
    Modified,
    Etag,
    ResourceType,
}

fn apply_write_condition(request: RequestBuilder, resource: &Resource) -> RequestBuilder {
    if let Some(etag) = resource.etag.as_deref().and_then(strong_etag) {
        return request.header(header::IF_MATCH, etag);
    }
    if let Some(modified) = resource
        .metadata
        .modified
        .as_deref()
        .and_then(|value| HeaderValue::from_str(value.trim()).ok())
    {
        return request.header(header::IF_UNMODIFIED_SINCE, modified);
    }
    request
}

fn strong_etag(value: &str) -> Option<HeaderValue> {
    let value = value.trim();
    if value.starts_with("W/") || !value.starts_with('"') || !value.ends_with('"') {
        return None;
    }
    HeaderValue::from_str(value).ok()
}

fn normalize_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".into()
    } else {
        format!("/{trimmed}", trimmed = trimmed.trim_start_matches('/'))
    }
}

fn decode_url_path(path: &str) -> Option<String> {
    let encoded_path = Url::parse(path)
        .map(|url| url.path().to_string())
        .unwrap_or_else(|_| path.to_string());
    let mut decoded_path = String::with_capacity(encoded_path.len());

    for (index, encoded_segment) in encoded_path.split('/').enumerate() {
        if index > 0 {
            decoded_path.push('/');
        }
        let decoded_segment = percent_decode_str(encoded_segment).decode_utf8().ok()?;
        if decoded_segment
            .chars()
            .any(|character| matches!(character, '/' | '\\' | '\0'))
        {
            return None;
        }
        decoded_path.push_str(&decoded_segment);
    }

    Some(normalize_path(&decoded_path))
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn truncate_body(body: &str) -> String {
    const MAX_BODY_LENGTH: usize = 256;
    body.chars().take(MAX_BODY_LENGTH).collect()
}

fn status_error(status: StatusCode, operation: &str, body: &str) -> EngineError {
    let message = format!(
        "WebDAV {operation} failed with {status}: {}",
        truncate_body(body)
    );
    let code = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => FsErrorCode::PermissionDenied,
        StatusCode::NOT_FOUND | StatusCode::GONE => FsErrorCode::NotFound,
        StatusCode::METHOD_NOT_ALLOWED if operation == "create directory" => {
            FsErrorCode::AlreadyExists
        }
        StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED => FsErrorCode::Unsupported,
        StatusCode::PRECONDITION_FAILED if operation == "create file" => FsErrorCode::AlreadyExists,
        StatusCode::PRECONDITION_FAILED => FsErrorCode::Busy,
        StatusCode::CONFLICT | StatusCode::RANGE_NOT_SATISFIABLE => FsErrorCode::InvalidArgument,
        StatusCode::LOCKED => FsErrorCode::Busy,
        _ => FsErrorCode::RemoteIo,
    };
    EngineError::filesystem(code, message)
}

fn move_fallback_allowed(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::BAD_REQUEST
            | StatusCode::METHOD_NOT_ALLOWED
            | StatusCode::NOT_IMPLEMENTED
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        task::JoinHandle,
    };

    use super::*;

    #[test]
    fn parses_multistatus_resources() {
        let xml = br#"<d:multistatus xmlns:d="DAV:">
          <d:response><d:href>/remote/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
          <d:response><d:href>/remote/file.txt</d:href><d:propstat><d:prop><d:getcontentlength>12</d:getcontentlength><d:getetag>&quot;version-1&quot;</d:getetag></d:prop></d:propstat></d:response>
        </d:multistatus>"#;

        let resources = parse_propfind(xml).unwrap();

        assert_eq!(resources.len(), 2);
        assert_eq!(resources[0].metadata.kind, EntryKind::Directory);
        assert_eq!(resources[1].metadata.size, 12);
        assert_eq!(resources[1].etag.as_deref(), Some("\"version-1\""));
    }

    #[test]
    fn maps_http_statuses_to_vfs_error_codes() {
        assert_eq!(
            status_error(StatusCode::NOT_FOUND, "GET", "").code(),
            FsErrorCode::NotFound
        );
        assert_eq!(
            status_error(StatusCode::LOCKED, "PUT", "").code(),
            FsErrorCode::Busy
        );
        assert_eq!(
            status_error(StatusCode::PRECONDITION_FAILED, "conditional write", "").code(),
            FsErrorCode::Busy
        );
        assert_eq!(
            status_error(StatusCode::PRECONDITION_FAILED, "create file", "").code(),
            FsErrorCode::AlreadyExists
        );
        assert_eq!(
            status_error(StatusCode::BAD_GATEWAY, "GET", "").code(),
            FsErrorCode::RemoteIo
        );
    }

    #[test]
    fn converts_webdav_hrefs_to_vfs_paths() {
        let file_system = WebDavFileSystem::new("https://example.test/remote", None, None).unwrap();

        assert_eq!(file_system.vfs_path("/remote/"), Some("/".into()));
        assert_eq!(
            file_system.vfs_path("https://example.test/remote/docs/file.txt"),
            Some("/docs/file.txt".into())
        );
        assert_eq!(
            file_system.vfs_path("/remote/%E4%B8%AD%E6%96%87.txt"),
            Some("/中文.txt".into())
        );
        assert_eq!(file_system.vfs_path("/other/file.txt"), None);
    }

    #[test]
    fn rejects_encoded_path_separators_from_webdav_hrefs() {
        let file_system = WebDavFileSystem::new("https://example.test/remote", None, None).unwrap();

        assert_eq!(file_system.vfs_path("/remote/dir%2Ffile.txt"), None);
        assert_eq!(file_system.vfs_path("/remote/dir%5Cfile.txt"), None);
    }

    #[test]
    fn encodes_unicode_vfs_paths_once_for_requests() {
        let file_system = WebDavFileSystem::new("https://example.test/remote", None, None).unwrap();

        assert_eq!(
            file_system.endpoint("/中文.txt").path(),
            "/remote/%E4%B8%AD%E6%96%87.txt"
        );
    }

    #[test]
    fn only_same_origin_redirects_are_allowed() {
        let origin = Url::parse("https://example.test:443/root").unwrap();
        let same = Url::parse("https://example.test/root/child").unwrap();
        let different_host = Url::parse("https://other.test/root").unwrap();
        let different_scheme = Url::parse("http://example.test/root").unwrap();

        assert!(same_origin(&origin, &same));
        assert!(!same_origin(&origin, &different_host));
        assert!(!same_origin(&origin, &different_scheme));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn slices_a_server_response_that_ignores_range() {
        let (base_url, server, _requests) = spawn_test_server(1, "abcdefghij").await;
        let file_system = test_file_system(&base_url);

        assert_eq!(
            file_system.read_range("/file.txt", 2, 4).await.unwrap(),
            b"cdef"
        );
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sends_bearer_tokens_without_a_username() {
        let (base_url, server, requests) = spawn_auth_test_server(false).await;
        let file_system = test_file_system_with_auth(
            &base_url,
            WebDavAuthentication::Bearer("secret-token".into()),
        );

        file_system.connect().await.unwrap();
        server.await.unwrap();

        let request = String::from_utf8_lossy(&requests.lock().unwrap()[0]).into_owned();
        assert!(request
            .lines()
            .any(|line| line == "authorization: Bearer secret-token"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn completes_the_digest_challenge_flow_for_webdav_methods() {
        let (base_url, server, requests) = spawn_auth_test_server(true).await;
        let file_system = test_file_system_with_auth(
            &base_url,
            WebDavAuthentication::Digest {
                username: "digest-user".into(),
                password: "digest-password".into(),
            },
        );

        file_system.connect().await.unwrap();
        server.await.unwrap();

        let requests = requests.lock().unwrap();
        let challenge = String::from_utf8_lossy(&requests[0]);
        let authenticated = String::from_utf8_lossy(&requests[1]);
        assert!(!challenge.to_ascii_lowercase().contains("authorization:"));
        assert!(authenticated
            .lines()
            .any(|line| line.starts_with("authorization: Digest ")));
        assert!(!authenticated.contains("digest-password"));
    }

    #[test]
    fn client_certificate_mode_requires_a_local_pem_bundle() {
        let config = MappingConfig {
            id: "webdav".into(),
            name: "WebDAV".into(),
            protocol: Protocol::Webdav,
            host: "files.example.com".into(),
            port: 443,
            username: None,
            auth: AuthMethod::Anonymous,
            remote_path: "/".into(),
            mount_point: "Z:".into(),
            ftp_tls: false,
            host_key_fingerprint: None,
            sftp_totp_required: false,
            ignore_system_proxy: true,
            webdav_auth: WebDavAuthMethod::ClientCertificate,
            webdav_client_certificate_path: None,
            auto_mount: false,
        };

        let error = WebDavFileSystem::from_config(&config, None)
            .err()
            .expect("missing client certificate must fail");
        assert!(error
            .to_string()
            .contains("client certificate PEM bundle is required"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn treats_an_unsatisfied_range_as_end_of_file() {
        let (base_url, server, _requests) =
            spawn_test_server_with_get_status(1, "416 Range Not Satisfiable", "invalid range")
                .await;
        let file_system = test_file_system(&base_url);

        assert_eq!(
            file_system.read_range("/file.txt", 10, 4).await.unwrap(),
            Vec::<u8>::new()
        );
        server.await.unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn sends_an_encoded_absolute_url_in_the_move_destination() {
        let (base_url, server, requests) = spawn_test_server(1, "").await;
        let file_system = test_file_system(&base_url);

        file_system
            .rename("/docs/source.txt", "/docs/中文.txt")
            .await
            .unwrap();
        server.await.unwrap();

        let request = String::from_utf8_lossy(&requests.lock().unwrap()[0]).into_owned();
        assert!(request.starts_with("MOVE /remote/docs/source.txt"));
        assert!(request.lines().any(|line| {
            line == format!("destination: {base_url}/docs/%E4%B8%AD%E6%96%87.txt")
        }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn falls_back_to_download_upload_and_delete_when_move_is_broken() {
        let (base_url, server, requests) =
            spawn_test_server_with_move_status(6, "500 Internal Server Error").await;
        let file_system = test_file_system(&base_url);

        file_system
            .rename("/docs/source.txt", "/docs/target.txt")
            .await
            .unwrap();
        server.await.unwrap();

        let requests = requests.lock().unwrap();
        assert!(String::from_utf8_lossy(&requests[0]).starts_with("MOVE /remote/docs/source.txt"));
        assert!(
            String::from_utf8_lossy(&requests[1]).starts_with("PROPFIND /remote/docs/source.txt")
        );
        assert!(
            String::from_utf8_lossy(&requests[2]).starts_with("PROPFIND /remote/docs/target.txt")
        );
        assert!(String::from_utf8_lossy(&requests[3]).starts_with("GET /remote/docs/source.txt"));
        let put = String::from_utf8_lossy(&requests[4]);
        assert!(put.starts_with("PUT /remote/docs/target.txt"));
        assert!(put.ends_with("hello"));
        assert!(String::from_utf8_lossy(&requests[5]).starts_with("DELETE /remote/docs/source.txt"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn performs_read_modify_write_for_nonzero_offsets() {
        let (base_url, server, requests) = spawn_test_server(3, "hello").await;
        let file_system = test_file_system(&base_url);

        assert_eq!(
            file_system
                .write("/file.txt", 2, b"XY".to_vec())
                .await
                .unwrap(),
            2
        );
        server.await.unwrap();

        let requests = requests.lock().unwrap();
        assert!(String::from_utf8_lossy(&requests[0]).starts_with("PROPFIND /remote/file.txt"));
        assert!(String::from_utf8_lossy(&requests[1]).starts_with("GET /remote/file.txt"));
        assert!(String::from_utf8_lossy(&requests[2]).starts_with("PUT /remote/file.txt"));
        assert!(String::from_utf8_lossy(&requests[2])
            .lines()
            .any(|line| line == "if-match: \"version-1\""));
        assert!(String::from_utf8_lossy(&requests[2]).ends_with("heXYo"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn writes_at_offset_zero_without_truncating_the_existing_tail() {
        let (base_url, server, requests) = spawn_test_server(3, "hello").await;
        let file_system = test_file_system(&base_url);

        assert_eq!(
            file_system
                .write("/file.txt", 0, b"XY".to_vec())
                .await
                .unwrap(),
            2
        );
        server.await.unwrap();

        let requests = requests.lock().unwrap();
        assert!(String::from_utf8_lossy(&requests[2]).ends_with("XYllo"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn truncates_with_the_resource_version_condition() {
        let (base_url, server, requests) = spawn_test_server(3, "hello").await;
        let file_system = test_file_system(&base_url);

        file_system.truncate("/file.txt", 3).await.unwrap();
        server.await.unwrap();

        let request = String::from_utf8_lossy(&requests.lock().unwrap()[2]).into_owned();
        assert!(request
            .lines()
            .any(|line| line == "if-match: \"version-1\""));
        assert!(request.ends_with("hel"));
    }

    #[test]
    fn uses_last_modified_when_only_a_weak_etag_is_available() {
        let file_system = WebDavFileSystem::new("https://example.test/remote", None, None).unwrap();
        let resource = Resource {
            href: "/remote/file.txt".into(),
            metadata: FileMetadata {
                kind: EntryKind::File,
                size: 5,
                modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".into()),
            },
            etag: Some("W/\"version-1\"".into()),
        };

        let request =
            apply_write_condition(file_system.request(Method::PUT, "/file.txt"), &resource)
                .build()
                .unwrap();

        assert!(request.headers().get(header::IF_MATCH).is_none());
        assert_eq!(
            request.headers().get(header::IF_UNMODIFIED_SINCE).unwrap(),
            "Wed, 21 Oct 2015 07:28:00 GMT"
        );
    }

    fn test_file_system(base_url: &str) -> WebDavFileSystem {
        test_file_system_with_auth(base_url, WebDavAuthentication::None)
    }

    fn test_file_system_with_auth(
        base_url: &str,
        authentication: WebDavAuthentication,
    ) -> WebDavFileSystem {
        WebDavFileSystem {
            client: Client::builder().build().unwrap(),
            base_url: Url::parse(base_url).unwrap(),
            authentication,
            write_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    async fn spawn_auth_test_server(
        digest_challenge: bool,
    ) -> (String, JoinHandle<()>, Arc<Mutex<Vec<Vec<u8>>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured_requests = requests.clone();
        let request_count = if digest_challenge { 2 } else { 1 };
        let server = tokio::spawn(async move {
            for index in 0..request_count {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                captured_requests.lock().unwrap().push(request);
                let response = if digest_challenge && index == 0 {
                    "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"guglefs\", qop=\"auth\", nonce=\"test-nonce\", algorithm=MD5\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                };
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{address}/remote"), server, requests)
    }

    async fn spawn_test_server(
        request_count: usize,
        get_body: &'static str,
    ) -> (String, JoinHandle<()>, Arc<Mutex<Vec<Vec<u8>>>>) {
        spawn_test_server_with_statuses(request_count, "200 OK", get_body, "204 No Content").await
    }

    async fn spawn_test_server_with_get_status(
        request_count: usize,
        get_status: &'static str,
        get_body: &'static str,
    ) -> (String, JoinHandle<()>, Arc<Mutex<Vec<Vec<u8>>>>) {
        spawn_test_server_with_statuses(request_count, get_status, get_body, "204 No Content").await
    }

    async fn spawn_test_server_with_move_status(
        request_count: usize,
        move_status: &'static str,
    ) -> (String, JoinHandle<()>, Arc<Mutex<Vec<Vec<u8>>>>) {
        spawn_test_server_with_statuses(request_count, "200 OK", "hello", move_status).await
    }

    async fn spawn_test_server_with_statuses(
        request_count: usize,
        get_status: &'static str,
        get_body: &'static str,
        move_status: &'static str,
    ) -> (String, JoinHandle<()>, Arc<Mutex<Vec<Vec<u8>>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured_requests = requests.clone();
        let server = tokio::spawn(async move {
            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                let request_text = String::from_utf8_lossy(&request);
                let method = request_text.split_whitespace().next().unwrap_or_default();
                let (status, body, content_type) = match method {
                    "PROPFIND"
                        if request_text.starts_with("PROPFIND /remote/docs/target.txt ") =>
                    {
                        ("404 Not Found", "", "text/plain")
                    }
                    "PROPFIND" => (
                        "207 Multi-Status",
                        "<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/remote/file.txt</d:href><d:propstat><d:prop><d:getcontentlength>5</d:getcontentlength><d:getlastmodified>Wed, 21 Oct 2015 07:28:00 GMT</d:getlastmodified><d:getetag>&quot;version-1&quot;</d:getetag></d:prop></d:propstat></d:response></d:multistatus>",
                        "application/xml",
                    ),
                    "GET" => (get_status, get_body, "text/plain"),
                    "PUT" => ("204 No Content", "", "text/plain"),
                    "MOVE" => (move_status, "", "text/plain"),
                    "DELETE" => ("204 No Content", "", "text/plain"),
                    _ => ("500 Internal Server Error", "unexpected method", "text/plain"),
                };
                captured_requests.lock().unwrap().push(request);
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: {content_type}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (format!("http://{address}/remote"), server, requests)
    }

    async fn read_request(stream: &mut TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = stream.read(&mut buffer).await.unwrap();
            if count == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..count]);
            let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let header_text = String::from_utf8_lossy(&request[..header_end]);
            let content_length = header_text
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }
        request
    }
}
