use super::super::rules::{Binding, Rule};
use super::*;
use parking_lot::Mutex as SyncMutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
struct Fake {
    busy: AtomicBool,
    offline: AtomicBool,
    calls: AtomicUsize,
    runs: SyncMutex<BTreeMap<String, Value>>,
    rules: SyncMutex<RuleSet>,
}
impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            busy: AtomicBool::new(false),
            offline: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            runs: SyncMutex::new(BTreeMap::new()),
            rules: SyncMutex::new(RuleSet {
                schema_version: 1,
                rules: vec![Rule {
                    public_name: String::new(),
                    id: "jump".into(),
                    name: "跳跃".into(),
                    enabled: true,
                    kind: "message".into(),
                    operator: "equals".into(),
                    value: "跳".into(),
                    min_count: 1,
                    entrypoint: "default#jump".into(),
                    args: BTreeMap::from([(
                        "who".into(),
                        Binding {
                            source: "event".into(),
                            field: "actor_name".into(),
                            value: Value::Null,
                        },
                    )]),
                    cooldown_secs: 0,
                    timeout_secs: 0,
                }],
            }),
        })
    }
    fn finish(&self, id: &str, state: &str) {
        self.runs
            .lock()
            .insert(id.into(), json!({"state":state,"run_id":id}));
    }
}
#[async_trait]
impl Backend for Fake {
    fn target(&self, device: &str, package: &str) -> Result<Target> {
        Ok(Target {
            device_id: device.into(),
            package_id: package.into(),
            android_package: Some("app.game".into()),
            package_stamp: "one".into(),
        })
    }
    fn check(&self, _: &Target) -> Result<()> {
        ensure!(!self.offline.load(Ordering::SeqCst), "设备离线");
        Ok(())
    }
    fn rules(&self, _: &str) -> Result<(RuleSet, Option<String>)> {
        Ok((self.rules.lock().clone(), Some("one".into())))
    }
    fn save_rules(&self, _: &str, r: &RuleSet, _: Option<&str>) -> Result<String> {
        *self.rules.lock() = r.clone();
        Ok("one".into())
    }
    fn describe(&self, _: &str) -> Result<Value> {
        Ok(
            json!({"schema":[{"name":"who","type":"string","required":true},{"name":"count","type":"integer","default":1}]}),
        )
    }
    async fn submit(
        &self,
        _: &Target,
        _: &str,
        _: &Map<String, Value>,
    ) -> std::result::Result<String, SubmitError> {
        if self.busy.load(Ordering::SeqCst) {
            return Err(SubmitError::Busy);
        }
        let id = format!("run-{}", self.calls.fetch_add(1, Ordering::SeqCst));
        self.runs
            .lock()
            .insert(id.clone(), json!({"run_id":id,"state":"running"}));
        Ok(id)
    }
    async fn run(&self, id: &str) -> Result<Option<Value>> {
        Ok(self.runs.lock().get(id).cloned())
    }
    async fn cancel(&self, id: &str) -> Result<()> {
        self.runs
            .lock()
            .insert(id.into(), json!({"run_id":id,"state":"stopping"}));
        Ok(())
    }
    fn active(&self, _: &str) -> Option<Value> {
        self.runs
            .lock()
            .values()
            .find(|v| ["running", "stopping"].contains(&v["state"].as_str().unwrap_or("")))
            .cloned()
    }
}
fn event(id: &str) -> LiveEvent {
    LiveEvent {
        schema_version: 1,
        seq: 0,
        platform_id: "bilibili".into(),
        connection_id: "connection".into(),
        room_id: "room".into(),
        event_id: Some(id.into()),
        kind: "message".into(),
        occurred_at: None,
        received_at: String::new(),
        actor: Some(json!({"id":"viewer","name":"观众"})),
        payload: json!({"text":" 跳 "}),
        platform_data: Value::Null,
    }
}
async fn setup() -> (tempfile::TempDir, Arc<Fake>, Arc<Queue>) {
    let dir = tempfile::tempdir().unwrap();
    let f = Fake::new();
    let q = Queue::open(dir.path().join("queue.json"), f.clone()).unwrap();
    q.configure("phone", "default").await.unwrap();
    (dir, f, q)
}

#[tokio::test]
async fn journal_cursors_and_refresh_follow_one_interaction_without_reordering() {
    let (_dir, f, q) = setup().await;
    f.busy.store(true, Ordering::SeqCst);
    let mut e = event("rich");
    e.actor = Some(
        json!({"id":"viewer","name":"观众","medal_level":12,"medal_name":"测试牌","medal_wearing":true}),
    );
    q.receive(e).await;
    for n in 0..60 {
        let mut e = event(&format!("chat-{n}"));
        e.payload["text"] = json!("聊天");
        q.receive(e).await;
    }
    let first = q.logs(0, 0, "all", "", &[]).await;
    assert_eq!(first["rows"].as_array().unwrap().len(), 50);
    assert_eq!(first["has_more"], true);
    q.receive(event("new")).await;
    let burst = q.logs(0, 1, "all", "", &[]).await;
    assert_eq!(burst["rows"][0]["seq"], 2);
    assert_eq!(burst["next_after"], 51);
    assert_eq!(burst["has_more"], true);
    let remaining = q.logs(0, 51, "all", "", &[]).await;
    assert_eq!(remaining["rows"].as_array().unwrap().len(), 11);
    assert_eq!(remaining["next_after"], 62);
    let second = q
        .logs(first["next"].as_u64().unwrap(), 0, "all", "", &[])
        .await;
    assert_eq!(second["rows"].as_array().unwrap().len(), 11);
    assert_eq!(
        second["rows"].as_array().unwrap().last().unwrap()["event"]["actor"]["medal_level"],
        12
    );
    f.busy.store(false, Ordering::SeqCst);
    q.tick().await;
    let updated = q.logs(0, 0, "trigger", "", &[1]).await;
    assert_eq!(updated["updates"][0]["item"]["state"], "running");
    f.finish("run-0", "success");
    q.tick().await;
    assert_eq!(
        q.logs(0, 0, "trigger", "", &[1]).await["updates"][0]["item"]["state"],
        "success"
    );
    assert_eq!(
        q.logs(0, 0, "all", "聊天", &[]).await["rows"]
            .as_array()
            .unwrap()
            .len(),
        50
    );
}

#[tokio::test]
async fn unavailable_target_has_a_public_decline_and_withdrawals_stay_display_only() {
    let (_dir, f, q) = setup().await;
    f.offline.store(true, Ordering::SeqCst);
    q.receive(event("offline")).await;
    let logs = q.logs(0, 0, "trigger", "", &[]).await;
    assert_eq!(logs["rows"][0]["reason"], "target_unavailable");
    assert_eq!(logs["rows"][0]["rule"], "跳跃");
    assert_eq!(q.audience().await["notices"][0]["state"], "unavailable");
    let mut message = event("sc");
    message.kind = "super_chat".into();
    message.payload = json!({"message_id":123,"text":"醒目留言","rmb":30});
    q.receive(message).await;
    let mut removed = event("removed");
    removed.kind = "message.removed".into();
    removed.payload = json!({"removed_ids":["123"]});
    q.receive(removed).await;
    let rows = q.logs(0, 0, "all", "", &[]).await;
    assert_eq!(rows["rows"][1]["withdrawn"], true);
    assert_eq!(rows["rows"][1]["reason"], "display_only");
    assert!(q.status(0, "").await["waiting"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn audience_is_read_only_revocable_and_whitelists_fields() {
    let (_dir, f, q) = setup().await;
    f.rules.lock().rules[0].public_name = "公开操作".into();
    q.receive(event("secret-event-id")).await;
    q.tick().await;
    let projection = q.audience().await;
    assert_eq!(projection["current"]["name"], "公开操作");
    assert_eq!(projection["current"]["number"], 1);
    let text = projection.to_string();
    for forbidden in [
        "secret-event-id",
        "entrypoint",
        "run_id",
        "actor_id",
        "args",
        "default#jump",
        "phone",
        "error",
    ] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
    let connection = Arc::new(SyncMutex::new(super::super::ConnectionStatus::default()));
    let window = super::super::audience::Window::open(q.clone(), connection)
        .await
        .unwrap();
    let (base, token) = window.url.split_once('#').unwrap();
    let url = format!("{base}state");
    let client = reqwest::Client::new();
    assert_eq!(client.get(&url).send().await.unwrap().status(), 403);
    assert_eq!(
        client
            .get(&url)
            .bearer_auth("invalid")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client
            .get(&url)
            .bearer_auth(token)
            .header("Origin", "https://example.com")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    let response = client.get(&url).bearer_auth(token).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(
        response.json::<Value>().await.unwrap()["current"]["name"],
        "公开操作"
    );
    assert_eq!(
        client
            .post(&url)
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        405
    );
    assert_eq!(
        client
            .post(format!("{base}api/runs"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let token = token.to_owned();
    drop(window);
    tokio::task::yield_now().await;
    if let Ok(response) = client.get(&url).bearer_auth(token).send().await {
        assert_eq!(response.status(), 403);
    }
    assert_eq!(q.status(0, "").await["current"]["state"], "running");
}

#[tokio::test]
async fn fifo_waits_for_terminal_and_busy_does_not_reorder() {
    let (_dir, f, q) = setup().await;
    q.receive(event("a")).await;
    q.receive(event("b")).await;
    assert_eq!(
        q.status(0, "").await["waiting"].as_array().unwrap().len(),
        2
    );
    f.busy.store(true, Ordering::SeqCst);
    q.tick().await;
    assert_eq!(
        q.status(0, "").await["waiting"][0]["event"]["message"]["event_id"],
        "a"
    );
    f.busy.store(false, Ordering::SeqCst);
    q.tick().await;
    q.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    q.control("cancel", &[], "").await.unwrap();
    q.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.finish("run-0", "cancelled");
    q.tick().await;
    q.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        q.status(0, "").await["current"]["event"]["message"]["event_id"],
        "b"
    );
}
#[tokio::test]
async fn preview_dedup_missing_identity_and_stable_bound_args() {
    let (_dir, f, q) = setup().await;
    let preview = q.preview(event("preview"), false, "").await.unwrap();
    assert!(preview["resolved"].is_object());
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    assert_eq!(q.status(0, "").await["waiting"], json!([]));
    q.receive(event("a")).await;
    let mut repeated = event("a");
    repeated.connection_id = "reconnect".into();
    q.receive(repeated).await;
    let mut anonymous = event("b");
    anonymous.event_id = None;
    q.receive(anonymous.clone()).await;
    q.receive(anonymous).await;
    assert_eq!(
        q.status(0, "").await["waiting"].as_array().unwrap().len(),
        3
    );
    f.rules.lock().rules[0].args.clear();
    assert_eq!(
        q.status(0, "").await["waiting"][0]["args"],
        json!({"who":"观众","count":1})
    );
    let mut mirror = event("c");
    mirror.kind = "message.mirror".into();
    q.receive(mirror).await;
    assert_eq!(
        q.status(0, "").await["waiting"].as_array().unwrap().len(),
        3
    );
}
#[tokio::test]
async fn restart_is_paused_and_running_is_review_not_replayed() {
    let (dir, f, q) = setup().await;
    q.receive(event("a")).await;
    q.receive(event("b")).await;
    q.tick().await;
    drop(q);
    let restored = Queue::open(dir.path().join("queue.json"), f.clone()).unwrap();
    let status = restored.status(0, "").await;
    assert_eq!(status["paused"], true);
    assert_eq!(status["current"]["state"], "review");
    assert_eq!(status["waiting"].as_array().unwrap().len(), 1);
    assert!(restored.control("resume", &[], "").await.is_err());
    assert!(restored
        .control(
            "resolve",
            &[status["current"]["id"].as_str().unwrap().into()],
            ""
        )
        .await
        .is_err());
    restored.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn controls_keep_current_and_clear_pending_durably() {
    let (dir, f, q) = setup().await;
    q.receive(event("a")).await;
    q.receive(event("b")).await;
    q.tick().await;
    let id = q.status(0, "").await["current"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let response = q.control("remove", &[id], "").await.unwrap();
    assert!(response["results"][0]["result"]
        .as_str()
        .unwrap()
        .contains("无法移除"));
    q.control("clear", &[], "").await.unwrap();
    assert_eq!(q.status(0, "").await["current"]["state"], "running");
    assert_eq!(q.status(0, "").await["waiting"], json!([]));
    drop(q);
    let q = Queue::open(dir.path().join("queue.json"), f).unwrap();
    assert_eq!(q.status(0, "").await["waiting"], json!([]));
}
#[tokio::test]
async fn offline_and_write_failure_stop_dispatch_without_draining() {
    let (dir, f, q) = setup().await;
    q.receive(event("a")).await;
    f.offline.store(true, Ordering::SeqCst);
    q.tick().await;
    assert_eq!(
        q.status(0, "").await["waiting"].as_array().unwrap().len(),
        1
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    f.offline.store(false, Ordering::SeqCst);
    q.control("resume", &[], "").await.unwrap();
    std::fs::remove_file(dir.path().join("queue.json")).unwrap();
    std::fs::create_dir(dir.path().join("queue.json")).unwrap();
    q.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    assert_eq!(q.status(0, "").await["paused"], true);
}
#[tokio::test]
async fn capacity_cooldown_and_test_request_identity() {
    let (_dir, f, q) = setup().await;
    f.rules.lock().rules[0].cooldown_secs = 60;
    q.receive(event("a")).await;
    q.receive(event("b")).await;
    assert_eq!(
        q.status(0, "").await["waiting"].as_array().unwrap().len(),
        1
    );
    f.rules.lock().rules[0].cooldown_secs = 0;
    q.preview(event("test"), true, "test-request")
        .await
        .unwrap();
    q.preview(event("test"), true, "test-request")
        .await
        .unwrap();
    assert_eq!(
        q.status(0, "").await["waiting"].as_array().unwrap().len(),
        2
    );
    for i in 0..100 {
        q.receive(event(&format!("many-{i}"))).await;
    }
    assert_eq!(
        q.status(0, "").await["waiting"].as_array().unwrap().len(),
        100
    );
    assert!(q.status(0, "").await["receipts"][0]["result"]
        .as_str()
        .unwrap()
        .contains("已满"));
}

#[tokio::test]
async fn disabling_plugin_cancels_current_and_unbind_waits_for_terminal() {
    let (_dir, f, q) = setup().await;
    q.receive(event("a")).await;
    q.receive(event("b")).await;
    q.tick().await;
    q.suspend(true).await;
    let s = q.status(0, "").await;
    assert_eq!(s["paused"], true);
    assert_eq!(s["waiting"], json!([]));
    assert_eq!(s["current"]["state"], "cancelling");
    assert!(q.control("unbind", &[], "").await.is_err());
    f.finish("run-0", "cancelled");
    q.tick().await;
    q.control("unbind", &[], "").await.unwrap();
    assert!(q.status(0, "").await["target"].is_null());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn changed_room_blocks_and_timeout_waits_for_cancellation() {
    let (_dir, f, q) = setup().await;
    q.receive(event("a")).await;
    let mut changed = event("b");
    changed.room_id = "another-room".into();
    q.receive(changed).await;
    let s = q.status(0, "").await;
    assert_eq!(s["waiting"].as_array().unwrap().len(), 1);
    q.control("resume", &[], "").await.unwrap();
    q.tick().await;
    {
        let mut s = q.state.lock().await;
        s.items[0].timeout_secs = 1;
        s.items[0].started_at = Some(now() - 2000);
    }
    q.tick().await;
    assert_eq!(q.status(0, "").await["current"]["state"], "cancelling");
    assert_eq!(f.active("phone").unwrap()["state"], "stopping");
    q.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn gifts_use_message_quantity_once_and_string_identity() {
    let (_dir, f, q) = setup().await;
    {
        let mut rules = f.rules.lock();
        let r = &mut rules.rules[0];
        r.kind = "gift".into();
        r.value = "42".into();
        r.args.insert(
            "count".into(),
            Binding {
                source: "event".into(),
                field: "count".into(),
                value: Value::Null,
            },
        );
        r.args.get_mut("who").unwrap().field = "gift_id".into();
    }
    let mut events = super::super::events::EventBuffer::default();
    let gift = events.push("connection", json!({"cmd":"OPEN_LIVEROOM_SEND_GIFT","data":{"room_id":1,"msg_id":"gift-one","gift_id":42,"gift_num":5}})).unwrap();
    q.receive(gift.clone()).await;
    q.receive(gift).await;
    let s = q.status(0, "").await;
    assert_eq!(s["waiting"].as_array().unwrap().len(), 1);
    assert_eq!(s["waiting"][0]["args"], json!({"count":5,"who":"42"}));
}

#[tokio::test]
async fn per_rule_switch_is_the_only_normal_trigger_gate() {
    let (_dir, f, q) = setup().await;
    q.toggle_rule("default", "jump", false, "one").unwrap();
    q.receive(event("off")).await;
    assert_eq!(q.status(0, "").await["waiting"], json!([]));
    q.toggle_rule("default", "jump", true, "one").unwrap();
    q.receive(event("on")).await;
    q.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    assert!(q.toggle_rule("default", "jump", false, "old").is_err());
    assert!(f.rules.lock().rules[0].enabled);
}

#[tokio::test]
async fn empty_restart_does_not_reintroduce_a_hidden_master_switch() {
    let (dir, f, q) = setup().await;
    q.suspend(true).await;
    assert_eq!(q.status(0, "").await["paused"], false);
    q.receive(event("stopped")).await;
    assert_eq!(q.status(0, "").await["waiting"], json!([]));
    q.suspend(false).await;
    drop(q);
    let q = Queue::open(dir.path().join("queue.json"), f.clone()).unwrap();
    assert_eq!(q.status(0, "").await["paused"], false);
    q.new_connection().await.unwrap();
    q.receive(event("fresh")).await;
    q.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}
