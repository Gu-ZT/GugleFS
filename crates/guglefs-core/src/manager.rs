use std::{
    collections::HashMap,
    sync::{RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use crate::{EngineError, EngineResult, MappingConfig, MappingRuntime, MappingState};

#[derive(Debug, Default)]
pub struct MappingManager {
    mappings: RwLock<HashMap<String, MappingRuntime>>,
}

impl MappingManager {
    pub fn list(&self) -> EngineResult<Vec<MappingRuntime>> {
        let mut values: Vec<_> = self.read()?.values().cloned().collect();
        values.sort_by(|left, right| left.config.name.cmp(&right.config.name));
        Ok(values)
    }

    pub fn upsert(&self, config: MappingConfig) -> EngineResult<MappingRuntime> {
        config.validate()?;
        let mut mappings = self.write()?;

        if mappings.values().any(|item| {
            item.config.id != config.id && item.config.mount_point == config.mount_point
        }) {
            return Err(EngineError::InvalidConfig(format!(
                "mount point is already in use: {}",
                config.mount_point
            )));
        }

        let runtime = MappingRuntime {
            config: config.clone(),
            state: mappings
                .get(&config.id)
                .map(|item| item.state)
                .unwrap_or(MappingState::Unmounted),
            last_error: None,
        };
        mappings.insert(config.id, runtime.clone());
        Ok(runtime)
    }

    pub fn remove(&self, id: &str) -> EngineResult<()> {
        let mut mappings = self.write()?;
        let runtime = mappings
            .get(id)
            .ok_or_else(|| EngineError::MappingNotFound(id.into()))?;
        if runtime.state != MappingState::Unmounted {
            return Err(EngineError::AlreadyMounted(id.into()));
        }
        mappings.remove(id);
        Ok(())
    }

    fn read(&self) -> EngineResult<RwLockReadGuard<'_, HashMap<String, MappingRuntime>>> {
        self.mappings
            .read()
            .map_err(|error| EngineError::Internal(error.to_string()))
    }

    fn write(&self) -> EngineResult<RwLockWriteGuard<'_, HashMap<String, MappingRuntime>>> {
        self.mappings
            .write()
            .map_err(|error| EngineError::Internal(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuthMethod, Protocol};

    fn config(id: &str, mount_point: &str) -> MappingConfig {
        MappingConfig {
            id: id.into(),
            name: id.into(),
            protocol: Protocol::Sftp,
            host: "example.test".into(),
            port: 22,
            username: Some("user".into()),
            auth: AuthMethod::Password {
                credential_id: None,
            },
            remote_path: "/data".into(),
            mount_point: mount_point.into(),
            auto_mount: false,
        }
    }

    #[test]
    fn rejects_duplicate_mount_points() {
        let manager = MappingManager::default();
        manager.upsert(config("first", "Z:")).unwrap();

        let result = manager.upsert(config("second", "Z:"));

        assert!(matches!(result, Err(EngineError::InvalidConfig(_))));
    }
}
