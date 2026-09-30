//! AI business belongs to the plugin. Core only owns runners, leases and input arbitration.
pub(crate) mod billing;
pub(crate) mod mcp;
pub(crate) mod provider;
pub(crate) mod runtime;
pub(crate) mod store;
#[cfg(test)]
mod tests;
pub(crate) mod tools;
use crate::{
    core::{ActivityLease, RunContext, RunPayload, RunRequest},
    extensions::{
        service::BuiltinService, ExtensionError, ExtensionId, ExtensionResult, ExtensionService,
        Permission,
    },
    run_manager::{RunExecutor, RunManager, RunOutcome, RunSource, StartRequest},
    timer_core::{TimerCompletion, TimerOutcome, TimerRun, TimerRunner, TimerRunnerError},
};
use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use futures_util::future::BoxFuture;
use parking_lot::Mutex;
use runtime::{Execution, Runtime, Settings};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
};
pub const ID: &str = "gamer-ai";
pub const ACTIONS: &[&str] = &[
    "settings.read",
    "settings.save",
    "settings.test",
    "plans.read",
    "plans.save",
    "plans.delete",
    "sessions.read",
    "sessions.events",
    "sessions.evidence",
    "sessions.resume",
    "sessions.budget",
    "sessions.cancel",
    "input.takeover",
    "identity.confirm",
    "memory.search",
    "memory.read",
    "memory.update",
    "memory.delete",
    "approvals.read",
    "approvals.resolve",
    "approvals.revoke",
    "usage.read",
    "usage.reconcile",
    "usage.sync",
    "credentials.issue",
    "credentials.read",
    "credentials.revoke",
];
pub fn accepts(id: &str, action: &str) -> bool {
    id == ID && ACTIONS.contains(&action)
}
pub fn permissions(id: &str, action: &str) -> Option<&'static [Permission]> {
    if !accepts(id, action) {
        return None;
    }
    Some(match action {
        "settings.test" => &[Permission::AiInfer],
        "input.takeover" | "sessions.cancel" => &[Permission::DeviceRead],
        _ => &[Permission::ResourceRead],
    })
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Goal {
    #[serde(default)]
    pub goal: String,
    #[serde(default)]
    pub model_profile_id: String,
    #[serde(default)]
    pub resume_session_id: Option<String>,
}
#[derive(Clone)]
struct Prepared {
    goal: Goal,
    settings: Settings,
    profile: provider::Profile,
    plan_version: String,
    external: Option<String>,
    task_id: Option<String>,
    credential_id: Option<String>,
}
pub struct AiService {
    pub runtime: Arc<Runtime>,
    pub runs: Arc<RunManager>,
    pub db: crate::store::Db,
    pub devices: Arc<crate::device::DeviceManager>,
    settings: Mutex<Settings>,
    extensions: std::sync::RwLock<Weak<ExtensionService>>,
    prepared: Arc<Mutex<HashMap<String, Prepared>>>,
    pub external: Arc<Mutex<HashMap<String, mcp::External>>>,
}
impl AiService {
    pub fn new(
        data: &std::path::Path,
        packages: Arc<crate::resources::PackageStore>,
        capabilities: crate::capabilities::CapabilityRegistry,
        devices: Arc<crate::device::DeviceManager>,
        yaml: Arc<crate::extensions::gamer_yaml::EngineExecutor>,
        runs: Arc<RunManager>,
        db: crate::store::Db,
    ) -> Result<Self> {
        let root = data.join("extension-data").join(ID);
        let repository = Arc::new(store::Repository::open(&root)?);
        let settings = Settings::load(&root)?;
        let backend = Arc::new(runtime::NativeBackend {
            capabilities,
            devices: devices.clone(),
            yaml,
        });
        let runtime = Arc::new(Runtime::native(repository, packages, backend)?);
        Ok(Self {
            runtime,
            runs,
            db,
            devices,
            settings: Mutex::new(settings),
            extensions: std::sync::RwLock::new(Weak::new()),
            prepared: Arc::new(Mutex::new(HashMap::new())),
            external: Arc::new(Mutex::new(HashMap::new())),
        })
    }
    pub fn attach(&self, extensions: &Arc<ExtensionService>) {
        *self.extensions.write().unwrap() = Arc::downgrade(extensions);
    }
    fn extensions(&self) -> Result<Arc<ExtensionService>> {
        self.extensions
            .read()
            .unwrap()
            .upgrade()
            .context("AI 插件服务未装配")
    }
    pub async fn live(&self) -> Result<()> {
        let id = ExtensionId::parse(ID)?;
        let extensions = self.extensions()?;
        extensions.plugin_call_context(&id).await?;
        let snapshot = extensions.snapshot_for(&id)?;
        for permission in [
            Permission::DeviceRead,
            Permission::DeviceApp,
            Permission::InputTap,
            Permission::InputSwipe,
            Permission::InputKey,
            Permission::InputText,
            Permission::ResourceRead,
            Permission::RuntimeSleep,
            Permission::LogWrite,
            Permission::AiInfer,
            Permission::AiSearch,
        ] {
            ensure!(
                snapshot.manifest().permissions().allows(permission),
                "AI Runner 所需权限未声明: {}",
                permission.as_str()
            );
        }
        Ok(())
    }
    pub fn settings(&self) -> Settings {
        self.settings.lock().clone()
    }
    fn resolve_goal(&self, request: &RunRequest) -> Result<(Goal, String)> {
        let mut goal: Goal = serde_json::from_value(request.payload.as_value().clone())?;
        let package = request
            .app
            .content_package
            .as_ref()
            .context("Package Context 缺失")?
            .as_str();
        let plan_version = if request.entrypoint == format!("{package}#goal") {
            store::hash(goal.goal.as_bytes())
        } else {
            let (pkg, path) = request
                .entrypoint
                .split_once('/')
                .context("AI entrypoint 必须是 <package>#goal 或 <package>/<plan>.json")?;
            ensure!(
                pkg == package && path.ends_with(".json"),
                "AI 方案与 Package 不一致"
            );
            crate::resources::sanitize_rel_path(path)?;
            let entry = self
                .runtime
                .packages
                .read_text(package, ID, &format!("plans/{path}"))?
                .context("AI 方案不存在")?;
            let plan: Goal = serde_json::from_str(&entry.content)?;
            ensure!(plan.resume_session_id.is_none(), "方案不能存储恢复会话");
            if goal.goal.is_empty() {
                goal.goal = plan.goal;
            }
            if goal.model_profile_id.is_empty() {
                goal.model_profile_id = plan.model_profile_id;
            }
            entry.version()
        };
        ensure!(
            !goal.goal.trim().is_empty() && goal.goal.len() <= 4000,
            "自然语言目标必填，最多 4000 字节"
        );
        Ok((goal, plan_version))
    }
    fn prepare_request(
        &self,
        mut request: RunRequest,
        task_id: Option<String>,
        external: Option<String>,
        credential_id: Option<String>,
    ) -> Result<RunRequest> {
        let (mut goal, plan_version) = self.resolve_goal(&request)?;
        let settings = self.settings();
        settings.budget.validate()?;
        let profile = settings
            .profiles
            .iter()
            .find(|p| p.id == goal.model_profile_id)
            .or_else(|| {
                if goal.model_profile_id.is_empty() {
                    settings.profiles.first()
                } else {
                    None
                }
            })
            .context("请配置并选择模型")?
            .clone();
        profile.validate()?;
        ensure!(
            profile.vision == "available" || external.is_some(),
            "请先测试多模态模型连接"
        );
        goal.model_profile_id = profile.id.clone();
        if let (Some(owner), Some(session)) = (&credential_id, &goal.resume_session_id) {
            ensure!(
                self.runtime
                    .repository
                    .data
                    .lock()
                    .mcp_sessions
                    .get(session)
                    == Some(owner),
                "MCP logical session belongs to another credential"
            );
        }
        // Reuse a pending logical session only when its confirmed cycle and full scope match.
        if task_id.is_some() && goal.resume_session_id.is_none() {
            let data = self.runtime.repository.data.lock();
            goal.resume_session_id = data
                .sessions
                .values()
                .rev()
                .find(|s| {
                    s.goal == goal.goal
                        && !data.mcp_sessions.contains_key(&s.id)
                        && request
                            .app
                            .android_package
                            .as_ref()
                            .is_some_and(|a| a.as_str() == s.app)
                        && Some(s.package.as_str())
                            == request.app.content_package.as_ref().map(|p| p.as_str())
                        && s.state == "waiting_user"
                        && s.cycle.is_some()
                        && s.account.is_some()
                        && s.plan_version == plan_version
                        && s.profile_version == profile.version()
                        && s.expires_at > self.runtime.clock.now().timestamp()
                })
                .map(|s| s.id.clone());
        }
        let key = store::id();
        self.prepared.lock().insert(
            key.clone(),
            Prepared {
                goal,
                settings,
                profile,
                plan_version,
                external,
                task_id,
                credential_id,
            },
        );
        request.payload = RunPayload::new(json!({"prepared_id":key}));
        Ok(request)
    }
    pub async fn submit(
        &self,
        request: RunRequest,
        task: &str,
        at: Option<i64>,
        complete: Arc<dyn Fn(TimerCompletion) + Send + Sync>,
    ) -> Result<TimerRun, TimerRunnerError> {
        self.submit_owned(request, task, at, complete, None).await
    }
    async fn submit_owned(
        &self,
        request: RunRequest,
        task: &str,
        at: Option<i64>,
        complete: Arc<dyn Fn(TimerCompletion) + Send + Sync>,
        credential: Option<String>,
    ) -> Result<TimerRun, TimerRunnerError> {
        self.live()
            .await
            .map_err(|e| TimerRunnerError::DependencyMissing(e.to_string()))?;
        let request = self
            .prepare_request(
                request,
                (!task.is_empty()).then(|| task.into()),
                None,
                credential,
            )
            .map_err(|e| TimerRunnerError::Invalid(e.to_string()))?;
        let key = request.payload.as_value()["prepared_id"]
            .as_str()
            .unwrap()
            .to_string();
        let task_id = task.to_string();
        let prepared_cleanup = self.prepared.clone();
        let prepared_key = key.clone();
        let hook = Arc::new(
            move |record: &crate::run_manager::RunRecord, outcome: &RunOutcome| {
                prepared_cleanup.lock().remove(&prepared_key);
                complete(TimerCompletion {
                    task_id: task_id.clone(),
                    scheduled_at: at,
                    run_id: record.run_id.clone(),
                    outcome: match outcome {
                        RunOutcome::Success(_) => TimerOutcome::Success,
                        RunOutcome::Failed(error, _) => TimerOutcome::Failed(error.clone()),
                        RunOutcome::Cancelled(_) => TimerOutcome::Cancelled,
                    },
                });
            },
        );
        match self.runs.submit(
            StartRequest {
                request,
                source: if task.is_empty() {
                    RunSource::Manual
                } else if at.is_some() {
                    RunSource::Scheduled
                } else {
                    RunSource::TaskNow
                },
                task_id: (!task.is_empty()).then(|| task.into()),
                scheduled_at: at,
                realtime_logs: true,
            },
            Some(hook),
        ) {
            Ok(record) => Ok(TimerRun::new(record.run_id)),
            Err(error) => {
                self.prepared.lock().remove(&key);
                Err(match error {
                    crate::run_manager::StartError::Conflict(record) => {
                        TimerRunnerError::Conflict(record)
                    }
                    crate::run_manager::StartError::ShuttingDown => TimerRunnerError::ShuttingDown,
                })
            }
        }
    }
    async fn notify(&self, e: &Execution, task: Option<&str>, error: Option<&str>) -> Result<()> {
        let state = if e.stop.load(Ordering::Acquire) {
            "cancelled"
        } else if error.is_some() {
            "failed"
        } else {
            "success"
        };
        let extensions = self.extensions()?;
        let caller = extensions
            .plugin_call_context(&ExtensionId::parse(ID)?)
            .await?;
        let terminal_owned = if let Some(task) = task {
            let t = self.db.get_timer_task_async(task).await?;
            t.is_some_and(|t| crate::extensions::notify::task::owns_result(&t, state))
        } else {
            false
        };
        let s = e.runtime.repository.data.lock().sessions[&e.session].clone();
        let pending: Vec<_> = e
            .runtime
            .repository
            .data
            .lock()
            .approvals
            .values()
            .filter(|a| a.session == e.session && a.status == "pending")
            .cloned()
            .collect();
        let mut notifications = Vec::new();
        if !pending.is_empty() {
            let key = format!(
                "approval:{}",
                store::hash(&serde_json::to_vec(
                    &pending
                        .iter()
                        .map(|a| (
                            &s.account,
                            &s.cycle,
                            &s.goal,
                            &a.consumption.category,
                            &a.consumption.resource,
                            &a.consumption.quantity,
                            &a.consumption.purpose
                        ))
                        .collect::<Vec<_>>()
                )?)
            );
            notifications.push((
                key,
                "需要授权".to_string(),
                format!(
                    "{}\n已完成：{}\n待处理：{}",
                    s.goal,
                    s.progress,
                    pending
                        .iter()
                        .map(|a| format!("{}（待办 {}）", a.consumption.resource, a.id))
                        .collect::<Vec<_>>()
                        .join("；")
                ),
            ));
        }
        if error.is_some_and(|e| e.contains("budget_")) {
            notifications.push((
                "budget_limit".into(),
                "AI 预算达到上限".into(),
                format!("{}\n{}", s.goal, error.unwrap()),
            ));
        }
        if e.settings.notify_results
            && !terminal_owned
            && pending.is_empty()
            && !error.is_some_and(|e| e.contains("budget_"))
        {
            notifications.push((
                format!("terminal:{}:{state}", e.context.run_id),
                format!("AI 目标：{state}"),
                format!(
                    "{}\n进度：{}\n{}",
                    s.goal,
                    s.progress,
                    error.unwrap_or("新画面验证通过")
                ),
            ));
        }
        for (key, title, content) in notifications {
            let send = e.runtime.repository.transaction(|data| {
                let s = data.sessions.get_mut(&e.session).unwrap();
                if s.notified.contains_key(&key) {
                    return Ok(false);
                }
                s.notified
                    .insert(key.clone(), json!({"status":"pending","sender":"ai"}));
                Ok(true)
            })?;
            if !send {
                continue;
            }
            let result=extensions.call_extension_from_plugin(&caller,&ExtensionId::parse(crate::extensions::notify::ID)?,crate::extensions::notify::SEND,json!({"channel":e.settings.notification_channel,"title":title,"content":content.chars().take(500).collect::<String>(),"source":"ai","source_id":e.session})).await;
            let value = match result {
                Ok(value) => value,
                Err(_) => {
                    json!({"status":"unavailable","message":"通知插件未安装、未启用或未配置；待办仍保留"})
                }
            };
            e.runtime.repository.transaction(|data| {
                data.sessions
                    .get_mut(&e.session)
                    .unwrap()
                    .notified
                    .insert(key.clone(), value.clone());
                Ok(())
            })?;
            e.event("notification", json!({"key":key,"result":value}))?;
        }
        Ok(())
    }
    async fn dispatch(&self, action: &str, values: Value) -> Result<Value> {
        let repo = &self.runtime.repository;
        match action {
            "settings.read" => Ok(self.settings().public()),
            "settings.save" => {
                let mut saved = self.settings.lock();
                ensure!(
                    values["expected_version"] == saved.version,
                    "配置已修改，请刷新"
                );
                let mut next: Settings = serde_json::from_value(values["settings"].clone())?;
                next.budget.validate()?;
                let mut seen = std::collections::HashSet::new();
                for p in &mut next.profiles {
                    ensure!(seen.insert(p.id.clone()), "模型 ID 重复");
                    if let Some(old) = saved.profiles.iter().find(|o| o.id == p.id) {
                        if p.key.is_empty() {
                            p.key = old.key.clone();
                        }
                        p.vision = if p.endpoint == old.endpoint
                            && p.model == old.model
                            && p.protocol == old.protocol
                            && p.key == old.key
                        {
                            old.vision.clone()
                        } else {
                            "untested".into()
                        };
                        p.native_search = if p.vision == "available" {
                            old.native_search.clone()
                        } else {
                            "untested".into()
                        };
                    } else {
                        p.vision = "untested".into();
                        p.native_search = "untested".into();
                    }
                    p.validate()?;
                }
                if let Some(search) = &mut next.search {
                    let url = reqwest::Url::parse(&search.endpoint)?;
                    ensure!(
                        matches!(url.scheme(), "http" | "https")
                            && url.username().is_empty()
                            && url.password().is_none()
                            && url.query().is_none(),
                        "搜索服务地址无效"
                    );
                    if search.key.is_empty() {
                        if let Some(old) = &saved.search {
                            if old.endpoint == search.endpoint {
                                search.key = old.key.clone();
                            }
                        }
                    }
                }
                if let Some(billing) = &mut next.billing {
                    billing.validate()?;
                    if billing.key.is_empty() {
                        if let Some(old) = &saved.billing {
                            if old.endpoint == billing.endpoint
                                && old.protocol == billing.protocol
                                && old.scope == billing.scope
                            {
                                billing.key = old.key.clone();
                            }
                        }
                    }
                }
                next.version = store::id();
                next.save(&repo.root)?;
                *saved = next;
                Ok(saved.public())
            }
            "settings.test" => self.test_profile(&values).await,
            "sessions.read" => {
                let data = repo.data.lock();
                if let Some(id) = values["session_id"].as_str() {
                    let s = data.sessions.get(id).context("会话不存在")?;
                    let mut v = serde_json::to_value(s)?;
                    v["events"] = Value::Null;
                    Ok(v)
                } else {
                    Ok(
                        json!({"sessions":data.sessions.values().rev().take(100).map(|s|json!({"id":s.id,"goal":s.goal,"state":s.state,"app":s.app,"package":s.package,"runs":s.runs,"progress":s.progress,"rounds":s.rounds,"account":s.account,"cycle":s.cycle})).collect::<Vec<_>>()}),
                    )
                }
            }
            "sessions.events" => {
                let data = repo.data.lock();
                let s = data
                    .sessions
                    .get(text(&values, "session_id")?)
                    .context("会话不存在")?;
                let cursor = values["after"].as_u64().unwrap_or(0);
                Ok(
                    json!({"events":s.events.iter().filter(|e|e.seq>cursor).take(100).collect::<Vec<_>>(),"state":s.state}),
                )
            }
            "sessions.evidence" => {
                use base64::Engine;
                let session = text(&values, "session_id")?;
                let observation = text(&values, "observation_id")?;
                uuid::Uuid::parse_str(session)?;
                uuid::Uuid::parse_str(observation)?;
                let data = repo.data.lock();
                let s = data.sessions.get(session).context("会话不存在")?;
                ensure!(s.events.iter().any(|e|e.kind=="observation"&&e.data["observation_id"]==observation),"证据不属于该会话");
                let bytes = std::fs::read(
                    repo.root
                        .join("evidence")
                        .join(session)
                        .join(format!("{observation}.png")),
                )?;
                ensure!(bytes.len() <= 16 * 1024 * 1024, "证据过大");
                Ok(json!({"png_b64":base64::engine::general_purpose::STANDARD.encode(bytes)}))
            }
            "sessions.cancel" => {
                let s = repo
                    .data
                    .lock()
                    .sessions
                    .get(text(&values, "session_id")?)
                    .cloned()
                    .context("会话不存在")?;
                for run in &s.runs {
                    self.runs.cancel(run);
                }
                Ok(json!({"accepted":true}))
            }
            "input.takeover" => {
                let device = text(&values, "device_id")?;
                if let Some(run) = self.runs.active_for_device(device) {
                    self.runs.cancel(&run.run_id);
                }
                crate::core::input_ownership::takeover(device).await?;
                Ok(json!({"manual_input":true}))
            }
            "sessions.budget" => {
                let budget = self.settings().budget;
                budget.validate()?;
                repo.transaction(|data| {
                    let session = data
                        .sessions
                        .get_mut(text(&values, "session_id")?)
                        .context("会话不存在")?;
                    ensure!(session.state != "running", "先停止会话再调整预算");
                    session.budget = budget.clone();
                    Ok(())
                })?;
                Ok(json!({"budget":budget,"usage_reset":false}))
            }
            "sessions.resume" => {
                let s = repo
                    .data
                    .lock()
                    .sessions
                    .get(text(&values, "session_id")?)
                    .cloned()
                    .context("会话不存在")?;
                let device = text(&values, "device_id")?;
                let settings = self.settings();
                let selected = values["model_profile_id"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        settings
                            .profiles
                            .iter()
                            .find(|p| p.version() == s.profile_version)
                            .map(|p| p.id.clone())
                    })
                    .context("原模型配置已改变，请选择匹配版本或新建目标")?;
                let request = self
                    .user_request(
                        device,
                        &s.package,
                        &if s.entrypoint.is_empty() {
                            format!("{}#goal", s.package)
                        } else {
                            s.entrypoint.clone()
                        },
                        json!({"goal":s.goal,"model_profile_id":selected,"resume_session_id":s.id}),
                    )
                    .await?;
                let result = self
                    .submit(request, "", None, Arc::new(|_| {}))
                    .await
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?;
                Ok(json!({"run_id":result.run_id}))
            }
            "identity.confirm" => {
                repo.transaction(|data| {
                    let s = data
                        .sessions
                        .get_mut(text(&values, "session_id")?)
                        .context("会话不存在")?;
                    ensure!(s.state != "running", "请先停止后确认账号或周期");
                    let account = text(&values, "account")?.to_string();
                    let cycle = text(&values, "cycle")?.to_string();
                    ensure!(
                        s.account.as_ref().is_none_or(|a| a == &account),
                        "账号变化不能继承原会话额度，需创建新目标"
                    );
                    s.account = Some(account);
                    s.cycle = Some(cycle);
                    Ok(())
                })?;
                Ok(json!({"confirmed":true}))
            }
            "approvals.read" => {
                let data = repo.data.lock();
                Ok(
                    json!({"approvals":data.approvals.values().filter(|a|values["session_id"].as_str().is_none_or(|s|s==a.session)).take(200).collect::<Vec<_>>(),"rules":data.rules.values().take(200).collect::<Vec<_>>(),"spends":data.spends.iter().rev().take(200).collect::<Vec<_>>()}),
                )
            }
            "approvals.resolve" => {
                repo.transaction(|data| {
                    let approval = data
                        .approvals
                        .get(text(&values, "approval_id")?)
                        .cloned()
                        .context("待办不存在")?;
                    ensure!(approval.status == "pending", "待办已处理");
                    let s = data.sessions.get(&approval.session).context("会话不存在")?;
                    if values["decision"] == "deny" {
                        data.approvals.get_mut(&approval.id).unwrap().status = "denied".into();
                        return Ok(());
                    }
                    let scope = text(&values, "scope")?;
                    ensure!(
                        ["operation", "session", "cycle", "persistent"].contains(&scope),
                        "授权范围无效"
                    );
                    ensure!(
                        scope != "persistent" && scope != "cycle" || s.account.is_some(),
                        "长期或周期授权须先确认账号"
                    );
                    ensure!(scope != "cycle" || s.cycle.is_some(), "周期尚未确认");
                    ensure!(
                        approval.consumption.quantity.is_some()
                            && !approval.consumption.resource.is_empty()
                            && approval.consumption.category != store::Category::Unknown,
                        "消耗不明确，需先核实资源及数量"
                    );
                    let limit = values["limit"]
                        .as_u64()
                        .filter(|v| *v > 0)
                        .context("授权额度必填")?;
                    let expires = values["expires_at"].as_i64().context("授权有效期必填")?;
                    ensure!(expires > self.runtime.clock.now().timestamp(), "授权已过期");
                    let key = store::id();
                    data.rules.insert(
                        key.clone(),
                        store::Rule {
                            id: key,
                            version: store::id(),
                            app: s.app.clone(),
                            account: s.account.clone(),
                            purpose: approval.consumption.purpose.clone(),
                            resource: approval.consumption.resource.clone(),
                            category: approval.consumption.category.clone(),
                            scope: scope.into(),
                            session: Some(s.id.clone()),
                            operation: Some(approval.operation.clone()),
                            cycle: s.cycle.clone(),
                            limit,
                            expires_at: expires,
                            revoked: false,
                        },
                    );
                    data.approvals.get_mut(&approval.id).unwrap().status = "approved".into();
                    Ok(())
                })?;
                Ok(json!({"resolved":true,"instruction":"恢复时重新观察，不重放旧坐标"}))
            }
            "approvals.revoke" => {
                repo.transaction(|d| {
                    d.rules
                        .get_mut(text(&values, "rule_id")?)
                        .context("规则不存在")?
                        .revoked = true;
                    Ok(())
                })?;
                Ok(json!({"revoked":true}))
            }
            "usage.read" => Ok(repo.totals(text(&values, "session_id")?)),
            "usage.sync" => {
                let settings = self.settings();
                let billing = settings.billing.context("供应商对账未配置")?;
                let start = values["start"].as_i64().context("起始时间必填")?;
                let end = values["end"].as_i64().context("结束时间必填")?;
                let mut snapshot = billing::sync(&billing, start, end).await?;
                snapshot["synced_at"] = json!(self.runtime.clock.now());
                repo.transaction(|d| {
                    d.reconciliation.push(snapshot.clone());
                    Ok(())
                })?;
                Ok(snapshot)
            }
            "usage.reconcile" => {
                repo.transaction(|d|{let r=d.requests.get_mut(text(&values,"request_id")?).context("请求不存在")?;ensure!(r.source!="reconciled"&&r.source!="provider","供应商已确认，不能重复计费");let cost=values["confirmed_micros"].as_u64().context("确认金额必填")?;let reference=text(&values,"billing_reference")?;r.actual=Some(cost);r.source="reconciled".into();r.status="settled".into();d.reconciliation.push(json!({"request_id":r.id,"scope":text(&values,"billing_scope")?,"reference":reference,"cost_micros":cost,"at":self.runtime.clock.now()}));Ok(())})?;
                Ok(json!({"reconciled":true}))
            }
            "plans.read" | "plans.save" | "plans.delete" | "memory.read" | "memory.update"
            | "memory.delete" | "memory.search" => self.resources(action, &values),
            "credentials.issue" | "credentials.read" | "credentials.revoke" => {
                mcp::manage(self, action, &values)
            }
            _ => anyhow::bail!("公开动作不存在"),
        }
    }
    pub async fn user_request(
        &self,
        device: &str,
        package: &str,
        entrypoint: &str,
        payload: Value,
    ) -> Result<RunRequest> {
        let d = self
            .db
            .get_device_async(device)
            .await?
            .context("设备不存在")?;
        let app = crate::core::AppContext::new(
            crate::core::DeviceId::new(device)?,
            crate::core::AndroidPackageName::new(d.pkg.context("设备未配置 Android 应用")?)?,
            Some(crate::core::AppPackageId::new(package)?),
        );
        Ok(RunRequest::for_app(
            app,
            ID,
            entrypoint,
            RunPayload::new(payload),
        )?)
    }
    fn resources(&self, action: &str, v: &Value) -> Result<Value> {
        let package = text(v, "package_id")?;
        self.runtime.packages.manifest(package)?;
        let prefix = if action.starts_with("plans.") {
            "plans"
        } else {
            "guides"
        };
        if action == "plans.read" || action == "memory.search" {
            let entries = self.runtime.packages.list(package, ID, prefix)?;
            return Ok(json!({"resources":entries.into_iter().take(100).collect::<Vec<_>>()}));
        }
        let path = text(v, "path")?;
        crate::resources::sanitize_rel_path(path)?;
        ensure!(
            path.starts_with(&format!("{prefix}/")) && path.ends_with(".json"),
            "资源路径无效"
        );
        if action.ends_with("delete") {
            self.runtime.packages.delete_resource(package, ID, path)?;
            return Ok(json!({"deleted":true}));
        }
        if action == "memory.read" {
            let entry = self
                .runtime
                .packages
                .read_text(package, ID, path)?
                .context("攻略不存在")?;
            return Ok(
                json!({"resource":entry,"memory":tools::read_local_memory(&self.runtime,package,path)?,"instruction":"本机验证仅按实例、哈希和证据派生"}),
            );
        }
        let content = if prefix == "plans" {
            let plan: Goal = serde_json::from_value(v["plan"].clone())?;
            ensure!(
                plan.resume_session_id.is_none() && !plan.goal.trim().is_empty(),
                "方案目标必填且不能保存恢复会话"
            );
            serde_json::to_string_pretty(&plan)?
        } else {
            let mut value = v["memory"].clone();
            ensure!(value.is_object(), "攻略必须是对象");
            value["author_status"] = json!("candidate");
            value.as_object_mut().unwrap().remove("verified");
            serde_json::to_string_pretty(&value)?
        };
        let entry = self.runtime.packages.write_text(
            package,
            ID,
            path,
            &content,
            v["expected_version"].as_str(),
            false,
        )?;
        Ok(json!({"resource":entry}))
    }

    async fn probe(
        &self,
        settings: &Settings,
        profile: &provider::Profile,
        session: &str,
        stop: &Arc<AtomicBool>,
        image: Vec<u8>,
        prompt: String,
    ) -> Result<provider::Reply> {
        let now = self.runtime.clock.now();
        let price = provider::reserve_price(profile, prompt.len());
        let reserve = if profile.protocol == "ollama" {
            0
        } else {
            price.max(settings.budget.request_micros).saturating_add(
                if profile.native_search_enabled {
                    profile.native_search_reserve_micros
                } else {
                    0
                },
            )
        };
        let request = self.runtime.repository.reserve(
            store::RequestRecord {
                id: store::id(),
                session: session.into(),
                run_id: session.into(),
                profile: profile.id.clone(),
                model: profile.model.clone(),
                price_version: profile.price_version.clone(),
                kind: if profile.native_search_enabled {
                    "search"
                } else {
                    "connection_test"
                }
                .into(),
                day: now.format("%Y-%m-%d").to_string(),
                reserved: reserve,
                actual: None,
                source: "unknown".into(),
                status: "reserved".into(),
                usage: Value::Null,
                at: now.timestamp_millis(),
                duration_ms: 0,
            },
            now.timestamp(),
        )?;
        let started = std::time::Instant::now();
        let result = tokio::select! {r=self.runtime.provider.infer(profile,provider::ModelInput{image,prompt})=>r,_=async{while !stop.load(Ordering::Acquire){tokio::time::sleep(std::time::Duration::from_millis(20)).await;}}=>Err(anyhow::anyhow!("CANCELLED: 连接测试取消"))};
        match result {
            Ok(reply) => {
                self.runtime.repository.settle(
                    &request,
                    reply.cost,
                    reply.usage.clone(),
                    &reply.source,
                    started.elapsed().as_millis() as u64,
                )?;
                ensure!(!stop.load(Ordering::Acquire), "CANCELLED: 连接测试已取消");
                ensure!(
                    reply.cost.is_some_and(|c| c <= reserve),
                    "budget_cost_unknown: 请核对测试费用"
                );
                Ok(reply)
            }
            Err(error) => {
                self.runtime.repository.settle(
                    &request,
                    None,
                    Value::Null,
                    "unknown",
                    started.elapsed().as_millis() as u64,
                )?;
                Err(error)
            }
        }
    }
    async fn test_profile(&self, values: &Value) -> Result<Value> {
        let settings = self.settings();
        let mut profile = settings
            .profiles
            .iter()
            .find(|p| Some(p.id.as_str()) == values["profile_id"].as_str())
            .context("模型不存在")?
            .clone();
        profile.native_search_enabled = false;
        if values["native_search"] == true {
            ensure!(
                ["responses", "claude", "gemini"].contains(&profile.protocol.as_str())
                    && profile.native_search_reserve_micros > 0,
                "原生搜索测试需协议支持并配置费用预留"
            );
        }
        let stop = Arc::new(AtomicBool::new(false));
        let extensions = self.extensions()?;
        let _lease = extensions
            .long_call(&ExtensionId::parse(ID)?, stop.clone())
            .await?;
        let key = store::id();
        let now = self.runtime.clock.now();
        self.runtime.repository.transaction(|d| {
            ensure!(d.sessions.len() < 1000, "测试会话达到保留上限");
            d.sessions.insert(
                key.clone(),
                store::Session {
                    entrypoint: String::new(),
                    id: key.clone(),
                    package: String::new(),
                    generation: String::new(),
                    app: String::new(),
                    goal: "多模态连接测试".into(),
                    plan_version: String::new(),
                    profile_version: profile.version(),
                    account: None,
                    cycle: None,
                    runs: vec![key.clone()],
                    state: "running".into(),
                    progress: Value::Null,
                    budget: settings.budget.clone(),
                    rounds: 0,
                    searches: 0,
                    active_ms: 0,
                    active_checkpoint: Some(now.timestamp_millis()),
                    tokens: 0,
                    expires_at: now.timestamp() + 600,
                    events: vec![],
                    operations: Default::default(),
                    notified: Default::default(),
                },
            );
            Ok(())
        })?;
        let random = uuid::Uuid::new_v4();
        let x = 30 + random.as_bytes()[0] as u32 % 120;
        let y = 30 + random.as_bytes()[1] as u32 % 120;
        let mut image = image::RgbImage::from_pixel(200, 200, image::Rgb([10, 10, 10]));
        for px in x - 15..x + 15 {
            for py in y - 15..y + 15 {
                image.put_pixel(px, py, image::Rgb([255, 0, 255]));
            }
        }
        let mut bytes = Vec::new();
        image.write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )?;
        let vision_test=async{
            let reply=self.probe(&settings,&profile,&key,&stop,bytes.clone(),"图片协议测试，无设备操作。找到唯一品红色矩形中心。返回 gamer_tool {tool:act,arguments:{position:[归一x,归一y],color:magenta},summary:简短描述}。".into()).await?;
            let pos=&reply.decision.arguments["position"];ensure!(reply.decision.tool=="act"&&reply.decision.arguments["color"]=="magenta"&&(pos[0].as_f64().unwrap_or(-1.0)-x as f64/200.0).abs()<0.06&&(pos[1].as_f64().unwrap_or(-1.0)-y as f64/200.0).abs()<0.06,"图片识别测试未通过");
            let nonce=store::id();let reply=self.probe(&settings,&profile,&key,&stop,bytes.clone(),json!({"previous_tool":reply.decision,"host_tool_result":{"accepted":true,"receipt":nonce},"instruction":"继续上一轮：返回 gamer_tool wait，arguments={duration_ms:0,receipt:宿主结果中的原样凭据}。本次测试不实际等待或操作设备。"}).to_string()).await?;
            ensure!(reply.decision.tool=="wait"&&reply.decision.arguments["receipt"]==nonce,"工具结果往返测试未通过");Ok::<_,anyhow::Error>(())
        }.await;
        let vision = if vision_test.is_ok() {
            "available"
        } else {
            "unavailable"
        };
        let mut native = "untested";
        if vision == "available" && values["native_search"] == true {
            ensure!(
                ["responses", "claude", "gemini"].contains(&profile.protocol.as_str())
                    && profile.native_search_reserve_micros > 0,
                "原生搜索测试需协议支持并配置费用预留"
            );
            profile.native_search_enabled = true;
            let reply=self.probe(&settings,&profile,&key,&stop,bytes,json!({"instruction":"本次为原生搜索与图片及自定义工具的组合测试。使用实际原生搜索查阅 MCP Tools 2025-11-25 规范图片工具结果说明，同时根据附图识别色块颜色。最后返回 gamer_tool observe arguments={color:图片颜色,url:实际查阅来源URL}。不得伪造来源。"}).to_string()).await;
            native = if reply.as_ref().is_ok_and(|r| {
                r.decision.tool == "observe"
                    && r.decision.arguments["color"] == "magenta"
                    && r.decision.arguments["url"]
                        .as_str()
                        .is_some_and(|url| r.sources.to_string().contains(url))
            }) {
                "available"
            } else {
                "unavailable"
            };
        }
        self.runtime.repository.checkpoint(
            &key,
            self.runtime.clock.now().timestamp_millis(),
            true,
        )?;
        self.runtime.repository.transaction(|d| {
            d.sessions.get_mut(&key).unwrap().state = if vision == "available" {
                "completed"
            } else {
                "failed"
            }
            .into();
            Ok(())
        })?;
        let mut saved = self.settings.lock();
        ensure!(
            saved.version == settings.version,
            "测试期间配置变化，请重试"
        );
        let p = saved
            .profiles
            .iter_mut()
            .find(|p| p.id == profile.id)
            .unwrap();
        p.vision = vision.into();
        if values["native_search"] == true {
            p.native_search = native.into();
            if native != "available" {
                p.native_search_enabled = false;
            }
        }
        saved.version = store::id();
        saved.save(&self.runtime.repository.root)?;
        Ok(
            json!({"vision":vision,"tools":if vision=="available"{"available"}else{"unavailable"},"native_search":native,"error":vision_test.err().map(|e|e.to_string()),"usage_session_id":key}),
        )
    }
}
fn text<'a>(v: &'a Value, name: &str) -> Result<&'a str> {
    v[name]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 4000)
        .with_context(|| format!("{name} 必填或超限"))
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
        self.runs.cancel_runner(ID);
        for external in self.external.lock().values() {
            self.runs.cancel(&external.run_id);
        }
        self.external.lock().clear();
        self.prepared.lock().clear();
    }
}
impl RunExecutor for AiService {
    fn prepare<'a>(
        &'a self,
        context: &'a RunContext,
        _: &'a RunRequest,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.live().await?;
            crate::targets::prepare(&self.devices, context.device_id().as_str()).await
        })
    }
    fn acquire(&self, context: &RunContext) -> Result<Box<dyn ActivityLease>> {
        crate::targets::acquire(&self.devices, context.device_id().as_str())
    }
    fn execute<'a>(
        &'a self,
        context: &'a RunContext,
        request: &'a RunRequest,
        _: bool,
        stop: Arc<AtomicBool>,
    ) -> BoxFuture<'a, Result<Vec<(String, String)>>> {
        Box::pin(async move {
            let key = request.payload.as_value()["prepared_id"]
                .as_str()
                .context("host prepared request missing")?;
            let prepared = self
                .prepared
                .lock()
                .remove(key)
                .context("prepared request expired")?;
            let ext = self.extensions()?;
            let _lease = ext
                .long_call(&ExtensionId::parse(ID)?, stop.clone())
                .await?;
            let owner =
                crate::core::input_ownership::acquire(context.device_id().as_str(), stop.clone())?;
            let held_keys = Arc::new(Mutex::new(vec![]));
            let backend = self.runtime.backend.clone();
            let app = context.app.clone();
            let cleanup_keys = held_keys.clone();
            crate::core::input_ownership::attach_cleanup(
                &owner.0,
                Arc::new(move || {
                    let backend = backend.clone();
                    let app = app.clone();
                    let keys = cleanup_keys.lock().clone();
                    Box::pin(async move { backend.release(&app, &keys).await })
                }),
            )?;
            self.runtime.backend.release(&context.app, &[]).await?;
            let session = self.runtime.begin(
                context,
                &prepared.goal.goal,
                &prepared.plan_version,
                &prepared.settings,
                &prepared.profile,
                prepared.goal.resume_session_id.as_deref(),
            )?;
            self.runtime.repository.transaction(|data| {
                let s = data.sessions.get_mut(&session).unwrap();
                if s.entrypoint.is_empty() {
                    s.entrypoint = request.entrypoint.clone();
                }
                if let Some(credential) = &prepared.credential_id {
                    data.mcp_sessions
                        .insert(session.clone(), credential.clone());
                } else {
                    // An explicit administrator resume takes ownership while
                    // retaining the same logical budget and consumption ledger.
                    data.mcp_sessions.remove(&session);
                }
                Ok(())
            })?;
            let e = Execution {
                runtime: self.runtime.clone(),
                context: context.clone(),
                session: session.clone(),
                profile: prepared.profile,
                settings: prepared.settings,
                stop: stop.clone(),
                permit: owner.0.clone(),
                observation: Arc::new(Mutex::new(None)),
                keys: held_keys,
            };
            e.event("run_start",json!({"run_id":context.run_id,"device_id":context.device_id(),"session_id":session}))?;
            let work = async {
                if let Some(external) = prepared.external {
                    mcp::run_external(self, &e, &external).await
                } else {
                    e.run().await
                }
            };
            tokio::pin!(work);
            let result = if let Some(credential) = &prepared.credential_id {
                let revoked = async {
                    loop {
                        let valid = self
                            .runtime
                            .repository
                            .data
                            .lock()
                            .credentials
                            .get(credential)
                            .is_some_and(|c| {
                                !c.revoked && c.expires_at > self.runtime.clock.now().timestamp()
                            });
                        if !valid {
                            return;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                };
                tokio::select! {result=&mut work=>result,_=revoked=>{stop.store(true,Ordering::Release);self.runs.cancel(context.run_id.as_str());work.await}}
            } else {
                work.await
            };
            if stop.load(Ordering::Acquire) {
                self.runs.cancel(context.run_id.as_str());
            }
            if let Err(error) = self
                .notify(
                    &e,
                    prepared.task_id.as_deref(),
                    result.as_ref().err().map(|e| e.to_string()).as_deref(),
                )
                .await
            {
                e.event("notification_error", json!({"error":error.to_string()}))?;
            }
            result?;
            Ok(vec![(
                "success".into(),
                format!("AI 目标完成；逻辑会话 {session}"),
            )])
        })
    }
}
pub struct Registrar {
    pub service: Arc<AiService>,
    pub scheduler: Arc<crate::scheduler::Scheduler>,
}
#[async_trait]
impl crate::extensions::TimerRunnerRegistrar for Registrar {
    fn cancel_owned(&self, id: &str) {
        if id == ID {
            self.service.runs.cancel_runner(ID);
        }
    }
    async fn extension_started(&self, id: &str) -> Result<()> {
        if id == ID {
            self.scheduler
                .register_extension_runner(ID, ID, Arc::new(AiRunner(self.service.clone())))
                .await?;
            self.scheduler.register_entrypoint_describer(
                ID,
                ID,
                Arc::new(GoalDescriber(self.service.clone())),
            );
            self.scheduler
                .register_functions_describer(ID, ID, Arc::new(ToolsDescriber));
        }
        Ok(())
    }
    async fn extension_stopped(&self, id: &str) -> Result<()> {
        if id == ID {
            self.scheduler.unregister_extension_owner(ID).await?;
        }
        Ok(())
    }
    fn executes_without_instance(&self, id: &str) -> bool {
        id == ID
    }
}
struct AiRunner(Arc<AiService>);
#[async_trait]
impl TimerRunner for AiRunner {
    fn runner_id(&self) -> &str {
        ID
    }
    async fn submit(
        &self,
        request: RunRequest,
        task: &str,
        at: Option<i64>,
        complete: Arc<dyn Fn(TimerCompletion) + Send + Sync>,
    ) -> Result<TimerRun, TimerRunnerError> {
        self.0.submit(request, task, at, complete).await
    }
    async fn cancel(&self, id: &str) -> Result<(), TimerRunnerError> {
        self.0.runs.cancel(id);
        Ok(())
    }
}

struct GoalDescriber(Arc<AiService>);
impl crate::scheduler::EntrypointDescriber for GoalDescriber {
    fn describe(
        &self,
        entrypoint: &str,
    ) -> std::result::Result<Value, crate::scheduler::EntrypointDescribeError> {
        let failure = || crate::scheduler::EntrypointDescribeError::NotFound {
            resource: entrypoint.into(),
        };
        let (package, plan) = if let Some(package) = entrypoint.strip_suffix("#goal") {
            (package, None)
        } else {
            let (package, path) = entrypoint.split_once('/').ok_or_else(failure)?;
            let resource = self
                .0
                .runtime
                .packages
                .read_text(package, ID, &format!("plans/{path}"))
                .map_err(|_| failure())?
                .ok_or_else(failure)?;
            let plan: Goal = serde_json::from_str(&resource.content).map_err(|e| {
                crate::scheduler::EntrypointDescribeError::Invalid {
                    diagnostics: json!([{ "message":e.to_string()}]),
                }
            })?;
            (package, Some(plan))
        };
        self.0
            .runtime
            .packages
            .manifest(package)
            .map_err(|_| failure())?;
        Ok(
            json!({"kind":"goal","format":"ai-goal-v1","schema":[{"name":"goal","type":"string","required":plan.is_none(),"default":plan.as_ref().map(|p|&p.goal)},{"name":"model_profile_id","type":"string","required":false,"default":plan.as_ref().map(|p|&p.model_profile_id)}]}),
        )
    }
}
struct ToolsDescriber;
impl crate::scheduler::RunnerFunctionsDescriber for ToolsDescriber {
    fn list_functions(&self) -> Value {
        tools::catalog()
    }
}
