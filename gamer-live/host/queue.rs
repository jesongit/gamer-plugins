//! Process-owned, durable FIFO. One lock serializes admission, controls and dispatch.
use super::{
    events::LiveEvent,
    rules::{CleanupSpec, CompletionSpec, ConfirmationSpec, ExternalRuleOrigin, RuleSet},
};
use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
    time::Duration,
};
use tokio::sync::Mutex;
use uuid::Uuid;
#[path = "queue/accounts.rs"]
mod accounts;
#[path = "queue/confirmation.rs"]
mod confirmation;
#[path = "queue/gifts.rs"]
mod gifts;
use accounts::{Account, Billing};
use confirmation::Confirmation;
pub trait NoticeSink: Send + Sync {
    /// Implementations enqueue without blocking; actual delivery belongs to the
    /// outbound worker. The queue invokes this after releasing its state lock.
    fn send(&self, event: Value);
}
struct NoticeGuard<'a>(&'a Queue);
impl Drop for NoticeGuard<'_> {
    fn drop(&mut self) {
        self.0.flush_notices();
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Target {
    pub device_id: String,
    pub package_id: String,
    pub android_package: Option<String>,
    pub package_stamp: String,
}
#[derive(Debug)]
pub enum SubmitError {
    Busy,
    Blocked(String),
    Failed(String),
}
#[async_trait]
pub trait Backend: Send + Sync {
    fn target(&self, device: &str, package: &str) -> Result<Target>;
    fn check(&self, target: &Target) -> Result<()>;
    fn rules(&self, package: &str) -> Result<(RuleSet, Option<String>)>;
    fn save_rules(&self, package: &str, rules: &RuleSet, version: Option<&str>) -> Result<String>;
    /// Implementations with a storage barrier recheck admission inside it.
    fn save_rules_admitted(
        &self,
        package: &str,
        rules: &RuleSet,
        version: Option<&str>,
        authorize: &dyn Fn() -> Result<()>,
    ) -> Result<String> {
        authorize()?;
        self.save_rules(package, rules, version)
    }
    fn describe(&self, entry: &str) -> Result<Value>;
    fn bind(&self, entry: &str, args: Map<String, Value>) -> Result<Map<String, Value>> {
        super::rules::bind_schema(args, &self.describe(entry)?)
    }
    async fn submit(
        &self,
        target: &Target,
        entry: &str,
        args: &Map<String, Value>,
    ) -> std::result::Result<String, SubmitError>;
    fn validate_external_origin(&self, _origin: &ExternalRuleOrigin) -> Result<()> {
        anyhow::bail!("external_execution_denied: Backend does not support restricted execution")
    }
    async fn submit_external(
        &self,
        _target: &Target,
        _entry: &str,
        _args: &Map<String, Value>,
        _origin: &ExternalRuleOrigin,
    ) -> std::result::Result<String, SubmitError> {
        Err(SubmitError::Blocked(
            "external_execution_denied: Backend does not support restricted execution".into(),
        ))
    }
    async fn submit_item(
        &self,
        item: &Item,
        entry: &str,
        args: &Map<String, Value>,
    ) -> std::result::Result<String, SubmitError> {
        if let Some(origin) = &item.external_origin {
            origin
                .authorize_target(&item.target.device_id, &item.target.package_id)
                .and_then(|_| self.validate_external_origin(origin))
                .map_err(|error| SubmitError::Blocked(error.to_string()))?;
            self.submit_external(&item.target, entry, args, origin)
                .await
        } else {
            self.submit(&item.target, entry, args).await
        }
    }
    async fn run(&self, id: &str) -> Result<Option<Value>>;
    async fn cancel(&self, id: &str) -> Result<()>;
    fn active(&self, device: &str) -> Option<Value>;
    /// Positive journal evidence only; implementations without that evidence
    /// cannot reconcile a historical result.
    async fn completed_child(&self, _run_id: &str, _full_entrypoint: &str) -> Result<bool> {
        Ok(false)
    }
    async fn completed_checkpoint(&self, _run_id: &str, _spec: &CompletionSpec) -> Result<bool> {
        Ok(false)
    }
}
/// Output preparation is separate from admission: an incoming message never
/// waits for FFmpeg or a target frame. Only dispatch runs these hooks.
#[async_trait]
pub trait DispatchHooks: Send + Sync {
    async fn before_submit(&self, target: &Target) -> Result<()>;
    fn executing(&self);
    async fn idle(&self, error: Option<String>) -> Result<()>;
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MainResult {
    pub state: String,
    pub error: Option<String>,
    pub finished_at: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CleanupRun {
    pub state: String,
    pub run_id: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SuccessReconciliation {
    pub at: i64,
    pub completed_entrypoint: String,
    pub core_run_id: String,
    pub previous_error: Option<String>,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub target: Target,
    pub id: String,
    pub request_id: String,
    pub rule_id: String,
    #[serde(default)]
    pub rule_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_origin: Option<ExternalRuleOrigin>,
    pub name: String,
    #[serde(default)]
    pub public_name: String,
    #[serde(default)]
    pub number: u64,
    pub entrypoint: String,
    pub args: Map<String, Value>,
    pub event: Value,
    pub state: String,
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub run_id: Option<String>,
    pub error: Option<String>,
    pub timeout_secs: u64,
    pub retry_of: Option<String>,
    #[serde(default)]
    pub cleanup_spec: Option<CleanupSpec>,
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub main_result: Option<MainResult>,
    #[serde(default)]
    pub cleanup: Option<CleanupRun>,
    #[serde(default)]
    pub cleanup_warning: Option<String>,
    #[serde(default)]
    pub stop_requested: bool,
    #[serde(default)]
    pub main_timed_out: bool,
    #[serde(default)]
    pub reconciliation: Option<SuccessReconciliation>,
    #[serde(default)]
    pub billing: Option<Billing>,
    #[serde(default)]
    pub completion_spec: Option<CompletionSpec>,
    #[serde(default)]
    pub confirmation_spec: Option<ConfirmationSpec>,
    #[serde(default)]
    pub confirmation: Option<Confirmation>,
}
impl Item {
    fn pending(&self) -> bool {
        self.state == "waiting"
    }
    fn active(&self) -> bool {
        ["starting", "running", "cancelling", "review"].contains(&self.state.as_str())
    }
    fn effective_run_id(&self) -> Option<&str> {
        if self.phase == "cleanup" {
            self.cleanup
                .as_ref()
                .and_then(|cleanup| cleanup.run_id.as_deref())
        } else {
            self.run_id.as_deref()
        }
    }
    fn finish_main(&mut self) {
        if let Some(main) = &self.main_result {
            self.state = main.state.clone();
            self.error = main.error.clone();
            self.finished_at = Some(now());
            self.phase = "finished".into();
        }
    }
    fn finish_cleanup(&mut self, state: &str, error: Option<String>) {
        if let Some(cleanup) = &mut self.cleanup {
            cleanup.state = state.into();
            cleanup.error = error.clone();
            cleanup.finished_at = Some(now());
        }
        if state != "success" {
            self.cleanup_warning = Some(error.unwrap_or_else(|| format!("收尾执行{state}")));
        }
        self.finish_main();
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub public_name: String,
    pub at: i64,
    pub event: Value,
    pub rule: Option<String>,
    pub result: String,
    pub item_id: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
struct State {
    schema_version: u8,
    #[serde(default)]
    next_seq: u64,
    revision: u64,
    target: Option<Target>,
    paused: bool,
    blocked: Option<String>,
    items: Vec<Item>,
    receipts: Vec<Receipt>,
    cooldowns: BTreeMap<String, i64>,
    seen: BTreeMap<String, i64>,
    session: String,
    #[serde(default)]
    room_id: Option<String>,
    #[serde(default)]
    automatic_mode: bool,
    #[serde(default)]
    automatic_video_folder: String,
    #[serde(default)]
    users: BTreeMap<String, Account>,
    #[serde(default = "default_quota")]
    default_quota: u64,
    #[serde(default)]
    account_settings_version: u64,
    #[serde(default)]
    gift_credits: BTreeMap<String, gifts::GiftCredit>,
}
fn default_quota() -> u64 {
    1
}
impl Default for State {
    fn default() -> Self {
        Self {
            schema_version: 1,
            next_seq: 1,
            revision: 0,
            target: None,
            paused: false,
            blocked: None,
            items: vec![],
            receipts: vec![],
            cooldowns: BTreeMap::new(),
            seen: BTreeMap::new(),
            session: Uuid::new_v4().to_string(),
            room_id: None,
            automatic_mode: false,
            automatic_video_folder: String::new(),
            users: BTreeMap::new(),
            default_quota: 1,
            account_settings_version: 0,
            gift_credits: BTreeMap::new(),
        }
    }
}
pub struct Queue {
    state: Mutex<State>,
    path: PathBuf,
    backend: Arc<dyn Backend>,
    accepting: AtomicBool,
    dispatch_gate: Mutex<()>,
    hooks: RwLock<Option<Arc<dyn DispatchHooks>>>,
    notifier: RwLock<Option<Arc<dyn NoticeSink>>>,
    notices: std::sync::Mutex<Vec<Value>>,
}
pub struct Preparation {
    target: Option<Target>,
    paused: bool,
    blocked: Option<String>,
    cooldowns: BTreeMap<String, i64>,
    automatic_mode: bool,
    automatic_video_folder: String,
    accepting: bool,
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn summary(e: &LiveEvent) -> Value {
    let short = |v: &Value| {
        v.as_str()
            .map(|s| Value::String(s.chars().take(512).collect()))
            .unwrap_or_else(|| v.clone())
    };
    let mut payload = e.payload.clone();
    if let Some(fields) = payload.as_object_mut() {
        for v in fields.values_mut() {
            *v = short(v);
        }
    }
    json!({"kind":e.kind,"actor":e.actor,"payload":payload,"room_id":e.room_id,
        "event_id":e.event_id,"connection_id":e.connection_id,"seq":e.seq,
        "received_at":e.received_at,"occurred_at":e.occurred_at,"platform_id":e.platform_id,"identity_namespace":e.identity_namespace})
}
impl Queue {
    pub fn open(path: PathBuf, backend: Arc<dyn Backend>) -> Result<Arc<Self>> {
        let mut state: State = if path.exists() {
            ensure!(
                std::fs::metadata(&path)?.len() <= 64 * 1024 * 1024,
                "互动队列文件过大"
            );
            serde_json::from_slice(&std::fs::read(&path)?)
                .context("互动队列损坏，保留原文件，请检查")?
        } else {
            State::default()
        };
        ensure!(state.schema_version == 1, "不支持此互动队列版本");
        // Assign stable journal cursors once to records saved by earlier versions.
        let mut next = state.next_seq.max(1);
        for r in &mut state.receipts {
            if r.seq == 0 {
                r.seq = next;
            }
            next = next.max(r.seq + 1);
        }
        for i in &mut state.items {
            if i.number == 0 {
                i.number = state
                    .receipts
                    .iter()
                    .find(|r| r.item_id.as_deref() == Some(&i.id))
                    .map_or_else(
                        || {
                            let n = next;
                            next += 1;
                            n
                        },
                        |r| r.seq,
                    );
            }
            next = next.max(i.number + 1);
        }
        state.next_seq = next;
        state.paused = state.items.iter().any(|i| i.pending() || i.active());
        state.blocked = state
            .paused
            .then(|| "已恢复上次未结束的队列，请核对后继续或清空".into());
        for item in &mut state.items {
            if item.active() {
                item.state = "review".into();
                item.error = Some("服务重启，执行结果待核对；不会自动重跑".into());
            }
        }
        let accepting = !state.automatic_mode;
        let queue = Arc::new(Self {
            state: Mutex::new(state),
            path,
            backend,
            accepting: AtomicBool::new(accepting),
            dispatch_gate: Mutex::new(()),
            hooks: RwLock::new(None),
            notifier: RwLock::new(None),
            notices: std::sync::Mutex::new(Vec::new()),
        });
        let weak = Arc::downgrade(&queue);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let Some(queue) = weak.upgrade() else {
                    break;
                };
                queue.tick().await;
            }
        });
        Ok(queue)
    }
    pub fn set_hooks(&self, hooks: Arc<dyn DispatchHooks>) {
        *self.hooks.write().unwrap() = Some(hooks);
    }
    pub fn set_notifier(&self, sink: Arc<dyn NoticeSink>) {
        *self.notifier.write().unwrap() = Some(sink);
    }
    fn flush_notices(&self) {
        let events = std::mem::take(&mut *self.notices.lock().unwrap());
        if let Some(sink) = self.notifier.read().unwrap().clone() {
            for event in events {
                sink.send(event);
            }
        }
    }
    fn hooks(&self) -> Option<Arc<dyn DispatchHooks>> {
        self.hooks.read().unwrap().clone()
    }
    pub fn stop_admission(&self) {
        self.accepting.store(false, Ordering::SeqCst);
    }
    pub async fn finish_preparation(&self) {
        let _gate = self.dispatch_gate.lock().await;
    }
    pub async fn prepare_automatic(
        &self,
        device: &str,
        package: &str,
        folder: &str,
    ) -> Result<Preparation> {
        let target = self.backend.target(device, package)?;
        self.backend.check(&target)?;
        let mut s = self.state.lock().await;
        ensure!(
            !s.items.iter().any(|i| i.pending() || i.active()),
            "请先处理当前及等待项，再开始自动直播"
        );
        let previous = Preparation {
            target: s.target.clone(),
            paused: s.paused,
            blocked: s.blocked.clone(),
            cooldowns: s.cooldowns.clone(),
            automatic_mode: s.automatic_mode,
            automatic_video_folder: s.automatic_video_folder.clone(),
            accepting: self.accepting.load(Ordering::SeqCst),
        };
        let mut n = s.clone();
        n.target = Some(target);
        n.automatic_mode = true;
        n.automatic_video_folder = folder.trim().to_owned();
        n.paused = true;
        n.blocked = None;
        n.cooldowns.clear();
        self.accepting.store(false, Ordering::SeqCst);
        if let Err(e) = self.commit(&mut s, n) {
            self.accepting.store(previous.accepting, Ordering::SeqCst);
            return Err(e);
        }
        Ok(previous)
    }
    pub async fn abort_automatic(&self, previous: Preparation) -> Result<()> {
        let mut s = self.state.lock().await;
        let mut n = s.clone();
        n.target = previous.target;
        n.paused = previous.paused;
        n.blocked = previous.blocked;
        n.cooldowns = previous.cooldowns;
        n.automatic_mode = previous.automatic_mode;
        n.automatic_video_folder = previous.automatic_video_folder;
        self.commit(&mut s, n)?;
        self.accepting.store(previous.accepting, Ordering::SeqCst);
        Ok(())
    }
    pub async fn activate_automatic(&self) -> Result<()> {
        let mut s = self.state.lock().await;
        let mut n = s.clone();
        n.paused = false;
        n.blocked = None;
        self.commit(&mut s, n)?;
        self.accepting.store(true, Ordering::SeqCst);
        Ok(())
    }
    pub async fn manual_mode(&self) -> Result<()> {
        let mut s = self.state.lock().await;
        let mut n = s.clone();
        if n.automatic_mode && !n.items.iter().any(|i| i.state == "review") {
            n.paused = false;
            n.blocked = None;
        }
        n.automatic_mode = false;
        self.commit(&mut s, n)?;
        self.accepting.store(true, Ordering::SeqCst);
        Ok(())
    }
    /// Stop only this queue's run. Wait for its own slot to disappear, even if
    /// a different manual run subsequently claims the same device.
    pub async fn stop_automatic(&self) -> Result<()> {
        let _notices = NoticeGuard(self);
        self.accepting.store(false, Ordering::SeqCst);
        {
            let mut s = self.state.lock().await;
            let mut n = s.clone();
            n.automatic_mode = true;
            n.paused = true;
            self.commit(&mut s, n)?;
        }
        self.control("stop", &[], "").await?;
        let _gate = self.dispatch_gate.lock().await;
        self.wait_active_terminal().await
    }
    async fn wait_active_terminal(&self) -> Result<()> {
        loop {
            let mut s = self.state.lock().await;
            let Some(index) = s.items.iter().position(Item::active) else {
                return Ok(());
            };
            let item_id = s.items[index].id.clone();
            ensure!(
                s.items[index].state != "review",
                "运行结果待核对，未释放自动直播输出"
            );
            ensure!(
                s.items[index].effective_run_id().is_some() || s.items[index].phase == "cleanup",
                "运行准备状态未结束"
            );
            self.poll_active(&mut s, index).await;
            ensure!(
                !s.items
                    .iter()
                    .any(|item| item.id == item_id && item.state == "review"),
                "无法确认运行结束，未释放自动直播输出"
            );
            drop(s);
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    async fn display_idle(&self, error: Option<String>) {
        if let Some(hooks) = self.hooks() {
            if let Err(e) = hooks.idle(error).await {
                let mut s = self.state.lock().await;
                let mut n = s.clone();
                n.paused = true;
                n.blocked = Some(e.to_string());
                let _ = self.commit(&mut s, n);
            }
        }
    }
    fn persist(&self, state: &mut State) -> Result<()> {
        state.revision += 1;
        let cutoff = now() - 7 * 86400 * 1000;
        state.receipts.retain(|r| r.at >= cutoff);
        if state.receipts.len() > 10000 {
            state.receipts.drain(..state.receipts.len() - 10000);
        }
        state.items.retain(|i| {
            i.pending() || i.active() || i.finished_at.unwrap_or(i.created_at) >= cutoff
        });
        let mut terminal = state
            .items
            .iter()
            .filter(|i| !i.pending() && !i.active())
            .count();
        state.items.retain(|i| {
            if !i.pending() && !i.active() && terminal > 10000 {
                terminal -= 1;
                false
            } else {
                true
            }
        });
        state.seen.retain(|_, at| *at >= cutoff);
        // Bound dedup independently of page/cache lifetimes.
        if state.seen.len() > 20000 {
            let mut entries: Vec<_> = state.seen.iter().map(|(k, v)| (k.clone(), *v)).collect();
            entries.sort_by_key(|(_, v)| *v);
            for (k, _) in entries.into_iter().take(state.seen.len() - 20000) {
                state.seen.remove(&k);
            }
        }
        std::fs::create_dir_all(self.path.parent().context("队列路径无效")?)?;
        let mut bytes = serde_json::to_vec(state)?;
        while bytes.len() > 16 * 1024 * 1024 {
            if !state.receipts.is_empty() {
                state.receipts.drain(..state.receipts.len().min(100));
            } else if let Some(index) = state.items.iter().position(|i| !i.pending() && !i.active())
            {
                state.items.remove(index);
            } else {
                anyhow::bail!("队列记录超出存储上限");
            }
            bytes = serde_json::to_vec(state)?;
        }
        crate::core::fs::atomic_write(&self.path, &bytes)
    }
    fn commit(&self, current: &mut State, mut next: State) -> Result<()> {
        let changed = |i: &Item| {
            json!([
                i.state,
                i.phase,
                i.main_result,
                i.cleanup,
                i.cleanup_warning,
                i.reconciliation,
                i.billing,
                i.confirmation
            ])
        };
        let previous: BTreeMap<_, _> = current
            .items
            .iter()
            .map(|i| (i.id.as_str(), changed(i)))
            .collect();
        for i in &mut next.items {
            if previous
                .get(i.id.as_str())
                .is_none_or(|old| *old != changed(i))
            {
                i.updated_at = now();
            }
        }
        if let Err(e) = self.persist(&mut next) {
            current.paused = true;
            current.blocked = Some(format!("队列保存失败：{e}"));
            current.revision += 1;
            return Err(e.context("队列保存失败，已暂停"));
        }
        let mut emitted = Vec::new();
        for receipt in next.receipts.iter().filter(|r| {
            r.seq >= current.next_seq
                && r.reason == "quota_unavailable"
                && r.item_id.is_none()
                && r.rule.is_some()
        }) {
            emitted.push(Self::notice_rejected(&next, receipt));
        }
        for item in &next.items {
            let old = current.items.iter().find(|old| old.id == item.id);
            if old.is_none() && item.pending() {
                emitted.push(Self::notice_item(&next, item, "queued"));
            }
            let old_confirmation = old
                .and_then(|i| i.confirmation.as_ref())
                .map(|c| c.state.as_str());
            if let Some(confirmation) = item
                .confirmation
                .as_ref()
                .filter(|c| Some(c.state.as_str()) != old_confirmation)
            {
                let kind = match confirmation.state.as_str() {
                    "awaiting_confirmation" => "confirmation_required",
                    "awaiting_scan" => "scan_required",
                    _ => confirmation.state.as_str(),
                };
                emitted.push(Self::notice_item(&next, item, kind));
            }
            let old_billing = old
                .and_then(|i| i.billing.as_ref())
                .map(|b| b.state.as_str());
            if let Some(billing) = item.billing.as_ref().filter(|b| {
                Some(b.state.as_str()) != old_billing
                    && ["charged", "released"].contains(&b.state.as_str())
            }) {
                emitted.push(Self::notice_item(&next, item, &billing.state));
            }
            if let Some(result) = item
                .main_result
                .as_ref()
                .filter(|_| old.is_none_or(|old| old.main_result.is_none()))
            {
                let state = result.state.as_str();
                emitted.push(Self::notice_item(
                    &next,
                    item,
                    if state == "success" {
                        "succeeded"
                    } else if state == "cancelled" {
                        if item
                            .confirmation
                            .as_ref()
                            .is_some_and(|c| c.state == "timeout")
                        {
                            "timeout"
                        } else {
                            "cancelled"
                        }
                    } else {
                        "failed"
                    },
                ));
            }
            if item.cleanup_warning.is_some() && old.is_none_or(|old| old.cleanup_warning.is_none())
            {
                emitted.push(Self::notice_item(&next, item, "cleanup_warning"));
            }
        }
        self.notices.lock().unwrap().extend(emitted);
        *current = next;
        Ok(())
    }
    pub async fn status(&self, offset: usize, filter: &str) -> Value {
        let s = self.state.lock().await;
        let waiting: Vec<_> = s.items.iter().filter(|i| i.pending()).collect();
        let current = s.items.iter().find(|i| i.active());
        let history: Vec<_> = s
            .items
            .iter()
            .rev()
            .filter(|i| !i.pending() && !i.active() && (filter.is_empty() || i.state == filter))
            .collect();
        json!({"revision":s.revision,"target":s.target,"paused":s.paused,"blocked":s.blocked,"automatic_video_folder":s.automatic_video_folder,
            "current":current,"waiting":waiting,"capacity":100,"history":history.iter().skip(offset).take(30).collect::<Vec<_>>(),"has_more":history.len()>offset+30,
            "receipts":s.receipts.iter().rev().take(100).collect::<Vec<_>>(),
            "device_run":s.target.as_ref().and_then(|t|self.backend.active(&t.device_id))})
    }
    /// External readers see only the requested recorded target. Filter before
    /// pagination and omit raw events, error text, receipts and local paths.
    pub async fn external_status(&self, device: &str, package: &str, offset: usize) -> Value {
        let s = self.state.lock().await;
        let matches =
            |item: &&Item| item.target.device_id == device && item.target.package_id == package;
        let project = |item: &Item| json!({"id":item.id,"device_id":item.target.device_id,"package_id":item.target.package_id,"rule_id":item.rule_id,"name":item.name,"number":item.number,"entrypoint":item.entrypoint,"state":item.state,"phase":item.phase,"run_id":item.run_id,"created_at":item.created_at,"started_at":item.started_at,"finished_at":item.finished_at,"has_error":item.error.is_some()});
        let current = s
            .items
            .iter()
            .filter(matches)
            .find(|item| item.active())
            .map(project);
        let waiting: Vec<_> = s
            .items
            .iter()
            .filter(matches)
            .filter(|item| item.pending())
            .map(project)
            .collect();
        let history: Vec<_> = s
            .items
            .iter()
            .rev()
            .filter(matches)
            .filter(|item| !item.pending() && !item.active())
            .collect();
        let bound = s
            .target
            .as_ref()
            .is_some_and(|target| target.device_id == device && target.package_id == package);
        json!({"device_id":device,"package_id":package,"bound":bound,"paused":bound.then_some(s.paused),"current":current,"waiting":waiting,"history":history.iter().skip(offset).take(30).map(|item|project(item)).collect::<Vec<_>>(),"has_more":history.len()>offset+30,"offset":offset})
    }
    pub async fn logs(
        &self,
        before: u64,
        after: u64,
        filter: &str,
        search: &str,
        refresh: &[u64],
    ) -> Value {
        let s = self.state.lock().await;
        let items: BTreeMap<_, _> = s.items.iter().map(|i| (i.id.as_str(), i)).collect();
        let removed: std::collections::HashSet<_> = s
            .receipts
            .iter()
            .filter(|r| r.event["kind"] == "message.removed")
            .flat_map(|r| {
                r.event["payload"]["removed_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|id| {
                        format!(
                            "{}:{}",
                            r.event["room_id"],
                            id.as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| id.to_string())
                        )
                    })
            })
            .collect();
        let row = |r: &Receipt| {
            let item = r.item_id.as_deref().and_then(|id| items.get(id));
            let id = &r.event["payload"]["message_id"];
            let withdrawn = r.event["kind"] == "super_chat"
                && removed.contains(&format!(
                    "{}:{}",
                    r.event["room_id"],
                    id.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| id.to_string())
                ));
            json!({"seq":r.seq,"at":r.at,"event":r.event,"rule":r.rule,"result":r.result,
                "reason":r.reason,"item":item,"withdrawn":withdrawn})
        };
        let search = search.to_lowercase();
        let matches = |r: &&Receipt| {
            let item = r.item_id.as_deref().and_then(|id| items.get(id));
            let related =
                r.rule.is_some() || !["unmatched", "display_only", ""].contains(&r.reason.as_str());
            let system = r.event["kind"].as_str().is_some_and(|k| {
                k.starts_with("room.")
                    || k.starts_with("connection.")
                    || k == "platform.event"
                    || k == "user.blocked"
            });
            (match filter {
                "trigger" => related,
                "rejected" => related && r.item_id.is_none() && r.reason != "gift_credited",
                "errors" => {
                    r.reason == "gift_credit_invalid"
                        || item.is_some_and(|i| {
                            ["failed", "review"].contains(&i.state.as_str())
                                || i.cleanup_warning.is_some()
                        })
                }
                "system" => system,
                _ => true,
            }) && (search.is_empty()
                || format!(
                    "{} {} {} {}",
                    r.event["actor"]["name"],
                    r.event["payload"]["text"],
                    r.event["payload"]["gift_name"],
                    r.rule.as_deref().unwrap_or("")
                )
                .to_lowercase()
                .contains(&search))
        };
        let page: Vec<_> = if after > 0 {
            s.receipts
                .iter()
                .filter(|r| r.seq > after)
                .filter(matches)
                .take(51)
                .collect()
        } else {
            s.receipts
                .iter()
                .rev()
                .filter(|r| before == 0 || r.seq < before)
                .filter(matches)
                .take(51)
                .collect()
        };
        let rows: Vec<_> = page.iter().take(50).map(|r| row(r)).collect();
        json!({"rows":rows,"next":rows.last().map(|r| &r["seq"]),"has_more":page.len()>50,
            "next_after":if after>0 && page.len()>50 {page[49].seq} else {s.next_seq.saturating_sub(1)},
            "gap":after>0 && s.receipts.first().is_some_and(|r|after.saturating_add(1)<r.seq),
            "updates":s.receipts.iter().filter(|r|refresh.contains(&r.seq)).map(row).collect::<Vec<_>>(),
            "oldest":s.receipts.first().map(|r|r.seq),"revision":s.revision})
    }
    /// Explicit audience whitelist. Never serialize an Item/Receipt into this surface.
    pub async fn audience(&self) -> Value {
        let s = self.state.lock().await;
        let changed_at = |i: &Item| {
            i.updated_at
                .max(i.finished_at.or(i.started_at).unwrap_or(i.created_at))
        };
        let item = |i: &Item, position: usize| {
            let user = i.billing.as_ref().and_then(|b| s.users.get(&b.user_id));
            json!({"number":i.number,
            "name":if i.public_name.is_empty(){&i.name}else{&i.public_name},
            "viewer":i.event["message"]["actor"]["name"].as_str().unwrap_or("观众"),
            "state":i.state,"position":position,"started_at":i.started_at,"finished_at":i.finished_at,
            "changed_at":changed_at(i),"test":i.event["test"] == true,
            "open_id":user.map(|u|u.open_id.as_str()),"medal_name":i.event["message"]["actor"]["medal_name"],"medal_level":i.event["message"]["actor"]["medal_level"],
            "phase":i.phase,"confirmation":i.confirmation.as_ref().filter(|_|i.active() && matches!(i.phase.as_str(),"awaiting_scan"|"awaiting_confirmation"|"rescanning")).map(|c|json!({"state":c.state,"deadline_at":c.deadline_at})),
            "cost":i.billing.as_ref().map(|b|b.cost),"billing_state":i.billing.as_ref().map(|b|b.state.as_str()),
            "quota":user.map(|u|json!({"remaining":u.balance,"reserved":u.reserved,"available":if u.unlimited {Value::Null}else{json!(u.available())},"unlimited":u.unlimited}))})
        };
        let waiting: Vec<_> = s.items.iter().filter(|i| i.pending()).collect();
        let current = s.items.iter().find(|i| i.active());
        let featured = current.or_else(|| {
            s.items
                .iter()
                .filter(|i| ["success", "failed", "cancelled"].contains(&i.state.as_str()))
                .max_by_key(|i| (changed_at(i), i.number))
        });
        let mut notices: Vec<Value> = s
            .items
            .iter()
            .filter(|i| !i.pending())
            .rev()
            .take(20)
            .map(|i| item(i, 0))
            .collect();
        notices.extend(waiting.iter().enumerate().map(|(p, i)| item(i, p + 1)));
        notices.extend(s.receipts.iter().rev().filter(|r|r.rule.is_some() && r.item_id.is_none()).take(20).map(|r|json!({
            "number":r.seq,"viewer":r.event["actor"]["name"].as_str().unwrap_or("观众"),
            "name":r.public_name,"state":if r.reason=="cooldown" {"cooldown"} else if r.reason=="queue_full" {"queue_full"} else {"unavailable"},
            "changed_at":r.at})));
        notices.sort_by_key(|v| std::cmp::Reverse(v["changed_at"].as_i64().unwrap_or(0)));
        notices.truncate(20);
        json!({"revision":s.revision,"time":now(),"paused":s.paused || s.blocked.is_some(),
            "configured":s.target.is_some(),"current":current.map(|i|item(i,0)),
            "featured":featured.map(|i|item(i,0)),
            "waiting":waiting.iter().enumerate().map(|(p,i)|item(i,p+1)).collect::<Vec<_>>(),
            "waiting_count":waiting.len(),"device_busy":s.target.as_ref().is_some_and(|t|self.backend.active(&t.device_id).is_some()) && current.is_none(),
            "notices":notices})
    }
    pub async fn configure(&self, device: &str, package: &str) -> Result<()> {
        self.configure_with_folder(device, package, None).await
    }
    pub async fn configure_with_folder(
        &self,
        device: &str,
        package: &str,
        folder: Option<&str>,
    ) -> Result<()> {
        ensure!(
            folder.is_none_or(|folder| folder.len() <= 4096),
            "视频文件夹路径过长"
        );
        let target = self.backend.target(device, package)?;
        let mut s = self.state.lock().await;
        ensure!(
            !s.items.iter().any(|i| i.pending() || i.active()),
            "请先处理当前及等待项，再更换目标"
        );
        let mut n = s.clone();
        n.target = Some(target);
        if let Some(folder) = folder {
            n.automatic_video_folder = folder.trim().to_owned();
        }
        n.paused = false;
        n.blocked = None;
        n.cooldowns.clear();
        self.commit(&mut s, n)
    }
    pub async fn new_connection(&self) -> Result<()> {
        let mut s = self.state.lock().await;
        ensure!(
            !s.items.iter().any(|i| i.pending() || i.active()),
            "更换连接前请处理当前及等待项"
        );
        let mut n = s.clone();
        n.session = Uuid::new_v4().to_string();
        n.room_id = None;
        n.cooldowns.clear();
        if !n.automatic_mode || self.accepting.load(Ordering::SeqCst) {
            n.paused = false;
            n.blocked = None;
        }
        let manual = !n.automatic_mode;
        self.commit(&mut s, n)?;
        if manual {
            self.accepting.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
    pub fn read_rules(&self, package: &str) -> Result<Value> {
        let (rules, version) = self.backend.rules(package)?;
        Ok(json!({"rules":rules.rules,"schema_version":1,"version":version}))
    }
    /// External rule edits can affect the configured queue. Keep its actual
    /// target stable through authorization and the synchronous resource commit.
    pub async fn external_save_rules(
        &self,
        device: &str,
        package: &str,
        rules: RuleSet,
        version: Option<&str>,
        authorize: impl Fn() -> Result<()> + Send,
    ) -> Result<Value> {
        let state = self.state.lock().await;
        if let Some(target) = &state.target {
            ensure!(
                target.package_id != package || target.device_id == device,
                "scope_denied: Package is bound to another live target"
            );
        }
        // Recheck after waiting for the target lock, before saving bytes.
        authorize()?;
        self.save_rules_checked(package, rules, version, Some(&authorize))
    }
    pub fn save_rules(
        &self,
        package: &str,
        rules: RuleSet,
        version: Option<&str>,
    ) -> Result<Value> {
        self.save_rules_checked(package, rules, version, None)
    }
    fn save_rules_checked(
        &self,
        package: &str,
        rules: RuleSet,
        version: Option<&str>,
        authorize: Option<&dyn Fn() -> Result<()>>,
    ) -> Result<Value> {
        rules.validate(package)?;
        if let Some(origin) = &rules.external_origin {
            self.backend.validate_external_origin(origin)?;
        }
        for r in rules.rules.iter().filter(|r| r.enabled) {
            self.backend.describe(&r.entrypoint)?;
            let sample = LiveEvent {
                schema_version: 1,
                seq: 0,
                platform_id: String::new(),
                identity_namespace: String::new(),
                connection_id: String::new(),
                room_id: String::new(),
                event_id: None,
                kind: r.kind.clone(),
                occurred_at: None,
                received_at: String::new(),
                actor: Some(json!({"id":"viewer","name":"观众"})),
                payload: json!({"text":"弹幕","gift_id":1,"gift_name":"礼物","count":1}),
                platform_data: Value::Null,
            };
            self.backend.bind(&r.entrypoint, r.bind(&sample)?)?;
            self.bind_cleanup(package, r.cleanup.as_ref())?;
            if let Some(confirmation) = &r.confirmation {
                self.bind_cleanup(package, Some(&confirmation.cancel_cleanup))?;
            }
        }
        let version = match authorize {
            Some(authorize) => self
                .backend
                .save_rules_admitted(package, &rules, version, authorize)?,
            None => self.backend.save_rules(package, &rules, version)?,
        };
        Ok(json!({"version":version}))
    }
    pub fn toggle_rule(
        &self,
        package: &str,
        id: &str,
        enabled: bool,
        version: &str,
    ) -> Result<Value> {
        let (mut rules, current) = self.backend.rules(package)?;
        ensure!(
            current.as_deref() == Some(version),
            "规则已被其他页面修改，请重新读取"
        );
        let rule = rules
            .rules
            .iter_mut()
            .find(|r| r.id == id)
            .context("规则不存在")?;
        rule.enabled = enabled;
        self.save_rules(package, rules, Some(version))
    }
    fn bind_cleanup(
        &self,
        package: &str,
        spec: Option<&CleanupSpec>,
    ) -> Result<Option<CleanupSpec>> {
        spec.map(|spec| {
            spec.validate(package)?;
            let mut frozen = spec.clone();
            frozen.args = self.backend.bind(&spec.entrypoint, spec.args.clone())?;
            ensure!(
                serde_json::to_vec(&frozen.args)?.len() <= 16 * 1024,
                "收尾参数总大小超过 16 KiB"
            );
            Ok(frozen)
        })
        .transpose()
    }
    /// Explicit repair of an old combined workflow, backed by an actual child
    /// completion event. This changes queue history only, never the Core run.
    pub async fn reconcile_success(
        &self,
        item_id: &str,
        completed_entrypoint: &str,
    ) -> Result<Value> {
        let _notices = NoticeGuard(self);
        let _dispatch = self.dispatch_gate.lock().await;
        let mut s = self.state.lock().await;
        let index = s
            .items
            .iter()
            .position(|item| item.id == item_id)
            .context("队列记录不存在")?;
        let item = s.items[index].clone();
        ensure!(
            item.billing.is_none() && item.completion_spec.is_none(),
            "计费任务须通过完成检查点核对，不能使用旧历史纠正"
        );
        super::rules::validate_entrypoint(&item.target.package_id, completed_entrypoint)?;
        if item.state == "success"
            && item.reconciliation.as_ref().is_some_and(|audit| {
                Some(audit.core_run_id.as_str()) == item.run_id.as_deref()
                    && audit.completed_entrypoint == completed_entrypoint
            })
        {
            return Ok(
                json!({"item_id":item.id,"state":item.state,"already_reconciled":true,
                "cleanup_warning":item.cleanup_warning,"reconciliation":item.reconciliation}),
            );
        }
        ensure!(item.state == "failed", "仅失败记录可核对为日常已完成");
        let id = item.run_id.as_deref().context("记录没有对应主运行")?;
        let run = self.backend.run(id).await?.context("无法确认原运行终态")?;
        ensure!(
            run["run_id"].as_str() == Some(id)
                && ["success", "failed", "cancelled"]
                    .contains(&run["state"].as_str().unwrap_or("")),
            "原运行尚未确认结束"
        );
        ensure!(
            self.backend
                .active(&item.target.device_id)
                .is_none_or(|run| run["run_id"].as_str() != Some(id)),
            "原运行仍占用设备，不能核对"
        );
        ensure!(
            self.backend
                .completed_child(id, completed_entrypoint)
                .await?,
            "没有匹配的日常子脚本成功完成证据"
        );
        let mut n = s.clone();
        let item = &mut n.items[index];
        let previous_error = item.error.clone();
        item.reconciliation = Some(SuccessReconciliation {
            at: now(),
            completed_entrypoint: completed_entrypoint.into(),
            core_run_id: id.into(),
            previous_error: previous_error.clone(),
            reason: "依据已完成子脚本的真实日志纠正业务结果；原 Core 运行记录保留".into(),
        });
        item.cleanup_warning = Some(format!(
            "原运行在日常完成后收尾失败：{}",
            previous_error
                .as_deref()
                .unwrap_or("原失败记录没有错误详情")
        ));
        item.main_result = Some(MainResult {
            state: "success".into(),
            error: None,
            finished_at: item.main_result.as_ref().map_or_else(
                || item.finished_at.unwrap_or(now()),
                |main| main.finished_at,
            ),
        });
        item.finish_main();
        let response = json!({"item_id":item.id,"state":item.state,"already_reconciled":false,
            "cleanup_warning":item.cleanup_warning,"reconciliation":item.reconciliation});
        self.commit(&mut s, n)?;
        Ok(response)
    }
    pub async fn preview(
        &self,
        event: LiveEvent,
        enqueue: bool,
        request_id: &str,
    ) -> Result<Value> {
        ensure!(
            event.kind == "message" || event.kind == "gift",
            "模拟仅支持弹幕或礼物"
        );
        ensure!(event.payload.to_string().len() < 8192, "模拟事件过大");
        if enqueue {
            ensure!(
                !request_id.is_empty() && request_id.len() <= 100,
                "测试入队需要请求 ID"
            );
        }
        if let Some(result) = self.route_control(&event, true, enqueue).await? {
            return Ok(result);
        }
        self.accept(event, enqueue, true, request_id).await
    }
    pub async fn receive(&self, event: LiveEvent) {
        if event.payload["text"]
            .as_str()
            .is_some_and(|text| text.trim_start().starts_with("【Gamer】"))
        {
            return;
        }
        match self.route_control(&event, false, true).await {
            Ok(Some(_)) => return,
            Err(error) => {
                tracing::warn!(%error,"直播确认控制保存失败");
                return;
            }
            _ => {}
        }
        if !self.accepting.load(Ordering::SeqCst) && event.kind != "gift" {
            return;
        }
        if let Err(e) = self.accept(event, true, false, "").await {
            tracing::warn!(error=%e, "直播互动未入队");
        }
    }
    async fn accept(
        &self,
        e: LiveEvent,
        enqueue: bool,
        test: bool,
        request_id: &str,
    ) -> Result<Value> {
        let _notices = NoticeGuard(self);
        let mut s = self.state.lock().await;
        if !test && !self.accepting.load(Ordering::SeqCst) && e.kind != "gift" {
            return Ok(json!({"result":"直播连接已停止"}));
        }
        if test && enqueue {
            ensure!(
                !s.automatic_mode || self.accepting.load(Ordering::SeqCst),
                "自动直播已停止，请先开始自动直播或手动输出"
            );
            if let Some(i) = s.items.iter().find(|i| i.request_id == request_id) {
                return Ok(json!({"result":"已入队","item_id":i.id}));
            }
        }
        let mut n = s.clone();
        let mut matched = None;
        let mut reason = "unavailable";
        let mut public_name = String::new();
        let mut item_id = None;
        let mut resolved = None;
        let event_key = e.event_id.as_ref().map(|id| {
            format!(
                "{}:{}:{}:{}:{}:{}",
                n.session, e.platform_id, e.identity_namespace, e.room_id, e.kind, id
            )
        });
        if enqueue && !test {
            if let Some(key) = &event_key {
                if n.seen.contains_key(key) {
                    return Ok(json!({"result":"重复事件"}));
                }
                n.seen.insert(key.clone(), now());
            }
        }
        let credit = if enqueue && !test {
            gifts::credit(&mut n, &e)
        } else {
            Ok(gifts::Outcome::Ignored)
        };
        let (gift_credit, gift_error) = match credit {
            Ok(gifts::Outcome::Added(credit)) => (Some(credit), None),
            Ok(gifts::Outcome::Duplicate) => {
                return Ok(json!({"result":"重复礼物，未重复增加次数"}))
            }
            Ok(gifts::Outcome::Ignored) => (None, None),
            Err(error) => (None, Some(error)),
        };
        let result: Result<String> = async {
            if let Some(error) = gift_error {
                reason = "gift_credit_invalid";
                return Err(error);
            }
            if !test && !self.accepting.load(Ordering::SeqCst) {
                reason = "admission_stopped";
                return Ok("互动执行已停止，未入队".into());
            }
            if !test && !e.room_id.is_empty() {
                if n.room_id.as_ref().is_some_and(|room| room != &e.room_id) {
                    reason = "room_changed";
                    n.paused = true;
                    n.blocked = Some("直播间发生变化，请处理旧队列后重新连接".into());
                    anyhow::bail!("直播间与当前队列会话不一致");
                }
                n.room_id = Some(e.room_id.clone());
            }
            if e.kind != "message" && e.kind != "gift" {
                reason = "display_only";
                return Ok("仅展示".into());
            }
            reason = "target_unavailable";
            let target = n.target.clone().context("请先绑定设备和配置包")?;
            reason = "rules_invalid";
            let (rules, rule_version) = self.backend.rules(&target.package_id)?;
            rules.validate(&target.package_id)?;
            if let Some(origin) = &rules.external_origin {
                origin.authorize_target(&target.device_id, &target.package_id)?;
                self.backend.validate_external_origin(origin)?;
            }
            let Some(rule) = rules.rules.iter().find(|r| r.matches(&e)) else {
                reason = "unmatched";
                return Ok("未匹配".into());
            };
            matched = Some(rule.name.clone());
            public_name = if rule.public_name.trim().is_empty() {
                rule.name.clone()
            } else {
                rule.public_name.clone()
            };
            reason = "target_unavailable";
            self.backend.check(&target)?;
            reason = "invalid_args";
            let args = self.backend.bind(&rule.entrypoint, rule.bind(&e)?)?;
            let cleanup_spec = self.bind_cleanup(&target.package_id, rule.cleanup.as_ref())?;
            let mut confirmation_spec = rule.confirmation.clone();
            if let Some(spec) = &mut confirmation_spec {
                spec.cancel_cleanup = self.bind_cleanup(&target.package_id,Some(&spec.cancel_cleanup))?.unwrap();
            }
            ensure!(
                serde_json::to_vec(&args)?.len() <= 16 * 1024,
                "参数总大小超过 16 KiB，未入队"
            );
            resolved = Some(
                json!({"entrypoint":rule.entrypoint,"args":args,"target":target,"rule":rule.name,"cleanup":cleanup_spec}),
            );
            if n.cooldowns
                .get(&rule.id)
                .is_some_and(|at| now() - at < rule.cooldown_secs as i64 * 1000)
            {
                reason = "cooldown";
                return Ok("冷却中，未入队".into());
            }
            reason = "queue_full";
            ensure!(
                n.items.iter().filter(|i| i.pending()).count() < 100,
                "队列已满，未入队"
            );
            if !enqueue {
                reason = "preview";
                return Ok("匹配成功（预览，未入队）".into());
            }
            reason = "quota_unavailable";
            let billing = accounts::reserve(&mut n,&e,test,rule.cost)?;
            let id = Uuid::new_v4().to_string();
            item_id = Some(id.clone());
            n.items.push(Item {
                target: target.clone(),
                id,
                request_id: request_id.into(),
                rule_id: rule.id.clone(),
                rule_version,
                external_origin: rules.external_origin.clone(),
                name: rule.name.clone(),
                public_name: public_name.clone(),
                number: n.next_seq,
                entrypoint: rule.entrypoint.clone(),
                args,
                event: json!({"test":test || e.platform_id == "test","session":n.session,"message":summary(&e)}),
                state: "waiting".into(),
                created_at: now(),
                updated_at: now(),
                started_at: None,
                finished_at: None,
                run_id: None,
                error: None,
                timeout_secs: rule.timeout_secs,
                retry_of: None,
                cleanup_spec,
                phase: String::new(),
                main_result: None,
                cleanup: None,
                cleanup_warning: None,
                stop_requested: false,
                main_timed_out: false,
                reconciliation: None,
                billing: Some(billing),
                completion_spec: rule.completion.clone(),
                confirmation_spec,
                confirmation: None,
            });
            n.cooldowns.insert(rule.id.clone(), now());
            reason = "accepted";
            Ok("已入队".into())
        }
        .await;
        let mut result = result.unwrap_or_else(|e| format!("未入队：{e}"));
        if let Some(credit) = &gift_credit {
            result = format!("赠礼增加 {} 次；{result}", credit.added);
            if matched.is_none() {
                reason = "gift_credited";
            }
        }
        if enqueue {
            n.receipts.push(Receipt {
                seq: n.next_seq,
                reason: reason.into(),
                public_name,
                at: now(),
                event: {
                    let mut event = summary(&e);
                    event["test"] = json!(test || e.platform_id == "test");
                    if let Some(credit) = &gift_credit {
                        event["gift_credit"] = serde_json::to_value(credit)?;
                    }
                    event
                },
                rule: matched,
                result: result.clone(),
                item_id: item_id.clone(),
            });
            n.next_seq += 1;
            self.commit(&mut s, n)?;
        }
        Ok(
            json!({"reason":reason,"result":result,"item_id":item_id,"resolved":resolved,"gift_credit":gift_credit}),
        )
    }
    pub async fn control(&self, op: &str, ids: &[String], request_id: &str) -> Result<Value> {
        let _notices = NoticeGuard(self);
        let mut s = self.state.lock().await;
        let mut n = s.clone();
        let mut outcomes = vec![];
        match op {
            "resume" => {
                ensure!(
                    !n.automatic_mode || self.accepting.load(Ordering::SeqCst),
                    "自动直播已停止，请重新开始"
                );
                let t = n.target.as_ref().context("请先选择执行设备和配置包")?;
                self.backend.check(t)?;
                ensure!(
                    !n.items.iter().any(|i| i.state == "review"),
                    "请先核对恢复的运行结果"
                );
                n.paused = false;
                n.blocked = None;
            }
            "unbind" => {
                ensure!(
                    !n.items.iter().any(|i| i.pending() || i.active()),
                    "请先处理当前及等待项"
                );
                n.target = None;
                n.paused = true;
                n.blocked = None;
                n.cooldowns.clear();
            }
            "clear" | "remove" | "stop" => {
                for i in &mut n.items {
                    if (op == "clear" || op == "stop" || ids.contains(&i.id))
                        && (i.pending()
                            || (op == "stop" && i.state == "starting" && i.run_id.is_none()))
                    {
                        i.state = "removed".into();
                        i.finished_at = Some(now());
                        outcomes.push(json!({"id":i.id,"result":"已移除"}));
                    } else if op == "remove" && ids.contains(&i.id) {
                        outcomes.push(json!({"id":i.id,"result":"已开始或已结束，无法移除"}));
                    }
                }
                if op == "stop" {
                    n.paused = n.automatic_mode || n.items.iter().any(Item::active);
                }
                if op == "clear" && !n.items.iter().any(Item::active) {
                    n.paused = false;
                    n.blocked = None;
                }
            }
            "cancel" => {}
            "resolve" => {
                let index = n
                    .items
                    .iter()
                    .position(|i| ids.contains(&i.id) && i.state == "review")
                    .context("没有待核对项")?;
                let i = n.items[index].clone();
                if let Some(run_id) = i.effective_run_id() {
                    let record = self.backend.run(run_id).await?;
                    // Ordinary main-run review keeps its explicit human
                    // acknowledgement path when the old record is unavailable.
                    // Cleanup review must still prove its separate run ended.
                    if i.phase == "cleanup" {
                        ensure!(
                            record
                                .as_ref()
                                .is_some_and(|run| ["success", "failed", "cancelled"]
                                    .contains(&run["state"].as_str().unwrap_or(""))),
                            "无法确认收尾运行终态，不能结束核对"
                        );
                    }
                    if let Some(r) = record {
                        ensure!(
                            !["starting", "running", "stopping"]
                                .contains(&r["state"].as_str().unwrap_or("")),
                            "运行仍在执行，不能结束核对"
                        );
                    }
                    if i.phase == "cleanup" {
                        ensure!(
                            self.backend
                                .active(&i.target.device_id)
                                .is_none_or(|active| active["run_id"].as_str() != Some(run_id)),
                            "运行仍占用设备，不能结束核对"
                        );
                    }
                }
                let confirmed = i.confirmation_spec.is_none()
                    || i.confirmation
                        .as_ref()
                        .is_some_and(|c| c.state == "confirmed");
                let checkpoint_success = if i.phase != "cleanup" && confirmed {
                    if let (Some(spec), Some(run_id)) = (&i.completion_spec, i.run_id.as_deref()) {
                        let record = self
                            .backend
                            .run(run_id)
                            .await?
                            .context("无法确认计费运行终态，不能结束核对")?;
                        ensure!(
                            record["run_id"].as_str() == Some(run_id)
                                && ["success", "failed", "cancelled"]
                                    .contains(&record["state"].as_str().unwrap_or("")),
                            "计费运行尚未确认结束"
                        );
                        ensure!(
                            self.backend
                                .active(&i.target.device_id)
                                .is_none_or(|active| active["run_id"].as_str() != Some(run_id)),
                            "计费运行仍占用设备，不能结束核对"
                        );
                        self.backend.completed_checkpoint(run_id, spec).await?
                    } else {
                        false
                    }
                } else {
                    false
                };
                if checkpoint_success {
                    accounts::settle(&mut n, index, true, "checkpoint_review")?;
                    let item = &mut n.items[index];
                    item.main_result = Some(MainResult {
                        state: "success".into(),
                        error: None,
                        finished_at: now(),
                    });
                    item.cleanup_warning =
                        Some("已确认领奖并结算；人工核对结束，未重新执行收尾".into());
                    item.finish_main();
                } else if i.phase == "cleanup" && i.main_result.is_some() {
                    n.items[index].finish_cleanup(
                        "acknowledged",
                        Some("用户已核对并结束收尾（不代表收尾执行成功）".into()),
                    );
                } else {
                    let i = &mut n.items[index];
                    i.state = "acknowledged".into();
                    i.finished_at = Some(now());
                    i.error = Some("用户已核对并结束此项（不代表执行成功）".into());
                }
            }
            "retry" => {
                ensure!(
                    !n.automatic_mode || self.accepting.load(Ordering::SeqCst),
                    "自动直播已停止，请先开始自动直播或手动输出"
                );
                ensure!(
                    !request_id.is_empty() && request_id.len() <= 100,
                    "重新入队需要请求 ID"
                );
                if let Some(i) = n.items.iter().find(|i| i.request_id == request_id) {
                    return Ok(json!({"item_id":i.id,"result":"已入队"}));
                }
                let original = n
                    .items
                    .iter()
                    .find(|i| ids.contains(&i.id))
                    .context("记录不存在")?;
                ensure!(
                    ["failed", "cancelled", "acknowledged"].contains(&original.state.as_str()),
                    "仅失败、取消或已核对项可重新入队"
                );
                let t = n.target.as_ref().context("缺少执行目标")?;
                self.backend.check(t)?;
                ensure!(
                    *t == original.target,
                    "此历史项属于其他执行目标，不能改派重跑"
                );
                super::rules::validate_entrypoint(&t.package_id, &original.entrypoint)?;
                let args = self
                    .backend
                    .bind(&original.entrypoint, original.args.clone())?;
                let cleanup_spec =
                    self.bind_cleanup(&t.package_id, original.cleanup_spec.as_ref())?;
                ensure!(
                    n.items.iter().filter(|i| i.pending()).count() < 100,
                    "队列已满"
                );
                let mut item = original.clone();
                let prior_billing = item
                    .billing
                    .clone()
                    .context("旧任务缺少计费身份，请重新发送弹幕")?;
                let billing =
                    accounts::reserve_existing(&mut n, &prior_billing.user_id, prior_billing.cost)?;
                item.retry_of = Some(item.id.clone());
                item.id = Uuid::new_v4().to_string();
                item.request_id = request_id.into();
                item.args = args;
                item.state = "waiting".into();
                item.created_at = now();
                item.started_at = None;
                item.finished_at = None;
                item.run_id = None;
                item.error = None;
                item.cleanup_spec = cleanup_spec;
                item.phase.clear();
                item.main_result = None;
                item.cleanup = None;
                item.cleanup_warning = None;
                item.stop_requested = false;
                item.main_timed_out = false;
                item.reconciliation = None;
                item.billing = Some(billing);
                item.confirmation = None;
                item.number = n.next_seq;
                n.receipts.push(Receipt {
                    seq: n.next_seq,
                    reason: "accepted".into(),
                    public_name: item.public_name.clone(),
                    at: item.created_at,
                    event: item.event["message"].clone(),
                    rule: Some(item.name.clone()),
                    result: "手动重试，已排到队尾".into(),
                    item_id: Some(item.id.clone()),
                });
                n.next_seq += 1;
                outcomes.push(json!({"item_id":item.id}));
                n.items.push(item);
            }
            _ => anyhow::bail!("未知队列操作"),
        }
        for index in 0..n.items.len() {
            if ["removed", "acknowledged"].contains(&n.items[index].state.as_str()) {
                accounts::settle(&mut n, index, false, "operator_removed")?;
            }
        }
        if op == "stop" || op == "cancel" {
            if let Some(index) = n
                .items
                .iter()
                .position(|item| item.active() && item.state != "review")
            {
                let cleanup = n.items[index].phase == "cleanup";
                n.items[index].stop_requested = true;
                if cleanup && n.items[index].effective_run_id().is_none() {
                    n.items[index].finish_cleanup("skipped", Some("已取消，未启动收尾脚本".into()));
                } else if n.items[index].state == "starting" && n.items[index].run_id.is_none() {
                    n.items[index].state = "cancelled".into();
                    n.items[index].finished_at = Some(now());
                    accounts::settle(&mut n, index, false, "cancelled_before_start")?;
                }
                if cleanup && op == "cancel" && n.automatic_mode {
                    n.paused = true;
                    n.blocked = Some("收尾已取消，请检查目标状态后恢复队列".into());
                }
            }
        }
        // Stop controls take effect even when persistence fails. No further dispatch can cross this lock.
        let cancel_id = if op == "stop" || op == "cancel" {
            n.items
                .iter()
                .find(|i| i.active() && i.state != "review")
                .and_then(|i| i.effective_run_id().map(str::to_owned))
        } else {
            None
        };
        if op == "stop" {
            s.paused = n.paused;
        }
        let stop_intents: Vec<_> = n
            .items
            .iter()
            .filter(|item| item.stop_requested)
            .map(|item| item.id.clone())
            .collect();
        let saved = self.commit(&mut s, n);
        if saved.is_err() {
            for item in &mut s.items {
                if stop_intents.contains(&item.id) {
                    item.stop_requested = true;
                }
            }
        }
        if let Some(id) = cancel_id {
            self.backend.cancel(&id).await?;
            if let Some(i) = s
                .items
                .iter_mut()
                .find(|i| i.effective_run_id() == Some(id.as_str()))
            {
                i.state = "cancelling".into();
                if i.phase == "cleanup" {
                    i.cleanup.as_mut().unwrap().state = "cancelling".into();
                }
                i.updated_at = now();
            }
            let next = s.clone();
            self.commit(&mut s, next)?;
        }
        saved?;
        Ok(json!({"results":outcomes}))
    }
    pub async fn suspend(&self, clear: bool) {
        self.accepting.store(false, Ordering::SeqCst);
        if clear {
            if let Err(e) = self.control("stop", &[], "").await {
                tracing::error!(error=%e,"直播互动停止未完成");
            }
            return;
        }
        let mut s = self.state.lock().await;
        self.accepting.store(false, Ordering::SeqCst);
        s.paused = true;
        let n = s.clone();
        if let Err(e) = self.commit(&mut s, n) {
            tracing::error!(error=%e,"直播队列停止状态保存失败");
        }
    }
    /// Poll exactly the queue-owned run. Core terminal records are not enough:
    /// its input slot must also be released before cleanup or the next item.
    /// Returns whether a completed item requires a video handoff this tick.
    async fn poll_active(&self, s: &mut State, index: usize) -> bool {
        if self.expire_confirmation(s, index).is_err() {
            return false;
        }
        let item = s.items[index].clone();
        if item.phase == "cleanup" {
            return self.poll_cleanup(s, index).await;
        }
        let Some(id) = item.run_id.as_deref() else {
            return false;
        };
        match self.backend.run(id).await {
            Ok(Some(run))
                if ["success", "failed", "cancelled"]
                    .contains(&run["state"].as_str().unwrap_or(""))
                    && self
                        .backend
                        .active(&item.target.device_id)
                        .is_none_or(|r| r["run_id"].as_str() != Some(id)) =>
            {
                let mut n = s.clone();
                let timed_out =
                    run["state"] == "cancelled" && item.main_timed_out && !item.stop_requested;
                let declined = item
                    .confirmation
                    .as_ref()
                    .is_some_and(Confirmation::declined);
                let confirmed = item.confirmation_spec.is_none()
                    || item
                        .confirmation
                        .as_ref()
                        .is_some_and(|c| c.state == "confirmed");
                let checkpoint = if !declined && confirmed {
                    if let Some(spec) = &item.completion_spec {
                        match self.backend.completed_checkpoint(id, spec).await {
                            Ok(verified) => Some(verified),
                            Err(error) => {
                                n.paused = true;
                                n.items[index].state = "review".into();
                                n.items[index].error =
                                    Some(format!("无法确认完成检查点，请核对：{error}"));
                                let _ = self.commit(s, n);
                                return false;
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                let mut main = MainResult {
                    state: if timed_out {
                        "failed".into()
                    } else {
                        run["state"].as_str().unwrap().into()
                    },
                    error: if timed_out {
                        item.error
                            .clone()
                            .or_else(|| Some("主任务执行超时，已取消".into()))
                    } else {
                        run["error"].as_str().map(str::to_owned)
                    },
                    finished_at: now(),
                };
                if declined {
                    main.state = "cancelled".into();
                    main.error = Some(
                        if item.confirmation.as_ref().unwrap().state == "timeout" {
                            "登录等待超时，未取得本人确认"
                        } else {
                            "观众取消执行"
                        }
                        .into(),
                    );
                    n.items[index].cleanup_spec = item
                        .confirmation_spec
                        .as_ref()
                        .map(|c| c.cancel_cleanup.clone());
                } else if !confirmed {
                    // Preserve the actual login/setup failure. An unconfirmed
                    // successful run still cannot claim success or debit quota.
                    let succeeded = main.state == "success";
                    main.state = "failed".into();
                    if succeeded || main.error.is_none() {
                        main.error = Some("未取得任务发起者本人确认，不能判定成功".into());
                    }
                    n.items[index].cleanup_spec = item
                        .confirmation_spec
                        .as_ref()
                        .map(|c| c.cancel_cleanup.clone());
                } else if let Some(verified) = checkpoint {
                    if verified {
                        main.state = "success".into();
                        main.error = None;
                        if run["state"] != "success" {
                            n.items[index].cleanup_warning =
                                Some("领奖已确认；主运行后续步骤异常".into());
                        }
                    } else if main.state != "cancelled" {
                        main.state = "failed".into();
                        main.error = Some("未取得配置的完成检查点，未扣除次数".into());
                    }
                }
                if let Err(error) = accounts::settle(
                    &mut n,
                    index,
                    main.state == "success",
                    if main.state == "success" {
                        "completed"
                    } else {
                        "main_not_completed"
                    },
                ) {
                    n.paused = true;
                    n.items[index].state = "review".into();
                    n.items[index].error = Some(error.to_string());
                    let _ = self.commit(s, n);
                    return false;
                }
                let cleanup = n.items[index].cleanup_spec.is_some()
                    && (["success", "failed"].contains(&main.state.as_str()) || declined)
                    && !item.stop_requested
                    && self.accepting.load(Ordering::SeqCst);
                let cancelled = main.state == "cancelled";
                let failed = main.state == "failed";
                let i = &mut n.items[index];
                i.main_result = Some(main);
                if cleanup {
                    i.phase = "cleanup".into();
                    i.state = "running".into();
                    i.error = None;
                    i.cleanup = Some(CleanupRun {
                        state: "waiting".into(),
                        run_id: None,
                        error: None,
                        created_at: now(),
                        started_at: None,
                        finished_at: None,
                    });
                } else {
                    if item.cleanup_spec.is_some() && !cancelled {
                        i.cleanup = Some(CleanupRun {
                            state: "skipped".into(),
                            run_id: None,
                            error: Some("已停止，未启动收尾脚本".into()),
                            created_at: now(),
                            started_at: None,
                            finished_at: Some(now()),
                        });
                        i.cleanup_warning = Some("已停止，未启动收尾脚本".into());
                    }
                    i.finish_main();
                }
                if n.automatic_mode
                    && self.accepting.load(Ordering::SeqCst)
                    && cancelled
                    && !declined
                {
                    n.paused = true;
                    n.blocked = Some("执行未完成，请检查目标状态后恢复队列".into());
                }
                let restore = !cleanup && n.automatic_mode && (failed || declined);
                let _ = self.commit(s, n);
                restore
            }
            Ok(Some(run)) => {
                if item.timeout_secs > 0
                    && !["success", "failed", "cancelled"]
                        .contains(&run["state"].as_str().unwrap_or(""))
                    && item.state == "running"
                    && now() - item.started_at.unwrap_or(now()) >= item.timeout_secs as i64 * 1000
                {
                    let mut n = s.clone();
                    match self.backend.cancel(id).await {
                        Ok(()) => {
                            n.items[index].state = "cancelling".into();
                            n.items[index].main_timed_out = true;
                            n.items[index].error = Some(format!(
                                "主任务执行超时（{}秒），正在取消",
                                item.timeout_secs
                            ));
                        }
                        Err(error) => {
                            n.blocked = Some(format!("取消失败：{error}"));
                            n.paused = true;
                        }
                    }
                    let _ = self.commit(s, n);
                }
                false
            }
            _ => {
                let mut n = s.clone();
                n.paused = true;
                n.items[index].state = "review".into();
                n.items[index].error = Some("无法确认运行状态，请核对".into());
                let _ = self.commit(s, n);
                false
            }
        }
    }
    async fn poll_cleanup(&self, s: &mut State, index: usize) -> bool {
        let item = s.items[index].clone();
        let Some(spec) = item.cleanup_spec.as_ref() else {
            let mut n = s.clone();
            n.paused = true;
            n.items[index].state = "review".into();
            n.items[index].cleanup_warning = Some("收尾配置缺失，请核对".into());
            let _ = self.commit(s, n);
            return false;
        };
        let Some(cleanup) = item.cleanup.as_ref() else {
            let mut n = s.clone();
            n.paused = true;
            n.items[index].state = "review".into();
            n.items[index].cleanup_warning = Some("收尾运行状态缺失，请核对".into());
            let _ = self.commit(s, n);
            return false;
        };
        let Some(id) = cleanup.run_id.as_deref() else {
            if item.stop_requested || !self.accepting.load(Ordering::SeqCst) {
                let mut n = s.clone();
                n.items[index].finish_cleanup("skipped", Some("已停止，未启动收尾脚本".into()));
                let _ = self.commit(s, n);
                return true;
            }
            // An unrelated manual owner can win the released slot. Never cancel
            // it, overwrite its run identity or let another viewer pass this one.
            if self.backend.active(&item.target.device_id).is_some() {
                return false;
            }
            let mut n = s.clone();
            match self
                .backend
                .submit_item(&item, &spec.entrypoint, &spec.args)
                .await
            {
                Ok(id) => {
                    let cleanup = n.items[index].cleanup.as_mut().unwrap();
                    cleanup.run_id = Some(id);
                    cleanup.state = "running".into();
                    cleanup.started_at = Some(now());
                    // Keep a submitted identity even if disk recording fails so
                    // stop still cancels and waits for the actual owned run.
                    let fallback = n.clone();
                    if self.commit(s, n).is_err() {
                        let blocked = s.blocked.clone();
                        *s = fallback;
                        s.paused = true;
                        s.blocked = blocked;
                    }
                    return false;
                }
                Err(SubmitError::Busy) => return false,
                Err(SubmitError::Blocked(error) | SubmitError::Failed(error)) => {
                    n.items[index].finish_cleanup("failed", Some(error));
                    if item.confirmation_spec.is_some() {
                        n.paused = true;
                        n.blocked = Some("登录退出未完成，请核对目标账号状态后恢复队列".into());
                    }
                    let _ = self.commit(s, n);
                    return true;
                }
            }
        };
        match self.backend.run(id).await {
            Ok(Some(run))
                if ["success", "failed", "cancelled"]
                    .contains(&run["state"].as_str().unwrap_or(""))
                    && self
                        .backend
                        .active(&item.target.device_id)
                        .is_none_or(|r| r["run_id"].as_str() != Some(id)) =>
            {
                let mut n = s.clone();
                let state = run["state"].as_str().unwrap();
                let error = run["error"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| cleanup.error.clone());
                n.items[index].finish_cleanup(state, error);
                if state != "success" && item.confirmation_spec.is_some() {
                    n.paused = true;
                    n.blocked = Some("登录退出未完成，请核对目标账号状态后恢复队列".into());
                }
                let _ = self.commit(s, n);
                state != "success"
                    || item
                        .main_result
                        .as_ref()
                        .is_some_and(|main| main.state == "failed")
            }
            Ok(Some(run)) => {
                if !["success", "failed", "cancelled"]
                    .contains(&run["state"].as_str().unwrap_or(""))
                    && cleanup.state == "running"
                    && now() - cleanup.started_at.unwrap_or(now())
                        >= spec.timeout_secs as i64 * 1000
                {
                    let mut n = s.clone();
                    match self.backend.cancel(id).await {
                        Ok(()) => {
                            let cleanup = n.items[index].cleanup.as_mut().unwrap();
                            cleanup.state = "cancelling".into();
                            cleanup.error = Some("收尾超时，正在取消".into());
                            n.items[index].state = "cancelling".into();
                        }
                        Err(error) => {
                            n.blocked = Some(format!("收尾取消失败：{error}"));
                            n.paused = true;
                        }
                    }
                    let _ = self.commit(s, n);
                }
                false
            }
            _ => {
                let mut n = s.clone();
                n.paused = true;
                n.items[index].state = "review".into();
                n.items[index].cleanup_warning = Some("无法确认收尾运行状态，请核对".into());
                let _ = self.commit(s, n);
                false
            }
        }
    }
    pub(super) async fn tick(&self) {
        let _notices = NoticeGuard(self);
        let _dispatch = self.dispatch_gate.lock().await;
        let mut s = self.state.lock().await;
        if let Some(index) = s.items.iter().position(|i| i.active()) {
            let item = s.items[index].clone();
            if item.state == "review" {
                return;
            }
            let restore_after_failure = self.poll_active(&mut s, index).await;
            let active = s.items.iter().any(Item::active);
            let idle = !active && !s.items.iter().any(Item::pending);
            let blocked = s.paused.then(|| s.blocked.clone()).flatten();
            drop(s);
            // A failed cancellation can pause admission while the original run
            // still owns input. Keep its live picture until terminal cleanup
            // is confirmed; restoring video must not hide an ongoing task.
            if idle || (!active && (restore_after_failure || blocked.is_some())) {
                self.display_idle(blocked).await;
            }
            return;
        }
        if s.paused {
            let error = s.blocked.clone();
            drop(s);
            self.display_idle(error).await;
            return;
        }
        let Some(index) = s.items.iter().position(Item::pending) else {
            drop(s);
            self.display_idle(None).await;
            return;
        };
        let Some(target) = s.target.clone() else {
            return;
        };
        if let Err(e) = self.backend.check(&target) {
            let message = e.to_string();
            if s.blocked.as_deref() != Some(&message) {
                let mut n = s.clone();
                n.blocked = Some(message);
                n.paused = true;
                let _ = self.commit(&mut s, n);
            }
            let error = s.blocked.clone();
            drop(s);
            self.display_idle(error).await;
            return;
        }
        if self.backend.active(&target.device_id).is_some() {
            drop(s);
            self.display_idle(None).await;
            return;
        }
        let item = s.items[index].clone();
        if item.target != target {
            let mut n = s.clone();
            n.paused = true;
            n.blocked = Some("等待项与当前目标不一致，请停止并重新绑定".into());
            let _ = self.commit(&mut s, n);
            let error = s.blocked.clone();
            drop(s);
            self.display_idle(error).await;
            return;
        }
        let mut n = s.clone();
        n.blocked = None;
        n.items[index].state = "starting".into();
        n.items[index].started_at = Some(now());
        if self.commit(&mut s, n).is_err() {
            return;
        }
        drop(s);
        // Frame preparation and the source handoff may take seconds. Keep the
        // admission lock free throughout, then recheck this exact queue item.
        let prepared = if let Some(hooks) = self.hooks() {
            hooks.before_submit(&target).await
        } else {
            Ok(())
        };
        let mut s = self.state.lock().await;
        let Some(index) = s.items.iter().position(|i| i.id == item.id) else {
            return;
        };
        if !self.accepting.load(Ordering::SeqCst)
            || s.paused
            || s.items[index].state != "starting"
            || s.items[index].stop_requested
            || s.target.as_ref() != Some(&target)
        {
            // A different admission/control failure may have paused the queue
            // while its frame was preparing. No Core run exists yet, so leave
            // this item retryable rather than stranded in starting forever.
            if s.items[index].state == "starting" && s.items[index].run_id.is_none() {
                let mut n = s.clone();
                n.items[index].state = "waiting".into();
                n.items[index].started_at = None;
                n.paused = true;
                if self.commit(&mut s, n).is_err() {
                    s.items[index].state = "waiting".into();
                    s.items[index].started_at = None;
                }
            }
            let error = s.blocked.clone();
            drop(s);
            self.display_idle(error).await;
            return;
        }
        let mut n = s.clone();
        if let Err(e) = prepared.and_then(|_| self.backend.check(&target)) {
            n.items[index].state = "waiting".into();
            n.items[index].started_at = None;
            n.paused = true;
            n.blocked = Some(e.to_string());
            if self.commit(&mut s, n).is_err() {
                s.items[index].state = "waiting".into();
                s.items[index].started_at = None;
            }
            let error = s.blocked.clone();
            drop(s);
            self.display_idle(error).await;
            return;
        }
        if self.backend.active(&target.device_id).is_some() {
            n.items[index].state = "waiting".into();
            n.items[index].started_at = None;
            if self.commit(&mut s, n).is_err() {
                s.items[index].state = "waiting".into();
                s.items[index].started_at = None;
            }
            drop(s);
            self.display_idle(None).await;
            return;
        }
        let mut n = s.clone();
        let mut executing = false;
        let mut restore = false;
        match self
            .backend
            .submit_item(&item, &item.entrypoint, &item.args)
            .await
        {
            Ok(id) => {
                n.items[index].run_id = Some(id);
                n.items[index].state = "running".into();
                n.items[index].started_at = Some(now());
                executing = true;
            }
            Err(SubmitError::Busy) => {
                n.items[index].state = "waiting".into();
                n.items[index].started_at = None;
                restore = true;
            }
            Err(SubmitError::Blocked(e)) => {
                n.items[index].state = "waiting".into();
                n.items[index].started_at = None;
                n.blocked = Some(e);
                n.paused = true;
                restore = true;
            }
            Err(SubmitError::Failed(e)) => {
                n.items[index].state = "failed".into();
                n.items[index].error = Some(e);
                n.items[index].finished_at = Some(now());
                if let Err(error) = accounts::settle(&mut n, index, false, "submit_failed") {
                    n.paused = true;
                    n.blocked = Some(error.to_string());
                }
                restore = n.automatic_mode || !n.items.iter().any(|i| i.pending());
            }
        }
        // Keep the known run identity in memory even if recording the submission fails.
        let fallback = n.clone();
        if self.commit(&mut s, n).is_err() {
            let error = s.blocked.clone();
            *s = fallback;
            s.paused = true;
            s.blocked = error;
        }
        let error = s.blocked.clone();
        drop(s);
        if executing {
            if let Some(hooks) = self.hooks() {
                hooks.executing();
            }
        } else if restore {
            self.display_idle(error).await;
        }
    }
}

#[cfg(test)]
#[path = "queue/tests.rs"]
mod tests;
