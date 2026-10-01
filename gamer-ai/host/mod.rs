//! AI gameplay and MCP are plugin business; target I/O and ownership remain Core mechanisms.
pub mod mcp;
mod provider;
mod settings;
#[cfg(test)]
mod tests;
mod tools;

use crate::{
    capabilities::CapabilityRegistry,
    core::{
        control::ControlLease, ActivityLease, AppPackageId, RunContext, RunPayload, RunRequest,
    },
    device::DeviceManager,
    extensions::{
        service::BuiltinService, ExtensionError, ExtensionId, ExtensionResult, ExtensionService,
        ExtensionState, Permission, TimerRunnerRegistrar,
    },
    resources::PackageStore,
    run_manager::{
        FinishHook, RunExecutor, RunManager, RunOutcome, RunSource, StartError, StartRequest,
    },
    scheduler::Scheduler,
    timer_core::{
        TimerCompletion, TimerCompletionHook, TimerOutcome, TimerRun, TimerRunner, TimerRunnerError,
    },
};
use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use futures_util::future::BoxFuture;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex as AsyncMutex, Notify};

pub const ID: &str = "gamer-ai";
pub const ACTIONS: &[&str] = &[
    "settings.get",
    "settings.save",
    "connection.probe",
    "session.start",
    "session.get",
    "session.pause",
    "session.resume",
    "session.stop",
    "mcp.tokens.create",
    "mcp.tokens.list",
    "mcp.tokens.revoke",
];
pub fn accepts(id: &str, action: &str) -> bool {
    id == ID && ACTIONS.contains(&action)
}
pub fn permissions(id: &str, action: &str) -> Option<&'static [Permission]> {
    accepts(id, action).then_some(match action {
        "connection.probe" | "settings.save" => &[Permission::AiConnect, Permission::UiHost],
        "session.start" => &[Permission::DeviceRead, Permission::UiHost],
        _ => &[Permission::UiHost],
    })
}

pub struct Runtime {
    pub devices: Arc<DeviceManager>,
    pub packages: Arc<PackageStore>,
    pub runs: Arc<RunManager>,
    pub scheduler: Arc<Scheduler>,
    pub capabilities: CapabilityRegistry,
}
pub struct AiService {
    pub(crate) state: Arc<State>,
}
pub(crate) struct State {
    runtime: Runtime,
    settings: settings::Settings,
    extensions: Mutex<Weak<ExtensionService>>,
    enabled: AtomicBool,
    sessions: Mutex<BTreeMap<String, Arc<Session>>>,
    tokens: Mutex<BTreeMap<String, Token>>,
    token_path: PathBuf,
}
pub(crate) struct Session {
    record: Mutex<SessionRecord>,
    lease: AsyncMutex<Option<ControlLease>>,
    transition: AsyncMutex<()>,
    operation: AsyncMutex<()>,
    cancelled: Mutex<Arc<AtomicBool>>,
    ending: AtomicBool,
    wake: Notify,
    deadline: Mutex<Instant>,
    active_since: Mutex<Option<Instant>>,
    frame: Mutex<Option<tools::Observation>>,
    binding: Mutex<Option<crate::capabilities::FrameStamp>>,
    results: Mutex<BTreeMap<String, Value>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub max_turns: u32,
    pub max_actions: u32,
    pub max_seconds: u64,
    pub max_tokens: u64,
    pub max_failures: u32,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_turns: 40,
            max_actions: 120,
            max_seconds: 600,
            max_tokens: 100_000,
            max_failures: 3,
        }
    }
}
impl Limits {
    fn validate(&self) -> Result<()> {
        ensure!(
            (1..=500).contains(&self.max_turns)
                && (1..=2000).contains(&self.max_actions)
                && (10..=7200).contains(&self.max_seconds)
                && (2048..=2_000_000).contains(&self.max_tokens)
                && (1..=20).contains(&self.max_failures),
            "运行预算超出允许范围"
        );
        Ok(())
    }
}
#[derive(Clone, Default, Serialize)]
pub struct Usage {
    pub turns: u32,
    pub actions: u32,
    pub active_seconds: f64,
    pub total_tokens: Option<u64>,
    pub known_tokens: u64,
    pub has_unknown_tokens: bool,
    pub consecutive_failures: u32,
}
#[derive(Clone, Serialize)]
pub struct Event {
    pub seq: u64,
    pub at: String,
    pub kind: String,
    pub message: String,
    pub data: Value,
}
#[derive(Clone, Serialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub run_id: String,
    pub device_id: String,
    pub android_package: Option<String>,
    pub content_package: String,
    pub goal: String,
    pub mode: String,
    pub state: String,
    pub generation: u64,
    pub reason: Option<String>,
    pub limits: Limits,
    pub usage: Usage,
    pub events: Vec<Event>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Token {
    token_id: String,
    hash: String,
    label: String,
    device_id: String,
    content_package: String,
    control: bool,
    ttl_seconds: u64,
    expires_at: i64,
}
impl Token {
    fn deadline(&self) -> Instant {
        Instant::now()
            + Duration::from_secs(
                self.ttl_seconds.min(
                    self.expires_at
                        .saturating_sub(Utc::now().timestamp())
                        .max(0) as u64,
                ),
            )
    }
    fn public(&self) -> Value {
        json!({"token_id":self.token_id,"label":self.label,"device_id":self.device_id,"content_package":self.content_package,"control":self.control,"ttl_seconds":self.ttl_seconds,"expires_at":self.expires_at})
    }
}

impl Session {
    fn event(&self, kind: &str, message: impl Into<String>, data: Value) {
        let mut r = self.record.lock();
        let seq = r.events.last().map_or(1, |e| e.seq + 1);
        if data.get("image_data_url").is_some() {
            let mut retained = 0;
            for event in r.events.iter_mut().rev() {
                if event.data.get("image_data_url").is_some() {
                    retained += 1;
                    if retained >= 3 {
                        event.data.as_object_mut().unwrap().remove("image_data_url");
                    }
                }
            }
        }
        r.events.push(Event {
            seq,
            at: Utc::now().to_rfc3339(),
            kind: kind.into(),
            message: message.into(),
            data,
        });
        if r.events.len() > 256 {
            r.events.remove(0);
        }
    }
    fn set_state(&self, state: &str, reason: Option<String>) {
        let mut r = self.record.lock();
        let mut since = self.active_since.lock();
        if let Some(started) = since.take() {
            r.usage.active_seconds += started.elapsed().as_secs_f64();
        }
        if state == "running" {
            *since = Some(Instant::now());
        }
        r.state = state.into();
        r.reason = reason;
    }
    fn charge_time(&self) {
        let mut r = self.record.lock();
        let mut since = self.active_since.lock();
        if let Some(started) = since.as_mut() {
            r.usage.active_seconds += started.elapsed().as_secs_f64();
            *started = Instant::now();
        }
    }
    fn cancel_generation(&self) {
        self.cancelled.lock().store(true, Ordering::Release);
        self.wake.notify_waiters();
    }
}

impl AiService {
    pub fn new(runtime: Runtime, root: &std::path::Path) -> Result<Self> {
        let token_path = root.join("extension-data/gamer-ai/private/tokens.dat");
        let tokens = match std::fs::read(&token_path) {
            Ok(bytes) => {
                ensure!(bytes.len() <= 128 * 1024, "MCP 令牌配置过大");
                serde_json::from_slice(&crate::core::secrets::protect(&bytes, false)?)
                    .map_err(|_| anyhow::anyhow!("MCP 令牌配置损坏"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(_) => anyhow::bail!("无法读取 MCP 令牌配置"),
        };
        Ok(Self {
            state: Arc::new(State {
                runtime,
                settings: settings::Settings::new(
                    root.join("extension-data/gamer-ai/private/connection.dat"),
                ),
                extensions: Mutex::new(Weak::new()),
                enabled: AtomicBool::new(false),
                sessions: Mutex::new(BTreeMap::new()),
                tokens: Mutex::new(tokens),
                token_path,
            }),
        })
    }
    pub fn attach(&self, extensions: &Arc<ExtensionService>) {
        *self.state.extensions.lock() = Arc::downgrade(extensions);
    }
    pub fn executor(&self) -> Arc<dyn RunExecutor> {
        Arc::new(AiExecutor(Arc::downgrade(&self.state)))
    }
    pub fn registrar(&self) -> Arc<dyn TimerRunnerRegistrar> {
        Arc::new(AiRegistrar(Arc::downgrade(&self.state)))
    }
    async fn dispatch(&self, action: &str, values: Value) -> Result<Value> {
        match action {
            "settings.get" => self.state.settings.read(),
            "settings.save" => self.state.settings.save(values),
            "connection.probe" => {
                let saved = self.state.settings.read()?;
                let connection = self.state.settings.connection()?;
                let probe = provider::Provider::new(connection)?
                    .probe(&AtomicBool::new(false))
                    .await?;
                self.state
                    .settings
                    .save_probe(probe, saved["version"].as_str())
            }
            "session.get" => {
                let id = values["session_id"].as_str();
                let sessions = self
                    .state
                    .sessions
                    .lock()
                    .values()
                    .filter(|s| id.is_none_or(|id| s.record.lock().session_id == id))
                    .map(|s| s.record.lock().clone())
                    .collect::<Vec<_>>();
                Ok(json!({"sessions":sessions}))
            }
            "session.start" => {
                let target = required(&values, "device_id")?;
                let package = required(&values, "content_package")?;
                ensure!(
                    self.state
                        .runtime
                        .packages
                        .list_packages()?
                        .iter()
                        .any(|p| p.id == package),
                    "配置包不存在"
                );
                let app = crate::targets::app_context(
                    &self.state.runtime.devices,
                    target,
                    Some(AppPackageId::new(package)?),
                )?;
                let request = RunRequest::for_app(
                    app,
                    ID,
                    format!("{package}/interactive"),
                    RunPayload::new(values.clone()),
                )?;
                let (record, session) = self
                    .state
                    .submit(request, None, None, None)
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                Ok(
                    json!({"run_id":record.run_id,"session_id":session.record.lock().session_id,"state":"starting"}),
                )
            }
            "session.pause" | "session.resume" | "session.stop" => {
                let session = self.state.session(required(&values, "session_id")?)?;
                match action {
                    "session.pause" => self.state.pause(&session, "用户暂停".into()).await?,
                    "session.resume" => self.state.resume(&session).await?,
                    _ => {
                        self.state.stop(&session, "cancelled").await?;
                    }
                }
                Ok(json!({"session":session.record.lock().clone()}))
            }
            "mcp.tokens.create" => self.state.create_token(values),
            "mcp.tokens.list" => Ok(
                json!({"tokens":self.state.tokens.lock().values().map(Token::public).collect::<Vec<_>>()}),
            ),
            "mcp.tokens.revoke" => {
                let token_id = required(&values, "token_id")?;
                let removed = {
                    let mut tokens = self.state.tokens.lock();
                    let token = tokens.remove(token_id);
                    self.state.persist_tokens(&tokens)?;
                    token
                };
                if let Some(token) = removed {
                    if let Some(session) = self.state.active(&token.device_id) {
                        let matched = {
                            let r = session.record.lock();
                            token.control
                                && r.mode == "mcp"
                                && r.content_package == token.content_package
                        };
                        if matched {
                            self.state
                                .pause(&session, "MCP 连接令牌已撤销".into())
                                .await?;
                        }
                    }
                }
                Ok(json!({"ok":true}))
            }
            _ => anyhow::bail!("未知 AI 插件动作"),
        }
    }
}

impl State {
    fn authorize(&self, permission: Option<Permission>) -> Result<()> {
        ensure!(self.enabled.load(Ordering::Acquire), "AI 插件未启用");
        let extensions = self
            .extensions
            .lock()
            .upgrade()
            .context("AI 插件宿主未装配")?;
        let snapshot = extensions.snapshot_for(&ExtensionId::parse(ID)?)?;
        ensure!(
            snapshot.state() == ExtensionState::Running,
            "AI 插件当前不可用"
        );
        if let Some(permission) = permission {
            ensure!(
                snapshot.manifest().permissions().allows(permission),
                "未授权能力 {}",
                permission.as_str()
            );
        }
        Ok(())
    }
    fn session(&self, id: &str) -> Result<Arc<Session>> {
        self.sessions
            .lock()
            .get(id)
            .cloned()
            .context("AI 会话不存在")
    }
    fn active(&self, target: &str) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .values()
            .find(|s| {
                let r = s.record.lock();
                r.device_id == target && r.state != "finished"
            })
            .cloned()
    }
    async fn submit(
        self: &Arc<Self>,
        mut request: RunRequest,
        task_id: Option<String>,
        scheduled_at: Option<i64>,
        completion: Option<TimerCompletionHook>,
    ) -> std::result::Result<(crate::run_manager::RunRecord, Arc<Session>), TimerRunnerError> {
        self.authorize(None)
            .map_err(|e| TimerRunnerError::DependencyMissing(e.to_string()))?;
        let values = request.payload.as_value();
        if !values.is_object() {
            return Err(TimerRunnerError::Invalid("AI payload 必须是对象".into()));
        }
        let mode = values["mode"].as_str().unwrap_or("api");
        if !matches!(mode, "api" | "mcp") {
            return Err(TimerRunnerError::Invalid("未知 AI 运行模式".into()));
        }
        let goal = values["goal"].as_str().unwrap_or("").trim();
        if (mode == "api" && goal.is_empty()) || goal.len() > 8000 {
            return Err(TimerRunnerError::Invalid(
                "目标不能为空或超过 8000 字节".into(),
            ));
        }
        let limits: Limits =
            serde_json::from_value(values.get("limits").cloned().unwrap_or(json!({})))
                .map_err(|_| TimerRunnerError::Invalid("运行预算格式无效".into()))?;
        limits
            .validate()
            .map_err(|e| TimerRunnerError::Invalid(e.to_string()))?;
        if mode == "api" {
            self.authorize(Some(Permission::AiConnect))
                .and_then(|_| self.settings.connection().map(|_| ()))
                .map_err(|e| TimerRunnerError::Invalid(e.to_string()))?;
        }
        let package = request
            .app
            .content_package
            .as_ref()
            .ok_or_else(|| TimerRunnerError::Invalid("请选择配置包".into()))?
            .as_str()
            .to_owned();
        if request.entrypoint != format!("{package}/interactive")
            || !self
                .runtime
                .packages
                .list_packages()
                .map_err(|e| TimerRunnerError::Invalid(e.to_string()))?
                .iter()
                .any(|p| p.id == package)
        {
            return Err(TimerRunnerError::Invalid(
                "AI entrypoint 应为 <配置包>/interactive，且配置包必须存在".into(),
            ));
        }
        let session_id = uuid::Uuid::new_v4().to_string();
        let session = Arc::new(Session {
            record: Mutex::new(SessionRecord {
                session_id: session_id.clone(),
                run_id: String::new(),
                device_id: request.device_id.as_str().into(),
                android_package: request
                    .app
                    .android_package
                    .as_ref()
                    .map(|a| a.as_str().into()),
                content_package: package,
                goal: goal.into(),
                mode: mode.into(),
                state: "starting".into(),
                generation: 0,
                reason: None,
                limits,
                usage: Usage::default(),
                events: vec![],
            }),
            lease: AsyncMutex::new(None),
            transition: AsyncMutex::new(()),
            operation: AsyncMutex::new(()),
            cancelled: Mutex::new(Arc::new(AtomicBool::new(false))),
            ending: AtomicBool::new(false),
            wake: Notify::new(),
            deadline: Mutex::new(Instant::now() + Duration::from_secs(120)),
            active_since: Mutex::new(None),
            frame: Mutex::new(None),
            binding: Mutex::new(None),
            results: Mutex::new(BTreeMap::new()),
        });
        {
            let mut sessions = self.sessions.lock();
            if sessions.len() >= 64 {
                if let Some(old) = sessions
                    .iter()
                    .find(|(_, s)| s.record.lock().state == "finished")
                    .map(|(id, _)| id.clone())
                {
                    sessions.remove(&old);
                } else {
                    return Err(TimerRunnerError::Invalid("AI 会话数量已达上限".into()));
                }
            }
            sessions.insert(session_id.clone(), session.clone());
        }
        let mut payload = request.payload.as_value().clone();
        payload["session_id"] = json!(session_id);
        request.payload = RunPayload::new(payload);
        let weak = Arc::downgrade(self);
        let weak_session = Arc::downgrade(&session);
        let hook: FinishHook = Arc::new(move |record, outcome| {
            if let Some(completion) = &completion {
                completion(TimerCompletion {
                    task_id: record.task_id.clone().unwrap_or_default(),
                    scheduled_at: record.scheduled_at,
                    run_id: record.run_id.clone(),
                    outcome: match outcome {
                        RunOutcome::Success(_) => TimerOutcome::Success,
                        RunOutcome::Cancelled(_) => TimerOutcome::Cancelled,
                        RunOutcome::Failed(e, _) => TimerOutcome::Failed(e.clone()),
                    },
                });
            }
            if let (Some(state), Some(session)) = (weak.upgrade(), weak_session.upgrade()) {
                let reason = match outcome {
                    RunOutcome::Success(_) => "completed".into(),
                    RunOutcome::Cancelled(_) => "cancelled".into(),
                    RunOutcome::Failed(e, _) => format!("failed: {e}"),
                };
                tokio::spawn(async move {
                    let _ = state.finish(&session, &reason).await;
                });
            }
        });
        match self.runtime.runs.submit(
            StartRequest {
                request,
                source: if task_id.is_some() {
                    RunSource::Scheduled
                } else {
                    RunSource::Manual
                },
                task_id,
                scheduled_at,
                realtime_logs: true,
            },
            Some(hook),
        ) {
            Ok(record) => {
                session.record.lock().run_id = record.run_id.clone();
                session.event("state", "AI 会话已创建", json!({}));
                Ok((record, session))
            }
            Err(error) => {
                self.sessions.lock().remove(&session_id);
                Err(match error {
                    StartError::Conflict(record) => TimerRunnerError::Conflict(record),
                    StartError::ShuttingDown => TimerRunnerError::ShuttingDown,
                })
            }
        }
    }
    async fn pause(&self, session: &Arc<Session>, reason: String) -> Result<()> {
        let _transition = session.transition.lock().await;
        let state = session.record.lock().state.clone();
        if state == "paused" {
            return Ok(());
        }
        ensure!(
            matches!(state.as_str(), "running" | "starting"),
            "当前状态不能暂停"
        );
        let mut lease = session.lease.lock().await;
        let current = lease.as_ref().context("会话尚未就绪，请稍后暂停")?.clone();
        session.set_state("pausing", Some(reason.clone()));
        session.cancel_generation();
        let next = self
            .runtime
            .devices
            .controls
            .pause(
                &current,
                self.runtime.devices.release_control_inputs(&current.target),
            )
            .await?;
        session.record.lock().generation = next.generation;
        *lease = Some(next);
        session.frame.lock().take();
        session.set_state("paused", Some(reason.clone()));
        session.event("state", reason, json!({"manual_allowed":true}));
        Ok(())
    }
    async fn resume(&self, session: &Arc<Session>) -> Result<()> {
        self.authorize(None)?;
        let _transition = session.transition.lock().await;
        ensure!(session.record.lock().state == "paused", "当前会话没有暂停");
        let r = session.record.lock().clone();
        if r.mode == "api" {
            self.settings.connection()?;
        }
        let app = crate::targets::app_context(
            &self.runtime.devices,
            &r.device_id,
            Some(AppPackageId::new(&r.content_package)?),
        )?;
        ensure!(
            app.android_package.as_ref().map(|p| p.as_str()) == r.android_package.as_deref(),
            "运行目标应用已改变，请停止后重新开始"
        );
        if crate::targets::is_browser(&r.device_id) {
            let browser = self.runtime.devices.browsers.session(&r.device_id)?;
            ensure!(browser.is_alive(), "浏览器目标已断开，请停止后重新开始");
            if let Some(binding) = session.binding.lock().as_ref() {
                let current = browser.stamp();
                ensure!(
                    current.target == binding.target && current.epoch == binding.epoch,
                    "浏览器绑定已改变，请停止后重新开始"
                );
            }
        } else {
            crate::targets::prepare(&self.runtime.devices, &r.device_id).await?;
        }
        let mut lease = session.lease.lock().await;
        let current = lease.as_ref().context("控制会话不存在")?.clone();
        session.set_state("resuming", None);
        let next = self
            .runtime
            .devices
            .controls
            .resume(
                &current,
                self.runtime.devices.release_control_inputs(&r.device_id),
            )
            .await?;
        session.record.lock().generation = next.generation;
        *lease = Some(next);
        *session.cancelled.lock() = Arc::new(AtomicBool::new(false));
        session.frame.lock().take();
        session.results.lock().clear();
        if !crate::targets::is_browser(&r.device_id) {
            session.binding.lock().take();
        }
        *session.deadline.lock() = Instant::now() + Duration::from_secs(120);
        session.set_state("running", None);
        session.event("state", "AI 已恢复，将重新观察画面", json!({}));
        session.wake.notify_waiters();
        Ok(())
    }
    async fn stop(&self, session: &Arc<Session>, reason: &str) -> Result<()> {
        let run_id = session.record.lock().run_id.clone();
        if session.record.lock().state != "finished" {
            session.set_state("stopping", Some(reason.into()));
            session.ending.store(true, Ordering::Release);
            session.cancel_generation();
            self.runtime.runs.cancel(&run_id);
        }
        tokio::time::timeout(
            Duration::from_secs(10),
            self.runtime.runs.wait_terminal(&run_id),
        )
        .await
        .context("正在结束已入场操作，请稍后查看状态")?;
        self.finish(session, reason).await
    }
    async fn finish(&self, session: &Arc<Session>, reason: &str) -> Result<()> {
        let _transition = session.transition.lock().await;
        if session.record.lock().state == "finished" {
            return Ok(());
        }
        session.ending.store(true, Ordering::Release);
        session.cancel_generation();
        let mut held = session.lease.lock().await;
        if let Some(lease) = held.as_ref() {
            if let Err(e) = self
                .runtime
                .devices
                .controls
                .release(
                    lease,
                    self.runtime.devices.release_control_inputs(&lease.target),
                )
                .await
            {
                session.event("error", format!("输入收尾失败：{e}"), json!({}));
                session.set_state("stopping", Some("输入收尾失败，请再次停止重试".into()));
                return Err(e);
            }
        }
        held.take();
        session.set_state("finished", Some(reason.into()));
        session.event("state", format!("会话结束：{reason}"), json!({}));
        session.wake.notify_waiters();
        Ok(())
    }
    async fn stop_all(&self) {
        let sessions = self.sessions.lock().values().cloned().collect::<Vec<_>>();
        // Stop new work on every target before draining any one target's
        // admitted operation. A slow release must not let other AI runs proceed.
        for session in &sessions {
            if session.record.lock().state != "finished" {
                session.set_state("stopping", Some("cancelled".into()));
                session.ending.store(true, Ordering::Release);
                session.cancel_generation();
            }
        }
        for session in sessions {
            let _ = self.stop(&session, "cancelled").await;
        }
    }
    fn persist_tokens(&self, tokens: &BTreeMap<String, Token>) -> Result<()> {
        let parent = self.token_path.parent().unwrap();
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        crate::core::fs::atomic_write(
            &self.token_path,
            &crate::core::secrets::protect(&serde_json::to_vec(tokens)?, true)?,
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.token_path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
    fn create_token(&self, values: Value) -> Result<Value> {
        let device_id = required(&values, "device_id")?.to_owned();
        let content_package = required(&values, "content_package")?.to_owned();
        crate::targets::check_available(&self.runtime.devices, &device_id)?;
        ensure!(
            self.runtime
                .packages
                .list_packages()?
                .iter()
                .any(|p| p.id == content_package),
            "配置包不存在"
        );
        let ttl_seconds = values["ttl_seconds"].as_u64().unwrap_or(120);
        ensure!(
            (30..=3600).contains(&ttl_seconds),
            "租约期限须为 30 到 3600 秒"
        );
        let secret = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let token = Token {
            token_id: uuid::Uuid::new_v4().to_string(),
            hash: format!("{:x}", Sha256::digest(secret.as_bytes())),
            label: values["label"]
                .as_str()
                .unwrap_or("MCP 客户端")
                .chars()
                .take(80)
                .collect(),
            device_id,
            content_package,
            control: values["control"].as_bool().unwrap_or(false),
            ttl_seconds,
            expires_at: Utc::now().timestamp() + 86400,
        };
        let mut result = token.public();
        result["token"] = json!(secret);
        let mut tokens = self.tokens.lock();
        ensure!(tokens.len() < 64, "连接令牌数量已达上限");
        tokens.insert(token.token_id.clone(), token);
        self.persist_tokens(&tokens)?;
        Ok(result)
    }
    fn token(&self, secret: &str) -> Result<Token> {
        ensure!(secret.len() == 64, "MCP 连接令牌无效");
        let hash = format!("{:x}", Sha256::digest(secret.as_bytes()));
        let token = self
            .tokens
            .lock()
            .values()
            .find(|t| constant_eq(&t.hash, &hash))
            .cloned()
            .context("MCP 连接令牌无效或已撤销")?;
        ensure!(
            token.expires_at > Utc::now().timestamp(),
            "MCP 连接令牌已到期"
        );
        Ok(token)
    }
    async fn mcp(self: &Arc<Self>, request: Value, secret: &str) -> Result<Option<Value>> {
        self.authorize(None)?;
        let token = self.token(secret)?;
        use mcp::Request;
        let response = match mcp::parse(request) {
            Request::Initialize {
                id,
                protocol_version,
            } => mcp::success(
                id,
                json!({"protocolVersion":if ["2025-11-25","2025-06-18","2025-03-26"].contains(&protocol_version.as_str()){protocol_version}else{"2025-11-25".into()},"capabilities":{"tools":{}},"serverInfo":{"name":ID,"version":"0.1.0"},"instructions":"截图查看目标；输入需用户在 Gamer 面板开始外部 MCP 控制会话，暂停后由用户恢复。"}),
            ),
            Request::Ping { id } => {
                if token.control {
                    if let Some(s) = self.active(&token.device_id) {
                        let matches = {
                            let r = s.record.lock();
                            r.mode == "mcp" && r.content_package == token.content_package
                        };
                        if matches {
                            *s.deadline.lock() = token.deadline();
                        }
                    }
                }
                mcp::success(id, json!({}))
            }
            Request::ToolsList { id } => mcp::success(
                id,
                json!({"tools":tools::catalog(crate::targets::capabilities(&self.runtime.devices,&token.device_id)?,token.control)}),
            ),
            Request::ToolsCall {
                id,
                name,
                arguments,
            } => {
                let operation = arguments["operation_id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                let call_id = format!("mcp:{}:{operation}", token.token_id);
                let result = match self.mcp_tool(&token, &name, arguments, &call_id).await {
                    Ok(value) => value,
                    Err(e) => {
                        json!({"content":[{"type":"text","text":e.to_string()}],"isError":true})
                    }
                };
                mcp::success(id, result)
            }
            Request::Notification => return Ok(None),
            Request::Error(value) => value,
        };
        Ok(Some(response))
    }
    async fn mcp_tool(
        self: &Arc<Self>,
        token: &Token,
        name: &str,
        args: Value,
        call_id: &str,
    ) -> Result<Value> {
        self.authorize(Some(tools::permission(name)?))?;
        match name {
            "target_list"=>Ok(json!({"content":[{"type":"text","text":json!({"targets":[{"device_id":token.device_id,"content_package":token.content_package,"capabilities":crate::targets::capabilities(&self.runtime.devices,&token.device_id)?}]}).to_string()}]})),
            "session_status"=>Ok(mcp::ToolResult::json(json!({"session":self.active(&token.device_id).filter(|s|s.record.lock().content_package==token.content_package).map(|s|s.record.lock().clone()),"input_control":self.runtime.devices.controls.status(&token.device_id)})).value()),
            "context_get"=>Ok(json!({"content":[{"type":"text","text":json!({"device_id":token.device_id,"content_package":token.content_package,"identity":crate::targets::identity(&self.runtime.devices,&token.device_id).await?}).to_string()}]})),
            _=> {
                if name=="screen_capture"&&args.get("session_id").is_none() {let width=args.get("max_width").map_or(Ok(1280u32),|v|v.as_u64().and_then(|n|u32::try_from(n).ok()).context("max_width 必须为整数"))?;ensure!((320..=1920).contains(&width),"max_width 超出允许范围");return Ok(self.capture(&token.device_id,0,width).await?.1);}
                let session=self.active(&token.device_id).context("请先在 Gamer 面板开始外部 MCP 会话")?;
                let record=session.record.lock().clone();ensure!(record.mode=="mcp"&&record.content_package==token.content_package,"MCP 令牌与运行会话不匹配");
                ensure!(token.control||name=="screen_capture","此令牌仅可观察，不能控制设备");
                if name!="screen_capture" {let operation=required(&args,"operation_id")?;ensure!(operation.len()<=128,"operation_id 超过长度上限");}
                let expected=args["generation"].as_u64().context("缺少 generation，请先查询 session_status")?;ensure!(expected==record.generation,"stale_generation: 请重新观察会话");
                ensure!(args["session_id"].as_str()==Some(&record.session_id),"MCP 会话身份不匹配");
                let result=self.tool(&session,name,args,call_id,expected).await?;
                if token.control {*session.deadline.lock()=token.deadline();}
                Ok(result)
            }
        }
    }
    async fn run(
        self: &Arc<Self>,
        session: &Arc<Session>,
        stop: Arc<AtomicBool>,
    ) -> Result<Vec<(String, String)>> {
        let mode = session.record.lock().mode.clone();
        let mut history = vec![];
        let mut active_provider = None;
        let mut seen_generation = u64::MAX;
        loop {
            if stop.load(Ordering::Acquire) || session.ending.load(Ordering::Acquire) {
                break;
            }
            if let Err(e) = self.authorize(None) {
                session.event("error", e.to_string(), json!({}));
                return Err(e);
            }
            let record = session.record.lock().clone();
            if record.state != "running" {
                tokio::select! {_=session.wake.notified()=>{},_=tokio::time::sleep(Duration::from_millis(100))=>{}}
                continue;
            }
            session.charge_time();
            let budget = { budget_reason(&session.record.lock()) };
            if let Some(reason) = budget {
                self.pause(session, reason).await?;
                continue;
            }
            if mode == "mcp" {
                if Instant::now() > *session.deadline.lock() {
                    self.pause(session, "MCP 控制租约到期，请重新连接后由用户恢复".into())
                        .await?;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
            if seen_generation != record.generation {
                seen_generation = record.generation;
                active_provider = Some(
                    provider::Provider::new(self.settings.connection()?)?.with_output_limit(2048),
                );
                history = vec![
                    json!({"role":"system","content":[{"type":"input_text","text":"你是通用游戏操作助手。根据用户目标和最新截图操作，只使用提供的工具。坐标以截图实际宽高为准，输入工具必须带最近截图的 frame_id。每次操作后观察效果，不假设成功。无法继续或目标完成时调用 session_finish 并说明原因。不要伪造观察。"}]}),
                    json!({"role":"user","content":[{"type":"input_text","text":format!("目标：{}",record.goal)}]}),
                ];
                let capture = self
                    .tool(
                        session,
                        "screen_capture",
                        json!({}),
                        &format!("capture:{}", record.generation),
                        record.generation,
                    )
                    .await;
                let capture = match capture {
                    Ok(capture) => capture,
                    Err(error) => {
                        let current = session.record.lock().clone();
                        if current.generation != record.generation || current.state != "running" {
                            continue;
                        }
                        self.pause(session, format!("无法观察目标：{error}"))
                            .await?;
                        continue;
                    }
                };
                append_observation(&mut history, &capture);
            }
            let cancel = session.cancelled.lock().clone();
            let provider = active_provider.as_ref().context("模型连接尚未就绪")?;
            let catalog = tools::catalog(
                crate::targets::capabilities(&self.runtime.devices, &record.device_id)?,
                true,
            );
            let functions = tools::function_catalog(&catalog);
            retain_recent_images(&mut history, 3);
            session.record.lock().usage.turns += 1;
            session.charge_time();
            let remaining = {
                let current = session.record.lock();
                Duration::from_secs_f64(
                    (current.limits.max_seconds as f64 - current.usage.active_seconds).max(0.0),
                )
            };
            let turn = tokio::select! {
                turn = provider.turn(&history, &functions, &cancel) => turn,
                _ = tokio::time::sleep(remaining) => {
                    record_usage(&mut session.record.lock().usage, None);
                    self.pause(session, "活动时长达到预算".into()).await?;
                    continue;
                }
            };
            if cancel.load(Ordering::Acquire)
                || stop.load(Ordering::Acquire)
                || session.record.lock().generation != record.generation
            {
                record_usage(
                    &mut session.record.lock().usage,
                    turn.as_ref().ok().and_then(|turn| turn.usage.as_ref()),
                );
                continue;
            }
            match turn {
                Err(e) => {
                    record_usage(&mut session.record.lock().usage, None);
                    session.record.lock().usage.consecutive_failures += 1;
                    session.event("error", format!("模型请求失败：{e}"), json!({}));
                    if session.record.lock().usage.consecutive_failures
                        >= record.limits.max_failures
                    {
                        self.pause(session, "连续模型请求失败".into()).await?;
                    } else {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                    }
                }
                Ok(turn) => {
                    record_usage(&mut session.record.lock().usage, turn.usage.as_ref());
                    if !turn.text.is_empty() {
                        session.event("assistant", turn.text.clone(), json!({}));
                    }
                    history.extend(turn.items);
                    if turn.calls.is_empty() {
                        session.record.lock().usage.consecutive_failures += 1;
                        history.push(json!({"role":"user","content":[{"type":"input_text","text":"请使用工具观察/操作；完成或无法继续请调用 session_finish。"}]}));
                        if session.record.lock().usage.consecutive_failures
                            >= record.limits.max_failures
                        {
                            self.pause(session, "模型连续没有提供可执行操作".into())
                                .await?;
                        }
                    }
                    for call in turn.calls {
                        if cancel.load(Ordering::Acquire) || session.ending.load(Ordering::Acquire)
                        {
                            break;
                        }
                        let result = match self
                            .tool(
                                session,
                                &call.name,
                                call.arguments,
                                &call.id,
                                record.generation,
                            )
                            .await
                        {
                            Ok(result) => {
                                session.record.lock().usage.consecutive_failures = 0;
                                result
                            }
                            Err(e) => {
                                session.record.lock().usage.consecutive_failures += 1;
                                session.event("error", e.to_string(), json!({"tool":call.name}));
                                json!({"content":[{"type":"text","text":e.to_string()}],"isError":true})
                            }
                        };
                        history.push(json!({"type":"function_call_output","call_id":call.id,"output":result["content"].clone()}));
                        if session.record.lock().usage.consecutive_failures
                            >= record.limits.max_failures
                        {
                            self.pause(session, "连续工具执行失败".into()).await?;
                            break;
                        }
                    }
                    if history.len() > 120 {
                        self.pause(
                            session,
                            "会话上下文达到上限，请停止后重新开始或人工处理".into(),
                        )
                        .await?;
                    }
                }
            }
        }
        self.finish(
            session,
            if stop.load(Ordering::Acquire) {
                "cancelled"
            } else {
                "completed"
            },
        )
        .await?;
        Ok(vec![("info".into(), "AI 会话结束".into())])
    }
}

struct AiExecutor(Weak<State>);
impl RunExecutor for AiExecutor {
    fn prepare<'a>(
        &'a self,
        context: &'a RunContext,
        request: &'a RunRequest,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let state = self.0.upgrade().context("AI 服务不可用")?;
            state.authorize(None)?;
            let session = state.session(required(request.payload.as_value(), "session_id")?)?;
            let _transition = session.transition.lock().await;
            session.record.lock().run_id = context.run_id.as_str().into();
            ensure!(session.record.lock().state == "starting", "AI 会话已停止");
            let r = session.record.lock().clone();
            let lease = state
                .runtime
                .devices
                .controls
                .claim(&r.device_id, &r.session_id)
                .await?;
            session.record.lock().generation = lease.generation;
            *session.lease.lock().await = Some(lease.clone());
            let app = crate::targets::app_context(
                &state.runtime.devices,
                &r.device_id,
                Some(AppPackageId::new(&r.content_package)?),
            )?;
            ensure!(
                app.android_package.as_ref().map(|p| p.as_str()) == r.android_package.as_deref(),
                "运行目标应用已改变，请停止后重新开始"
            );
            state
                .runtime
                .devices
                .controls
                .execute(
                    &lease,
                    state.runtime.devices.release_control_inputs(&r.device_id),
                )
                .await?;
            crate::targets::prepare(&state.runtime.devices, &r.device_id).await?;
            session.set_state("running", None);
            session.event("state", "AI 已取得控制权", json!({}));
            Ok(())
        })
    }
    fn execute<'a>(
        &'a self,
        _context: &'a RunContext,
        request: &'a RunRequest,
        _realtime: bool,
        stop: Arc<AtomicBool>,
    ) -> BoxFuture<'a, Result<Vec<(String, String)>>> {
        Box::pin(async move {
            let state = self.0.upgrade().context("AI 服务不可用")?;
            let session = state.session(required(request.payload.as_value(), "session_id")?)?;
            state.run(&session, stop).await
        })
    }
    fn acquire(&self, context: &RunContext) -> Result<Box<dyn ActivityLease>> {
        crate::targets::acquire(
            &self.0.upgrade().context("AI 服务不可用")?.runtime.devices,
            context.device_id().as_str(),
        )
    }
}
struct AiRunner(Weak<State>);
#[async_trait]
impl TimerRunner for AiRunner {
    fn runner_id(&self) -> &str {
        ID
    }
    async fn submit(
        &self,
        request: RunRequest,
        task_id: &str,
        scheduled_at: Option<i64>,
        completion: TimerCompletionHook,
    ) -> std::result::Result<TimerRun, TimerRunnerError> {
        let state = self
            .0
            .upgrade()
            .ok_or_else(|| TimerRunnerError::DependencyMissing("AI 服务不可用".into()))?;
        let (record, session) = state
            .submit(
                request,
                (!task_id.is_empty()).then(|| task_id.into()),
                scheduled_at,
                Some(completion),
            )
            .await?;
        Ok(TimerRun {
            run_id: record.run_id,
            detail: Some(json!({"session_id":session.record.lock().session_id})),
        })
    }
    async fn cancel(&self, id: &str) -> std::result::Result<(), TimerRunnerError> {
        self.0
            .upgrade()
            .ok_or(TimerRunnerError::ShuttingDown)?
            .runtime
            .runs
            .cancel(id);
        Ok(())
    }
}
struct AiRegistrar(Weak<State>);
#[async_trait]
impl TimerRunnerRegistrar for AiRegistrar {
    async fn extension_started(&self, id: &str) -> Result<()> {
        if id == ID {
            let state = self.0.upgrade().context("AI 服务不可用")?;
            state.enabled.store(true, Ordering::Release);
            state
                .runtime
                .scheduler
                .register_extension_runner(ID, ID, Arc::new(AiRunner(Arc::downgrade(&state))))
                .await?;
        }
        Ok(())
    }
    async fn extension_stopped(&self, id: &str) -> Result<()> {
        if id == ID {
            let state = self.0.upgrade().context("AI 服务不可用")?;
            state.enabled.store(false, Ordering::Release);
            state.stop_all().await;
            state
                .runtime
                .scheduler
                .unregister_extension_owner(ID)
                .await?;
        }
        Ok(())
    }
    fn executes_without_instance(&self, id: &str) -> bool {
        id == ID
    }
}
#[async_trait]
impl BuiltinService for AiService {
    fn extension_id(&self) -> &str {
        ID
    }
    async fn call(&self, action: &str, values: Value) -> ExtensionResult<Value> {
        self.dispatch(action, values)
            .await
            .map_err(|e| ExtensionError::CallRejected(e.to_string()))
    }
    async fn stop(&self) {
        self.state.stop_all().await;
    }
    async fn mcp(&self, request: Value, token: &str) -> ExtensionResult<Option<Value>> {
        self.state
            .token(token)
            .map_err(|_| ExtensionError::ProtocolUnauthorized)?;
        self.state
            .mcp(request, token)
            .await
            .map_err(|e| ExtensionError::CallRejected(e.to_string()))
    }
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("缺少 {key}"))
}
fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}
fn budget_reason(r: &SessionRecord) -> Option<String> {
    if r.usage.turns >= r.limits.max_turns {
        Some("模型轮数达到预算".into())
    } else if r.usage.actions >= r.limits.max_actions {
        Some("工具调用次数达到预算".into())
    } else if r.usage.active_seconds >= r.limits.max_seconds as f64 {
        Some("活动时长达到预算".into())
    } else if r.usage.known_tokens >= r.limits.max_tokens
        || r.usage
            .total_tokens
            .is_some_and(|t| t >= r.limits.max_tokens)
    {
        Some("Token 使用达到预算".into())
    } else {
        None
    }
}
fn record_usage(usage: &mut Usage, value: Option<&Value>) {
    let total = value.and_then(|value| {
        value["total_tokens"].as_u64().or_else(|| {
            value["input_tokens"]
                .as_u64()?
                .checked_add(value["output_tokens"].as_u64()?)
        })
    });
    if let Some(total) = total {
        usage.known_tokens = usage.known_tokens.saturating_add(total);
    } else {
        usage.has_unknown_tokens = true;
    }
    usage.total_tokens = (!usage.has_unknown_tokens).then_some(usage.known_tokens);
}
fn append_observation(history: &mut Vec<Value>, result: &Value) {
    let content=result["content"].as_array().into_iter().flatten().filter_map(|c|match c["type"].as_str(){Some("image")=>Some(json!({"type":"input_image","image_url":format!("data:{};base64,{}",c["mimeType"].as_str().unwrap_or("image/png"),c["data"].as_str().unwrap_or(""))})),Some("text")=>Some(json!({"type":"input_text","text":c["text"]})),_=>None}).collect::<Vec<_>>();
    history.push(json!({"role":"user","content":content}));
}

fn retain_recent_images(history: &mut [Value], keep: usize) {
    fn count(value: &Value) -> usize {
        match value {
            Value::Array(values) => values.iter().map(count).sum(),
            Value::Object(object) => {
                usize::from(matches!(
                    object.get("type").and_then(Value::as_str),
                    Some("image" | "input_image")
                )) + object.values().map(count).sum::<usize>()
            }
            _ => 0,
        }
    }
    fn prune(value: &mut Value, remaining: &mut usize) {
        if *remaining == 0 {
            return;
        }
        if let Some(kind) = value.get("type").and_then(Value::as_str) {
            if matches!(kind, "image" | "input_image") {
                let input = kind == "input_image";
                *value = json!({"type":if input{"input_text"}else{"text"},"text":"较早的截图已从上下文移除，请以最近截图为准。"});
                *remaining -= 1;
                return;
            }
        }
        match value {
            Value::Array(values) => {
                for v in values {
                    prune(v, remaining);
                }
            }
            Value::Object(object) => {
                for v in object.values_mut() {
                    prune(v, remaining);
                }
            }
            _ => {}
        }
    }
    let mut remaining = history
        .iter()
        .map(count)
        .sum::<usize>()
        .saturating_sub(keep);
    for item in history {
        prune(item, &mut remaining);
    }
}
