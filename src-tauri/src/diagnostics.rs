use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use guglefs_core::{AuthMethod, MappingRuntime, MappingState, Protocol};
use serde::Serialize;

const LOG_FILE_NAME: &str = "guglefs.jsonl";
const MAX_LOG_BYTES: u64 = 1024 * 1024;
const ROTATED_LOGS: usize = 3;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticEvent<'a> {
    timestamp_unix_ms: u128,
    level: &'a str,
    event: &'a str,
    protocol: Option<&'a str>,
    outcome: &'a str,
    app_version: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MappingSummary {
    protocol: &'static str,
    state: &'static str,
    auth: &'static str,
    auto_mount: bool,
    requires_sftp_mfa: bool,
    has_last_error: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticReport {
    schema_version: u32,
    generated_at_unix_ms: u128,
    app_version: &'static str,
    os: &'static str,
    architecture: &'static str,
    mappings: Vec<MappingSummary>,
    events: Vec<serde_json::Value>,
}

pub struct DiagnosticStore {
    directory: PathBuf,
    max_log_bytes: u64,
    rotated_logs: usize,
    writer: Mutex<()>,
}

impl DiagnosticStore {
    pub fn new(directory: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&directory).map_err(|error| format!("创建诊断日志目录失败: {error}"))?;
        Ok(Self {
            directory,
            max_log_bytes: MAX_LOG_BYTES,
            rotated_logs: ROTATED_LOGS,
            writer: Mutex::new(()),
        })
    }

    pub fn record(&self, event: &'static str, protocol: Option<Protocol>, outcome: &'static str) {
        let protocol = protocol.map(protocol_name);
        let entry = DiagnosticEvent {
            timestamp_unix_ms: unix_timestamp_ms(),
            level: if outcome == "success" {
                "info"
            } else {
                "error"
            },
            event,
            protocol,
            outcome,
            app_version: env!("CARGO_PKG_VERSION"),
        };
        let Ok(mut line) = serde_json::to_vec(&entry) else {
            return;
        };
        line.push(b'\n');
        let Ok(_guard) = self.writer.lock() else {
            return;
        };
        if self.rotate_if_needed(line.len() as u64).is_err() {
            return;
        }
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.current_log_path())
        {
            let _ = file.write_all(&line);
        }
    }

    pub fn export_report(
        &self,
        path: &Path,
        mappings: Vec<MappingRuntime>,
    ) -> Result<usize, String> {
        if path.starts_with(&self.directory) {
            return Err("诊断报告不能覆盖应用日志文件".into());
        }
        let events = self.read_events()?;
        let event_count = events.len();
        let report = DiagnosticReport {
            schema_version: 1,
            generated_at_unix_ms: unix_timestamp_ms(),
            app_version: env!("CARGO_PKG_VERSION"),
            os: std::env::consts::OS,
            architecture: std::env::consts::ARCH,
            mappings: mappings.into_iter().map(mapping_summary).collect(),
            events,
        };
        let content = serde_json::to_vec_pretty(&report)
            .map_err(|error| format!("序列化诊断报告失败: {error}"))?;
        let parent = path
            .parent()
            .ok_or_else(|| "诊断报告路径缺少父目录".to_string())?;
        fs::create_dir_all(parent).map_err(|error| format!("创建诊断报告目录失败: {error}"))?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, content).map_err(|error| format!("写入诊断报告失败: {error}"))?;
        if path.exists() {
            fs::remove_file(path).map_err(|error| format!("替换诊断报告失败: {error}"))?;
        }
        fs::rename(&temporary, path).map_err(|error| format!("保存诊断报告失败: {error}"))?;
        Ok(event_count)
    }

    fn current_log_path(&self) -> PathBuf {
        self.directory.join(LOG_FILE_NAME)
    }

    fn rotated_log_path(&self, index: usize) -> PathBuf {
        self.directory.join(format!("{LOG_FILE_NAME}.{index}"))
    }

    fn rotate_if_needed(&self, incoming_bytes: u64) -> Result<(), String> {
        let current = self.current_log_path();
        let current_bytes = current.metadata().map(|value| value.len()).unwrap_or(0);
        if current_bytes.saturating_add(incoming_bytes) <= self.max_log_bytes {
            return Ok(());
        }
        for index in (1..=self.rotated_logs).rev() {
            let destination = self.rotated_log_path(index);
            if destination.exists() {
                fs::remove_file(&destination)
                    .map_err(|error| format!("轮转诊断日志失败: {error}"))?;
            }
            let source = if index == 1 {
                current.clone()
            } else {
                self.rotated_log_path(index - 1)
            };
            if source.exists() {
                fs::rename(source, destination)
                    .map_err(|error| format!("轮转诊断日志失败: {error}"))?;
            }
        }
        Ok(())
    }

    fn read_events(&self) -> Result<Vec<serde_json::Value>, String> {
        let _guard = self
            .writer
            .lock()
            .map_err(|_| "诊断日志锁已损坏".to_string())?;
        let mut events = Vec::new();
        for index in (1..=self.rotated_logs).rev() {
            read_json_lines(&self.rotated_log_path(index), &mut events)?;
        }
        read_json_lines(&self.current_log_path(), &mut events)?;
        Ok(events)
    }

    #[cfg(test)]
    fn with_limits(directory: PathBuf, max_log_bytes: u64, rotated_logs: usize) -> Self {
        fs::create_dir_all(&directory).unwrap();
        Self {
            directory,
            max_log_bytes,
            rotated_logs,
            writer: Mutex::new(()),
        }
    }
}

fn read_json_lines(path: &Path, events: &mut Vec<serde_json::Value>) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let content = fs::read_to_string(path).map_err(|error| format!("读取诊断日志失败: {error}"))?;
    events.extend(
        content
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok()),
    );
    Ok(())
}

fn mapping_summary(runtime: MappingRuntime) -> MappingSummary {
    MappingSummary {
        protocol: protocol_name(runtime.config.protocol),
        state: match runtime.state {
            MappingState::Unmounted => "unmounted",
            MappingState::Mounting => "mounting",
            MappingState::Mounted => "mounted",
            MappingState::Unmounting => "unmounting",
            MappingState::Error => "error",
        },
        auth: match runtime.config.auth {
            AuthMethod::Password { .. } => "password",
            AuthMethod::PrivateKey { .. } => "private_key",
            AuthMethod::SshAgent => "ssh_agent",
            AuthMethod::Anonymous => "anonymous",
        },
        auto_mount: runtime.config.auto_mount,
        requires_sftp_mfa: runtime.config.sftp_totp_required,
        has_last_error: runtime.last_error.is_some(),
    }
}

const fn protocol_name(protocol: Protocol) -> &'static str {
    match protocol {
        Protocol::Ftp => "ftp",
        Protocol::Sftp => "sftp",
        Protocol::Webdav => "webdav",
    }
}

fn unix_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "guglefs-{name}-{}-{}",
            std::process::id(),
            unix_timestamp_ms()
        ))
    }

    #[test]
    fn rotates_json_lines_without_recording_free_form_details() {
        let directory = temporary_directory("diagnostics-rotation");
        let store = DiagnosticStore::with_limits(directory.clone(), 100, 2);
        for _ in 0..8 {
            store.record("mapping_mount", Some(Protocol::Sftp), "success");
        }

        let events = store.read_events().unwrap();

        assert!(!events.is_empty());
        assert!(events.iter().all(|event| event.get("event").is_some()));
        assert!(directory.join(format!("{LOG_FILE_NAME}.1")).exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn exported_report_contains_only_sanitized_mapping_metadata() {
        let directory = temporary_directory("diagnostics-export");
        let output = directory.with_extension("json");
        let store = DiagnosticStore::new(directory.clone()).unwrap();
        store.record("application_start", None, "success");
        let runtime = MappingRuntime {
            config: guglefs_core::MappingConfig {
                id: "secret-id".into(),
                name: "private-name".into(),
                protocol: Protocol::Sftp,
                host: "private.example.com".into(),
                port: 22,
                username: Some("private-user".into()),
                auth: AuthMethod::Password {
                    credential_id: Some("private-credential".into()),
                },
                remote_path: "/private/path".into(),
                mount_point: "Z:".into(),
                ftp_tls: false,
                host_key_fingerprint: Some("SHA256:private".into()),
                sftp_totp_required: false,
                ignore_system_proxy: false,
                webdav_auth: Default::default(),
                webdav_client_certificate_path: None,
                auto_mount: true,
            },
            state: MappingState::Unmounted,
            last_error: Some("private error".into()),
        };

        store.export_report(&output, vec![runtime]).unwrap();
        let content = fs::read_to_string(&output).unwrap();

        for secret in [
            "secret-id",
            "private-name",
            "private.example.com",
            "private-user",
            "private-credential",
            "/private/path",
            "SHA256:private",
            "private error",
        ] {
            assert!(!content.contains(secret));
        }
        fs::remove_file(output).unwrap();
        fs::remove_dir_all(directory).unwrap();
    }
}
