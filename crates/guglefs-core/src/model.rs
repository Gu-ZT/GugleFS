use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Ftp,
    Sftp,
    Webdav,
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
    pub auto_mount: bool,
}

impl MappingConfig {
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
        Ok(())
    }

    pub fn portable(mut self) -> Self {
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
    use super::{AuthMethod, MappingConfig, Protocol};

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
        assert!(!portable.auto_mount);
        assert_eq!(
            portable.host_key_fingerprint.as_deref(),
            Some("SHA256:test")
        );
    }
}
