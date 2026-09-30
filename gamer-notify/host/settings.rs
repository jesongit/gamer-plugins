use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub enabled: bool,
    #[serde(default)]
    pub key: String,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Settings {
    pub version: Option<String>,
    pub default_channel: Option<String>,
    pub channels: Vec<Channel>,
}

impl Settings {
    pub fn load(path: &PathBuf) -> Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(_) => anyhow::bail!("无法读取通知通道配置"),
        };
        ensure!(bytes.len() <= 256 * 1024, "通知通道配置过大");
        let bytes = crate::core::secrets::protect(&bytes, false)
            .context("无法解密通知配置，请使用保存配置的系统账号")?;
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("通知通道配置损坏"))
    }

    pub fn write(&mut self, path: &std::path::Path) -> Result<()> {
        self.version = Some(uuid::Uuid::new_v4().to_string());
        let parent = path.parent().context("通知配置路径无效")?;
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        let bytes = crate::core::secrets::protect(&serde_json::to_vec(self)?, true)?;
        crate::core::fs::atomic_write(path, &bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn public(&self) -> Value {
        json!({"version":self.version,"default_channel":self.default_channel,
            "channels":self.channels.iter().map(|c| json!({"id":c.id,"name":c.name,
                "kind":c.kind,"enabled":c.enabled,"has_key":!c.key.is_empty()})).collect::<Vec<_>>()})
    }

    pub fn resolve(&self, id: Option<&str>) -> Result<Channel> {
        let id = id
            .filter(|id| !id.is_empty())
            .or(self.default_channel.as_deref())
            .context("未配置默认通知通道")?;
        let channel = self
            .channels
            .iter()
            .find(|c| c.id == id)
            .context("通知通道不存在")?;
        ensure!(channel.enabled, "通知通道已停用");
        ensure!(!channel.key.is_empty(), "通知通道未配置密钥");
        Ok(channel.clone())
    }
}
