use super::{
    provider::{ModelInput, Profile, Provider, Reply},
    store::{self, Budget, Repository, RequestRecord, Session},
};
use crate::{
    capabilities::{
        CapabilityRegistry, DeviceHandle, DeviceId, KeyAction, SwipeGesture, TextInput, TouchPoint,
    },
    core::{
        input_ownership::{self, Permit},
        AppContext, RunContext,
    },
    timer_core::{Clock, SystemClock},
};
use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSettings {
    pub endpoint: String,
    #[serde(default)]
    pub key: String,
    pub request_micros: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub version: String,
    #[serde(default)]
    pub billing: Option<super::billing::Billing>,
    pub profiles: Vec<Profile>,
    pub budget: Budget,
    #[serde(default)]
    pub search: Option<SearchSettings>,
    #[serde(default)]
    pub notification_channel: Option<String>,
    #[serde(default)]
    pub notify_results: bool,
    #[serde(default = "default_question_timeout")]
    pub question_timeout_secs: u64,
}
fn default_question_timeout() -> u64 {
    120
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            version: store::id(),
            billing: None,
            profiles: vec![],
            budget: Budget::default(),
            search: None,
            notification_channel: None,
            notify_results: true,
            question_timeout_secs: default_question_timeout(),
        }
    }
}
impl Settings {
    pub fn load(root: &std::path::Path) -> Result<Self> {
        match std::fs::read(root.join("private/settings.dat")) {
            Ok(bytes) => serde_json::from_slice(&crate::core::secrets::protect(&bytes, false)?)
                .context("AI 模型配置损坏"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self, root: &std::path::Path) -> Result<()> {
        crate::core::fs::atomic_write(
            &root.join("private/settings.dat"),
            &crate::core::secrets::protect(&serde_json::to_vec(self)?, true)?,
        )
    }
    pub fn public(&self) -> Value {
        json!({"version":self.version,"billing":self.billing.as_ref().map(super::billing::Billing::public),"profiles":self.profiles.iter().map(Profile::public).collect::<Vec<_>>(),"budget":self.budget,"search":self.search.as_ref().map(|s|json!({"endpoint":s.endpoint,"request_micros":s.request_micros,"has_key":!s.key.is_empty()})),"notification_channel":self.notification_channel,"notify_results":self.notify_results,"question_timeout_secs":self.question_timeout_secs})
    }
}
pub struct Capture {
    pub original: Vec<u8>,
    pub model: Vec<u8>,
    pub size: [u32; 2],
    pub model_size: [u32; 2],
    pub epoch: String,
}
#[async_trait]
pub trait Backend: Send + Sync {
    async fn capture(&self, app: &AppContext) -> Result<Capture>;
    async fn inject(&self, app: &AppContext, action: &Value, size: [u32; 2]) -> Result<()>;
    async fn release(&self, app: &AppContext, keys: &[String]) -> Result<()>;
    async fn automation(
        &self,
        context: &RunContext,
        target: &str,
        args: serde_json::Map<String, Value>,
        stop: Arc<AtomicBool>,
        scope: crate::core::side_effect::Scope,
    ) -> Result<()>;
}
pub struct NativeBackend {
    pub capabilities: CapabilityRegistry,
    pub devices: Arc<crate::device::DeviceManager>,
    pub yaml: Arc<crate::extensions::gamer_yaml::EngineExecutor>,
}
impl NativeBackend {
    fn validate_target(&self, app: &AppContext) -> Result<()> {
        let (device, _, _) = self
            .devices
            .snapshot(app.device_id.as_str())
            .context("设备不存在")?;
        ensure!(
            device.pkg.as_deref() == app.android_package.as_ref().map(|p| p.as_str()),
            "target_app_changed: Android 目标已改变，请停止并重新建立目标"
        );
        Ok(())
    }
}
#[async_trait]
impl Backend for NativeBackend {
    async fn capture(&self, app: &AppContext) -> Result<Capture> {
        self.validate_target(app)?;
        ensure!(
            !crate::targets::is_browser(app.device_id.as_str()),
            "AI V1 仅验证 Android 目标"
        );
        let device = DeviceHandle::new(DeviceId::new(app.device_id.as_str()));
        let frame = self
            .capabilities
            .frame()
            .context("Frame service unavailable")?;
        let session = self
            .devices
            .session(app.device_id.as_str())
            .context("设备未连接")?;
        let epoch = format!("{:p}", Arc::as_ptr(&session));
        let handle = frame.capture(&device).await?;
        let (original, size) = frame.png(handle, 0).await?;
        let (model, model_size) = frame.png(handle, 1280).await?;
        Ok(Capture {
            original,
            model,
            size: [size.width, size.height],
            model_size: [model_size.width, model_size.height],
            epoch,
        })
    }
    async fn inject(&self, app: &AppContext, action: &Value, size: [u32; 2]) -> Result<()> {
        self.validate_target(app)?;
        let device = DeviceHandle::new(DeviceId::new(app.device_id.as_str()));
        let input = self
            .capabilities
            .input()
            .context("Input service unavailable")?;
        let point = |name: &str| -> Result<TouchPoint> {
            let v = action[name].as_array().context("坐标必须为 [x,y]")?;
            ensure!(v.len() == 2, "坐标必须为 [x,y]");
            let x = v[0].as_f64().context("x 无效")?;
            let y = v[1].as_f64().context("y 无效")?;
            ensure!(
                (0.0..1.0).contains(&x) && (0.0..1.0).contains(&y),
                "坐标超出画面"
            );
            Ok(TouchPoint::new(
                (x * size[0] as f64) as u32,
                (y * size[1] as f64) as u32,
                1.0,
            ))
        };
        match action["kind"].as_str().context("操作 kind 缺失")? {
            "tap" => input.tap(&device, point("position")?).await?,
            "swipe" => {
                let ms = action["duration_ms"].as_u64().unwrap_or(300);
                ensure!((50..=2000).contains(&ms), "滑动时长超限");
                input
                    .swipe(
                        &device,
                        SwipeGesture::new(point("from")?, point("to")?, Duration::from_millis(ms)),
                    )
                    .await?;
            }
            "key" => {
                input
                    .key_named(
                        &device,
                        action["key"].as_str().context("key 缺失")?,
                        KeyAction::Press,
                    )
                    .await?
            }
            "text" => {
                let text = action["text"].as_str().context("text 缺失")?;
                ensure!(text.len() <= 1024, "文本超限");
                input.text(&device, TextInput::new(text)).await?;
            }
            _ => anyhow::bail!("未知输入动作"),
        }
        Ok(())
    }
    async fn release(&self, app: &AppContext, keys: &[String]) -> Result<()> {
        if let Some(session) = self.devices.session(app.device_id.as_str()) {
            session.release_inputs().await?;
        }
        let device = DeviceHandle::new(DeviceId::new(app.device_id.as_str()));
        if let Some(input) = self.capabilities.input() {
            for key in keys {
                input_ownership::cleanup(input.key_named(&device, key, KeyAction::Up)).await?;
            }
        }
        Ok(())
    }
    async fn automation(
        &self,
        context: &RunContext,
        target: &str,
        args: serde_json::Map<String, Value>,
        stop: Arc<AtomicBool>,
        scope: crate::core::side_effect::Scope,
    ) -> Result<()> {
        self.yaml
            .execute_scoped(context, target, args, stop, scope)
            .await
    }
}
#[async_trait]
pub trait Search: Send + Sync {
    async fn query(&self, settings: &SearchSettings, query: &str) -> Result<Value>;
}
pub struct HttpSearch;
#[async_trait]
impl Search for HttpSearch {
    async fn query(&self, settings: &SearchSettings, query: &str) -> Result<Value> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(30))
            .build()?;
        let mut request = http
            .post(&settings.endpoint)
            .json(&json!({"query":query,"max_results":5}));
        if !settings.key.is_empty() {
            request = request.bearer_auth(&settings.key);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| anyhow::anyhow!("搜索请求失败，费用待核对"))?;
        ensure!(
            response.status().is_success(),
            "搜索服务 HTTP {}",
            response.status().as_u16()
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(bytes.len() + chunk.len() <= 128 * 1024, "搜索结果过大");
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)?;
        ensure!(
            value["results"].is_array(),
            "搜索 API 必须返回 results 数组，元素含 url/title/content"
        );
        let mut results = Vec::new();
        for item in value["results"].as_array().unwrap().iter().take(5) {
            let url = item["url"].as_str().context("搜索来源缺失")?;
            let parsed = reqwest::Url::parse(url)?;
            ensure!(
                matches!(parsed.scheme(), "https" | "http"),
                "搜索来源 URL 无效"
            );
            let content = item["content"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(4000)
                .collect::<String>();
            results.push(json!({"url":url,"title":item["title"],"content":content,"retrieved_at":chrono::Utc::now()}));
        }
        Ok(json!({"results":results}))
    }
}
pub struct Runtime {
    pub repository: Arc<Repository>,
    pub packages: Arc<crate::resources::PackageStore>,
    pub provider: Arc<dyn Provider>,
    pub backend: Arc<dyn Backend>,
    pub search: Arc<dyn Search>,
    pub clock: Arc<dyn Clock>,
}
impl Runtime {
    pub fn native(
        repository: Arc<Repository>,
        packages: Arc<crate::resources::PackageStore>,
        backend: Arc<dyn Backend>,
    ) -> Result<Self> {
        Ok(Self {
            repository,
            packages,
            provider: Arc::new(super::provider::HttpProvider::new()?),
            backend,
            search: Arc::new(HttpSearch),
            clock: Arc::new(SystemClock),
        })
    }
    pub fn begin(
        &self,
        context: &RunContext,
        goal: &str,
        plan_version: &str,
        settings: &Settings,
        profile: &Profile,
        resume: Option<&str>,
    ) -> Result<String> {
        let package = context
            .app
            .content_package
            .as_ref()
            .context("Package Context 缺失")?
            .as_str();
        let generation = self.packages.instance_generation(package)?;
        let now = self.clock.now().timestamp();
        self.repository.transaction(|data| {
            if let Some(resume) = resume {
                let s = data.sessions.get_mut(resume).context("恢复会话不存在")?;
                ensure!(
                    s.package == package
                        && s.generation == generation
                        && s.app
                            == context
                                .app
                                .android_package
                                .as_ref()
                                .context("Android App Context 缺失")?
                                .as_str()
                        && s.goal == goal
                        && s.plan_version == plan_version
                        && s.profile_version == profile.version(),
                    "resume_scope_mismatch: 目标、模型、账号或 Package 实例已变化，旧账本保留"
                );
                ensure!(s.expires_at > now, "会话已过期");
                ensure!(
                    !s.questions.iter().any(store::Question::pending),
                    "waiting_user: 请先回答对话中的问题"
                );
                ensure!(
                    s.state != "running" && s.state != "completed",
                    "会话正在运行或已经完成"
                );
                s.runs.push(context.run_id.to_string());
                s.state = "running".into();
                s.device = context.device_id().as_str().into();
                s.execution_plan = Value::Null;
                s.active_checkpoint = Some(self.clock.now().timestamp_millis());
                return Ok(resume.into());
            }
            ensure!(
                data.sessions.len() < 1000,
                "AI 会话达到保留上限，请归档历史"
            );
            let key = store::id();
            // Private user-confirmed identity is a reference, not proof of the
            // current screen. Every extra consumption still passes identity_gate.
            // Android app/device scope is independent of the Package namespace.
            let account = data
                .sessions
                .values()
                .filter(|s| {
                    s.device == context.device_id().as_str()
                        && context
                            .app
                            .android_package
                            .as_ref()
                            .is_some_and(|app| app.as_str() == s.app)
                        && s.account.is_some()
                })
                .max_by_key(|s| s.events.last().map_or(0, |e| e.at))
                .and_then(|s| s.account.clone());
            data.sessions.insert(
                key.clone(),
                Session {
                    device: context.device_id().as_str().into(),
                    entrypoint: String::new(),
                    id: key.clone(),
                    package: package.into(),
                    generation,
                    app: context
                        .app
                        .android_package
                        .as_ref()
                        .context("Android App Context 缺失")?
                        .to_string(),
                    goal: goal.into(),
                    plan_version: plan_version.into(),
                    profile_version: profile.version(),
                    account_reference_only: account.is_some(),
                    account,
                    cycle: None,
                    runs: vec![context.run_id.to_string()],
                    state: "running".into(),
                    progress: json!([]),
                    budget: settings.budget.clone(),
                    rounds: 0,
                    searches: 0,
                    active_ms: 0,
                    active_checkpoint: Some(self.clock.now().timestamp_millis()),
                    tokens: 0,
                    expires_at: now + 7 * 86400,
                    events: vec![],
                    questions: vec![],
                    conversation_revision: 0,
                    execution_plan: Value::Null,
                    trial_mode: false,
                    trial_operations: 0,
                    auto_resumes: 0,
                    operations: Default::default(),
                    notified: Default::default(),
                },
            );
            Ok(key)
        })
    }
}
#[derive(Clone)]
pub struct Observation {
    pub id: String,
    pub run: String,
    pub at: i64,
    pub size: [u32; 2],
    pub model_size: [u32; 2],
    pub epoch: String,
    pub hash: String,
    pub image: Vec<u8>,
}
#[derive(Clone)]
pub struct Execution {
    pub runtime: Arc<Runtime>,
    pub context: RunContext,
    pub session: String,
    pub profile: Profile,
    pub settings: Settings,
    pub stop: Arc<AtomicBool>,
    pub permit: Permit,
    pub observation: Arc<parking_lot::Mutex<Option<Observation>>>,
    pub keys: Arc<parking_lot::Mutex<Vec<String>>>,
}
impl Execution {
    pub fn check(&self) -> Result<()> {
        let credential_valid = {
            let data = self.runtime.repository.data.lock();
            data.mcp_sessions.get(&self.session).is_none_or(|id| {
                data.credentials.get(id).is_some_and(|c| {
                    !c.revoked && c.expires_at > self.runtime.clock.now().timestamp()
                })
            })
        };
        if !credential_valid {
            self.stop.store(true, Ordering::Release);
            anyhow::bail!("CANCELLED: MCP 凭据已撤销或过期");
        }
        ensure!(
            !self.stop.load(Ordering::Acquire) && input_ownership::current(&self.permit),
            "CANCELLED: 运行已取消或人工接管"
        );
        self.runtime.repository.checkpoint(
            &self.session,
            self.runtime.clock.now().timestamp_millis(),
            false,
        )?;
        let s = self
            .runtime
            .repository
            .data
            .lock()
            .sessions
            .get(&self.session)
            .cloned()
            .context("会话不存在")?;
        ensure!(
            s.active_ms < s.budget.max_active_secs * 1000,
            "budget_exhausted: 执行时长已达上限"
        );
        ensure!(
            self.runtime.packages.instance_generation(&s.package)? == s.generation,
            "package_instance_changed: 需要重新建立目标上下文"
        );
        Ok(())
    }
    pub fn event(&self, kind: &str, data: Value) -> Result<()> {
        self.runtime.repository.event(
            &self.session,
            self.context.run_id.as_str(),
            self.runtime.clock.now().timestamp_millis(),
            kind,
            data,
        )
    }
    pub async fn observe(&self) -> Result<Observation> {
        self.check()?;
        let capture = self.runtime.backend.capture(&self.context.app).await?;
        self.check()?;
        ensure!(
            capture.original.len() <= 16 * 1024 * 1024
                && capture.size[0] > 0
                && capture.size[1] > 0,
            "截图超限或尺寸无效"
        );
        let o = Observation {
            id: store::id(),
            run: self.context.run_id.to_string(),
            at: self.runtime.clock.now().timestamp_millis(),
            size: capture.size,
            model_size: capture.model_size,
            epoch: capture.epoch,
            hash: store::hash(&capture.original),
            image: capture.model,
        };
        let dir = self
            .runtime
            .repository
            .root
            .join("evidence")
            .join(&self.session);
        std::fs::create_dir_all(&dir)?;
        ensure!(
            std::fs::read_dir(&dir)?.count() < 200,
            "截图证据达到会话保留上限"
        );
        crate::core::fs::atomic_write(&dir.join(format!("{}.png", o.id)), &capture.original)?;
        self.event("observation",json!({"observation_id":o.id,"size":o.size,"model_size":o.model_size,"at":o.at,"hash":o.hash,"epoch":o.epoch}))?;
        *self.observation.lock() = Some(o.clone());
        Ok(o)
    }
    pub async fn valid_observation(&self, id: &str) -> Result<Observation> {
        self.check()?;
        let o = self
            .observation
            .lock()
            .clone()
            .context("observe_required: 需要新画面")?;
        ensure!(
            o.id == id
                && o.run == self.context.run_id.as_str()
                && self.runtime.clock.now().timestamp_millis() - o.at <= 15_000,
            "observation_expired: 请重新观察"
        );
        let capture = self.runtime.backend.capture(&self.context.app).await?;
        ensure!(
            o.size == capture.size && o.epoch == capture.epoch,
            "observation_target_changed: 设备会话或画面方向已变化"
        );
        self.check()?;
        Ok(o)
    }
    pub async fn cancelled(&self) {
        while !self.stop.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
    fn reserve(&self, kind: &str, amount: u64) -> Result<String> {
        let now = self.runtime.clock.now();
        self.runtime.repository.reserve(
            RequestRecord {
                id: store::id(),
                session: self.session.clone(),
                run_id: self.context.run_id.to_string(),
                profile: self.profile.id.clone(),
                model: self.profile.model.clone(),
                price_version: self.profile.price_version.clone(),
                kind: kind.into(),
                day: now.format("%Y-%m-%d").to_string(),
                reserved: amount,
                actual: None,
                source: "unknown".into(),
                status: "reserved".into(),
                usage: Value::Null,
                at: now.timestamp_millis(),
                duration_ms: 0,
            },
            now.timestamp(),
        )
    }
    pub async fn infer(
        &self,
        prompt: String,
        observation: &Observation,
        kind: &str,
    ) -> Result<Reply> {
        self.check()?;
        ensure!(
            self.profile.protocol == "ollama"
                || self.profile.has_prices()
                || self.settings.budget.request_micros > 0,
            "未配置价格时，API 每次请求费用预留必须大于零"
        );
        ensure!(
            self.profile.vision == "available",
            "multimodal_unverified: 请先完成图片连接测试"
        );
        ensure!(prompt.len() <= 64 * 1024, "模型上下文超限");
        let mut effective = self.profile.clone();
        {
            let data = self.runtime.repository.data.lock();
            let session = &data.sessions[&self.session];
            if session.searches >= session.budget.max_searches {
                effective.native_search_enabled = false;
            }
        }
        let price_bound = super::provider::reserve_price(&self.profile, prompt.len());
        let reserve = if self.profile.protocol == "ollama" {
            0
        } else {
            price_bound
                .max(self.settings.budget.request_micros)
                .saturating_add(if effective.native_search_enabled {
                    self.profile.native_search_reserve_micros
                } else {
                    0
                })
        };
        let id = self.reserve(
            if effective.native_search_enabled {
                "search"
            } else {
                kind
            },
            reserve,
        )?;
        let conversation_revision = serde_json::from_str::<Value>(&prompt)
            .ok()
            .and_then(|v| v["conversation_revision"].as_u64())
            .unwrap_or_else(|| {
                self.runtime.repository.data.lock().sessions[&self.session].conversation_revision
            });
        self.event(
            "thinking",
            json!({"kind":kind,"summary":match kind {
                "verification" => "正在用新画面核对目标是否完成",
                "mcp_gate" | "yaml_gate" | "identity_gate" => "正在核对操作、账号与消耗",
                _ => "正在结合当前画面、你的要求与已有经验判断下一步",
            }}),
        )?;
        let started = std::time::Instant::now();
        let request = self.runtime.provider.infer(
            &effective,
            ModelInput {
                prompt,
                image: observation.image.clone(),
            },
        );
        let reply = tokio::select! {r=request=>r,_=self.cancelled()=>Err(anyhow::anyhow!("CANCELLED: 等待模型期间取消，预留费用待核对"))};
        match reply {
            Ok(reply) => {
                self.runtime.repository.settle(
                    &id,
                    reply.cost,
                    reply.usage.clone(),
                    &reply.source,
                    started.elapsed().as_millis() as u64,
                )?;
                self.check()?;
                ensure!(
                    reply.cost.is_some_and(|cost| cost <= reserve),
                    "budget_cost_unknown: 费用未知或超过请求预留，保存进度后对账"
                );
                ensure!(
                    self.runtime.repository.data.lock().sessions[&self.session]
                        .conversation_revision
                        == conversation_revision,
                    "conversation_changed: 已收到用户补充，旧决策丢弃并重新观察"
                );
                self.event("decision",json!({"request_id":id,"tool":reply.decision.tool,"summary":reply.decision.summary,"sources":reply.sources}))?;
                Ok(reply)
            }
            Err(error) => {
                self.runtime.repository.settle(
                    &id,
                    None,
                    Value::Null,
                    "unknown",
                    started.elapsed().as_millis() as u64,
                )?;
                Err(error)
            }
        }
    }
    pub async fn search(&self, query: &str) -> Result<Value> {
        self.check()?;
        ensure!(
            !query.trim().is_empty() && query.len() <= 500,
            "搜索查询无效"
        );
        let settings = self
            .settings
            .search
            .as_ref()
            .context("search_unavailable: 未配置外部搜索，继续画面探索")?;
        let id = self.reserve("search", settings.request_micros)?;
        let started = std::time::Instant::now();
        let reply = tokio::select! {r=self.runtime.search.query(settings,query)=>r,_=self.cancelled()=>Err(anyhow::anyhow!("CANCELLED: 搜索取消"))};
        match reply {
            Ok(value) => {
                self.runtime.repository.settle(
                    &id,
                    Some(settings.request_micros),
                    json!({"total_tokens":0}),
                    "estimated",
                    started.elapsed().as_millis() as u64,
                )?;
                self.check()?;
                self.event("search", json!({"query":query,"result":value}))?;
                Ok(value)
            }
            Err(error) => {
                self.runtime.repository.settle(
                    &id,
                    None,
                    Value::Null,
                    "unknown",
                    started.elapsed().as_millis() as u64,
                )?;
                Err(error)
            }
        }
    }
    pub async fn run(&self) -> Result<()> {
        let result = self.run_loop().await;
        let error = result.as_ref().err().map(ToString::to_string);
        let cancelled = self.stop.load(Ordering::Acquire);
        let waiting = {
            let data = self.runtime.repository.data.lock();
            data.approvals
                .values()
                .any(|a| a.session == self.session && a.status == "pending")
                || data.sessions[&self.session]
                    .questions
                    .iter()
                    .any(store::Question::pending)
        };
        let state = if cancelled {
            "cancelled"
        } else if result.is_ok() {
            "completed"
        } else if waiting {
            "waiting_user"
        } else {
            "partial"
        };
        self.runtime.repository.checkpoint(
            &self.session,
            self.runtime.clock.now().timestamp_millis(),
            true,
        )?;
        self.runtime.repository.transaction(|data| {
            let s = data.sessions.get_mut(&self.session).unwrap();
            s.state = state.into();
            Ok(())
        })?;
        if error.as_ref().is_some_and(|s| {
            s.starts_with("no_progress")
                || s.starts_with("partial")
                || s.starts_with("trial_boundary")
        }) {
            let s = self.runtime.repository.data.lock().sessions[&self.session].clone();
            self.runtime.repository.transaction(|d|{d.failures.push(json!({"app":s.app,"goal":s.goal,"reason":error,"conditions":"本次目标与画面；先重新观察，不能凭此判断延迟或攻略错误","observation_id":self.observation.lock().as_ref().map(|o|o.id.clone()),"at":self.runtime.clock.now()}));Ok(())})?;
        }
        self.event("terminal",json!({"state":state,"error":error,"usage":self.runtime.repository.totals(&self.session)}))?;
        let keys = self.keys.lock().clone();
        let release = self.runtime.backend.release(&self.context.app, &keys).await;
        release?;
        self.keys.lock().clear();
        input_ownership::cleaned(&self.permit);
        result
    }
    async fn run_loop(&self) -> Result<()> {
        let mut last_result = Value::Null;
        let mut previous = String::new();
        let mut repeated = 0;
        let mut blocked = 0;
        loop {
            self.check()?;
            let s = self.runtime.repository.data.lock().sessions[&self.session].clone();
            let o = self.observe().await?;
            let memories = super::tools::memory_search(self, "", 5)?;
            ensure!(
                !s.questions.iter().any(store::Question::pending),
                "waiting_user: 等待用户回答，设备已释放"
            );
            let prompt=json!({"goal":s.goal,"progress":s.progress,"execution_plan":s.execution_plan,"conversation_revision":s.conversation_revision,"trial_policy":{"active":s.trial_mode,"operations_used":s.trial_operations,"auto_resumes_used":s.auto_resumes,"limit":s.budget.max_trials,"instruction":"超时只允许逐步可恢复导航或明确的常规行动资源；不执行整段自动化、不消耗额外资源、不代替账号或授权选择。到边界保存进度并 finish。"},"account_confirmed":s.account,"account_reference_only":s.account_reference_only,"cycle_confirmed":s.cycle,"observation_id":o.id,"original_size":o.size,"model_size":o.model_size,"coordinate_mapping":"normalized coordinates refer to the original frame","tools":super::tools::catalog(),"relevant_memory_untrusted":memories,"last_tool_result":last_result,"recent_host_events":s.events.iter().rev().take(8).collect::<Vec<_>>(),"user_messages":s.events.iter().filter(|e| e.kind == "user_message").rev().take(20).collect::<Vec<_>>(),"user_questions":s.questions,"pending_approvals":self.runtime.repository.data.lock().approvals.values().filter(|p|p.session==self.session).collect::<Vec<_>>()}).to_string();
            let reply = match self.infer(prompt, &o, "decision").await {
                Err(error) if error.to_string().starts_with("conversation_changed") => {
                    last_result = json!({"instruction":"用户要求已更新，已丢弃旧决策"});
                    continue;
                }
                result => result?,
            };
            let mut semantic = reply.decision.arguments.clone();
            if let Some(a) = semantic.as_object_mut() {
                a.remove("observation_id");
                a.remove("operation_id");
            }
            let fingerprint = store::hash(&serde_json::to_vec(&(
                reply.decision.tool.clone(),
                semantic,
                o.hash.clone(),
            ))?);
            if fingerprint == previous {
                repeated += 1
            } else {
                repeated = 0;
                previous = fingerprint;
            }
            ensure!(
                repeated < 3,
                "no_progress: 相同画面与动作重复，已保存失败经验"
            );
            match super::tools::execute(self, &reply.decision.tool, reply.decision.arguments).await
            {
                Ok(value) if value["waiting_user"] == true => {
                    anyhow::bail!("waiting_user: 已保存问题和进度，等待用户回答");
                }
                Ok(value) if value["completed"] == true => {
                    ensure!(
                        !self
                            .runtime
                            .repository
                            .data
                            .lock()
                            .approvals
                            .values()
                            .any(|p| p.session == self.session && p.status == "pending"),
                        "waiting_user: 部分目标仍有待处理授权"
                    );
                    return Ok(());
                }
                Ok(value) => {
                    if value["blocked"] == true {
                        blocked += 1;
                    }
                    last_result = value;
                    self.event(
                        "tool_result",
                        json!({"tool":reply.decision.tool,"result":last_result}),
                    )?;
                }
                Err(error) => {
                    let message = error.to_string();
                    if message.starts_with("CANCELLED")
                        || message.contains("budget_")
                        || message.contains("trial_boundary")
                    {
                        return Err(error);
                    }
                    last_result = json!({"error":message});
                    self.event("tool_error", last_result.clone())?;
                    blocked += 1;
                }
            }
            ensure!(
                blocked < 8,
                "partial: 已完成允许部分，剩余步骤受阻，待办及进度已保存"
            );
        }
    }
}
