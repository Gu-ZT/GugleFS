use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Ftp,
    Sftp,
    Webdav,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebDavAuthMethod {
    #[default]
    Basic,
    Digest,
    Bearer,
    ClientCertificate,
    Anonymous,
}

impl Protocol {
    pub const fn default_port(self) -> u16 {
        match self {
            Self::Ftp => 21,
            Self::Sftp => 22,
            Self::Webdav => 443,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthMethod {
    Password {
        credential_id: Option<String>,
    },
    PrivateKey {
        #[serde(default)]
        key_path: Option<String>,
        #[serde(default)]
        key_id: Option<String>,
        #[serde(default)]
        credential_id: Option<String>,
    },
    SshAgent,
    Anonymous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingConfig {
    pub id: String,
    pub name: String,
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub auth: AuthMethod,
    pub remote_path: String,
    pub mount_point: String,
    #[serde(default)]
    pub ftp_tls: bool,
    #[serde(default)]
    pub host_key_fingerprint: Option<String>,
    #[serde(default)]
    pub sftp_totp_required: bool,
    #[serde(default)]
    pub ignore_system_proxy: bool,
    #[serde(default)]
    pub webdav_auth: WebDavAuthMethod,
    #[serde(default)]
    pub webdav_client_certificate_path: Option<String>,
    pub auto_mount: bool,
}

impl MappingConfig {
    pub fn migrated(mut self) -> Self {
        let legacy_anonymous_webdav = self.protocol == Protocol::Webdav
            && self.webdav_auth == WebDavAuthMethod::Basic
            && self
                .username
                .as_deref()
                .is_none_or(|username| username.trim().is_empty())
            && matches!(
                &self.auth,
                AuthMethod::Anonymous
                    | AuthMethod::Password {
                        credential_id: None
                    }
            );
        if legacy_anonymous_webdav {
            self.auth = AuthMethod::Anonymous;
            self.webdav_auth = WebDavAuthMethod::Anonymous;
        }
        self
    }

    pub fn validate(&self) -> crate::EngineResult<()> {
        if self.id.trim().is_empty() {
            return Err(crate::EngineError::InvalidConfig(
                "id cannot be empty".into(),
            ));
        }
        if self.name.trim().is_empty() {
            return Err(crate::EngineError::InvalidConfig(
                "name cannot be empty".into(),
            ));
        }
        if self.host.trim().is_empty() {
            return Err(crate::EngineError::InvalidConfig(
                "host cannot be empty".into(),
            ));
        }
        if self.host.chars().any(|character| {
            character.is_whitespace() || matches!(character, '/' | '?' | '#' | '@')
        }) {
            return Err(crate::EngineError::InvalidConfig(
                "host contains invalid URL characters".into(),
            ));
        }
        if self.port == 0 {
            return Err(crate::EngineError::InvalidConfig(
                "port must be greater than zero".into(),
            ));
        }
        if self.remote_path.trim().is_empty() {
            return Err(crate::EngineError::InvalidConfig(
                "remote path cannot be empty".into(),
            ));
        }
        if self.mount_point.trim().is_empty() {
            return Err(crate::EngineError::InvalidConfig(
                "mount point cannot be empty".into(),
            ));
        }
        if self.protocol != Protocol::Sftp && self.sftp_totp_required {
            return Err(crate::EngineError::InvalidConfig(
                "MFA is only supported for SFTP mappings".into(),
            ));
        }
        if self.protocol != Protocol::Sftp && matches!(&self.auth, AuthMethod::SshAgent) {
            return Err(crate::EngineError::InvalidConfig(
                "SSH Agent authentication is only supported for SFTP mappings".into(),
            ));
        }
        if self.protocol != Protocol::Webdav {
            if self.webdav_auth != WebDavAuthMethod::Basic
                || self.webdav_client_certificate_path.is_some()
            {
                return Err(crate::EngineError::InvalidConfig(
                    "WebDAV authentication settings require a WebDAV mapping".into(),
                ));
            }
        } else {
            match self.webdav_auth {
                WebDavAuthMethod::Basic | WebDavAuthMethod::Digest => {
                    if !matches!(&self.auth, AuthMethod::Password { .. }) {
                        return Err(crate::EngineError::InvalidConfig(
                            "WebDAV password authentication requires a stored credential".into(),
                        ));
                    }
                    if self
                        .username
                        .as_deref()
                        .is_none_or(|username| username.trim().is_empty())
                    {
                        return Err(crate::EngineError::InvalidConfig(
                            "WebDAV Basic and Digest authentication require a username".into(),
                        ));
                    }
                }
                WebDavAuthMethod::Bearer => {
                    if !matches!(&self.auth, AuthMethod::Password { .. }) {
                        return Err(crate::EngineError::InvalidConfig(
                            "WebDAV Bearer authentication requires a stored token".into(),
                        ));
                    }
                }
                WebDavAuthMethod::ClientCertificate => {
                    if !matches!(&self.auth, AuthMethod::Anonymous) {
                        return Err(crate::EngineError::InvalidConfig(
                            "WebDAV client certificate authentication cannot store a password"
                                .into(),
                        ));
                    }
                }
                WebDavAuthMethod::Anonymous => {
                    if !matches!(&self.auth, AuthMethod::Anonymous) {
                        return Err(crate::EngineError::InvalidConfig(
                            "anonymous WebDAV authentication cannot store a credential".into(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn portable(mut self) -> Self {
        self = self.migrated();
        self.auth = match self.auth {
            AuthMethod::Password { .. } => AuthMethod::Password {
                credential_id: None,
            },
            AuthMethod::PrivateKey { .. } => AuthMethod::PrivateKey {
                key_path: None,
                key_id: None,
                credential_id: None,
            },
            AuthMethod::SshAgent => AuthMethod::SshAgent,
            AuthMethod::Anonymous => AuthMethod::Anonymous,
        };
        self.webdav_client_certificate_path = None;
        self.auto_mount = false;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MappingState {
    Unmounted,
    Mounting,
    Mounted,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MappingRuntime {
    pub config: MappingConfig,
    pub state: MappingState,
    pub last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{AuthMethod, MappingConfig, Protocol, WebDavAuthMethod};

    #[test]
    fn protocols_have_expected_default_ports() {
        assert_eq!(Protocol::Ftp.default_port(), 21);
        assert_eq!(Protocol::Sftp.default_port(), 22);
        assert_eq!(Protocol::Webdav.default_port(), 443);
    }

    #[test]
    fn rejects_hosts_that_can_change_the_request_authority() {
        let mut config = MappingConfig {
            id: "id".into(),
            name: "name".into(),
            protocol: Protocol::Webdav,
            host: "files.example.com".into(),
            port: 443,
            username: None,
            auth: AuthMethod::Anonymous,
            remote_path: "/".into(),
            mount_point: "/mnt/files".into(),
            ftp_tls: false,
            host_key_fingerprint: None,
            sftp_totp_required: false,
            ignore_system_proxy: false,
            webdav_auth: WebDavAuthMethod::Anonymous,
            webdav_client_certificate_path: None,
            auto_mount: false,
        };
        config.host = "files.example.com@attacker.test".into();

        assert!(config.validate().is_err());
    }

    #[test]
    fn old_mapping_json_defaults_sftp_mfa_to_disabled() {
        let config: MappingConfig = serde_json::from_value(serde_json::json!({
            "id": "id",
            "name": "name",
            "protocol": "sftp",
            "host": "files.example.com",
            "port": 22,
            "username": "user",
            "auth": { "type": "password", "credential_id": null },
            "remotePath": "/",
            "mountPoint": "/mnt/files",
            "ftpTls": false,
            "hostKeyFingerprint": null,
            "ignoreSystemProxy": false,
            "autoMount": false
        }))
        .unwrap();

        assert!(!config.sftp_totp_required);
        assert_eq!(config.webdav_auth, WebDavAuthMethod::Basic);
        assert!(config.webdav_client_certificate_path.is_none());
    }

    #[test]
    fn migrates_the_legacy_anonymous_webdav_representation() {
        let config: MappingConfig = serde_json::from_value(serde_json::json!({
            "id": "id",
            "name": "name",
            "protocol": "webdav",
            "host": "files.example.com",
            "port": 443,
            "username": null,
            "auth": { "type": "password", "credential_id": null },
            "remotePath": "/",
            "mountPoint": "/mnt/files",
            "ftpTls": false,
            "hostKeyFingerprint": null,
            "ignoreSystemProxy": false,
            "autoMount": false
        }))
        .unwrap();

        let migrated = config.migrated();
        assert_eq!(migrated.webdav_auth, WebDavAuthMethod::Anonymous);
        assert_eq!(migrated.auth, AuthMethod::Anonymous);
        assert!(migrated.validate().is_ok());
    }

    #[test]
    fn ssh_agent_authentication_is_portable_and_sftp_only() {
        let mut config = MappingConfig {
            id: "id".into(),
            name: "name".into(),
            protocol: Protocol::Sftp,
            host: "files.example.com".into(),
            port: 22,
            username: Some("user".into()),
            auth: AuthMethod::SshAgent,
            remote_path: "/".into(),
            mount_point: "/mnt/files".into(),
            ftp_tls: false,
            host_key_fingerprint: Some("SHA256:test".into()),
            sftp_totp_required: false,
            ignore_system_proxy: false,
            webdav_auth: WebDavAuthMethod::Basic,
            webdav_client_certificate_path: None,
            auto_mount: true,
        };

        assert!(config.validate().is_ok());
        let portable = config.clone().portable();
        assert_eq!(portable.auth, AuthMethod::SshAgent);
        assert!(!portable.auto_mount);

        config.protocol = Protocol::Webdav;
        assert!(config.validate().is_err());
    }

    #[test]
    fn portable_configs_remove_credential_and_private_key_references() {
        let config = MappingConfig {
            id: "id".into(),
            name: "name".into(),
            protocol: Protocol::Sftp,
            host: "files.example.com".into(),
            port: 22,
            username: Some("user".into()),
            auth: AuthMethod::PrivateKey {
                key_path: Some("/home/user/.ssh/id_ed25519".into()),
                key_id: Some("stored-key".into()),
                credential_id: Some("stored-passphrase".into()),
            },
            remote_path: "/".into(),
            mount_point: "/mnt/files".into(),
            ftp_tls: false,
            host_key_fingerprint: Some("SHA256:test".into()),
            sftp_totp_required: false,
            ignore_system_proxy: false,
            webdav_auth: WebDavAuthMethod::Basic,
            webdav_client_certificate_path: None,
            auto_mount: true,
        };

        let portable = config.portable();

        assert_eq!(
            portable.auth,
            AuthMethod::PrivateKey {
                key_path: None,
                key_id: None,
                credential_id: None,
            }
        );
        assert!(portable.webdav_client_certificate_path.is_none());
        assert!(!portable.auto_mount);
        assert_eq!(
            portable.host_key_fingerprint.as_deref(),
            Some("SHA256:test")
        );
    }

    #[test]
    fn validates_webdav_authentication_modes() {
        let mut config = MappingConfig {
            id: "id".into(),
            name: "name".into(),
            protocol: Protocol::Webdav,
            host: "files.example.com".into(),
            port: 443,
            username: Some("user".into()),
            auth: AuthMethod::Password {
                credential_id: None,
            },
            remote_path: "/".into(),
            mount_point: "/mnt/files".into(),
            ftp_tls: false,
            host_key_fingerprint: None,
            sftp_totp_required: false,
            ignore_system_proxy: false,
            webdav_auth: WebDavAuthMethod::Digest,
            webdav_client_certificate_path: None,
            auto_mount: false,
        };

        assert!(config.validate().is_ok());
        config.username = None;
        assert!(config.validate().is_err());

        config.webdav_auth = WebDavAuthMethod::Bearer;
        assert!(config.validate().is_ok());

        config.webdav_auth = WebDavAuthMethod::ClientCertificate;
        config.auth = AuthMethod::Anonymous;
        config.webdav_client_certificate_path = Some("client-identity.pem".into());
        assert!(config.validate().is_ok());

        let portable = config.portable();
        assert_eq!(portable.webdav_auth, WebDavAuthMethod::ClientCertificate);
        assert!(portable.webdav_client_certificate_path.is_none());
    }
}
