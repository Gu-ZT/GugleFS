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
        key_path: String,
        credential_id: Option<String>,
    },
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
        Ok(())
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
    use super::Protocol;

    #[test]
    fn protocols_have_expected_default_ports() {
        assert_eq!(Protocol::Ftp.default_port(), 21);
        assert_eq!(Protocol::Sftp.default_port(), 22);
        assert_eq!(Protocol::Webdav.default_port(), 443);
    }
}
