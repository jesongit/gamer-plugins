//! Local connection profiles. API responses never contain reusable secrets.
use super::bilibili::Credentials;
use anyhow::{ensure, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    version: Option<String>,
    mode: String,
    profiles: BTreeMap<String, Credentials>,
}

pub struct Settings {
    path: PathBuf,
    gate: Mutex<()>,
}
impl Settings {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            gate: Mutex::new(()),
        }
    }
    fn load(&self) -> Result<Saved> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Saved::default()),
            Err(_) => anyhow::bail!("无法读取直播接入配置"),
        };
        ensure!(bytes.len() <= 64 * 1024, "直播接入配置文件过大");
        let bytes =
            protect(&bytes, false).context("无法解密直播接入配置，请使用保存配置的系统账号")?;
        // Do not include serde's input excerpt in an API error.
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("直播接入配置文件损坏"))
    }
    fn write(&self, saved: &mut Saved) -> Result<()> {
        saved.version = Some(uuid::Uuid::new_v4().to_string());
        let parent = self.path.parent().context("接入配置路径无效")?;
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        let bytes = protect(&serde_json::to_vec(saved)?, true)?;
        crate::core::fs::atomic_write(&self.path, &bytes).context("直播接入配置保存失败")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
    fn public(saved: &Saved) -> Value {
        let profiles: BTreeMap<_, _> = saved
            .profiles
            .iter()
            .map(|(mode, c)| {
                (
                    mode,
                    json!({
                        "access_key":c.access_key,"app_id":c.app_id,
                        "has_access_secret":!c.access_secret.is_empty(),
                        "has_identity_code":!c.identity_code.is_empty(),
                        "has_access_token":!c.access_token.is_empty()
                    }),
                )
            })
            .collect();
        json!({"version":saved.version,"mode":saved.mode,"profiles":profiles})
    }
    pub fn read(&self) -> Result<Value> {
        let _gate = self.gate.lock();
        Ok(Self::public(&self.load()?))
    }
    pub fn save(&self, mut c: Credentials, expected: Option<&str>) -> Result<(Credentials, Value)> {
        let _gate = self.gate.lock();
        let mut saved = self.load()?;
        ensure!(
            saved.version.as_deref() == expected,
            "接入配置已更新，请刷新后重试"
        );
        if let Some(previous) = saved
            .profiles
            .get(&c.mode)
            .filter(|p| p.access_key == c.access_key)
        {
            if c.access_secret.is_empty() {
                c.access_secret = previous.access_secret.clone();
            }
            if c.mode == "open_live" && c.app_id == previous.app_id && c.identity_code.is_empty() {
                c.identity_code = previous.identity_code.clone();
            }
            if c.mode == "oauth" && c.access_token.is_empty() {
                c.access_token = previous.access_token.clone();
            }
        }
        c.validate()?;
        saved.mode = c.mode.clone();
        saved.profiles.insert(c.mode.clone(), c.clone());
        self.write(&mut saved)?;
        Ok((c, Self::public(&saved)))
    }
    pub fn clear(&self, mode: &str, expected: Option<&str>) -> Result<Value> {
        ensure!(["open_live", "oauth"].contains(&mode), "接入方式无效");
        let _gate = self.gate.lock();
        let mut saved = self.load()?;
        ensure!(
            saved.version.as_deref() == expected,
            "接入配置已更新，请刷新后重试"
        );
        saved.profiles.remove(mode);
        saved.mode = mode.into();
        self.write(&mut saved)?;
        Ok(Self::public(&saved))
    }
}

use crate::core::secrets::protect;

#[cfg(test)]
mod tests {
    use super::*;
    fn credentials() -> Credentials {
        Credentials {
            mode: "open_live".into(),
            access_key: "test-key".into(),
            access_secret: "secret-fixture".into(),
            app_id: "123".into(),
            identity_code: "identity-fixture".into(),
            access_token: String::new(),
        }
    }
    #[test]
    fn survives_reopen_redacts_and_reuses_saved_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.dat");
        let (_, public) = Settings::new(path.clone())
            .save(credentials(), None)
            .unwrap();
        assert!(!public.to_string().contains("secret-fixture"));
        assert!(!public.to_string().contains("identity-fixture"));
        #[cfg(windows)]
        assert!(!String::from_utf8_lossy(&std::fs::read(&path).unwrap()).contains("secret-fixture"));
        let store = Settings::new(path);
        let public = store.read().unwrap();
        let mut c = credentials();
        c.access_secret.clear();
        c.identity_code.clear();
        let (resolved, updated) = store.save(c, public["version"].as_str()).unwrap();
        assert_eq!(resolved.access_secret, "secret-fixture");
        assert_eq!(resolved.identity_code, "identity-fixture");
        assert_ne!(public["version"], updated["version"]);
    }
    #[test]
    fn rejects_stale_account_changes_and_keeps_modes_separate() {
        let dir = tempfile::tempdir().unwrap();
        let store = Settings::new(dir.path().join("connection.dat"));
        let (_, saved) = store.save(credentials(), None).unwrap();
        assert!(store.save(credentials(), Some("stale")).is_err());
        let mut c = credentials();
        c.access_key = "another".into();
        c.access_secret.clear();
        c.identity_code.clear();
        assert!(store.save(c, saved["version"].as_str()).is_err());
        let mut c = credentials();
        c.mode = "oauth".into();
        c.app_id.clear();
        c.identity_code.clear();
        c.access_token = "token-fixture".into();
        let (_, saved) = store.save(c, saved["version"].as_str()).unwrap();
        assert!(saved["profiles"]["oauth"]["has_access_token"]
            .as_bool()
            .unwrap());
        assert!(saved["profiles"]["open_live"]["has_identity_code"]
            .as_bool()
            .unwrap());
        let cleared = store.clear("oauth", saved["version"].as_str()).unwrap();
        assert!(cleared["profiles"].get("oauth").is_none());
        assert!(cleared["profiles"].get("open_live").is_some());
    }
    #[test]
    fn failed_save_and_corruption_do_not_silently_reset_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connection.dat");
        std::fs::create_dir(&path).unwrap();
        assert!(Settings::new(path.clone())
            .save(credentials(), None)
            .is_err());
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, b"corrupt-secret-fixture").unwrap();
        let error = Settings::new(path.clone()).read().unwrap_err().to_string();
        assert!(!error.contains("secret-fixture"));
        assert_eq!(std::fs::read(path).unwrap(), b"corrupt-secret-fixture");
    }
}
