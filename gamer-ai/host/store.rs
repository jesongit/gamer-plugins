use crate::core::fs::atomic_write;
use anyhow::{ensure, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    pub session_micros: u64,
    pub daily_micros: u64,
    pub global_micros: u64,
    pub request_micros: u64,
    pub max_rounds: u32,
    pub max_searches: u32,
    pub max_active_secs: u64,
    pub max_tokens: u64,
    pub concurrency: usize,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            session_micros: 1_000_000,
            daily_micros: 5_000_000,
            global_micros: 20_000_000,
            request_micros: 50_000,
            max_rounds: 40,
            max_searches: 4,
            max_active_secs: 600,
            max_tokens: 100_000,
            concurrency: 2,
        }
    }
}
impl Budget {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.max_rounds > 0
                && self.max_rounds <= 1000
                && self.max_active_secs > 0
                && self.max_active_secs <= 3600
                && self.concurrency > 0
                && self.concurrency <= 8
                && self.max_tokens > 0,
            "预算轮数、时长、token 或并发上限无效"
        );
        ensure!(
            self.request_micros <= self.session_micros && self.session_micros <= self.global_micros,
            "请求预算必须在会话及全局预算以内"
        );
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Event {
    pub seq: u64,
    pub run_id: String,
    pub at: i64,
    pub kind: String,
    pub data: Value,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Session {
    pub id: String,
    #[serde(default)]
    pub entrypoint: String,
    pub package: String,
    pub generation: String,
    pub app: String,
    pub goal: String,
    pub plan_version: String,
    pub profile_version: String,
    pub account: Option<String>,
    pub cycle: Option<String>,
    pub runs: Vec<String>,
    pub state: String,
    pub progress: Value,
    pub budget: Budget,
    pub rounds: u32,
    pub searches: u32,
    pub active_ms: u64,
    #[serde(default)]
    pub active_checkpoint: Option<i64>,
    pub tokens: u64,
    pub expires_at: i64,
    pub events: Vec<Event>,
    pub operations: BTreeMap<String, Value>,
    pub notified: BTreeMap<String, Value>,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct RequestRecord {
    pub id: String,
    pub session: String,
    pub run_id: String,
    pub profile: String,
    pub model: String,
    pub price_version: String,
    pub kind: String,
    pub day: String,
    pub reserved: u64,
    pub actual: Option<u64>,
    pub source: String,
    pub status: String,
    pub usage: Value,
    pub at: i64,
    pub duration_ms: u64,
}
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Navigation,
    RegenerativeResource,
    Item,
    Currency,
    Paid,
    Unknown,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Consumption {
    pub category: Category,
    #[serde(default)]
    pub resource: String,
    #[serde(default)]
    pub quantity: Option<u64>,
    pub purpose: String,
    pub evidence: String,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Approval {
    pub id: String,
    pub session: String,
    pub operation: String,
    pub observation: String,
    pub consumption: Consumption,
    pub status: String,
    pub reason: String,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Rule {
    pub id: String,
    pub version: String,
    pub app: String,
    pub account: Option<String>,
    pub purpose: String,
    pub resource: String,
    pub category: Category,
    pub scope: String,
    pub session: Option<String>,
    pub operation: Option<String>,
    pub cycle: Option<String>,
    pub limit: u64,
    pub expires_at: i64,
    pub revoked: bool,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Spend {
    pub operation: String,
    pub session: String,
    pub rule: String,
    pub version: String,
    pub quantity: u64,
    pub status: String,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Verification {
    #[serde(default)]
    pub resource_instance: String,
    pub package: String,
    pub generation: String,
    pub path: String,
    pub hash: String,
    pub session: String,
    pub observation: String,
    pub conditions: String,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Credential {
    pub id: String,
    pub digest: String,
    pub expires_at: i64,
    pub revoked: bool,
    pub package: String,
    pub app: String,
    pub device: String,
    pub tools: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Data {
    pub schema: u32,
    pub revision: u64,
    pub sessions: BTreeMap<String, Session>,
    pub requests: BTreeMap<String, RequestRecord>,
    pub approvals: BTreeMap<String, Approval>,
    pub rules: BTreeMap<String, Rule>,
    pub spends: Vec<Spend>,
    pub verifications: Vec<Verification>,
    pub credentials: BTreeMap<String, Credential>,
    #[serde(default)]
    pub mcp_runs: BTreeMap<String, String>,
    #[serde(default)]
    pub mcp_sessions: BTreeMap<String, String>,
    #[serde(default)]
    pub failures: Vec<Value>,
    pub completions: Vec<Value>,
    pub reconciliation: Vec<Value>,
}
pub struct Repository {
    pub root: PathBuf,
    pub data: Mutex<Data>,
}
impl Repository {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let path = root.join("state.json");
        let mut data: Data = match std::fs::read(&path) {
            Ok(bytes) => {
                ensure!(bytes.len() <= 32 * 1024 * 1024, "AI 状态文件过大");
                serde_json::from_slice(&bytes).context("AI 账本损坏，禁止重置预算")?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Data {
                schema: 1,
                ..Default::default()
            },
            Err(e) => return Err(e.into()),
        };
        ensure!(data.schema == 1, "AI 状态版本不支持");
        for request in data.requests.values_mut() {
            if request.status == "reserved" {
                request.status = "pending_reconciliation".into();
            }
        }
        for s in data.sessions.values_mut() {
            if s.state == "running" {
                s.state = "partial".into();
                if let Some(at) = s.active_checkpoint.take() {
                    let elapsed = chrono::Utc::now()
                        .timestamp_millis()
                        .saturating_sub(at)
                        .max(0) as u64;
                    s.active_ms = s
                        .active_ms
                        .saturating_add(elapsed)
                        .min(s.budget.max_active_secs * 1000);
                }
            }
        }
        atomic_write(&path, &serde_json::to_vec(&data)?)?;
        Ok(Self {
            root: root.into(),
            data: Mutex::new(data),
        })
    }
    pub fn transaction<T>(&self, update: impl FnOnce(&mut Data) -> Result<T>) -> Result<T> {
        let mut saved = self.data.lock();
        let mut next = saved.clone();
        let value = update(&mut next)?;
        next.revision += 1;
        let bytes = serde_json::to_vec(&next)?;
        ensure!(
            bytes.len() <= 32 * 1024 * 1024,
            "AI 状态达到保留容量；原账本保留"
        );
        atomic_write(&self.root.join("state.json"), &bytes)?;
        *saved = next;
        Ok(value)
    }
    pub fn checkpoint(&self, session: &str, now: i64, finish: bool) -> Result<()> {
        {
            let data = self.data.lock();
            let s = data.sessions.get(session).context("会话不存在")?;
            if !finish
                && s.active_checkpoint
                    .is_some_and(|at| now.saturating_sub(at) < 1000)
            {
                return Ok(());
            }
        }
        self.transaction(|data| {
            let s = data.sessions.get_mut(session).context("会话不存在")?;
            if let Some(at) = s.active_checkpoint {
                s.active_ms = s
                    .active_ms
                    .saturating_add(now.saturating_sub(at).max(0) as u64);
            }
            s.active_checkpoint = if finish { None } else { Some(now) };
            Ok(())
        })
    }
    pub fn event(&self, session: &str, run: &str, at: i64, kind: &str, data: Value) -> Result<()> {
        self.transaction(|state| {
            let s = state.sessions.get_mut(session).context("会话不存在")?;
            ensure!(s.events.len() < 10_000, "会话事件达到容量上限");
            s.events.push(Event {
                seq: s.events.last().map_or(1, |e| e.seq + 1),
                run_id: run.into(),
                at,
                kind: kind.into(),
                data,
            });
            Ok(())
        })
    }
    pub fn reserve(&self, mut request: RequestRecord, now: i64) -> Result<String> {
        self.transaction(|data| {
            let session = data.sessions.get(&request.session).context("会话不存在")?;
            let b = session.budget.clone();
            ensure!(
                session.expires_at > now
                    && session.active_ms < b.max_active_secs * 1000
                    && session.rounds < b.max_rounds
                    && session.tokens < b.max_tokens,
                "budget_exhausted: 会话轮数、token、时长或有效期已达到上限"
            );
            let outstanding = |r: &RequestRecord| r.actual.unwrap_or(r.reserved);
            let session_total: u64 = data
                .requests
                .values()
                .filter(|r| r.session == session.id)
                .map(outstanding)
                .fold(0u64, u64::saturating_add);
            let day_total: u64 = data
                .requests
                .values()
                .filter(|r| r.day == request.day || r.actual.is_none())
                .map(outstanding)
                .fold(0u64, u64::saturating_add);
            let total: u64 = data
                .requests
                .values()
                .map(outstanding)
                .fold(0u64, u64::saturating_add);
            ensure!(
                session_total.saturating_add(request.reserved) <= b.session_micros
                    && day_total.saturating_add(request.reserved) <= b.daily_micros
                    && total.saturating_add(request.reserved) <= b.global_micros,
                "budget_exhausted: 金额预留超出预算"
            );
            ensure!(
                data.requests
                    .values()
                    .filter(|r| r.status == "reserved")
                    .count()
                    < b.concurrency,
                "budget_concurrency: 模型请求并发已满"
            );
            let session = data.sessions.get_mut(&request.session).unwrap();
            session.rounds += 1;
            if request.kind == "search" {
                ensure!(
                    session.searches < b.max_searches,
                    "budget_exhausted: 搜索轮数达到上限"
                );
                session.searches += 1;
            }
            request.status = "reserved".into();
            let key = request.id.clone();
            data.requests.insert(key.clone(), request);
            Ok(key)
        })
    }
    pub fn settle(
        &self,
        id: &str,
        cost: Option<u64>,
        usage: Value,
        source: &str,
        elapsed: u64,
    ) -> Result<()> {
        self.transaction(|data| {
            let r = data.requests.get_mut(id).context("请求不存在")?;
            ensure!(r.actual.is_none(), "请求已结算");
            r.actual = cost;
            r.usage = usage.clone();
            r.duration_ms = elapsed;
            r.source = source.into();
            r.status = if cost.is_some() {
                "settled"
            } else {
                "pending_reconciliation"
            }
            .into();
            let s = data.sessions.get_mut(&r.session).unwrap();
            s.tokens = s
                .tokens
                .saturating_add(usage["total_tokens"].as_u64().unwrap_or(0));
            Ok(())
        })
    }
    pub fn gate(
        &self,
        session: &str,
        operation: &str,
        observation: &str,
        consumption: Consumption,
        now: i64,
    ) -> Result<bool> {
        ensure!(
            !consumption.evidence.trim().is_empty() && !consumption.purpose.trim().is_empty(),
            "consumption_unknown: 缺少画面依据或用途"
        );
        if matches!(
            consumption.category,
            Category::Navigation | Category::RegenerativeResource
        ) {
            return Ok(true);
        }
        self.transaction(|data| {
            let s = data.sessions.get(session).context("会话不存在")?;
            ensure!(!data.spends.iter().any(|p|p.session==session&&p.operation!=operation&&s.operations.get(&p.operation).is_some_and(|op|op["status"]=="outcome_unknown")&&data.rules.get(&p.rule).is_some_and(|r|r.category==consumption.category&&r.resource==consumption.resource&&r.purpose==consumption.purpose)),"outcome_unknown: 上次同用途消耗结果未知，先完成其他部分并人工核实，不能换 operation_id 重试");
            if data
                .spends
                .iter()
                .any(|p| p.session == session && p.operation == operation)
            {
                return Ok(true);
            }
            let matched = consumption
                .quantity
                .filter(|q| *q > 0)
                .and_then(|quantity| {
                    data.rules
                        .values()
                        .find(|r| {
                            let scope_ok = match r.scope.as_str() {
                                "operation" => {
                                    r.session.as_deref() == Some(session)
                                        && r.operation.as_deref() == Some(operation)
                                }
                                "session" => r.session.as_deref() == Some(session),
                                "cycle" => {
                                    s.account.is_some()
                                        && s.cycle.is_some()
                                        && r.account == s.account
                                        && r.cycle == s.cycle
                                }
                                "persistent" => s.account.is_some() && r.account == s.account,
                                _ => false,
                            };
                            let used: u64 = data
                                .spends
                                .iter()
                                .filter(|p| p.rule == r.id && p.version == r.version)
                                .map(|p| p.quantity)
                                .fold(0u64, u64::saturating_add);
                            scope_ok
                                && !r.revoked
                                && r.expires_at > now
                                && r.app == s.app
                                && r.category == consumption.category
                                && r.resource == consumption.resource
                                && r.purpose == consumption.purpose
                                && used.saturating_add(quantity) <= r.limit
                        })
                        .map(|r| (r.id.clone(), r.version.clone(), quantity))
                });
            if let Some((rule, version, quantity)) = matched {
                data.spends.push(Spend {
                    operation: operation.into(),
                    session: session.into(),
                    rule,
                    version,
                    quantity,
                    status: "reserved".into(),
                });
                return Ok(true);
            }
            if !data.approvals.values().any(|p| {
                p.session == session
                    && p.consumption.resource == consumption.resource
                    && p.consumption.purpose == consumption.purpose
                    && p.consumption.category == consumption.category
                    && p.consumption.quantity == consumption.quantity
                    && p.status == "pending"
            }) {
                let key = id();
                data.approvals.insert(
                    key.clone(),
                    Approval {
                        id: key,
                        session: session.into(),
                        operation: operation.into(),
                        observation: observation.into(),
                        consumption,
                        status: "pending".into(),
                        reason: "先完成其他允许部分，再汇总授权待办".into(),
                    },
                );
            }
            Ok(false)
        })
    }
    pub fn totals(&self, session: &str) -> Value {
        let d = self.data.lock();
        let rs: Vec<_> = d
            .requests
            .values()
            .filter(|r| r.session == session)
            .collect();
        json!({"estimated_micros":rs.iter().filter(|r|r.source.starts_with("estimated")).filter_map(|r|r.actual).fold(0u64,u64::saturating_add),"confirmed_micros":rs.iter().filter(|r|r.source=="provider"||r.source=="reconciled").filter_map(|r|r.actual).fold(0u64,u64::saturating_add),"pending_micros":rs.iter().filter(|r|r.actual.is_none()).map(|r|r.reserved).fold(0u64,u64::saturating_add),"requests":rs})
    }
}
