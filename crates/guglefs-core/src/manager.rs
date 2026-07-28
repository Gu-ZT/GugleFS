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

    pub fn get(&self, id: &str) -> EngineResult<MappingRuntime> {
        self.read()?
            .get(id)
            .cloned()
            .ok_or_else(|| EngineError::MappingNotFound(id.into()))
    }

    pub fn begin_mount(&self, id: &str) -> EngineResult<MappingRuntime> {
        let mut mappings = self.write()?;
        let runtime = mappings
            .get_mut(id)
            .ok_or_else(|| EngineError::MappingNotFound(id.into()))?;
        if matches!(
            runtime.state,
            MappingState::Mounting | MappingState::Mounted
        ) {
            return Err(EngineError::AlreadyMounted(id.into()));
        }
        runtime.state = MappingState::Mounting;
        runtime.last_error = None;
        Ok(runtime.clone())
    }

    pub fn finish_mount(
        &self,
        id: &str,
        state: MappingState,
        last_error: Option<String>,
    ) -> EngineResult<MappingRuntime> {
        let mut mappings = self.write()?;
        let runtime = mappings
            .get_mut(id)
            .ok_or_else(|| EngineError::MappingNotFound(id.into()))?;
        runtime.state = state;
        runtime.last_error = last_error;
        Ok(runtime.clone())
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

        if let Some(existing) = mappings.get(&config.id) {
            if matches!(
                existing.state,
                MappingState::Mounting | MappingState::Mounted
            ) && existing.config != config
            {
                return Err(EngineError::AlreadyMounted(config.id));
            }
        }

        let runtime = MappingRuntime {
            config: config.clone(),
            state: mappings
                .get(&config.id)
                .map(|item| match item.state {
                    MappingState::Mounting | MappingState::Mounted => item.state,
                    MappingState::Unmounted | MappingState::Error => MappingState::Unmounted,
                })
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
        if matches!(
            runtime.state,
            MappingState::Mounting | MappingState::Mounted
        ) {
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
            ftp_tls: false,
            host_key_fingerprint: Some("SHA256:test".into()),
            ignore_system_proxy: false,
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

    #[test]
    fn tracks_mount_lifecycle_and_clears_errors() {
        let manager = MappingManager::default();
        manager.upsert(config("mapping", "Z:")).unwrap();

        assert_eq!(
            manager.begin_mount("mapping").unwrap().state,
            MappingState::Mounting
        );
        assert!(matches!(
            manager.begin_mount("mapping"),
            Err(EngineError::AlreadyMounted(_))
        ));
        manager
            .finish_mount(
                "mapping",
                MappingState::Error,
                Some("connection failed".into()),
            )
            .unwrap();
        assert_eq!(
            manager.begin_mount("mapping").unwrap().state,
            MappingState::Mounting
        );
        let runtime = manager
            .finish_mount("mapping", MappingState::Unmounted, None)
            .unwrap();
        assert_eq!(runtime.state, MappingState::Unmounted);
        assert_eq!(runtime.last_error, None);
    }

    #[test]
    fn rejects_editing_a_mounted_mapping() {
        let manager = MappingManager::default();
        manager.upsert(config("mapping", "Z:")).unwrap();
        manager.begin_mount("mapping").unwrap();
        manager
            .finish_mount("mapping", MappingState::Mounted, None)
            .unwrap();

        let mut changed = config("mapping", "Z:");
        changed.name = "changed".into();
        assert!(matches!(
            manager.upsert(changed),
            Err(EngineError::AlreadyMounted(_))
        ));
    }

    #[test]
    fn error_mappings_can_be_edited_retried_and_removed() {
        let manager = MappingManager::default();
        manager.upsert(config("mapping", "Z:")).unwrap();
        manager.begin_mount("mapping").unwrap();
        manager
            .finish_mount("mapping", MappingState::Error, Some("mount failed".into()))
            .unwrap();

        let mut changed = config("mapping", "Z:");
        changed.name = "changed".into();
        let updated = manager.upsert(changed).unwrap();
        assert_eq!(updated.state, MappingState::Unmounted);
        assert_eq!(updated.last_error, None);

        manager
            .finish_mount(
                "mapping",
                MappingState::Error,
                Some("mount failed again".into()),
            )
            .unwrap();
        manager.remove("mapping").unwrap();
        assert!(matches!(
            manager.get("mapping"),
            Err(EngineError::MappingNotFound(_))
        ));
    }
}
