//! Independent, local-only speaking credentials and a bounded best-effort outbox.
use super::queue::NoticeSink;
use anyhow::{ensure, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{sync::Notify, task::JoinHandle};

const ENDPOINT: &str = "https://api.live.bilibili.com/msg/send";
const PREFIX: &str = "【Gamer】";
const MAX_TEXT: usize = 40;
const MAX_PENDING: usize = 32;
const INTERVAL: Duration = Duration::from_secs(3);
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    version: Option<String>,
    enabled: bool,
    room_id: String,
    session_cookie: String,
    csrf_token: String,
    templates: BTreeMap<String, String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            version: None,
            enabled: false,
            room_id: String::new(),
            session_cookie: String::new(),
            csrf_token: String::new(),
            templates: [
                ("accepted", "{viewer}已排队#{number}"),
                ("quota_rejected", "{viewer}次数不足"),
                (
                    "confirmation_required",
                    "{viewer}登录后请发确认、取消或扫码",
                ),
                ("scan_required", "{viewer}请在5分钟内扫码，可发取消或扫码"),
                ("rescan_requested", "{viewer}正在重新扫码，等待期限不延长"),
                ("cancelled", "{viewer}已取消，次数退回"),
                ("timeout", "{viewer}登录等待超时，次数退回"),
                ("success", "{viewer}领奖成功，扣{cost}次"),
                ("failed", "{viewer}任务失败，次数退回"),
            ]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect(),
        }
    }
}
impl Config {
    fn ready(&self) -> bool {
        !self.room_id.is_empty() && !self.session_cookie.is_empty() && !self.csrf_token.is_empty()
    }
    fn public(&self) -> Value {
        json!({"version":self.version,"enabled":self.enabled,"room_id":self.room_id,
        "configured":self.ready(),"has_session":!self.session_cookie.is_empty(),"has_csrf":!self.csrf_token.is_empty(),"templates":self.templates,"max_text":MAX_TEXT,"prefix":PREFIX})
    }
}
#[derive(Clone)]
struct Job {
    id: String,
    text: String,
    version: Option<String>,
    room_id: String,
    created: std::time::Instant,
}
struct Inner {
    config: Config,
    jobs: VecDeque<Job>,
    seen: VecDeque<(String, String)>,
    results: VecDeque<Value>,
    last_error: Option<String>,
}
pub struct Sender {
    path: PathBuf,
    inner: Arc<Mutex<Inner>>,
    wake: Arc<Notify>,
    worker: Mutex<Option<JoinHandle<()>>>,
    active: AtomicBool,
    #[cfg(test)]
    endpoint: Option<String>,
    #[cfg(test)]
    interval: Duration,
}
impl Sender {
    pub fn open(path: PathBuf) -> Result<Arc<Self>> {
        let config = match std::fs::read(&path) {
            Ok(bytes) => {
                ensure!(bytes.len() <= 64 * 1024, "发言账号配置过大");
                let plain = crate::core::secrets::protect_file(&path, &bytes, false)
                    .map_err(|_| anyhow::anyhow!("无法解密发言账号配置"))?;
                serde_json::from_slice(&plain).map_err(|_| anyhow::anyhow!("发言账号配置损坏"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(_) => anyhow::bail!("无法读取发言账号配置"),
        };
        Ok(Arc::new(Self {
            path,
            inner: Arc::new(Mutex::new(Inner {
                config,
                jobs: VecDeque::new(),
                seen: VecDeque::new(),
                results: VecDeque::new(),
                last_error: None,
            })),
            wake: Arc::new(Notify::new()),
            worker: Mutex::new(None),
            active: AtomicBool::new(true),
            #[cfg(test)]
            endpoint: None,
            #[cfg(test)]
            interval: INTERVAL,
        }))
    }
    pub fn activate(&self) {
        self.active.store(true, Ordering::Release);
    }
    pub fn read(&self) -> Value {
        let s = self.inner.lock();
        let mut v = s.config.public();
        v["pending"] = json!(s.jobs.len());
        v["last_error"] = json!(s.last_error);
        v["recent"] = json!(s.results);
        v
    }
    fn persist(&self, config: &mut Config) -> Result<()> {
        config.version = Some(uuid::Uuid::new_v4().to_string());
        let parent = self.path.parent().context("发言账号路径无效")?;
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        let bytes =
            crate::core::secrets::protect_file(&self.path, &serde_json::to_vec(config)?, true)?;
        crate::core::fs::atomic_write(&self.path, &bytes).context("发言账号保存失败")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
    pub fn save(&self, v: Value) -> Result<Value> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Update {
            enabled: bool,
            room_id: String,
            #[serde(default)]
            session_cookie: Option<String>,
            #[serde(default)]
            csrf_token: Option<String>,
            expected_version: Option<String>,
            templates: BTreeMap<String, String>,
        }
        let u: Update =
            serde_json::from_value(v).map_err(|_| anyhow::anyhow!("发言设置格式无效"))?;
        let mut s = self.inner.lock();
        ensure!(
            u.expected_version == s.config.version,
            "发言设置已更新，请刷新后重试"
        );
        let mut c = s.config.clone();
        c.enabled = u.enabled;
        c.room_id = u.room_id.trim().into();
        ensure!(
            c.room_id.is_empty()
                || (c.room_id.len() <= 20
                    && c.room_id.bytes().all(|b| b.is_ascii_digit())
                    && c.room_id.parse::<u64>().is_ok_and(|id| id > 0)),
            "直播间 ID 无效"
        );
        if let Some(value) = u.session_cookie.filter(|v| !v.is_empty()) {
            ensure!(
                value.len() <= 4096
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_graphic() && b != b';' && b != b','),
                "SESSDATA 须填写单项值"
            );
            c.session_cookie = value;
        }
        if let Some(value) = u.csrf_token.filter(|v| !v.is_empty()) {
            ensure!(
                value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit()),
                "bili_jct 须为 32 位十六进制值"
            );
            c.csrf_token = value;
        }
        ensure!(
            !c.enabled || c.ready(),
            "启用前请填写直播间、SESSDATA 和 bili_jct"
        );
        ensure!(
            u.templates.len() <= Config::default().templates.len()
                && u.templates
                    .iter()
                    .all(|(k, v)| Config::default().templates.contains_key(k)
                        && v.chars().count() <= 200
                        && !v.chars().any(char::is_control)),
            "提醒模板无效或超过 200 字"
        );
        for (k, v) in u.templates {
            c.templates.insert(k, v);
        }
        self.persist(&mut c)?;
        s.config = c;
        s.jobs.clear();
        s.last_error = None;
        Ok(s.config.public())
    }
    pub fn clear(&self, expected: Option<&str>) -> Result<Value> {
        let mut s = self.inner.lock();
        ensure!(
            s.config.version.as_deref() == expected,
            "发言设置已更新，请刷新后重试"
        );
        let mut c = s.config.clone();
        c.enabled = false;
        c.session_cookie.clear();
        c.csrf_token.clear();
        self.persist(&mut c)?;
        s.config = c;
        s.jobs.clear();
        s.last_error = None;
        Ok(s.config.public())
    }
    pub fn manual(&self, text: &str, id: &str) -> Result<Value> {
        ensure!(
            !text.trim().is_empty() && !text.chars().any(char::is_control),
            "弹幕内容无效"
        );
        let text = format!("{PREFIX}{}", text.trim());
        ensure!(text.chars().count() <= MAX_TEXT, "弹幕正文最多 33 字");
        self.enqueue(text, id, None)
    }
    fn enqueue(&self, text: String, id: &str, room: Option<&str>) -> Result<Value> {
        ensure!(self.active.load(Ordering::Acquire), "主动弹幕已停止");
        ensure!(!id.is_empty() && id.len() <= 200, "发送请求 ID 无效");
        {
            let mut s = self.inner.lock();
            ensure!(self.active.load(Ordering::Acquire), "主动弹幕已停止");
            ensure!(
                s.config.enabled && s.config.ready(),
                "主动弹幕未启用或账号未配置"
            );
            ensure!(
                room.is_none_or(|r| r == s.config.room_id),
                "提醒所属直播间与发言设置不一致"
            );
            if let Some((_, previous)) = s.seen.iter().find(|(key, _)| key == id) {
                ensure!(previous == &text, "请求 ID 已用于其他弹幕");
                return Ok(json!({"accepted":true,"duplicate":true,"request_id":id}));
            }
            ensure!(s.jobs.len() < MAX_PENDING, "弹幕发送队列已满，请稍后再试");
            let job = Job {
                id: id.into(),
                text: text.clone(),
                version: s.config.version.clone(),
                room_id: s.config.room_id.clone(),
                created: std::time::Instant::now(),
            };
            s.jobs.push_back(job);
            s.seen.push_back((id.into(), text));
            while s.seen.len() > 2048 {
                s.seen.pop_front();
            }
        }
        self.start_worker()?;
        self.wake.notify_one();
        Ok(json!({"accepted":true,"request_id":id}))
    }
    fn start_worker(&self) -> Result<()> {
        let mut slot = self.worker.lock();
        ensure!(self.active.load(Ordering::Acquire), "主动弹幕已停止");
        if slot.as_ref().is_some_and(|h| !h.is_finished()) {
            return Ok(());
        }
        let handle = tokio::runtime::Handle::try_current().context("主动弹幕工作线程不可用")?;
        let inner = self.inner.clone();
        let wake = self.wake.clone();
        let endpoint = ENDPOINT.to_owned();
        #[cfg(not(test))]
        let interval = INTERVAL;
        #[cfg(test)]
        let endpoint = self.endpoint.clone().unwrap_or(endpoint);
        #[cfg(test)]
        let interval = self.interval;
        *slot = Some(handle.spawn(async move {
            let client = match reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
            {
                Ok(c) => c,
                Err(_) => {
                    inner.lock().last_error = Some("发言网络客户端不可用".into());
                    return;
                }
            };
            let mut next = tokio::time::Instant::now();
            loop {
                let notified = wake.notified();
                let candidate = { inner.lock().jobs.pop_front() };
                let Some(job) = candidate else {
                    notified.await;
                    continue;
                };
                tokio::time::sleep_until(next).await;
                let config = {
                    let s = inner.lock();
                    (s.config.enabled
                        && s.config.ready()
                        && s.config.version == job.version
                        && s.config.room_id == job.room_id
                        && job.created.elapsed() < Duration::from_secs(120))
                    .then(|| s.config.clone())
                };
                let Some(config) = config else {
                    record(&inner, &job, "skipped", None);
                    continue;
                };
                let outcome = deliver(&client, &endpoint, &config, &job.text).await;
                next = tokio::time::Instant::now() + interval;
                match outcome {
                    Ok(()) => record(&inner, &job, "sent", None),
                    Err(e) => record(&inner, &job, "failed", Some(e.to_string())),
                }
            }
        }));
        Ok(())
    }
    pub async fn stop(&self) {
        self.active.store(false, Ordering::Release);
        self.inner.lock().jobs.clear();
        let handle = self.worker.lock().take();
        if let Some(handle) = handle {
            handle.abort();
            let _ = handle.await;
        }
    }
}
fn record(inner: &Mutex<Inner>, job: &Job, state: &str, error: Option<String>) {
    let mut s = inner.lock();
    s.last_error = error.clone();
    s.results.push_front(json!({"request_id":job.id,"state":state,"error":error,"at":chrono::Utc::now().to_rfc3339()}));
    s.results.truncate(50);
}
async fn deliver(client: &reqwest::Client, endpoint: &str, c: &Config, text: &str) -> Result<()> {
    let form = [
        ("roomid", c.room_id.clone()),
        ("msg", text.into()),
        ("rnd", chrono::Utc::now().timestamp().to_string()),
        ("fontsize", "25".into()),
        ("color", "16777215".into()),
        ("mode", "1".into()),
        ("bubble", "0".into()),
        ("csrf", c.csrf_token.clone()),
        ("csrf_token", c.csrf_token.clone()),
    ];
    let response = client
        .post(endpoint)
        .header(
            "Cookie",
            format!("SESSDATA={}; bili_jct={}", c.session_cookie, c.csrf_token),
        )
        .header(
            "Referer",
            format!("https://live.bilibili.com/{}", c.room_id),
        )
        .header("User-Agent", "Mozilla/5.0 Gamer Live")
        .form(&form)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("发送请求失败，未重试，请检查账号或网络"))?;
    ensure!(
        response.status().is_success(),
        "发言服务返回 HTTP {}",
        response.status().as_u16()
    );
    ensure!(
        response.content_length().is_none_or(|n| n <= 64 * 1024),
        "发言服务响应过大"
    );
    let bytes = response
        .bytes()
        .await
        .map_err(|_| anyhow::anyhow!("发言服务响应读取失败"))?;
    ensure!(bytes.len() <= 64 * 1024, "发言服务响应过大");
    let body: Value =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("发言服务响应格式无效"))?;
    let code = body["code"].as_i64().context("发言服务没有返回结果码")?;
    ensure!(
        code == 0,
        "平台拒绝发言（代码 {code}），请检查账号、权限或频率"
    );
    ensure!(
        body["msg"].as_str().unwrap_or("").is_empty()
            && body["message"].as_str().unwrap_or("").is_empty(),
        "平台未接受发言，请稍后检查账号状态"
    );
    Ok(())
}
impl NoticeSink for Sender {
    fn send(&self, e: Value) {
        if e["test"] == true || e["platform_id"] != "bilibili" {
            return;
        }
        let kind = match e["kind"].as_str().unwrap_or("") {
            "queued" => "accepted",
            "rejected" if e["reason"] == "quota_unavailable" => "quota_rejected",
            "confirmation_required" => "confirmation_required",
            "scan_required" => "scan_required",
            "rescan_requested" => "rescan_requested",
            "cancelled" => "cancelled",
            "timeout" => "timeout",
            "succeeded" => "success",
            "failed" => "failed",
            _ => return,
        };
        let template = {
            let s = self.inner.lock();
            if !s.config.enabled {
                return;
            }
            s.config.templates.get(kind).cloned().unwrap_or_default()
        };
        if template.trim().is_empty() {
            return;
        }
        let mut text = template;
        for key in [
            "viewer",
            "number",
            "position",
            "cost",
            "remaining",
            "available",
        ] {
            let value = if key == "viewer" {
                e[key].as_str().unwrap_or("观众").chars().take(12).collect()
            } else if e["unlimited"] == true && ["remaining", "available"].contains(&key) {
                "无限".into()
            } else {
                e[key].as_str().map(str::to_owned).unwrap_or_else(|| {
                    if e[key].is_null() {
                        "-".into()
                    } else {
                        e[key].to_string()
                    }
                })
            };
            text = text.replace(&format!("{{{key}}}"), &value);
        }
        let text = format!("{PREFIX}{text}")
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_TEXT)
            .collect();
        if let Err(error) =
            self.enqueue(text, e["id"].as_str().unwrap_or(""), e["room_id"].as_str())
        {
            self.inner.lock().last_error = Some(error.to_string());
        }
    }
}

#[cfg(test)]
mod tests;
