use std::{fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::{EngineError, EngineResult, MappingConfig, MappingManager};

pub const CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigDocument {
    pub schema_version: u32,
    pub mappings: Vec<MappingConfig>,
}

impl ConfigDocument {
    pub fn current(mappings: Vec<MappingConfig>) -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            mappings,
        }
    }

    pub fn validate(&self) -> EngineResult<()> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(EngineError::InvalidConfig(format!(
                "unsupported config schema version: {}",
                self.schema_version
            )));
        }
        Ok(())
    }
}

impl MappingManager {
    pub fn from_configs(configs: impl IntoIterator<Item = MappingConfig>) -> EngineResult<Self> {
        let manager = Self::default();
        for config in configs {
            manager.upsert(config)?;
        }
        Ok(manager)
    }

    pub fn to_document(&self) -> EngineResult<ConfigDocument> {
        let mappings = self
            .list()?
            .into_iter()
            .map(|runtime| runtime.config)
            .collect();
        Ok(ConfigDocument::current(mappings))
    }

    pub fn load_from_path(path: &Path) -> EngineResult<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(path)
            .map_err(|error| EngineError::Internal(format!("read config: {error}")))?;
        let document: ConfigDocument = serde_json::from_str(&content)
            .map_err(|error| EngineError::InvalidConfig(format!("parse config: {error}")))?;
        document.validate()?;
        Self::from_configs(document.mappings)
    }

    pub fn save_to_path(&self, path: &Path) -> EngineResult<()> {
        let document = self.to_document()?;
        let content = serde_json::to_vec_pretty(&document)
            .map_err(|error| EngineError::Internal(format!("serialize config: {error}")))?;
        let parent = path.parent().ok_or_else(|| {
            EngineError::Internal("config path does not have a parent directory".into())
        })?;
        fs::create_dir_all(parent)
            .map_err(|error| EngineError::Internal(format!("create config directory: {error}")))?;

        let temporary_path = path.with_extension("json.tmp");
        fs::write(&temporary_path, content)
            .map_err(|error| EngineError::Internal(format!("write config: {error}")))?;
        if path.exists() {
            fs::remove_file(path)
                .map_err(|error| EngineError::Internal(format!("replace config: {error}")))?;
        }
        fs::rename(&temporary_path, path)
            .map_err(|error| EngineError::Internal(format!("commit config: {error}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_schema_versions() {
        let document = ConfigDocument {
            schema_version: CONFIG_SCHEMA_VERSION + 1,
            mappings: Vec::new(),
        };

        assert!(document.validate().is_err());
    }
}
