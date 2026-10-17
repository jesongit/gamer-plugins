//! Read-only audience view served by the host's loopback HTTP bridge.
use super::{queue::Queue, ConnectionStatus};
use anyhow::{ensure, Context, Result};
use axum::{
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    Json,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio::sync::Mutex as AsyncMutex;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    text: String,
    version: Option<String>,
}
struct Settings {
    path: PathBuf,
    gate: Mutex<()>,
}
impl Settings {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            gate: Mutex::new(()),
        }
    }
    fn load(&self) -> Result<Saved> {
        let metadata = match std::fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Saved::default())
            }
            Err(_) => anyhow::bail!("无法读取观众提示配置"),
        };
        ensure!(metadata.len() <= 16 * 1024, "观众提示配置文件过大");
        let saved: Saved = serde_json::from_slice(&std::fs::read(&self.path)?)
            .map_err(|_| anyhow::anyhow!("观众提示配置文件损坏"))?;
        ensure!(
            saved.text.chars().count() <= 1000,
            "提示文字最多 1000 个字符"
        );
        Ok(saved)
    }
    fn public(saved: Saved, url: Option<&str>) -> Value {
        json!({"enabled":url.is_some(),"url":url,"text":saved.text,"version":saved.version})
    }
    fn read(&self, url: Option<&str>) -> Result<Value> {
        let _gate = self.gate.lock();
        Ok(Self::public(self.load()?, url))
    }
    fn save(&self, text: String, expected: Option<&str>, url: Option<&str>) -> Result<Value> {
        ensure!(text.chars().count() <= 1000, "提示文字最多 1000 个字符");
        let _gate = self.gate.lock();
        let previous = self.load()?;
        ensure!(
            previous.version.as_deref() == expected,
            "观众提示配置已更新，请刷新后重试"
        );
        let saved = Saved {
            text,
            version: Some(uuid::Uuid::new_v4().to_string()),
        };
        std::fs::create_dir_all(self.path.parent().context("观众提示配置路径无效")?)?;
        crate::core::fs::atomic_write(&self.path, &serde_json::to_vec(&saved)?)
            .context("观众提示配置保存失败")?;
        Ok(Self::public(saved, url))
    }
}

pub struct Audience {
    settings: Settings,
    enabled: AsyncMutex<bool>,
    url: String,
}
impl Audience {
    pub fn new(path: PathBuf, url: String) -> Self {
        Self {
            settings: Settings::new(path),
            enabled: AsyncMutex::new(false),
            url,
        }
    }
    fn public(&self, enabled: bool) -> Result<Value> {
        let mut value = self.settings.read(Some(&self.url))?;
        value["enabled"] = json!(enabled);
        Ok(value)
    }
    pub async fn read(&self) -> Result<Value> {
        self.public(*self.enabled.lock().await)
    }
    pub async fn save(&self, text: String, expected: Option<&str>) -> Result<Value> {
        let enabled = self.enabled.lock().await;
        let mut value = self.settings.save(text, expected, Some(&self.url))?;
        value["enabled"] = json!(*enabled);
        Ok(value)
    }
    pub async fn open(&self) -> Result<Value> {
        let mut enabled = self.enabled.lock().await;
        let value = self.public(true)?;
        *enabled = true;
        Ok(value)
    }
    pub async fn close(&self) -> Result<Value> {
        let mut enabled = self.enabled.lock().await;
        *enabled = false;
        self.public(false)
    }
    pub async fn shutdown(&self) {
        *self.enabled.lock().await = false;
    }
    pub async fn page(&self) -> Response {
        if !*self.enabled.lock().await {
            return protect(StatusCode::FORBIDDEN.into_response());
        }
        protect(Html(include_str!("audience.html")).into_response())
    }
    pub async fn snapshot(&self, queue: &Queue, connection: &Mutex<ConnectionStatus>) -> Response {
        let enabled = self.enabled.lock().await;
        if !*enabled {
            return protect(StatusCode::FORBIDDEN.into_response());
        }
        // Keep the enable gate until the projection is ready: close/shutdown
        // cannot complete while an earlier reader is still observing a run.
        let mut value = queue.audience().await;
        let settings = match self.settings.read(None) {
            Ok(settings) => settings,
            Err(_) => return protect(StatusCode::SERVICE_UNAVAILABLE.into_response()),
        };
        value["connection"] = json!(connection.lock().state);
        value["text"] = settings["text"].clone();
        protect(Json(value).into_response())
    }
}
pub(super) fn protect(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert("cache-control", "no-store".parse().unwrap());
    headers.insert("referrer-policy", "no-referrer".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("content-security-policy", "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'".parse().unwrap());
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn text_preserves_newlines_and_empty_values_with_durable_versions() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audience.json");
        let settings = Settings::new(path.clone());
        assert_eq!(
            settings.read(None).unwrap(),
            json!({"enabled":false,"url":null,"text":"","version":null})
        );
        assert!(!path.exists(), "reading defaults must not create a file");
        let text = "  发送日常即可排队\n请耐心等待🙂\n";
        let saved = settings.save(text.into(), None, None).unwrap();
        assert_eq!(saved["text"], text);
        let version = saved["version"].as_str().unwrap();
        assert!(uuid::Uuid::parse_str(version).is_ok());
        let reopened = Settings::new(path.clone());
        assert_eq!(reopened.read(None).unwrap(), saved);
        let bytes = std::fs::read(&path).unwrap();
        assert!(reopened.save("其他页面的修改".into(), None, None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let empty = reopened.save(String::new(), Some(version), None).unwrap();
        assert_eq!(empty["text"], "");
        assert_ne!(empty["version"], saved["version"]);
        assert_eq!(Settings::new(path).read(None).unwrap(), empty);
    }
    #[test]
    fn text_limit_counts_unicode_characters_and_rejects_without_writing() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audience.json");
        let settings = Settings::new(path.clone());
        let text = "🙂".repeat(1000);
        let saved = settings.save(text.clone(), None, None).unwrap();
        assert_eq!(saved["text"], text);
        let bytes = std::fs::read(&path).unwrap();
        assert!(settings
            .save(format!("{text}\n"), saved["version"].as_str(), None)
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::write(&path, b"not valid settings").unwrap();
        assert!(settings.read(None).is_err());
        assert!(settings
            .save("保留损坏文件".into(), saved["version"].as_str(), None)
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not valid settings");
    }
}
