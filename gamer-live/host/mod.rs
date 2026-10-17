//! Live workspace: platform sessions belong here; media transport belongs to Core.
mod audience;
mod automatic;
mod bilibili;
mod events;
pub(crate) mod external;
mod local_links;
mod queue;
mod rules;
pub mod runtime;
mod sender;
mod settings;
use crate::{
    device::DeviceManager,
    extensions::{service::BuiltinService, ExtensionError, ExtensionResult, Permission},
    media::output::{self, OutputRequest},
};
use anyhow::{ensure, Context, Result};
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
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
    "stream.switch",
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
    "audience.read",
    "audience.save",
    "rules.read",
    "rules.save",
    "rules.toggle",
    "rules.preview",
    "queue.status",
    "queue.configure",
    "queue.control",
    "queue.reconcile_success",
    "queue.test",
    "automatic.start",
    "automatic.stop",
    "automatic.status",
    "automatic.preview",
    "users.read",
    "users.update",
    "users.save",
    "users.settings.save",
    "sender.read",
    "sender.save",
    "sender.clear",
    "sender.send",
    "confirmation.begin",
    "confirmation.poll",
];
pub fn internal_action(action: &str) -> bool {
    matches!(action, "confirmation.begin" | "confirmation.poll")
}
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
        "queue.reconcile_success" => &[Permission::RunControl],
        "stream.start" | "stream.stop" | "stream.switch" => &[Permission::MediaStream],
        "automatic.start" => &[Permission::MediaStream, Permission::RunSubmit],
        "automatic.stop" => &[
            Permission::MediaStream,
            Permission::RunSubmit,
            Permission::RunControl,
        ],
        "automatic.preview" => &[Permission::MediaStream],
        "connection.connect"
        | "connection.disconnect"
        | "events.read"
        | "logs.read"
        | "audience.open"
        | "audience.close"
        | "audience.read"
        | "audience.save"
        | "connection.settings.read"
        | "connection.settings.save"
        | "connection.settings.clear" => &[Permission::LiveConnect],
        "users.read"
        | "users.update"
        | "users.save"
        | "users.settings.save"
        | "sender.read"
        | "sender.save"
        | "sender.clear"
        | "sender.send"
        | "confirmation.begin"
        | "confirmation.poll" => &[Permission::LiveConnect],
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
    output: automatic::OutputSlot,
    automatic: Arc<automatic::Automatic>,
    connection: AsyncMutex<Option<Connection>>,
    status: Arc<Mutex<ConnectionStatus>>,
    events: Arc<Mutex<events::EventBuffer>>,
    queue: Option<Arc<queue::Queue>>,
    queue_error: Option<String>,
    settings: settings::Settings,
    audience: audience::Audience,
    links: Arc<local_links::LocalLinks>,
    sender: Option<Arc<sender::Sender>>,
    sender_error: Option<String>,
}
impl LiveService {
    pub fn new(runtime: runtime::Runtime, data_root: &std::path::Path) -> Result<Self> {
        let devices = runtime.devices.clone();
        let links = Arc::new(local_links::LocalLinks::open(
            &data_root.join("extension-data/gamer-live/local-links.json"),
            devices.cfg.port,
        )?);
        let queue = queue::Queue::open(
            data_root.join("extension-data/gamer-live/queue.json"),
            Arc::new(runtime),
        );
        let (queue, queue_error) = match queue {
            Ok(queue) => (Some(queue), None),
            Err(error) => (None, Some(format!("互动队列不可用：{error:#}"))),
        };
        let output = Arc::new(AsyncMutex::new(None));
        let automatic = automatic::Automatic::new(
            devices.clone(),
            output.clone(),
            queue.as_ref().map(Arc::downgrade).unwrap_or_default(),
            links.stream_url().to_owned(),
        );
        let (sender, sender_error) = match sender::Sender::open(
            data_root.join("extension-data/gamer-live/private/sender.dat"),
        ) {
            Ok(sender) => (Some(sender), None),
            Err(error) => (None, Some(format!("主动弹幕不可用：{error}"))),
        };
        if let Some(queue) = &queue {
            queue.set_hooks(automatic.clone());
            if let Some(sender) = &sender {
                queue.set_notifier(sender.clone());
            }
        }
        Ok(Self {
            devices,
            output,
            automatic,
            audience: audience::Audience::new(
                data_root.join("extension-data/gamer-live/audience.json"),
                links.audience_url().to_owned(),
            ),
            links,
            sender,
            sender_error,
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
    fn sender(&self) -> Result<&Arc<sender::Sender>> {
        self.sender.as_ref().ok_or_else(|| {
            anyhow::anyhow!(self
                .sender_error
                .clone()
                .unwrap_or_else(|| "主动弹幕不可用".into()))
        })
    }
    async fn dispatch(&self, action: &str, values: Value) -> Result<Value> {
        if let Some(sender) = &self.sender {
            sender.activate();
        }
        match action {
            "sender.read" => Ok(self.sender()?.read()),
            "sender.save" => self.sender()?.save(values),
            "sender.clear" => self.sender()?.clear(values["expected_version"].as_str()),
            "sender.send" => self.sender()?.manual(
                values["text"].as_str().unwrap_or(""),
                values["request_id"].as_str().unwrap_or(""),
            ),
            "users.read" => Ok(self
                .queue()?
                .accounts_list(
                    values["offset"].as_u64().unwrap_or(0).min(100000) as usize,
                    values["search"].as_str().unwrap_or(""),
                )
                .await),
            "users.settings.save" => {
                self.queue()?
                    .accounts_save_settings(
                        values["default_quota"].as_u64().context("缺少默认次数")?,
                        values["expected_version"]
                            .as_u64()
                            .context("缺少设置版本")?,
                    )
                    .await
            }
            "users.update" | "users.save" => {
                self.queue()?
                    .accounts_update(
                        values["id"].as_str().unwrap_or(""),
                        values["balance"].as_u64().context("缺少次数")?,
                        values["unlimited"].as_bool().context("缺少无限次数开关")?,
                        values["expected_version"]
                            .as_u64()
                            .context("缺少用户版本")?,
                        values["request_id"].as_str().unwrap_or(""),
                    )
                    .await
            }
            "confirmation.begin" | "confirmation.poll" => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Request {
                    run_id: String,
                    device_id: String,
                    package_id: String,
                    #[serde(default)]
                    timeout_secs: Option<u64>,
                }
                let request: Request = serde_json::from_value(values)?;
                ensure!(
                    !request.run_id.is_empty()
                        && request.run_id.len() <= 128
                        && request.device_id.len() <= 256
                        && request.package_id.len() <= 256,
                    "确认上下文无效"
                );
                if action == "confirmation.begin" {
                    self.queue()?
                        .begin_confirmation(
                            &request.run_id,
                            &request.device_id,
                            &request.package_id,
                            request.timeout_secs.unwrap_or(300),
                        )
                        .await
                } else {
                    self.queue()?
                        .poll_confirmation(&request.run_id, &request.device_id, &request.package_id)
                        .await
                }
            }
            "automatic.start" => self.automatic.start(values).await,
            "automatic.stop" => self.automatic.stop().await,
            "automatic.status" => self.automatic.status().await,
            "automatic.preview" => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Preview {
                    video_folder: String,
                }
                let request: Preview = serde_json::from_value(values)?;
                Ok(json!(
                    output::inspect_video_folder(&request.video_folder).await?
                ))
            }
            "audience.read" => self.audience.read().await,
            "audience.save" => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Save {
                    text: String,
                    expected_version: Option<String>,
                }
                let request: Save = serde_json::from_value(values)?;
                self.audience
                    .save(request.text, request.expected_version.as_deref())
                    .await
            }
            "audience.open" => {
                self.queue()?;
                self.audience.open().await
            }
            "audience.close" => self.audience.close().await,
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
            "queue.reconcile_success" => {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Reconcile {
                    item_id: String,
                    completed_entrypoint: String,
                }
                let request: Reconcile = serde_json::from_value(values)?;
                ensure!(
                    !request.item_id.is_empty() && request.item_id.len() <= 100,
                    "队列项 ID 无效"
                );
                ensure!(
                    !request.completed_entrypoint.is_empty()
                        && request.completed_entrypoint.len() <= 1024,
                    "完成入口无效"
                );
                self.queue()?
                    .reconcile_success(&request.item_id, &request.completed_entrypoint)
                    .await
            }
            "queue.configure" => {
                let _manual = self.automatic.manual_guard().await?;
                let queue = self.queue()?;
                let device = values["device_id"].as_str().unwrap_or("");
                let package = values["package_id"].as_str().unwrap_or("");
                if let Some(folder) = values.get("video_folder") {
                    let folder = folder
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("视频文件夹必须是文本"))?;
                    queue
                        .configure_with_folder(device, package, Some(folder))
                        .await?;
                } else {
                    queue.configure(device, package).await?;
                }
                Ok(json!({"ok":true}))
            }
            "queue.control" => {
                let _manual = if values["op"].as_str() == Some("unbind") {
                    Some(self.automatic.manual_guard().await?)
                } else {
                    None
                };
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
                    identity_namespace: "local-simulation".into(),
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
                let stream = self
                    .output
                    .try_lock()
                    .ok()
                    .and_then(|output| output.as_ref().map(|h| h.status.lock().clone()));
                Ok(
                    json!({"stream":stream,"automatic":self.automatic.status().await?,"connection":self.status.lock().clone(),"features":{"workflow_cleanup":true,"queue_reconcile_success":true,"user_quotas":true,"viewer_confirmation":true,"danmaku_sender":true},"local_sources":{"stream_url":self.links.stream_url(),"audience_url":self.links.audience_url()},"platforms":[{"id":"bilibili","name":"哔哩哔哩","modes":["open_live","oauth"]}]}),
                )
            }
            "stream.start" => {
                let _manual = self.automatic.manual_guard().await?;
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
                let source_url = (req.mode == "local").then(|| self.links.stream_url().to_owned());
                let handle = output::start(self.devices.clone(), req, source_url).await?;
                let result = json!(handle.status.lock().clone());
                *slot = Some(handle);
                drop(slot);
                if let Some(queue) = &self.queue {
                    queue.manual_mode().await?;
                }
                Ok(result)
            }
            "stream.stop" => {
                let _manual = self.automatic.manual_guard().await?;
                if let Some(handle) = self.output.lock().await.take() {
                    handle.stop().await;
                }
                Ok(json!({"state":"stopped"}))
            }
            "stream.switch" => {
                let _manual = self.automatic.manual_guard().await?;
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Switch {
                    device_id: String,
                }
                let request: Switch = serde_json::from_value(values)?;
                ensure!(!request.device_id.trim().is_empty(), "请选择输出设备");
                let slot = self.output.lock().await;
                let output = slot.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("live.output_not_running: 请先在直播助手开始输出")
                })?;
                Ok(json!(output.switch_device(&request.device_id).await?))
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
    async fn local_http(&self, path: &str, _headers: HeaderMap) -> Response {
        // Core admits only real loopback peers with a valid loopback Host and
        // same-origin request. This layer grants an exact read capability only.
        let response = match self.links.resolve(path) {
            Some(local_links::LocalTarget::Stream(name)) => {
                let output = self.output.lock().await;
                match output.as_ref() {
                    Some(handle) => handle.local_file(name).await,
                    None => {
                        (StatusCode::SERVICE_UNAVAILABLE, [("retry-after", "1")]).into_response()
                    }
                }
            }
            Some(local_links::LocalTarget::AudiencePage) => self.audience.page().await,
            Some(local_links::LocalTarget::AudienceState) => match self.queue() {
                Ok(queue) => self.audience.snapshot(queue, &self.status).await,
                Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
            },
            None => StatusCode::NOT_FOUND.into_response(),
        };
        audience::protect(response)
    }
    async fn stop(&self) {
        if self.automatic.owns_output() {
            if let Err(error) = self.automatic.stop().await {
                tracing::error!(%error, "自动直播停止未完成");
            }
        }
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
        if let Some(sender) = &self.sender {
            sender.stop().await;
        }
        self.automatic.shutdown().await;
        self.audience.shutdown().await;
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
