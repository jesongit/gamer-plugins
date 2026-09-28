//! Process-owned, durable FIFO. One lock serializes admission, controls and dispatch.
use super::{events::LiveEvent, rules::RuleSet};
use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::Mutex;
use uuid::Uuid;

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
    async fn run(&self, id: &str) -> Result<Option<Value>>;
    async fn cancel(&self, id: &str) -> Result<()>;
    fn active(&self, device: &str) -> Option<Value>;
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Item {
    pub target: Target,
    pub id: String,
    pub request_id: String,
    pub rule_id: String,
    #[serde(default)]
    pub rule_version: Option<String>,
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
}
impl Item {
    fn pending(&self) -> bool {
        self.state == "waiting"
    }
    fn active(&self) -> bool {
        ["starting", "running", "cancelling", "review"].contains(&self.state.as_str())
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
        }
    }
}
pub struct Queue {
    state: Mutex<State>,
    path: PathBuf,
    backend: Arc<dyn Backend>,
    accepting: AtomicBool,
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
        "received_at":e.received_at,"occurred_at":e.occurred_at})
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
        let queue = Arc::new(Self {
            state: Mutex::new(state),
            path,
            backend,
            accepting: AtomicBool::new(true),
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
        let previous: BTreeMap<_, _> = current
            .items
            .iter()
            .map(|i| (i.id.as_str(), i.state.as_str()))
            .collect();
        for i in &mut next.items {
            if previous
                .get(i.id.as_str())
                .is_none_or(|old| *old != i.state)
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
        json!({"revision":s.revision,"target":s.target,"paused":s.paused,"blocked":s.blocked,
            "current":current,"waiting":waiting,"capacity":100,"history":history.iter().skip(offset).take(30).collect::<Vec<_>>(),"has_more":history.len()>offset+30,
            "receipts":s.receipts.iter().rev().take(100).collect::<Vec<_>>(),
            "device_run":s.target.as_ref().and_then(|t|self.backend.active(&t.device_id))})
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
                "rejected" => related && r.item_id.is_none(),
                "errors" => item.is_some_and(|i| ["failed", "review"].contains(&i.state.as_str())),
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
        let item = |i: &Item, position: usize| {
            json!({"number":i.number,
            "name":if i.public_name.is_empty(){&i.name}else{&i.public_name},
            "viewer":i.event["message"]["actor"]["name"].as_str().unwrap_or("观众"),
            "state":i.state,"position":position,"started_at":i.started_at,
            "changed_at":i.updated_at.max(i.finished_at.or(i.started_at).unwrap_or(i.created_at)),"test":i.event["test"] == true})
        };
        let waiting: Vec<_> = s.items.iter().filter(|i| i.pending()).collect();
        let current = s.items.iter().find(|i| i.active());
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
            "waiting":waiting.iter().enumerate().take(10).map(|(p,i)|item(i,p+1)).collect::<Vec<_>>(),
            "waiting_count":waiting.len(),"device_busy":s.target.as_ref().is_some_and(|t|self.backend.active(&t.device_id).is_some()) && current.is_none(),
            "notices":notices})
    }
    pub async fn configure(&self, device: &str, package: &str) -> Result<()> {
        let target = self.backend.target(device, package)?;
        let mut s = self.state.lock().await;
        ensure!(
            !s.items.iter().any(|i| i.pending() || i.active()),
            "请先处理当前及等待项，再更换目标"
        );
        let mut n = s.clone();
        n.target = Some(target);
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
        n.paused = false;
        n.blocked = None;
        self.commit(&mut s, n)?;
        self.accepting.store(true, Ordering::SeqCst);
        Ok(())
    }
    pub fn read_rules(&self, package: &str) -> Result<Value> {
        let (rules, version) = self.backend.rules(package)?;
        Ok(json!({"rules":rules.rules,"schema_version":1,"version":version}))
    }
    pub fn save_rules(
        &self,
        package: &str,
        rules: RuleSet,
        version: Option<&str>,
    ) -> Result<Value> {
        rules.validate(package)?;
        for r in rules.rules.iter().filter(|r| r.enabled) {
            self.backend.describe(&r.entrypoint)?;
            let sample = LiveEvent {
                schema_version: 1,
                seq: 0,
                platform_id: String::new(),
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
        }
        let version = self.backend.save_rules(package, &rules, version)?;
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
        self.accept(event, enqueue, true, request_id).await
    }
    pub async fn receive(&self, event: LiveEvent) {
        if !self.accepting.load(Ordering::SeqCst) {
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
        let mut s = self.state.lock().await;
        if !test && !self.accepting.load(Ordering::SeqCst) {
            return Ok(json!({"result":"直播连接已停止"}));
        }
        if test && enqueue {
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
                "{}:{}:{}:{}:{}",
                n.session, e.platform_id, e.room_id, e.kind, id
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
        let result: Result<String> = async {
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
            let target = n.target.as_ref().context("请先绑定设备和配置包")?;
            reason = "rules_invalid";
            let (rules, rule_version) = self.backend.rules(&target.package_id)?;
            rules.validate(&target.package_id)?;
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
            self.backend.check(target)?;
            reason = "invalid_args";
            let args = self.backend.bind(&rule.entrypoint, rule.bind(&e)?)?;
            ensure!(
                serde_json::to_vec(&args)?.len() <= 16 * 1024,
                "参数总大小超过 16 KiB，未入队"
            );
            resolved = Some(
                json!({"entrypoint":rule.entrypoint,"args":args,"target":target,"rule":rule.name}),
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
            let id = Uuid::new_v4().to_string();
            item_id = Some(id.clone());
            n.items.push(Item {
                target: target.clone(),
                id,
                request_id: request_id.into(),
                rule_id: rule.id.clone(),
                rule_version,
                name: rule.name.clone(),
                public_name: public_name.clone(),
                number: n.next_seq,
                entrypoint: rule.entrypoint.clone(),
                args,
                event: json!({"test":test,"session":n.session,"message":summary(&e)}),
                state: "waiting".into(),
                created_at: now(),
                updated_at: now(),
                started_at: None,
                finished_at: None,
                run_id: None,
                error: None,
                timeout_secs: rule.timeout_secs,
                retry_of: None,
            });
            n.cooldowns.insert(rule.id.clone(), now());
            reason = "accepted";
            Ok("已入队".into())
        }
        .await;
        let result = result.unwrap_or_else(|e| format!("未入队：{e}"));
        if enqueue {
            n.receipts.push(Receipt {
                seq: n.next_seq,
                reason: reason.into(),
                public_name,
                at: now(),
                event: summary(&e),
                rule: matched,
                result: result.clone(),
                item_id: item_id.clone(),
            });
            n.next_seq += 1;
            self.commit(&mut s, n)?;
        }
        Ok(json!({"reason":reason,"result":result,"item_id":item_id,"resolved":resolved}))
    }
    pub async fn control(&self, op: &str, ids: &[String], request_id: &str) -> Result<Value> {
        let mut s = self.state.lock().await;
        let mut n = s.clone();
        let mut outcomes = vec![];
        match op {
            "resume" => {
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
                    if (op == "clear" || op == "stop" || ids.contains(&i.id)) && i.pending() {
                        i.state = "removed".into();
                        i.finished_at = Some(now());
                        outcomes.push(json!({"id":i.id,"result":"已移除"}));
                    } else if op == "remove" && ids.contains(&i.id) {
                        outcomes.push(json!({"id":i.id,"result":"已开始或已结束，无法移除"}));
                    }
                }
                if op == "stop" {
                    n.paused = n.items.iter().any(Item::active);
                }
                if op == "clear" && !n.items.iter().any(Item::active) {
                    n.paused = false;
                    n.blocked = None;
                }
            }
            "cancel" => {}
            "resolve" => {
                let i = n
                    .items
                    .iter_mut()
                    .find(|i| ids.contains(&i.id) && i.state == "review")
                    .context("没有待核对项")?;
                if let Some(run_id) = &i.run_id {
                    if let Some(r) = self.backend.run(run_id).await? {
                        ensure!(
                            !["starting", "running", "stopping"]
                                .contains(&r["state"].as_str().unwrap_or("")),
                            "运行仍在执行，不能结束核对"
                        );
                    }
                }
                i.state = "acknowledged".into();
                i.finished_at = Some(now());
                i.error = Some("用户已核对并结束此项（不代表执行成功）".into());
            }
            "retry" => {
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
                ensure!(
                    n.items.iter().filter(|i| i.pending()).count() < 100,
                    "队列已满"
                );
                let mut item = original.clone();
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
        // Stop controls take effect even when persistence fails. No further dispatch can cross this lock.
        let cancel_id = if op == "stop" || op == "cancel" {
            n.items
                .iter()
                .find(|i| i.active() && i.state != "review")
                .and_then(|i| i.run_id.clone())
        } else {
            None
        };
        if op == "stop" {
            s.paused = n.paused;
        }
        let saved = self.commit(&mut s, n);
        if let Some(id) = cancel_id {
            self.backend.cancel(&id).await?;
            if let Some(i) = s
                .items
                .iter_mut()
                .find(|i| i.run_id.as_deref() == Some(&id))
            {
                i.state = "cancelling".into();
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
        s.paused = true;
        let n = s.clone();
        if let Err(e) = self.commit(&mut s, n) {
            tracing::error!(error=%e,"直播队列停止状态保存失败");
        }
    }
    async fn tick(&self) {
        let mut s = self.state.lock().await;
        if let Some(index) = s.items.iter().position(|i| i.active()) {
            let item = s.items[index].clone();
            if item.state == "review" {
                return;
            }
            let Some(id) = item.run_id else {
                return;
            };
            match self.backend.run(&id).await {
                Ok(Some(run))
                    if ["success", "failed", "cancelled"]
                        .contains(&run["state"].as_str().unwrap_or("")) =>
                {
                    let mut n = s.clone();
                    let i = &mut n.items[index];
                    i.state = run["state"].as_str().unwrap().into();
                    i.error = run["error"].as_str().map(str::to_owned);
                    i.finished_at = Some(now());
                    let _ = self.commit(&mut s, n);
                }
                Ok(Some(_)) => {
                    if item.timeout_secs > 0
                        && item.state == "running"
                        && now() - item.started_at.unwrap_or(now())
                            >= item.timeout_secs as i64 * 1000
                    {
                        match self.backend.cancel(&id).await {
                            Ok(()) => {
                                let mut n = s.clone();
                                n.items[index].state = "cancelling".into();
                                n.items[index].error = Some("超时，正在取消".into());
                                let _ = self.commit(&mut s, n);
                            }
                            Err(e) => {
                                let mut n = s.clone();
                                n.blocked = Some(format!("取消失败：{e}"));
                                n.paused = true;
                                let _ = self.commit(&mut s, n);
                            }
                        }
                    }
                }
                _ => {
                    let mut n = s.clone();
                    n.paused = true;
                    n.items[index].state = "review".into();
                    n.items[index].error = Some("无法确认运行状态，请核对".into());
                    let _ = self.commit(&mut s, n);
                }
            }
            return;
        }
        if s.paused {
            return;
        }
        let Some(index) = s.items.iter().position(Item::pending) else {
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
            return;
        }
        if self.backend.active(&target.device_id).is_some() {
            return;
        }
        let item = s.items[index].clone();
        if item.target != target {
            let mut n = s.clone();
            n.paused = true;
            n.blocked = Some("等待项与当前目标不一致，请停止并重新绑定".into());
            let _ = self.commit(&mut s, n);
            return;
        }
        let mut n = s.clone();
        n.blocked = None;
        n.items[index].state = "starting".into();
        n.items[index].started_at = Some(now());
        if self.commit(&mut s, n).is_err() {
            return;
        }
        let mut n = s.clone();
        match self
            .backend
            .submit(&target, &item.entrypoint, &item.args)
            .await
        {
            Ok(id) => {
                n.items[index].run_id = Some(id);
                n.items[index].state = "running".into();
                n.items[index].started_at = Some(now());
            }
            Err(SubmitError::Busy) => {
                n.items[index].state = "waiting".into();
                n.items[index].started_at = None;
            }
            Err(SubmitError::Blocked(e)) => {
                n.items[index].state = "waiting".into();
                n.items[index].started_at = None;
                n.blocked = Some(e);
                n.paused = true;
            }
            Err(SubmitError::Failed(e)) => {
                n.items[index].state = "failed".into();
                n.items[index].error = Some(e);
                n.items[index].finished_at = Some(now());
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
    }
}

#[cfg(test)]
mod tests;
