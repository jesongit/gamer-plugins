//! Account-scoped model credentials. None of this storage belongs to a Package.
use anyhow::{ensure, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SettingsConfig {
    pub base_url: String,
    pub model: String,
    pub protocol: String,
    pub request_timeout_secs: u64,
}

impl Default for SettingsConfig {
    fn default() -> Self {
        Self {
            base_url: "https://open.bigmodel.cn/api/v1".into(),
            model: "glm-5.3-flash".into(),
            protocol: "responses".into(),
            request_timeout_secs: 90,
        }
    }
}

// Deliberately no Debug: a failed test or log must not print the credential.
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PrivateSettings {
    pub version: Option<String>,
    #[serde(default)]
    pub config: SettingsConfig,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub probe: Option<Value>,
}

#[derive(Clone)]
pub struct ConnectionConfig {
    pub base_url: String,
    pub model: String,
    pub protocol: String,
    pub request_timeout_secs: u64,
    pub api_key: String,
}

pub struct Settings {
    path: PathBuf,
    gate: Mutex<()>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveRequest {
    expected_version: Option<String>,
    base_url: String,
    model: String,
    protocol: String,
    request_timeout_secs: u64,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    clear_key: bool,
}

impl Settings {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            gate: Mutex::new(()),
        }
    }

    pub fn read(&self) -> Result<Value> {
        let _guard = self.gate.lock();
        Ok(self.load()?.public())
    }

    pub fn save(&self, values: Value) -> Result<Value> {
        let request: SaveRequest =
            serde_json::from_value(values).map_err(|_| anyhow::anyhow!("AI 配置字段无效"))?;
        let config = SettingsConfig {
            base_url: normalize_base_url(&request.base_url)?,
            model: request.model.trim().to_string(),
            protocol: request.protocol,
            request_timeout_secs: request.request_timeout_secs,
        };
        validate_config(&config)?;
        ensure!(request.api_key.len() <= 4096, "AI 密钥过长");
        ensure!(
            !request.api_key.chars().any(char::is_control),
            "AI 密钥不能包含控制字符"
        );
        ensure!(
            !request.clear_key || request.api_key.trim().is_empty(),
            "清除密钥时不能同时填写新密钥"
        );
        let _guard = self.gate.lock();
        let mut settings = self.load()?;
        ensure!(
            settings.version == request.expected_version,
            "version_conflict: AI 配置已变更，请刷新后保存"
        );
        let key = if request.clear_key {
            String::new()
        } else if !request.api_key.trim().is_empty() {
            request.api_key.trim().to_string()
        } else {
            settings.api_key.clone()
        };
        if settings.config != config || settings.api_key != key {
            settings.probe = None;
        }
        settings.config = config;
        settings.api_key = key;
        self.write(&mut settings)?;
        Ok(settings.public())
    }

    /// A probe is attached only to exactly the connection which was tested.
    pub fn save_probe(&self, probe: Value, expected_version: Option<&str>) -> Result<Value> {
        let _guard = self.gate.lock();
        let mut settings = self.load()?;
        ensure!(
            settings.version.as_deref() == expected_version,
            "version_conflict: 测试期间 AI 配置已变更，请重新测试"
        );
        ensure!(probe.is_object(), "AI 能力测试结果无效");
        // Whitelist the public diagnostic fields. Never persist request bodies or
        // a supplier response which might echo an Authorization header.
        let mut safe = serde_json::Map::new();
        for key in [
            "ok",
            "protocol",
            "model",
            "checked_at",
            "checks",
            "usage",
            "error",
        ] {
            if let Some(value) = probe.get(key) {
                let encoded = value.to_string();
                ensure!(
                    settings.api_key.is_empty() || !encoded.contains(&settings.api_key),
                    "AI 能力测试结果含私密信息"
                );
                safe.insert(key.to_string(), value.clone());
            }
        }
        settings.probe = Some(Value::Object(safe));
        self.write(&mut settings)?;
        Ok(settings.public())
    }

    pub fn connection(&self) -> Result<ConnectionConfig> {
        let _guard = self.gate.lock();
        let settings = self.load()?;
        validate_config(&settings.config)?;
        ensure!(!settings.api_key.is_empty(), "请先保存 AI API 密钥");
        Ok(ConnectionConfig {
            base_url: settings.config.base_url,
            model: settings.config.model,
            protocol: settings.config.protocol,
            request_timeout_secs: settings.config.request_timeout_secs,
            api_key: settings.api_key,
        })
    }

    fn load(&self) -> Result<PrivateSettings> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PrivateSettings::default())
            }
            Err(_) => anyhow::bail!("无法读取 AI 私密配置"),
        };
        ensure!(bytes.len() <= 256 * 1024, "AI 私密配置过大");
        let bytes = crate::core::secrets::protect(&bytes, false)
            .context("无法解密 AI 配置，请使用保存配置的系统账号")?;
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("AI 私密配置损坏"))
    }

    fn write(&self, settings: &mut PrivateSettings) -> Result<()> {
        settings.version = Some(uuid::Uuid::new_v4().to_string());
        let parent = self.path.parent().context("AI 配置路径无效")?;
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        let bytes = crate::core::secrets::protect(&serde_json::to_vec(settings)?, true)?;
        crate::core::fs::atomic_write(&self.path, &bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

impl PrivateSettings {
    fn public(&self) -> Value {
        json!({
            "version": self.version,
            "base_url": self.config.base_url,
            "model": self.config.model,
            "protocol": self.config.protocol,
            "request_timeout_secs": self.config.request_timeout_secs,
            "has_key": !self.api_key.is_empty(),
            "probe": self.probe,
        })
    }
}

fn normalize_base_url(input: &str) -> Result<String> {
    let input = input.trim().trim_end_matches('/');
    ensure!(input.len() <= 2048, "AI API 地址过长");
    let url = reqwest::Url::parse(input).map_err(|_| anyhow::anyhow!("AI API 地址无效"))?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "AI API 地址不能包含账号、查询参数或片段"
    );
    let loopback = url
        .host_str()
        .map(|host| {
            host == "localhost"
                || host == "[::1]"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
        .unwrap_or(false);
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "AI API 必须使用 HTTPS（本机测试地址可使用 HTTP）"
    );
    ensure!(url.host_str().is_some(), "AI API 地址缺少主机");
    Ok(url.as_str().trim_end_matches('/').to_string())
}

pub(super) fn validate_config(config: &SettingsConfig) -> Result<()> {
    normalize_base_url(&config.base_url)?;
    ensure!(
        !config.model.is_empty()
            && config.model.len() <= 128
            && !config.model.chars().any(char::is_control),
        "AI 模型名称无效"
    );
    ensure!(
        matches!(config.protocol.as_str(), "responses" | "chat_completions"),
        "AI 协议必须是 responses 或 chat_completions"
    );
    ensure!(
        (5..=300).contains(&config.request_timeout_secs),
        "AI 请求超时应为 5 至 300 秒"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(version: Value, key: &str) -> Value {
        json!({"expected_version":version,"base_url":"https://example.com/v1/",
            "model":"test-vision","protocol":"responses","request_timeout_secs":30,
            "api_key":key})
    }

    #[test]
    fn credentials_stay_private_and_stale_updates_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let settings = Settings::new(directory.path().join("private/config.json"));
        let saved = settings
            .save(request(Value::Null, "test-secret-value"))
            .unwrap();
        assert_eq!(saved["has_key"], true);
        assert!(!saved.to_string().contains("test-secret-value"));
        assert_eq!(saved["base_url"], "https://example.com/v1");
        assert!(settings.save(request(Value::Null, "wrong")).is_err());
        let second = settings
            .save(request(saved["version"].clone(), ""))
            .unwrap();
        assert_eq!(settings.connection().unwrap().api_key, "test-secret-value");
        assert_ne!(saved["version"], second["version"]);
        let mut clear = request(second["version"].clone(), "");
        clear["clear_key"] = json!(true);
        settings.save(clear).unwrap();
        assert!(settings.connection().is_err());
    }

    #[test]
    fn changed_connection_invalidates_probe_and_probe_uses_optimistic_lock() {
        let directory = tempfile::tempdir().unwrap();
        let settings = Settings::new(directory.path().join("config.json"));
        let saved = settings
            .save(request(Value::Null, "secret-test-only"))
            .unwrap();
        let tested = settings
            .save_probe(
                json!({"ok":true,"checks":[],"private":"omit"}),
                saved["version"].as_str(),
            )
            .unwrap();
        assert_eq!(tested["probe"]["ok"], true);
        assert!(tested["probe"].get("private").is_none());
        assert!(settings
            .save_probe(json!({"ok":true}), saved["version"].as_str())
            .is_err());
        let mut changed = request(tested["version"].clone(), "");
        changed["protocol"] = json!("chat_completions");
        let changed = settings.save(changed).unwrap();
        assert_eq!(changed["probe"], Value::Null);
    }

    #[test]
    fn rejects_credential_urls_and_insecure_remote_http() {
        assert!(normalize_base_url("https://user:password@example.com/v1").is_err());
        assert!(normalize_base_url("https://example.com/v1?key=test").is_err());
        assert!(normalize_base_url("http://example.com/v1").is_err());
        assert!(normalize_base_url("http://127.0.0.1:1234/v1").is_ok());
        assert!(normalize_base_url("http://[::1]:1234/v1").is_ok());
    }
}
