//! Live workspace: platform sessions belong here; media transport belongs to Core.
mod audience;
mod bilibili;
mod events;
mod queue;
mod rules;
pub mod runtime;
mod settings;
use crate::{
    device::DeviceManager,
    extensions::{service::BuiltinService, ExtensionError, ExtensionResult, Permission},
    media::output::{self, OutputHandle, OutputRequest},
};
use anyhow::{ensure, Result};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::{watch, Mutex as AsyncMutex},
    task::JoinHandle,
};

pub const ID: &str = "gamer-live";
pub const ACTIONS: &[&str] = &[
    "live.status",
    "stream.start",
    "stream.stop",
    "connection.connect",
    "connection.disconnect",
    "connection.settings.read",
    "connection.settings.save",
    "connection.settings.clear",
    "events.read",
    "logs.read",
    "audience.open",
    "audience.close",
    "rules.read",
    "rules.save",
    "rules.toggle",
    "rules.preview",
    "queue.status",
    "queue.configure",
    "queue.control",
    "queue.test",
];
pub fn accepts(id: &str, action: &str) -> bool {
    id == ID && ACTIONS.contains(&action)
}
pub fn permissions(id: &str, action: &str) -> Option<&'static [Permission]> {
    if !accepts(id, action) {
        return None;
    }
    Some(match action {
        "rules.save" | "rules.toggle" => &[Permission::UiHost, Permission::RunSubmit],
        "rules.read" | "rules.preview" => &[Permission::ResourceRead],
        "queue.configure" | "queue.test" => &[Permission::RunSubmit],
        "queue.control" => &[Permission::RunSubmit, Permission::RunControl],
        "stream.start" | "stream.stop" => &[Permission::MediaStream],
        "connection.connect"
        | "connection.disconnect"
        | "events.read"
        | "logs.read"
        | "audience.open"
        | "audience.close"
        | "connection.settings.read"
        | "connection.settings.save"
        | "connection.settings.clear" => &[Permission::LiveConnect],
        _ => &[],
    })
}
#[derive(Clone, Serialize)]
pub struct ConnectionStatus {
    pub state: String,
    pub platform_id: String,
    pub mode: String,
    pub connection_id: String,
    pub room: Option<Value>,
    pub error: Option<String>,
    pub reconnects: u32,
}
impl Default for ConnectionStatus {
    fn default() -> Self {
        Self {
            state: "disconnected".into(),
            platform_id: "bilibili".into(),
            mode: String::new(),
            connection_id: String::new(),
            room: None,
            error: None,
            reconnects: 0,
        }
    }
}
struct Connection {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}
impl Connection {
    async fn stop(mut self) {
        let _ = self.stop.send(true);
        if tokio::time::timeout(Duration::from_secs(12), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
            let _ = (&mut self.task).await;
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}
pub struct LiveService {
    devices: Arc<DeviceManager>,
    output: AsyncMutex<Option<OutputHandle>>,
    connection: AsyncMutex<Option<Connection>>,
    status: Arc<Mutex<ConnectionStatus>>,
    events: Arc<Mutex<events::EventBuffer>>,
    queue: Option<Arc<queue::Queue>>,
    queue_error: Option<String>,
    settings: settings::Settings,
    audience: AsyncMutex<Option<audience::Window>>,
}
impl LiveService {
    pub fn new(runtime: runtime::Runtime, data_root: &std::path::Path) -> Result<Self> {
        let devices = runtime.devices.clone();
        let queue = queue::Queue::open(
            data_root.join("extension-data/gamer-live/queue.json"),
            Arc::new(runtime),
        );
        let (queue, queue_error) = match queue {
            Ok(queue) => (Some(queue), None),
            Err(error) => (None, Some(format!("互动队列不可用：{error:#}"))),
        };
        Ok(Self {
            devices,
            output: AsyncMutex::new(None),
            audience: AsyncMutex::new(None),
            connection: AsyncMutex::new(None),
            status: Arc::new(Mutex::new(ConnectionStatus::default())),
            events: Arc::new(Mutex::new(events::EventBuffer::default())),
            queue,
            queue_error,
            settings: settings::Settings::new(
                data_root.join("extension-data/gamer-live/private/connection.dat"),
            ),
        })
    }
    fn queue(&self) -> Result<&Arc<queue::Queue>> {
        self.queue.as_ref().ok_or_else(|| {
            anyhow::anyhow!(self
                .queue_error
                .clone()
                .unwrap_or_else(|| "互动队列不可用".into()))
        })
    }
    async fn dispatch(&self, action: &str, values: Value) -> Result<Value> {
        match action {
            "audience.open" => {
                let mut window = self.audience.lock().await;
                if window.is_none() {
                    *window = Some(
                        audience::Window::open(self.queue()?.clone(), self.status.clone()).await?,
                    );
                }
                Ok(json!({"url":window.as_ref().unwrap().url}))
            }
            "audience.close" => {
                self.audience.lock().await.take();
                Ok(json!({"ok":true}))
            }
            "logs.read" => {
                let refresh: Vec<u64> = values["refresh"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(500)
                    .filter_map(Value::as_u64)
                    .collect();
                Ok(self
                    .queue()?
                    .logs(
                        values["before"].as_u64().unwrap_or(0),
                        values["after"].as_u64().unwrap_or(0),
                        values["filter"].as_str().unwrap_or("trigger"),
                        values["search"].as_str().unwrap_or(""),
                        &refresh,
                    )
                    .await)
            }
            "queue.status" => Ok(self
                .queue()?
                .status(
                    values["offset"].as_u64().unwrap_or(0).min(10000) as usize,
                    values["filter"].as_str().unwrap_or(""),
                )
                .await),
            "queue.configure" => {
                self.queue()?
                    .configure(
                        values["device_id"].as_str().unwrap_or(""),
                        values["package_id"].as_str().unwrap_or(""),
                    )
                    .await?;
                Ok(json!({"ok":true}))
            }
            "queue.control" => {
                let ids: Vec<String> =
                    serde_json::from_value(values.get("ids").cloned().unwrap_or(json!([])))?;
                self.queue()?
                    .control(
                        values["op"].as_str().unwrap_or(""),
                        &ids,
                        values["request_id"].as_str().unwrap_or(""),
                    )
                    .await
            }
            "rules.read" => self
                .queue()?
                .read_rules(values["package_id"].as_str().unwrap_or("")),
            "rules.toggle" => self.queue()?.toggle_rule(
                values["package_id"].as_str().unwrap_or(""),
                values["id"].as_str().unwrap_or(""),
                values["enabled"]
                    .as_bool()
                    .ok_or_else(|| anyhow::anyhow!("缺少规则开关值"))?,
                values["expected_version"].as_str().unwrap_or(""),
            ),
            "rules.save" => self.queue()?.save_rules(
                values["package_id"].as_str().unwrap_or(""),
                serde_json::from_value(values["ruleset"].clone())?,
                values["expected_version"].as_str(),
            ),
            "rules.preview" | "queue.test" => {
                let event = events::LiveEvent {
                    schema_version: 1,
                    seq: 0,
                    platform_id: "test".into(),
                    connection_id: "test".into(),
                    room_id: "test".into(),
                    event_id: None,
                    kind: values["kind"].as_str().unwrap_or("message").into(),
                    occurred_at: None,
                    received_at: chrono::Utc::now().to_rfc3339(),
                    actor: Some(json!({"id":"test","name":"模拟观众"})),
                    payload: values["payload"].clone(),
                    platform_data: Value::Null,
                };
                self.queue()?
                    .preview(
                        event,
                        action == "queue.test",
                        values["request_id"].as_str().unwrap_or(""),
                    )
                    .await
            }
            "live.status" => {
                let output = self.output.lock().await;
                Ok(
                    json!({"stream":output.as_ref().map(|h|h.status.lock().clone()),"connection":self.status.lock().clone(),"platforms":[{"id":"bilibili","name":"哔哩哔哩","modes":["open_live","oauth"]}]}),
                )
            }
            "stream.start" => {
                let req: OutputRequest = serde_json::from_value(values)?;
                req.validate()?;
                let mut slot = self.output.lock().await;
                ensure!(
                    slot.as_ref().is_none_or(|h| h.finished()),
                    "已有音视频输出，请先停止再更换设备或地址"
                );
                if let Some(old) = slot.take() {
                    old.stop().await;
                }
                let handle = output::start(self.devices.clone(), req).await?;
                let result = json!(handle.status.lock().clone());
                *slot = Some(handle);
                Ok(result)
            }
            "stream.stop" => {
                if let Some(handle) = self.output.lock().await.take() {
                    handle.stop().await;
                }
                Ok(json!({"state":"stopped"}))
            }
            "connection.settings.read" => self.settings.read(),
            "connection.settings.save" => {
                let credentials = serde_json::from_value(values["credentials"].clone())
                    .map_err(|_| anyhow::anyhow!("接入配置格式无效"))?;
                self.settings
                    .save(credentials, values["expected_version"].as_str())
                    .map(|(_, public)| public)
            }
            "connection.settings.clear" => self.settings.clear(
                values["mode"].as_str().unwrap_or(""),
                values["expected_version"].as_str(),
            ),
            "connection.connect" => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Request {
                    platform_id: String,
                    credentials: bilibili::Credentials,
                    #[serde(default)]
                    expected_version: Option<String>,
                }
                let request: Request = serde_json::from_value(values)?;
                ensure!(request.platform_id == "bilibili", "暂不支持此直播平台");
                let mut slot = self.connection.lock().await;
                ensure!(
                    slot.as_ref().is_none_or(|h| h.task.is_finished()),
                    "互动已连接或正在连接，请先断开"
                );
                if let Some(queue) = &self.queue {
                    queue.new_connection().await?;
                }
                let (credentials, saved) = self
                    .settings
                    .save(request.credentials, request.expected_version.as_deref())?;
                if let Some(old) = slot.take() {
                    old.stop().await;
                }
                *self.status.lock() = ConnectionStatus {
                    state: "connecting".into(),
                    mode: credentials.mode.clone(),
                    ..Default::default()
                };
                let (stop, rx) = watch::channel(false);
                let task = tokio::spawn(bilibili::run(
                    credentials,
                    self.status.clone(),
                    self.events.clone(),
                    self.queue.clone(),
                    rx,
                ));
                *slot = Some(Connection { stop, task });
                let mut result = json!(self.status.lock().clone());
                result["settings"] = saved;
                Ok(result)
            }
            "connection.disconnect" => {
                if let Some(handle) = self.connection.lock().await.take() {
                    handle.stop().await;
                }
                self.status.lock().state = "disconnected".into();
                Ok(json!({"state":"disconnected"}))
            }
            "events.read" => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Cursor {
                    #[serde(default)]
                    after: u64,
                }
                let cursor: Cursor = serde_json::from_value(values)?;
                Ok(self.events.lock().page(cursor.after))
            }
            _ => anyhow::bail!("未知直播动作"),
        }
    }
}
#[async_trait::async_trait]
impl BuiltinService for LiveService {
    fn extension_id(&self) -> &str {
        ID
    }
    async fn call(&self, action: &str, values: Value) -> ExtensionResult<Value> {
        self.dispatch(action, values)
            .await
            .map_err(|e| ExtensionError::CallRejected(e.to_string()))
    }
    async fn stop(&self) {
        if let Some(queue) = &self.queue {
            queue.suspend(true).await;
        }
        self.stop_connections().await;
    }
    async fn shutdown(&self) {
        if let Some(queue) = &self.queue {
            queue.suspend(false).await;
        }
        self.stop_connections().await;
    }
}
impl LiveService {
    async fn stop_connections(&self) {
        self.audience.lock().await.take();
        // Stop active tasks; saved local connection profiles survive disable/restart.
        tokio::join!(
            async {
                if let Some(h) = self.output.lock().await.take() {
                    h.stop().await;
                }
            },
            async {
                if let Some(h) = self.connection.lock().await.take() {
                    h.stop().await;
                }
            }
        );
        *self.status.lock() = ConnectionStatus::default();
        *self.events.lock() = events::EventBuffer::default();
    }
}

#[cfg(test)]
mod integration_tests;
