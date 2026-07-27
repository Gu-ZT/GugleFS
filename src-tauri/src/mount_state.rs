use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    sync::{Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};

const MOUNT_STATE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MountStateDocument {
    schema_version: u32,
    mounted_mapping_ids: BTreeSet<String>,
}

#[derive(Debug)]
pub struct MountStateStore {
    path: PathBuf,
    mounted_mapping_ids: Mutex<BTreeSet<String>>,
}

impl MountStateStore {
    pub fn load_from_path(path: PathBuf) -> Result<Self, String> {
        let mounted_mapping_ids = if path.exists() {
            let content = fs::read_to_string(&path)
                .map_err(|error| format!("读取挂载恢复状态失败: {error}"))?;
            let document: MountStateDocument = serde_json::from_str(&content)
                .map_err(|error| format!("解析挂载恢复状态失败: {error}"))?;
            if document.schema_version != MOUNT_STATE_SCHEMA_VERSION {
                return Err(format!(
                    "不支持的挂载恢复状态版本: {}",
                    document.schema_version
                ));
            }
            document.mounted_mapping_ids
        } else {
            BTreeSet::new()
        };
        Ok(Self {
            path,
            mounted_mapping_ids: Mutex::new(mounted_mapping_ids),
        })
    }

    pub fn mounted_mapping_ids(&self) -> Result<BTreeSet<String>, String> {
        Ok(self.lock()?.clone())
    }

    pub fn contains(&self, mapping_id: &str) -> Result<bool, String> {
        Ok(self.lock()?.contains(mapping_id))
    }

    pub fn remember(&self, mapping_id: &str) -> Result<(), String> {
        let mut ids = self.lock()?;
        if !ids.insert(mapping_id.to_string()) {
            return Ok(());
        }
        if let Err(error) = self.save(&ids) {
            ids.remove(mapping_id);
            return Err(error);
        }
        Ok(())
    }

    pub fn forget(&self, mapping_id: &str) -> Result<(), String> {
        let mut ids = self.lock()?;
        if !ids.remove(mapping_id) {
            return Ok(());
        }
        if let Err(error) = self.save(&ids) {
            ids.insert(mapping_id.to_string());
            return Err(error);
        }
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, BTreeSet<String>>, String> {
        self.mounted_mapping_ids
            .lock()
            .map_err(|error| format!("访问挂载恢复状态失败: {error}"))
    }

    fn save(&self, mounted_mapping_ids: &BTreeSet<String>) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "挂载恢复状态路径缺少父目录".to_string())?;
        fs::create_dir_all(parent).map_err(|error| format!("创建挂载恢复状态目录失败: {error}"))?;
        let document = MountStateDocument {
            schema_version: MOUNT_STATE_SCHEMA_VERSION,
            mounted_mapping_ids: mounted_mapping_ids.clone(),
        };
        let content = serde_json::to_vec_pretty(&document)
            .map_err(|error| format!("序列化挂载恢复状态失败: {error}"))?;
        let temporary_path = self.path.with_extension("json.tmp");
        fs::write(&temporary_path, content)
            .map_err(|error| format!("写入挂载恢复状态失败: {error}"))?;
        if self.path.exists() {
            fs::remove_file(&self.path)
                .map_err(|error| format!("替换挂载恢复状态失败: {error}"))?;
        }
        fs::rename(&temporary_path, &self.path)
            .map_err(|error| format!("提交挂载恢复状态失败: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path(name: &str) -> PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "guglefs-{name}-{}-{unique}.json",
            std::process::id()
        ))
    }

    #[test]
    fn remembers_and_forgets_mounted_mapping_ids() {
        let path = test_path("mount-state");
        let store = MountStateStore::load_from_path(path.clone()).unwrap();

        store.remember("first").unwrap();
        store.remember("first").unwrap();
        store.remember("second").unwrap();

        let reloaded = MountStateStore::load_from_path(path.clone()).unwrap();
        assert_eq!(
            reloaded.mounted_mapping_ids().unwrap(),
            BTreeSet::from(["first".to_string(), "second".to_string()])
        );
        reloaded.forget("first").unwrap();
        assert!(!reloaded.contains("first").unwrap());
        assert!(reloaded.contains("second").unwrap());

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_unknown_mount_state_versions() {
        let path = test_path("mount-state-version");
        fs::write(&path, r#"{"schemaVersion":2,"mountedMappingIds":[]}"#).unwrap();

        assert!(MountStateStore::load_from_path(path.clone()).is_err());

        fs::remove_file(path).unwrap();
    }
}
