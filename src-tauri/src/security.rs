use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use serde::Serialize;
use totp_rs::{Algorithm, Secret, TOTP};

const TOTP_SERVICE: &str = "dev.guglefs.desktop.security";
const TOTP_ACCOUNT: &str = "startup-totp";
const MAPPING_SERVICE: &str = "dev.guglefs.desktop.mapping";
const MAX_FAILURES: u8 = 5;
const LOCKOUT_DURATION: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub configured: bool,
    pub unlocked: bool,
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
    pending_setup: Mutex<Option<String>>,
    attempts: Mutex<AttemptState>,
}

impl SecurityManager {
    pub fn status(&self) -> Result<AuthStatus, String> {
        let configured = read_secure_value(TOTP_SERVICE, TOTP_ACCOUNT)?.is_some();
        Ok(AuthStatus {
            configured,
            unlocked: configured && self.unlocked.load(Ordering::Acquire),
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
        self.verify_code(&secret, code)?;
        self.unlocked.store(true, Ordering::Release);
        self.status()
    }

    pub fn lock(&self) -> Result<AuthStatus, String> {
        self.unlocked.store(false, Ordering::Release);
        self.status()
    }

    pub fn require_unlocked(&self) -> Result<(), String> {
        if self.unlocked.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err("应用已锁定，请先完成 2FA 验证".into())
        }
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

#[cfg(target_os = "windows")]
fn read_secure_value(service: &str, account: &str) -> Result<Option<String>, String> {
    let entry = keyring::Entry::new(service, account)
        .map_err(|error| format!("打开 Windows 凭据失败: {error}"))?;
    match entry.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!("读取 Windows 凭据失败: {error}")),
    }
}

#[cfg(not(target_os = "windows"))]
fn read_secure_value(_service: &str, _account: &str) -> Result<Option<String>, String> {
    Err("当前平台尚未接入安全凭据库".into())
}

#[cfg(target_os = "windows")]
fn write_secure_value(service: &str, account: &str, value: &str) -> Result<(), String> {
    keyring::Entry::new(service, account)
        .map_err(|error| format!("打开 Windows 凭据失败: {error}"))?
        .set_password(value)
        .map_err(|error| format!("保存 Windows 凭据失败: {error}"))
}

#[cfg(not(target_os = "windows"))]
fn write_secure_value(_service: &str, _account: &str, _value: &str) -> Result<(), String> {
    Err("当前平台尚未接入安全凭据库".into())
}

#[cfg(target_os = "windows")]
fn delete_secure_value(service: &str, account: &str) -> Result<(), String> {
    let entry = keyring::Entry::new(service, account)
        .map_err(|error| format!("打开 Windows 凭据失败: {error}"))?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!("删除 Windows 凭据失败: {error}")),
    }
}

#[cfg(not(target_os = "windows"))]
fn delete_secure_value(_service: &str, _account: &str) -> Result<(), String> {
    Err("当前平台尚未接入安全凭据库".into())
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
    fn a_new_security_manager_starts_locked() {
        assert!(SecurityManager::default().require_unlocked().is_err());
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
}
