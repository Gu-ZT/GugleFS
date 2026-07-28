use std::{io, net::IpAddr, time::Duration};

#[cfg(not(windows))]
use std::env;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use guglefs_core::{EngineError, EngineResult, MappingConfig, Protocol};
use percent_encoding::percent_decode_str;
use reqwest::Url;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};

const PROXY_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_HTTP_RESPONSE: usize = 16 * 1024;

#[derive(Clone)]
pub(crate) struct ProxyConfig {
    url: Url,
}

impl ProxyConfig {
    pub(crate) fn url(&self) -> &Url {
        &self.url
    }

    pub(crate) async fn connect(&self, host: &str, port: u16) -> io::Result<TcpStream> {
        timeout(PROXY_TIMEOUT, self.connect_inner(host, port))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "proxy connection timed out"))?
    }

    async fn connect_inner(&self, host: &str, port: u16) -> io::Result<TcpStream> {
        match self.url.scheme() {
            "http" => self.connect_http(host, port).await,
            "socks5" | "socks5h" => self.connect_socks5(host, port).await,
            scheme => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("proxy scheme {scheme} cannot tunnel this protocol"),
            )),
        }
    }

    async fn tcp_stream(&self) -> io::Result<TcpStream> {
        let host = self
            .url
            .host_str()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "proxy host is missing"))?;
        let port = self
            .url
            .port_or_known_default()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "proxy port is missing"))?;
        TcpStream::connect((host, port)).await
    }

    async fn connect_http(&self, host: &str, port: u16) -> io::Result<TcpStream> {
        let mut stream = self.tcp_stream().await?;
        let authority = format_authority(host, port);
        let mut request = format!(
            "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Connection: Keep-Alive\r\n"
        );
        if let Some(credentials) = self.credentials()? {
            request.push_str(&format!(
                "Proxy-Authorization: Basic {}\r\n",
                BASE64.encode(credentials)
            ));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).await?;

        let mut response = Vec::with_capacity(1024);
        let mut byte = [0_u8; 1];
        while !response.windows(4).any(|window| window == b"\r\n\r\n") {
            stream.read_exact(&mut byte).await?;
            response.push(byte[0]);
            if response.len() > MAX_HTTP_RESPONSE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "proxy CONNECT response is too large",
                ));
            }
        }
        let status_line = response
            .split(|byte| *byte == b'\n')
            .next()
            .and_then(|line| std::str::from_utf8(line).ok())
            .unwrap_or_default()
            .trim();
        let accepted = status_line
            .split_ascii_whitespace()
            .nth(1)
            .is_some_and(|status| status == "200");
        if !accepted {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("proxy CONNECT failed: {status_line}"),
            ));
        }
        Ok(stream)
    }

    async fn connect_socks5(&self, host: &str, port: u16) -> io::Result<TcpStream> {
        let mut stream = self.tcp_stream().await?;
        let credentials = self.credentials()?;
        let methods: &[u8] = if credentials.is_some() {
            &[0x00, 0x02]
        } else {
            &[0x00]
        };
        let mut greeting = vec![0x05, methods.len() as u8];
        greeting.extend_from_slice(methods);
        stream.write_all(&greeting).await?;
        let mut selection = [0_u8; 2];
        stream.read_exact(&mut selection).await?;
        if selection[0] != 0x05 || selection[1] == 0xff {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "SOCKS5 proxy rejected authentication methods",
            ));
        }
        if selection[1] == 0x02 {
            let (username, password) = self.username_password()?;
            if username.len() > u8::MAX as usize || password.len() > u8::MAX as usize {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "SOCKS5 proxy credentials are too long",
                ));
            }
            let mut auth = vec![0x01, username.len() as u8];
            auth.extend_from_slice(username.as_bytes());
            auth.push(password.len() as u8);
            auth.extend_from_slice(password.as_bytes());
            stream.write_all(&auth).await?;
            let mut result = [0_u8; 2];
            stream.read_exact(&mut result).await?;
            if result != [0x01, 0x00] {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "SOCKS5 proxy authentication failed",
                ));
            }
        } else if selection[1] != 0x00 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SOCKS5 proxy selected an unsupported authentication method",
            ));
        }

        let mut request = vec![0x05, 0x01, 0x00];
        append_socks_address(&mut request, host)?;
        request.extend_from_slice(&port.to_be_bytes());
        stream.write_all(&request).await?;

        let mut response = [0_u8; 4];
        stream.read_exact(&mut response).await?;
        if response[0] != 0x05 || response[1] != 0x00 {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!(
                    "SOCKS5 proxy connection failed with code 0x{:02x}",
                    response[1]
                ),
            ));
        }
        consume_socks_address(&mut stream, response[3]).await?;
        Ok(stream)
    }

    fn credentials(&self) -> io::Result<Option<String>> {
        if self.url.username().is_empty() && self.url.password().is_none() {
            return Ok(None);
        }
        let (username, password) = self.username_password()?;
        Ok(Some(format!("{username}:{password}")))
    }

    fn username_password(&self) -> io::Result<(String, String)> {
        Ok((
            decode_url_component(self.url.username())?,
            decode_url_component(self.url.password().unwrap_or_default())?,
        ))
    }
}

pub(crate) fn system_proxy(config: &MappingConfig) -> EngineResult<Option<ProxyConfig>> {
    proxy_for_target(
        config.protocol,
        &config.host,
        config.port,
        config.ignore_system_proxy,
    )
}

pub(crate) fn proxy_for_target(
    protocol: Protocol,
    host: &str,
    port: u16,
    ignore_system_proxy: bool,
) -> EngineResult<Option<ProxyConfig>> {
    if ignore_system_proxy {
        return Ok(None);
    }
    let Some((value, bypass)) = system_proxy_value(protocol)? else {
        return Ok(None);
    };
    if bypasses_proxy(host, port, bypass.as_deref()) {
        return Ok(None);
    }
    parse_proxy(&value, protocol).map(Some)
}

pub(crate) async fn connect_target(
    host: &str,
    port: u16,
    proxy: Option<&ProxyConfig>,
) -> EngineResult<TcpStream> {
    let result = match proxy {
        Some(proxy) => proxy.connect(host, port).await,
        None => TcpStream::connect((host, port)).await,
    };
    result.map_err(|error| EngineError::Remote(format!("connect to {host}:{port}: {error}")))
}

#[cfg(not(windows))]
fn system_proxy_value(protocol: Protocol) -> EngineResult<Option<(String, Option<String>)>> {
    let keys: &[&str] = match protocol {
        Protocol::Webdav => &["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"],
        Protocol::Ftp => &[
            "FTP_PROXY",
            "ftp_proxy",
            "ALL_PROXY",
            "all_proxy",
            "HTTP_PROXY",
            "http_proxy",
        ],
        Protocol::Sftp => &[
            "SFTP_PROXY",
            "sftp_proxy",
            "ALL_PROXY",
            "all_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
        ],
    };
    let Some(value) = environment_value(keys) else {
        return Ok(None);
    };
    let bypass = environment_value(&["NO_PROXY", "no_proxy"]);
    Ok(Some((value, bypass)))
}

#[cfg(not(windows))]
fn environment_value(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        env::var_os(key)
            .map(|value| value.to_string_lossy().trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

#[cfg(windows)]
fn system_proxy_value(protocol: Protocol) -> EngineResult<Option<(String, Option<String>)>> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};

    let current_user = RegKey::predef(HKEY_CURRENT_USER);
    let settings = match current_user
        .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings")
    {
        Ok(settings) => settings,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(EngineError::Internal(format!(
                "read Windows system proxy settings: {error}"
            )))
        }
    };
    let enabled = settings.get_value::<u32, _>("ProxyEnable").unwrap_or(0) != 0;
    if !enabled {
        return Ok(None);
    }
    let value = settings
        .get_value::<String, _>("ProxyServer")
        .map_err(|error| EngineError::Internal(format!("read Windows ProxyServer: {error}")))?;
    let Some(value) = select_windows_proxy(&value, protocol) else {
        return Ok(None);
    };
    let bypass = settings
        .get_value::<String, _>("ProxyOverride")
        .ok()
        .filter(|value| !value.trim().is_empty());
    Ok(Some((value, bypass)))
}

#[cfg(windows)]
fn select_windows_proxy(value: &str, protocol: Protocol) -> Option<String> {
    let value = value.trim();
    if !value.contains('=') {
        return (!value.is_empty()).then(|| value.to_string());
    }
    let entries: Vec<_> = value
        .split(';')
        .filter_map(|entry| entry.split_once('='))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim()))
        .filter(|(_, value)| !value.is_empty())
        .collect();
    let preferred: &[&str] = match protocol {
        Protocol::Webdav => &["https", "http", "socks"],
        Protocol::Ftp => &["socks", "ftp", "http", "https"],
        Protocol::Sftp => &["socks", "https", "http"],
    };
    preferred.iter().find_map(|wanted| {
        entries
            .iter()
            .find(|(key, _)| key == wanted)
            .map(|(key, value)| {
                if key == "socks" && !value.contains("://") {
                    format!("socks5://{value}")
                } else {
                    value.to_string()
                }
            })
    })
}

fn parse_proxy(value: &str, protocol: Protocol) -> EngineResult<ProxyConfig> {
    let value = value.trim();
    let normalized = if value.contains("://") {
        value.to_string()
    } else {
        let scheme = if protocol == Protocol::Sftp && value.starts_with("socks=") {
            "socks5"
        } else {
            "http"
        };
        format!("{scheme}://{value}")
    };
    let url = Url::parse(&normalized).map_err(|error| {
        EngineError::InvalidConfig(format!("invalid system proxy URL: {error}"))
    })?;
    if url.host_str().is_none() || url.port_or_known_default().is_none() {
        return Err(EngineError::InvalidConfig(
            "system proxy must include a host and port".into(),
        ));
    }
    match url.scheme() {
        "http" | "https" | "socks5" | "socks5h" => Ok(ProxyConfig { url }),
        scheme => Err(EngineError::InvalidConfig(format!(
            "unsupported system proxy scheme: {scheme}"
        ))),
    }
}

fn bypasses_proxy(host: &str, port: u16, bypass: Option<&str>) -> bool {
    let host = host
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim_end_matches('.')
        .to_ascii_lowercase();
    bypass.is_some_and(|bypass| {
        bypass
            .split([',', ';'])
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .any(|entry| bypass_entry_matches(entry, &host, port))
    })
}

fn bypass_entry_matches(entry: &str, host: &str, port: u16) -> bool {
    let entry = entry.trim().to_ascii_lowercase();
    if entry == "*" || (entry == "<local>" && !host.contains('.')) {
        return true;
    }
    if let Some((network, prefix)) = entry.split_once('/') {
        if let (Ok(address), Ok(network), Ok(prefix)) = (
            host.parse::<IpAddr>(),
            network.parse::<IpAddr>(),
            prefix.parse::<u8>(),
        ) {
            return address_in_network(address, network, prefix);
        }
    }
    if let (Ok(entry), Ok(host)) = (entry.parse::<IpAddr>(), host.parse::<IpAddr>()) {
        if entry == host {
            return true;
        }
    }
    let entry = strip_bypass_port(&entry, port);
    let entry = entry
        .trim_start_matches("*.")
        .trim_start_matches('.')
        .trim_end_matches('.');
    !entry.is_empty() && (host == entry || host.ends_with(&format!(".{entry}")))
}

fn strip_bypass_port(entry: &str, target_port: u16) -> &str {
    if let Some(bracket) = entry.strip_prefix('[').and_then(|value| value.find(']')) {
        let host_end = bracket + 2;
        let suffix = &entry[host_end..];
        return if suffix
            .strip_prefix(':')
            .and_then(|port| port.parse::<u16>().ok())
            .is_some_and(|port| port == target_port)
        {
            &entry[1..host_end - 1]
        } else {
            entry
        };
    }
    match entry.rsplit_once(':') {
        Some((host, value)) if value.parse::<u16>().ok() == Some(target_port) => host,
        _ => entry,
    }
}

fn address_in_network(address: IpAddr, network: IpAddr, prefix: u8) -> bool {
    match (address, network) {
        (IpAddr::V4(address), IpAddr::V4(network)) if prefix <= 32 => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            u32::from(address) & mask == u32::from(network) & mask
        }
        (IpAddr::V6(address), IpAddr::V6(network)) if prefix <= 128 => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            u128::from(address) & mask == u128::from(network) & mask
        }
        _ => false,
    }
}

fn format_authority(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn decode_url_component(value: &str) -> io::Result<String> {
    percent_decode_str(value)
        .decode_utf8()
        .map(|value| value.into_owned())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn append_socks_address(request: &mut Vec<u8>, host: &str) -> io::Result<()> {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => {
            request.push(0x01);
            request.extend_from_slice(&address.octets());
        }
        Ok(IpAddr::V6(address)) => {
            request.push(0x04);
            request.extend_from_slice(&address.octets());
        }
        Err(_) => {
            if host.len() > u8::MAX as usize {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "SOCKS5 destination host is too long",
                ));
            }
            request.push(0x03);
            request.push(host.len() as u8);
            request.extend_from_slice(host.as_bytes());
        }
    }
    Ok(())
}

async fn consume_socks_address(stream: &mut TcpStream, address_type: u8) -> io::Result<()> {
    let length = match address_type {
        0x01 => 4,
        0x04 => 16,
        0x03 => {
            let mut length = [0_u8; 1];
            stream.read_exact(&mut length).await?;
            length[0] as usize
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SOCKS5 proxy returned an invalid address type",
            ))
        }
    };
    let mut address_and_port = vec![0_u8; length + 2];
    stream.read_exact(&mut address_and_port).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn parses_proxy_urls_and_defaults_to_http() {
        let http = parse_proxy("proxy.example:8080", Protocol::Webdav).unwrap();
        let socks = parse_proxy("socks5://proxy.example:1080", Protocol::Sftp).unwrap();

        assert_eq!(http.url().as_str(), "http://proxy.example:8080/");
        assert_eq!(socks.url().as_str(), "socks5://proxy.example:1080");
    }

    #[test]
    fn applies_domain_port_local_and_cidr_bypass_rules() {
        assert!(bypasses_proxy("api.example.com", 443, Some(".example.com")));
        assert!(bypasses_proxy(
            "api.example.com",
            443,
            Some("api.example.com:443")
        ));
        assert!(!bypasses_proxy(
            "api.example.com",
            8443,
            Some("api.example.com:443")
        ));
        assert!(bypasses_proxy("intranet", 22, Some("<local>")));
        assert!(bypasses_proxy("10.20.30.40", 21, Some("10.0.0.0/8")));
        assert!(!bypasses_proxy("11.20.30.40", 21, Some("10.0.0.0/8")));
    }

    #[test]
    fn serializes_socks_destination_addresses() {
        let mut ipv4 = Vec::new();
        append_socks_address(&mut ipv4, "127.0.0.1").unwrap();
        assert_eq!(ipv4, [0x01, 127, 0, 0, 1]);

        let mut domain = Vec::new();
        append_socks_address(&mut domain, "example.test").unwrap();
        assert_eq!(domain[0], 0x03);
        assert_eq!(domain[1] as usize, "example.test".len());
    }

    #[test]
    fn network_matching_supports_both_ip_families() {
        assert!(address_in_network(
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5)),
            IpAddr::V4(Ipv4Addr::new(192, 168, 0, 0)),
            16,
        ));
        assert!(address_in_network(
            IpAddr::V6("2001:db8::5".parse::<Ipv6Addr>().unwrap()),
            IpAddr::V6("2001:db8::".parse::<Ipv6Addr>().unwrap()),
            32,
        ));
    }
}
