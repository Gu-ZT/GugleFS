use async_trait::async_trait;
use guglefs_core::{
    AuthMethod, DirectoryEntry, EngineError, EngineResult, EntryKind, FileMetadata, FsErrorCode,
    MappingConfig, Protocol, RemoteFileSystem,
};
use quick_xml::{events::Event, Reader};
use reqwest::{header, Client, Method, RequestBuilder, StatusCode, Url};

#[derive(Debug, Clone)]
pub struct WebDavFileSystem {
    client: Client,
    base_url: Url,
    username: Option<String>,
    password: Option<String>,
}

impl WebDavFileSystem {
    pub fn new(
        base_url: impl AsRef<str>,
        username: Option<String>,
        password: Option<String>,
    ) -> EngineResult<Self> {
        let base_url = Url::parse(base_url.as_ref())
            .map_err(|error| EngineError::InvalidConfig(format!("invalid WebDAV URL: {error}")))?;
        if base_url.scheme() != "https" {
            return Err(EngineError::InvalidConfig(
                "WebDAV requires an HTTPS URL".into(),
            ));
        }
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|error| EngineError::Internal(format!("create WebDAV client: {error}")))?;
        Ok(Self {
            client,
            base_url,
            username,
            password,
        })
    }

    pub fn from_config(config: &MappingConfig, password: Option<String>) -> EngineResult<Self> {
        if config.protocol != Protocol::Webdav {
            return Err(EngineError::InvalidConfig(
                "WebDAV backend requires a WebDAV mapping".into(),
            ));
        }
        let remote_path = config.remote_path.trim_start_matches('/');
        let url = format!("https://{}:{}/{}", config.host, config.port, remote_path);
        let username = match &config.auth {
            AuthMethod::Password { .. } => config.username.clone(),
            AuthMethod::Anonymous => None,
            AuthMethod::PrivateKey { .. } => {
                return Err(EngineError::InvalidConfig(
                    "private keys are not valid WebDAV credentials".into(),
                ));
            }
        };
        Self::new(url, username, password)
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
        match (&self.username, &self.password) {
            (Some(username), Some(password)) => request.basic_auth(username, Some(password)),
            _ => request,
        }
    }

    fn vfs_path(&self, href: &str) -> Option<String> {
        let resource_path = normalize_path(href);
        let base_path = normalize_path(self.base_url.path());
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
        let response = self
            .request(Method::from_bytes(b"PROPFIND").expect("valid method"), path)
            .header("Depth", depth)
            .header(header::CONTENT_TYPE, "application/xml; charset=utf-8")
            .body(PROPFIND_BODY)
            .send()
            .await
            .map_err(|error| EngineError::Remote(format!("WebDAV PROPFIND request: {error}")))?;
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
}

#[async_trait]
impl RemoteFileSystem for WebDavFileSystem {
    async fn connect(&self) -> EngineResult<()> {
        let response = self
            .request(Method::OPTIONS, "")
            .send()
            .await
            .map_err(|error| EngineError::Remote(format!("WebDAV OPTIONS request: {error}")))?;
        Self::finish(response, "connect").await
    }

    async fn disconnect(&self) -> EngineResult<()> {
        Ok(())
    }

    async fn metadata(&self, path: &str) -> EngineResult<FileMetadata> {
        let resources = self.propfind(path, "0").await?;
        resources
            .into_iter()
            .next()
            .map(|resource| resource.metadata)
            .ok_or_else(|| EngineError::Remote("WebDAV returned no resource metadata".into()))
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
        let response = self
            .request(Method::GET, path)
            .header(header::RANGE, format!("bytes={offset}-{end}"))
            .send()
            .await
            .map_err(|error| EngineError::Remote(format!("WebDAV GET request: {error}")))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(status_error(status, "GET", &body));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| EngineError::Remote(format!("read WebDAV GET response: {error}")))
    }

    async fn write(&self, path: &str, offset: u64, data: Vec<u8>) -> EngineResult<u64> {
        if offset != 0 {
            return Err(EngineError::NotImplemented(
                "WebDAV partial writes require a read-modify-write layer".into(),
            ));
        }
        let length = data.len() as u64;
        let response = self
            .request(Method::PUT, path)
            .body(data)
            .send()
            .await
            .map_err(|error| EngineError::Remote(format!("WebDAV PUT request: {error}")))?;
        Self::finish(response, "write").await?;
        Ok(length)
    }

    async fn create_dir(&self, path: &str) -> EngineResult<()> {
        let method = Method::from_bytes(b"MKCOL").expect("valid method");
        let response = self
            .request(method, path)
            .send()
            .await
            .map_err(|error| EngineError::Remote(format!("WebDAV MKCOL request: {error}")))?;
        Self::finish(response, "create directory").await
    }

    async fn remove(&self, path: &str, _directory: bool) -> EngineResult<()> {
        let response = self
            .request(Method::DELETE, path)
            .send()
            .await
            .map_err(|error| EngineError::Remote(format!("WebDAV DELETE request: {error}")))?;
        Self::finish(response, "remove").await
    }

    async fn rename(&self, from: &str, to: &str) -> EngineResult<()> {
        let destination = self.endpoint(to).to_string();
        let method = Method::from_bytes(b"MOVE").expect("valid method");
        let response = self
            .request(method, from)
            .header("Destination", destination)
            .header("Overwrite", "T")
            .send()
            .await
            .map_err(|error| EngineError::Remote(format!("WebDAV MOVE request: {error}")))?;
        Self::finish(response, "rename").await
    }
}

#[derive(Debug)]
struct Resource {
    href: String,
    metadata: FileMetadata,
}

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8" ?>
<d:propfind xmlns:d="DAV:">
  <d:prop>
    <d:resourcetype />
    <d:getcontentlength />
    <d:getlastmodified />
  </d:prop>
</d:propfind>"#;

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
}

#[derive(Debug, Clone, Copy)]
enum Field {
    None,
    Href,
    Size,
    Modified,
    ResourceType,
}

fn normalize_path(path: &str) -> String {
    let path = Url::parse(path)
        .map(|url| url.path().to_string())
        .unwrap_or_else(|_| path.to_string());
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".into()
    } else {
        format!("/{trimmed}", trimmed = trimmed.trim_start_matches('/'))
    }
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
        StatusCode::PRECONDITION_FAILED => FsErrorCode::AlreadyExists,
        StatusCode::CONFLICT | StatusCode::RANGE_NOT_SATISFIABLE => FsErrorCode::InvalidArgument,
        StatusCode::LOCKED => FsErrorCode::Busy,
        _ => FsErrorCode::RemoteIo,
    };
    EngineError::filesystem(code, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multistatus_resources() {
        let xml = br#"<d:multistatus xmlns:d="DAV:">
          <d:response><d:href>/remote/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
          <d:response><d:href>/remote/file.txt</d:href><d:propstat><d:prop><d:getcontentlength>12</d:getcontentlength></d:prop></d:propstat></d:response>
        </d:multistatus>"#;

        let resources = parse_propfind(xml).unwrap();

        assert_eq!(resources.len(), 2);
        assert_eq!(resources[0].metadata.kind, EntryKind::Directory);
        assert_eq!(resources[1].metadata.size, 12);
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
        assert_eq!(file_system.vfs_path("/other/file.txt"), None);
    }
}
