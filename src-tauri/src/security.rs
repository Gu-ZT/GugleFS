use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use serde::Serialize;
use totp_rs::{Algorithm, Secret, TOTP};

const TOTP_SERVICE: &str = "dev.guglefs.desktop.security";
const TOTP_ACCOUNT: &str = "startup-totp";
const TOTP_REQUIRED_ACCOUNT: &str = "startup-totp-required";
const MAPPING_SERVICE: &str = "dev.guglefs.desktop.mapping";
const PRIVATE_KEY_SERVICE: &str = "dev.guglefs.desktop.private-key";
const PRIVATE_KEY_CHUNK_BYTES: usize = 900;
const MAX_PRIVATE_KEY_BYTES: usize = 128 * 1024;
static PRIVATE_KEY_ID_COUNTER: AtomicU64 = AtomicU64::new(0);
const MAX_FAILURES: u8 = 5;
const LOCKOUT_DURATION: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub configured: bool,
    pub unlocked: bool,
    pub two_factor_enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TotpSetup {
    pub secret: String,
    pub qr_code: String,
}

#[derive(Debug, Default)]
struct AttemptState {
    failures: u8,
    blocked_until: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct SecurityManager {
    unlocked: AtomicBool,
    startup_initialized: AtomicBool,
    pending_setup: Mutex<Option<String>>,
    attempts: Mutex<AttemptState>,
}

impl SecurityManager {
    pub fn status(&self) -> Result<AuthStatus, String> {
        let configured = read_secure_value(TOTP_SERVICE, TOTP_ACCOUNT)?.is_some();
        let two_factor_enabled = if configured {
            self.two_factor_enabled()?
        } else {
            true
        };
        if configured
            && !self.startup_initialized.swap(true, Ordering::AcqRel)
            && !two_factor_enabled
        {
            self.unlocked.store(true, Ordering::Release);
        }
        Ok(AuthStatus {
            configured,
            unlocked: configured && self.unlocked.load(Ordering::Acquire),
            two_factor_enabled,
        })
    }

    pub fn begin_setup(&self) -> Result<TotpSetup, String> {
        if self.status()?.configured {
            return Err("2FA 已配置，请先解锁应用".into());
        }

        let secret = Secret::generate_secret().to_encoded().to_string();
        let totp = totp_from_secret(&secret)?;
        let qr = totp
            .get_qr_base64()
            .map_err(|error| format!("生成 2FA 二维码失败: {error}"))?;
        *self
            .pending_setup
            .lock()
            .map_err(|error| format!("访问 2FA 设置状态失败: {error}"))? = Some(secret.clone());
        Ok(TotpSetup {
            secret,
            qr_code: if qr.starts_with("data:") {
                qr
            } else {
                format!("data:image/png;base64,{qr}")
            },
        })
    }

    pub fn confirm_setup(&self, code: &str) -> Result<AuthStatus, String> {
        let secret = self
            .pending_setup
            .lock()
            .map_err(|error| format!("访问 2FA 设置状态失败: {error}"))?
            .clone()
            .ok_or_else(|| "2FA 设置已过期，请重新生成".to_string())?;
        self.verify_code(&secret, code)?;
        write_secure_value(TOTP_SERVICE, TOTP_ACCOUNT, &secret)?;
        *self
            .pending_setup
            .lock()
            .map_err(|error| format!("更新 2FA 设置状态失败: {error}"))? = None;
        self.unlocked.store(true, Ordering::Release);
        self.status()
    }

    pub fn unlock(&self, code: &str) -> Result<AuthStatus, String> {
        let secret = read_secure_value(TOTP_SERVICE, TOTP_ACCOUNT)?
            .ok_or_else(|| "尚未配置 2FA".to_string())?;
        if self.two_factor_enabled()? {
            self.verify_code(&secret, code)?;
        }
        self.unlocked.store(true, Ordering::Release);
        self.status()
    }

    pub fn set_two_factor_enabled(
        &self,
        enabled: bool,
        code: Option<&str>,
    ) -> Result<AuthStatus, String> {
        self.require_unlocked()?;
        let secret = read_secure_value(TOTP_SERVICE, TOTP_ACCOUNT)?
            .ok_or_else(|| "尚未配置 2FA".to_string())?;
        let current = self.two_factor_enabled()?;
        if !enabled && current {
            self.verify_code(&secret, code.unwrap_or_default())?;
        }
        if current != enabled {
            write_secure_value(
                TOTP_SERVICE,
                TOTP_REQUIRED_ACCOUNT,
                if enabled { "true" } else { "false" },
            )?;
        }
        self.status()
    }

    pub fn lock(&self) -> Result<AuthStatus, String> {
        self.startup_initialized.store(true, Ordering::Release);
        self.unlocked.store(false, Ordering::Release);
        self.status()
    }

    pub fn require_unlocked(&self) -> Result<(), String> {
        if self.unlocked.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err("应用已锁定，请先解锁".into())
        }
    }

    fn two_factor_enabled(&self) -> Result<bool, String> {
        Ok(read_secure_value(TOTP_SERVICE, TOTP_REQUIRED_ACCOUNT)?
            .as_deref()
            .map(parse_two_factor_enabled)
            .unwrap_or(true))
    }

    pub fn store_mapping_password(
        &self,
        credential_id: &str,
        password: &str,
    ) -> Result<(), String> {
        self.require_unlocked()?;
        if password.is_empty() {
            return Err("密码不能为空".into());
        }
        write_secure_value(MAPPING_SERVICE, credential_id, password)
    }

    pub fn mapping_password(&self, credential_id: &str) -> Result<Option<String>, String> {
        self.require_unlocked()?;
        read_secure_value(MAPPING_SERVICE, credential_id)
    }

    pub fn delete_mapping_password(&self, credential_id: &str) -> Result<(), String> {
        self.require_unlocked()?;
        delete_secure_value(MAPPING_SERVICE, credential_id)
    }

    pub fn mapping_credential_id(mapping_id: &str) -> String {
        format!("mapping-{mapping_id}")
    }

    pub fn new_private_key_id(mapping_id: &str) -> String {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let counter = PRIVATE_KEY_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        format!(
            "private-key-{mapping_id}-{}-{nonce}-{counter}",
            std::process::id()
        )
    }

    pub fn store_mapping_private_key(&self, key_id: &str, private_key: &str) -> Result<(), String> {
        self.require_unlocked()?;
        let private_key = private_key.trim();
        if private_key.is_empty() {
            return Err("SSH 私钥不能为空".into());
        }
        if !private_key.is_ascii() {
            return Err("SSH 私钥必须是 ASCII PEM/OpenSSH 文本".into());
        }
        if private_key.len() > MAX_PRIVATE_KEY_BYTES {
            return Err("SSH 私钥超过 128 KiB 限制".into());
        }
        let chunks: Vec<_> = private_key
            .as_bytes()
            .chunks(PRIVATE_KEY_CHUNK_BYTES)
            .collect();
        for (index, chunk) in chunks.iter().enumerate() {
            let value = std::str::from_utf8(chunk)
                .map_err(|error| format!("分割 SSH 私钥失败: {error}"))?;
            if let Err(error) = write_secure_value(
                PRIVATE_KEY_SERVICE,
                &private_key_chunk_account(key_id, index),
                value,
            ) {
                for cleanup_index in 0..index {
                    let _ = delete_secure_value(
                        PRIVATE_KEY_SERVICE,
                        &private_key_chunk_account(key_id, cleanup_index),
                    );
                }
                return Err(error);
            }
        }
        if let Err(error) = write_secure_value(
            PRIVATE_KEY_SERVICE,
            &private_key_manifest_account(key_id),
            &chunks.len().to_string(),
        ) {
            for index in 0..chunks.len() {
                let _ = delete_secure_value(
                    PRIVATE_KEY_SERVICE,
                    &private_key_chunk_account(key_id, index),
                );
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn mapping_private_key(&self, key_id: &str) -> Result<Option<String>, String> {
        self.require_unlocked()?;
        let Some(manifest) =
            read_secure_value(PRIVATE_KEY_SERVICE, &private_key_manifest_account(key_id))?
        else {
            return Ok(None);
        };
        let chunk_count: usize = manifest
            .parse()
            .map_err(|error| format!("SSH 私钥凭据索引损坏: {error}"))?;
        if chunk_count == 0 || chunk_count > MAX_PRIVATE_KEY_BYTES.div_ceil(PRIVATE_KEY_CHUNK_BYTES)
        {
            return Err("SSH 私钥凭据索引超出有效范围".into());
        }
        let mut private_key = String::new();
        for index in 0..chunk_count {
            let chunk = read_secure_value(
                PRIVATE_KEY_SERVICE,
                &private_key_chunk_account(key_id, index),
            )?
            .ok_or_else(|| format!("SSH 私钥凭据缺少第 {} 个分块", index + 1))?;
            private_key.push_str(&chunk);
        }
        Ok(Some(private_key))
    }

    pub fn delete_mapping_private_key(&self, key_id: &str) -> Result<(), String> {
        self.require_unlocked()?;
        let manifest_account = private_key_manifest_account(key_id);
        let chunk_count = read_secure_value(PRIVATE_KEY_SERVICE, &manifest_account)?
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or_default()
            .min(MAX_PRIVATE_KEY_BYTES.div_ceil(PRIVATE_KEY_CHUNK_BYTES));
        for index in 0..chunk_count {
            delete_secure_value(
                PRIVATE_KEY_SERVICE,
                &private_key_chunk_account(key_id, index),
            )?;
        }
        delete_secure_value(PRIVATE_KEY_SERVICE, &manifest_account)
    }

    fn verify_code(&self, secret: &str, code: &str) -> Result<(), String> {
        let code = code.trim();
        if code.len() != 6 || !code.bytes().all(|value| value.is_ascii_digit()) {
            return Err("请输入 6 位验证码".into());
        }
        self.check_rate_limit()?;
        let valid = totp_from_secret(secret)?
            .check_current(code)
            .map_err(|error| format!("读取系统时间失败: {error}"))?;
        if valid {
            let mut attempts = self
                .attempts
                .lock()
                .map_err(|error| format!("更新验证状态失败: {error}"))?;
            *attempts = AttemptState::default();
            return Ok(());
        }

        let mut attempts = self
            .attempts
            .lock()
            .map_err(|error| format!("更新验证状态失败: {error}"))?;
        attempts.failures = attempts.failures.saturating_add(1);
        if attempts.failures >= MAX_FAILURES {
            attempts.failures = 0;
            attempts.blocked_until = Some(Instant::now() + LOCKOUT_DURATION);
            return Err("验证码错误次数过多，请 30 秒后重试".into());
        }
        Err("验证码无效".into())
    }

    fn check_rate_limit(&self) -> Result<(), String> {
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|error| format!("读取验证状态失败: {error}"))?;
        if let Some(blocked_until) = attempts.blocked_until {
            if blocked_until > Instant::now() {
                return Err("验证码错误次数过多，请稍后重试".into());
            }
            *attempts = AttemptState::default();
        }
        Ok(())
    }
}

fn private_key_manifest_account(key_id: &str) -> String {
    format!("{key_id}-manifest")
}

fn private_key_chunk_account(key_id: &str, index: usize) -> String {
    format!("{key_id}-chunk-{index}")
}

fn parse_two_factor_enabled(value: &str) -> bool {
    value != "false"
}

fn totp_from_secret(secret: &str) -> Result<TOTP, String> {
    let bytes = Secret::Encoded(secret.to_string())
        .to_bytes()
        .map_err(|error| format!("解析 2FA 密钥失败: {error}"))?;
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        bytes,
        Some("GugleFS".into()),
        "Desktop".into(),
    )
    .map_err(|error| format!("创建 2FA 验证器失败: {error}"))
}

fn read_secure_value(service: &str, account: &str) -> Result<Option<String>, String> {
    let entry = keyring::Entry::new(service, account)
        .map_err(|error| format!("打开{}失败: {error}", secure_store_name()))?;
    match entry.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!("读取{}失败: {error}", secure_store_name())),
    }
}

fn write_secure_value(service: &str, account: &str, value: &str) -> Result<(), String> {
    keyring::Entry::new(service, account)
        .map_err(|error| format!("打开{}失败: {error}", secure_store_name()))?
        .set_password(value)
        .map_err(|error| format!("保存{}失败: {error}", secure_store_name()))
}

fn delete_secure_value(service: &str, account: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(service, account)
        .map_err(|error| format!("打开{}失败: {error}", secure_store_name()))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!("删除{}失败: {error}", secure_store_name())),
    }
}

pub const fn secure_store_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Windows 凭据管理器"
    }
    #[cfg(target_os = "macos")]
    {
        "macOS 钥匙串"
    }
    #[cfg(target_os = "linux")]
    {
        "Linux Secret Service"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_standard_totp_and_checks_its_code() {
        let secret = "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP";
        let totp = totp_from_secret(secret).unwrap();
        let code = totp.generate(1_700_000_000);

        assert_eq!(code.len(), 6);
        assert!(totp.check(&code, 1_700_000_000));
    }

    #[test]
    fn mapping_credential_ids_are_stable_and_scoped() {
        assert_eq!(
            SecurityManager::mapping_credential_id("example"),
            "mapping-example"
        );
    }

    #[test]
    fn private_key_ids_are_unique_and_scoped() {
        let first = SecurityManager::new_private_key_id("example");
        let second = SecurityManager::new_private_key_id("example");
        assert!(first.starts_with("private-key-example-"));
        assert_ne!(first, second);
    }

    #[test]
    fn a_new_security_manager_starts_locked() {
        assert!(SecurityManager::default().require_unlocked().is_err());
    }

    #[test]
    fn missing_or_unknown_two_factor_setting_stays_enabled() {
        assert!(parse_two_factor_enabled("true"));
        assert!(parse_two_factor_enabled("unknown"));
        assert!(!parse_two_factor_enabled("false"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_credential_manager_round_trip() {
        let account = format!(
            "test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );

        write_secure_value(MAPPING_SERVICE, &account, "credential-test").unwrap();
        assert_eq!(
            read_secure_value(MAPPING_SERVICE, &account).unwrap(),
            Some("credential-test".into())
        );
        delete_secure_value(MAPPING_SERVICE, &account).unwrap();
        assert_eq!(read_secure_value(MAPPING_SERVICE, &account).unwrap(), None);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_credential_manager_round_trips_chunked_private_keys() {
        let manager = SecurityManager::default();
        manager.unlocked.store(true, Ordering::Release);
        let key_id = SecurityManager::new_private_key_id("test");
        let private_key = format!(
            "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----",
            "A".repeat(PRIVATE_KEY_CHUNK_BYTES * 3)
        );

        manager
            .store_mapping_private_key(&key_id, &private_key)
            .unwrap();
        assert_eq!(
            manager.mapping_private_key(&key_id).unwrap(),
            Some(private_key)
        );
        manager.delete_mapping_private_key(&key_id).unwrap();
        assert_eq!(manager.mapping_private_key(&key_id).unwrap(), None);
    }
}
