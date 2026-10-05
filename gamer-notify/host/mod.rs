//! Notification business: global channels, bounded asynchronous delivery and local history.
mod settings;
pub mod task;
#[cfg(test)]
mod tests;

use crate::extensions::{service::BuiltinService, ExtensionError, ExtensionResult, Permission};
use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use settings::{Channel, Settings};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::Semaphore;

pub const ID: &str = "gamer-notify";
pub const SEND: &str = "notification.send";
pub const ACTIONS: &[&str] = &[
    "channels.read",
    "channels.save",
    "channels.delete",
    "channels.default",
    "notification.send",
    "records.read",
    "records.query",
];
pub fn accepts(id: &str, action: &str) -> bool {
    id == ID && ACTIONS.contains(&action)
}
pub fn permissions(id: &str, action: &str) -> Option<&'static [Permission]> {
    accepts(id, action).then_some(&[Permission::NotifySend])
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Record {
    pub id: String,
    pub channel_id: Option<String>,
    pub channel_name: Option<String>,
    pub title: String,
    pub content: String,
    pub source: String,
    pub source_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub status: String,
    pub message: String,
    pub remote_id: Option<String>,
    pub wecom_code: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendRequest {
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    title: String,
    content: String,
    #[serde(default = "manual_source")]
    source: String,
    #[serde(default)]
    source_id: Option<String>,
}
fn manual_source() -> String {
    "manual".into()
}

pub struct NotifyService {
    state: Arc<State>,
}
struct State {
    private_path: PathBuf,
    history_path: PathBuf,
    gate: Mutex<()>,
    records: Mutex<Vec<Record>>,
    http: reqwest::Client,
    endpoint: String,
    runtime: tokio::runtime::Handle,
    workers: Arc<Semaphore>,
    pending: AtomicUsize,
    generation: AtomicU64,
}

impl NotifyService {
    pub fn new(data_root: &std::path::Path) -> Result<Self> {
        Self::open(data_root, "https://wx.posase.net".into())
    }
    fn open(data_root: &std::path::Path, endpoint: String) -> Result<Self> {
        let root = data_root.join("extension-data").join(ID);
        let history_path = root.join("records.json");
        let mut records: Vec<Record> = match std::fs::read(&history_path) {
            Ok(bytes) => {
                ensure!(bytes.len() <= 8 * 1024 * 1024, "通知记录过大");
                serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("通知记录损坏"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => anyhow::bail!("无法读取通知记录"),
        };
        for record in &mut records {
            if record.status == "sending" {
                record.status = "unknown".into();
                record.message = "发送期间服务重启，结果未知，请先核实接收端".into();
            } else if record.status == "queued" {
                record.status = "skipped".into();
                record.message = "服务重启，未补发历史通知".into();
            }
        }
        let state = Arc::new(State {
            private_path: root.join("private/channels.dat"),
            history_path,
            gate: Mutex::new(()),
            records: Mutex::new(records),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(60))
                .build()?,
            endpoint,
            runtime: tokio::runtime::Handle::current(),
            workers: Arc::new(Semaphore::new(4)),
            pending: AtomicUsize::new(0),
            generation: AtomicU64::new(0),
        });
        // Recovery is persisted before any new delivery can be admitted.
        if state.history_path.exists() {
            state.persist(&mut state.records.lock())?;
        }
        Ok(Self { state })
    }

    async fn dispatch(&self, action: &str, values: Value) -> Result<Value> {
        match action {
            "notification.send" => self.submit(values),
            "records.read" => Ok(
                json!({"records":self.state.records.lock().iter().rev().take(200).collect::<Vec<_>>()}),
            ),
            "records.query" => self.query(values).await,
            "channels.read" => {
                let _gate = self.state.gate.lock();
                Ok(Settings::load(&self.state.private_path)?.public())
            }
            "channels.save" | "channels.delete" | "channels.default" => {
                let _gate = self.state.gate.lock();
                let mut saved = Settings::load(&self.state.private_path)?;
                ensure!(
                    saved.version.as_deref() == values["expected_version"].as_str(),
                    "通知通道配置已更新，请刷新后重试"
                );
                match action {
                    "channels.save" => {
                        let mut c: Channel = serde_json::from_value(values["channel"].clone())
                            .context("通知通道配置无效")?;
                        crate::resources::validate_scope_id("channel id", &c.id).map_err(|_| {
                            anyhow::anyhow!("通道 ID 必须为小写字母、数字、点、下划线或连字符")
                        })?;
                        ensure!(
                            !c.name.trim().is_empty() && c.name.len() <= 256,
                            "通道名称不能为空或过长"
                        );
                        ensure!(c.kind == "wecomlink", "不支持的通知通道类型");
                        if c.key.is_empty() {
                            if let Some(previous) = saved.channels.iter().find(|p| p.id == c.id) {
                                c.key = previous.key.clone();
                            }
                        }
                        ensure!(
                            !c.key.is_empty()
                                && c.key.len() <= 512
                                && !c.key.chars().any(char::is_control),
                            "调用密钥不能为空或包含控制字符"
                        );
                        if let Some(previous) = saved.channels.iter_mut().find(|p| p.id == c.id) {
                            *previous = c;
                        } else {
                            ensure!(saved.channels.len() < 100, "最多配置 100 个通道");
                            saved.channels.push(c);
                        }
                    }
                    "channels.delete" => {
                        let id = values["id"].as_str().context("缺少通道 ID")?;
                        saved.channels.retain(|c| c.id != id);
                        if saved.default_channel.as_deref() == Some(id) {
                            saved.default_channel = None;
                        }
                    }
                    _ => {
                        let id = values["id"].as_str();
                        if let Some(id) = id {
                            ensure!(
                                saved.channels.iter().any(|c| c.id == id && c.enabled),
                                "默认通道不存在或已停用"
                            );
                        }
                        saved.default_channel = id.map(str::to_string);
                    }
                }
                saved.write(&self.state.private_path)?;
                Ok(saved.public())
            }
            _ => anyhow::bail!("未知通知动作"),
        }
    }

    fn submit(&self, values: Value) -> Result<Value> {
        let req: SendRequest = serde_json::from_value(values).context("通知参数无效")?;
        let _gate = self.state.gate.lock();
        let channel = Settings::load(&self.state.private_path)?.resolve(req.channel.as_deref());
        let mut record = Record {
            id: uuid::Uuid::new_v4().to_string(),
            channel_id: req.channel.clone(),
            channel_name: None,
            title: req.title,
            content: req.content,
            source: req.source,
            source_id: req.source_id,
            created_at: Utc::now(),
            status: "queued".into(),
            message: "已提交，等待发送".into(),
            remote_id: None,
            wecom_code: None,
        };
        ensure!(
            record.source.len() <= 64 && record.source_id.as_ref().is_none_or(|s| s.len() <= 256),
            "通知来源过长"
        );
        let validation = validate_message(&record.title, &record.content);
        let channel = match channel {
            Ok(c) => {
                record.channel_id = Some(c.id.clone());
                record.channel_name = Some(c.name.clone());
                Some(c)
            }
            Err(e) => {
                record.status = "skipped".into();
                record.message = e.to_string();
                None
            }
        };
        if let Err(e) = validation {
            record.status = "failed".into();
            record.message = e.to_string();
        }
        let mut records = self.state.records.lock();
        if let Some(source_id) = &record.source_id {
            if let Some(previous) = records.iter().find(|r| {
                r.source_id.as_ref() == Some(source_id)
                    && r.channel_id == record.channel_id
                    && r.source == record.source
            }) {
                return Ok(
                    json!({"accepted":previous.status != "skipped" && previous.status != "failed", "record":previous, "replayed":true}),
                );
            }
        }
        let mut admitted = false;
        if record.status == "queued" {
            let mut pending = self.state.pending.load(Ordering::Acquire);
            while pending < 128 {
                match self.state.pending.compare_exchange_weak(
                    pending,
                    pending + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        admitted = true;
                        break;
                    }
                    Err(current) => pending = current,
                }
            }
            if !admitted {
                record.status = "failed".into();
                record.message = "通知发送队列已满".into();
            }
        }
        // Do not persist arbitrarily large rejected bodies.
        record.title = truncate(&record.title, 2048);
        record.content = truncate(&record.content, 4096);
        records.push(record.clone());
        if let Err(error) = self.state.persist(&mut records) {
            records.retain(|r| r.id != record.id);
            if admitted {
                self.state.pending.fetch_sub(1, Ordering::AcqRel);
            }
            return Err(error);
        }
        drop(records);
        if admitted {
            let state = self.state.clone();
            let generation = state.generation.load(Ordering::Acquire);
            let id = record.id.clone();
            let channel = channel.expect("queued record has a resolved channel");
            let payload = json!({"title":record.title,"content":record.content});
            let runtime = state.runtime.clone();
            runtime.spawn(async move {
                let _pending = PendingGuard(state.clone());
                let Ok(_worker) = state.workers.clone().acquire_owned().await else {
                    return;
                };
                if state.generation.load(Ordering::Acquire) != generation {
                    state.update(&id, "skipped", "通知插件已停用，未发送", None, None);
                    return;
                }
                if !state.update(&id, "sending", "正在发送", None, None) {
                    return;
                }
                let response = state
                    .http
                    .post(format!("{}/api/v1/notify", state.endpoint))
                    .bearer_auth(&channel.key)
                    .header("Idempotency-Key", format!("gamer-{id}"))
                    .json(&payload)
                    .send()
                    .await;
                match response {
                    Ok(response) => {
                        state
                            .apply_response(&id, response, &channel.key, false)
                            .await
                    }
                    Err(_) => {
                        state.update(
                            &id,
                            "unknown",
                            "请求未完成，发送结果未知；请先核实接收端",
                            None,
                            None,
                        );
                    }
                }
            });
        }
        Ok(json!({"accepted":admitted,"record":record}))
    }

    async fn query(&self, values: Value) -> Result<Value> {
        let id = values["id"].as_str().context("缺少通知记录 ID")?;
        let record = self
            .state
            .records
            .lock()
            .iter()
            .find(|r| r.id == id)
            .cloned()
            .context("通知记录不存在")?;
        ensure!(
            ["unknown", "pending", "partial"].contains(&record.status.as_str()),
            "该记录无需查询"
        );
        let remote = record
            .remote_id
            .context("未获得远端记录 ID，请先核实接收端")?;
        let channel = {
            let _gate = self.state.gate.lock();
            Settings::load(&self.state.private_path)?.resolve(record.channel_id.as_deref())?
        };
        let mut url =
            reqwest::Url::parse(&format!("{}/api/v1/notifications/", self.state.endpoint))?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("查询地址无效"))?
            .pop_if_empty()
            .push(&remote);
        let response = self
            .state
            .http
            .get(url)
            .bearer_auth(&channel.key)
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("通知结果查询失败"))?;
        self.state
            .apply_response(id, response, &channel.key, true)
            .await;
        Ok(json!({"record":self.state.records.lock().iter().find(|r| r.id == id)}))
    }
}

struct PendingGuard(Arc<State>);
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.0.pending.fetch_sub(1, Ordering::AcqRel);
    }
}
impl State {
    fn persist(&self, records: &mut Vec<Record>) -> Result<()> {
        let cutoff = Utc::now() - chrono::Duration::days(30);
        records.retain(|r| {
            r.created_at >= cutoff || ["queued", "sending"].contains(&r.status.as_str())
        });
        let mut excess = records.len().saturating_sub(1000);
        records.retain(|r| {
            if excess > 0 && !["queued", "sending"].contains(&r.status.as_str()) {
                excess -= 1;
                false
            } else {
                true
            }
        });
        let parent = self.history_path.parent().context("通知记录路径无效")?;
        std::fs::create_dir_all(parent)?;
        let mut bytes = serde_json::to_vec(records)?;
        while bytes.len() > 8 * 1024 * 1024 {
            let Some(index) = records
                .iter()
                .position(|r| !["queued", "sending"].contains(&r.status.as_str()))
            else {
                anyhow::bail!("通知记录过大");
            };
            records.remove(index);
            bytes = serde_json::to_vec(records)?;
        }
        crate::core::fs::atomic_write(&self.history_path, &bytes)?;
        Ok(())
    }
    fn update(
        &self,
        id: &str,
        status: &str,
        message: &str,
        remote_id: Option<String>,
        code: Option<i64>,
    ) -> bool {
        let mut records = self.records.lock();
        let Some(record) = records.iter_mut().find(|r| r.id == id) else {
            return false;
        };
        record.status = status.into();
        record.message = truncate(message, 512);
        record.wecom_code = code;
        if remote_id.is_some() {
            record.remote_id = remote_id;
        }
        if self.persist(&mut records).is_err() {
            tracing::error!(notification_id = %id, "通知结果保存失败");
            return false;
        }
        true
    }
    async fn apply_response(
        &self,
        id: &str,
        mut response: reqwest::Response,
        key: &str,
        query: bool,
    ) {
        let http_status = response.status().as_u16();
        let mut bytes = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) if bytes.len() + chunk.len() <= 64 * 1024 => {
                    bytes.extend_from_slice(&chunk)
                }
                Ok(None) => break,
                _ => {
                    self.update(id, "unknown", "服务响应异常，发送结果未知", None, None);
                    return;
                }
            }
        }
        let data: Value = match serde_json::from_slice(&bytes) {
            Ok(data) => data,
            Err(_) => {
                self.update(
                    id,
                    "unknown",
                    "服务响应不是有效 JSON，发送结果未知",
                    None,
                    None,
                );
                return;
            }
        };
        if query && http_status != 200 {
            let previous = self
                .records
                .lock()
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.status.clone());
            if let Some(status) = previous {
                self.update(
                    id,
                    &status,
                    &format!("查询失败（HTTP {http_status}），原发送状态保留"),
                    None,
                    None,
                );
            }
            return;
        }
        let (status, message) = response_status(http_status, &data);
        let message = message.replace(key, "[密钥已隐藏]");
        let remote_id = data["id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 128 && !s.contains(key))
            .map(str::to_string);
        self.update(
            id,
            status,
            &message,
            remote_id,
            data["wecom"]["errcode"].as_i64(),
        );
    }
}
fn response_status(http: u16, data: &Value) -> (&'static str, String) {
    match (http, data["status"].as_str()) {
        (200, Some("sent")) => ("sent", "企业微信接口已接受，请在接收端确认".into()),
        (200, Some("partial")) => ("partial", "部分接收人无效或无权限，请检查企微连后台".into()),
        (200 | 202, Some("pending")) => ("pending", "服务仍在处理，请稍后查询".into()),
        (_, Some("unknown")) | (504, _) => ("unknown", "发送结果未知，请先核实接收端".into()),
        (_, Some("failed")) => ("failed", "企业微信明确拒绝发送，请检查服务配置".into()),
        (400 | 401 | 404 | 409 | 429, _) => (
            "failed",
            format!(
                "通知服务拒绝请求（HTTP {http}）：{}",
                data["error"]["message"]
                    .as_str()
                    .unwrap_or("请检查密钥、消息或频率限制")
            ),
        ),
        _ => ("unknown", "通知服务响应异常，发送结果未知".into()),
    }
}
fn validate_message(title: &str, content: &str) -> Result<()> {
    ensure!(!content.trim().is_empty(), "通知正文不能为空");
    ensure!(
        title.len() + usize::from(!title.is_empty()) + content.len() <= 2048,
        "标题、换行和正文合计不能超过 2048 个 UTF-8 字节"
    );
    Ok(())
}
fn truncate(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].into()
}

#[async_trait]
impl BuiltinService for NotifyService {
    fn extension_id(&self) -> &'static str {
        ID
    }
    async fn call(&self, action: &str, values: Value) -> ExtensionResult<Value> {
        self.dispatch(action, values)
            .await
            .map_err(|e| ExtensionError::CallRejected(e.to_string()))
    }
    async fn stop(&self) {
        self.state.generation.fetch_add(1, Ordering::AcqRel);
        // Admitted HTTP requests finish, queued requests observe the new generation.
        let _ = self.state.workers.clone().acquire_many_owned(4).await;
    }
    async fn shutdown(&self) {
        self.stop().await;
    }
}
