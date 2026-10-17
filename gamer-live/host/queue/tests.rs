use super::super::rules::{Binding, Rule};
use super::*;
use parking_lot::Mutex as SyncMutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
struct Fake {
    busy: AtomicBool,
    offline: AtomicBool,
    cancel_failure: AtomicBool,
    calls: AtomicUsize,
    runs: SyncMutex<BTreeMap<String, Value>>,
    rules: SyncMutex<RuleSet>,
    submitted: SyncMutex<Vec<Value>>,
    cancelled: SyncMutex<Vec<String>>,
    active_override: SyncMutex<Option<Value>>,
    completed: SyncMutex<BTreeMap<String, bool>>,
    proof_queries: SyncMutex<Vec<Value>>,
    checkpoint_error: AtomicBool,
}
impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            busy: AtomicBool::new(false),
            offline: AtomicBool::new(false),
            cancel_failure: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            runs: SyncMutex::new(BTreeMap::new()),
            rules: SyncMutex::new(RuleSet {
                schema_version: 1,
                external_origin: None,
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
                    cleanup: None,
                    cost: 1,
                    confirmation: None,
                    completion: None,
                }],
            }),
            submitted: SyncMutex::new(Vec::new()),
            cancelled: SyncMutex::new(Vec::new()),
            active_override: SyncMutex::new(None),
            completed: SyncMutex::new(BTreeMap::new()),
            proof_queries: SyncMutex::new(Vec::new()),
            checkpoint_error: AtomicBool::new(false),
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
        target: &Target,
        entrypoint: &str,
        args: &Map<String, Value>,
    ) -> std::result::Result<String, SubmitError> {
        if self.busy.load(Ordering::SeqCst) {
            return Err(SubmitError::Busy);
        }
        if self.offline.load(Ordering::SeqCst) {
            return Err(SubmitError::Blocked("设备离线".into()));
        }
        let id = format!("run-{}", self.calls.fetch_add(1, Ordering::SeqCst));
        self.submitted.lock().push(
            json!({"run_id":id,"device":target.device_id,"entrypoint":entrypoint,"args":args}),
        );
        self.runs
            .lock()
            .insert(id.clone(), json!({"run_id":id,"state":"running"}));
        Ok(id)
    }
    fn validate_external_origin(&self, origin: &ExternalRuleOrigin) -> Result<()> {
        origin.authorize_target(&origin.device_id, &origin.package_id)
    }
    async fn submit_external(
        &self,
        target: &Target,
        entry: &str,
        args: &Map<String, Value>,
        origin: &ExternalRuleOrigin,
    ) -> std::result::Result<String, SubmitError> {
        origin
            .authorize_target(&target.device_id, &target.package_id)
            .map_err(|error| SubmitError::Blocked(error.to_string()))?;
        let id = self.submit(target, entry, args).await?;
        self.submitted
            .lock()
            .iter_mut()
            .find(|item| item["run_id"] == id)
            .unwrap()["external_origin"] = serde_json::to_value(origin).unwrap();
        Ok(id)
    }
    async fn run(&self, id: &str) -> Result<Option<Value>> {
        Ok(self.runs.lock().get(id).cloned())
    }
    async fn cancel(&self, id: &str) -> Result<()> {
        ensure!(
            !self.cancel_failure.load(Ordering::SeqCst),
            "取消暂时不可用"
        );
        self.cancelled.lock().push(id.into());
        self.runs
            .lock()
            .insert(id.into(), json!({"run_id":id,"state":"stopping"}));
        Ok(())
    }
    fn active(&self, _: &str) -> Option<Value> {
        if let Some(run) = self.active_override.lock().clone() {
            return Some(run);
        }
        self.runs
            .lock()
            .values()
            .find(|v| ["running", "stopping"].contains(&v["state"].as_str().unwrap_or("")))
            .cloned()
    }
    async fn completed_child(&self, run_id: &str, full_entrypoint: &str) -> Result<bool> {
        self.proof_queries
            .lock()
            .push(json!({"run_id":run_id,"entrypoint":full_entrypoint}));
        Ok(*self
            .completed
            .lock()
            .get(&format!("{run_id}:{full_entrypoint}"))
            .unwrap_or(&false))
    }
    async fn completed_checkpoint(&self, run_id: &str, spec: &CompletionSpec) -> Result<bool> {
        ensure!(
            !self.checkpoint_error.load(Ordering::SeqCst),
            "journal unavailable"
        );
        self.proof_queries.lock().push(json!({"run_id":run_id,"entrypoint":spec.entrypoint,"checkpoint_id":spec.checkpoint_id}));
        Ok(*self
            .completed
            .lock()
            .get(&format!(
                "checkpoint:{run_id}:{}:{}",
                spec.entrypoint, spec.checkpoint_id
            ))
            .unwrap_or(&false))
    }
}
fn event(id: &str) -> LiveEvent {
    LiveEvent {
        schema_version: 1,
        seq: 0,
        platform_id: "bilibili".into(),
        identity_namespace: "qa-app".into(),
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
    // Existing lifecycle/FIFO tests intentionally have ample credits; dedicated
    // quota tests below use the production default of one.
    q.accounts_save_settings(10000, 0).await.unwrap();
    (dir, f, q)
}

#[tokio::test]
async fn external_queue_filters_recorded_target_before_pagination_and_redacts_details() {
    let (_dir, f, q) = setup().await;
    f.busy.store(true, Ordering::SeqCst);
    q.receive(event("external-scope")).await;
    {
        let mut state = q.state.lock().await;
        let template = state.items[0].clone();
        state.items.clear();
        for n in 0..40 {
            let mut item = template.clone();
            item.id = format!("item-{n}");
            item.state = "success".into();
            item.event = json!({"private":"audience event"});
            item.error = Some("C:/private/path".into());
            if n != 0 && n != 39 {
                item.target.device_id = "other-device".into();
            }
            state.items.push(item);
        }
    }
    let visible = q.external_status("phone", "default", 0).await;
    assert_eq!(visible["history"].as_array().unwrap().len(), 2);
    assert_eq!(visible["history"][0]["id"], "item-39");
    assert_eq!(visible["history"][1]["id"], "item-0");
    assert_eq!(visible["has_more"], false);
    let text = visible.to_string();
    for hidden in [
        "private",
        "audience event",
        "C:/",
        "other-device",
        "receipts",
        "automatic_video_folder",
    ] {
        assert!(!text.contains(hidden), "{hidden}");
    }
    let other_package = q.external_status("phone", "another-package", 0).await;
    assert!(other_package["history"].as_array().unwrap().is_empty());
    assert_eq!(other_package["bound"], false);
}

#[tokio::test]
async fn external_rules_write_rechecks_binding_and_credential_after_waiting_for_target_lock() {
    for target_changed in [true, false] {
        let (_dir, backend, queue) = setup().await;
        let active = Arc::new(AtomicBool::new(true));
        let mut target_lock = queue.state.lock().await;
        let pending = {
            let queue = queue.clone();
            let active = active.clone();
            tokio::spawn(async move {
                queue
                    .external_save_rules(
                        "phone",
                        "default",
                        RuleSet::default(),
                        Some("one"),
                        move || {
                            ensure!(active.load(Ordering::SeqCst), "credential_invalid");
                            Ok(())
                        },
                    )
                    .await
            })
        };
        tokio::task::yield_now().await;
        if target_changed {
            target_lock.target.as_mut().unwrap().device_id = "other-device".into();
        } else {
            active.store(false, Ordering::SeqCst);
        }
        drop(target_lock);
        assert!(pending.await.unwrap().is_err());
        assert_eq!(
            backend.rules.lock().rules.len(),
            1,
            "A rejected write must preserve existing rules"
        );
    }
}

fn external_origin() -> ExternalRuleOrigin {
    ExternalRuleOrigin {
        policy: crate::core::ExecutionPolicy::external(
            "frozen-token".into(),
            ["gamer-yaml".into()].into(),
            ["run.submit".into()].into(),
        ),
        device_id: "phone".into(),
        package_id: "default".into(),
        runner_id: super::super::rules::RUNNER.into(),
    }
}

#[tokio::test]
async fn external_rules_freeze_authority_in_queue_and_both_main_and_cleanup_submissions() {
    let (directory, backend, queue) = setup_cleanup().await;
    let origin = external_origin();
    backend.rules.lock().external_origin = Some(origin.clone());
    backend.busy.store(true, Ordering::SeqCst);
    queue.receive(event("external-origin")).await;
    let stored: Value =
        serde_json::from_slice(&std::fs::read(directory.path().join("queue.json")).unwrap())
            .unwrap();
    assert_eq!(
        stored["items"][0]["external_origin"],
        serde_json::to_value(&origin).unwrap()
    );
    let item: Item = serde_json::from_value(stored["items"][0].clone()).unwrap();
    assert_eq!(item.external_origin, Some(origin.clone()));
    // Later resource edits cannot replace the accepted item's authority.
    backend.rules.lock().external_origin = None;
    backend.busy.store(false, Ordering::SeqCst);
    queue.tick().await;
    assert_eq!(
        backend.submitted.lock()[0]["external_origin"],
        serde_json::to_value(&origin).unwrap()
    );
    backend.finish("run-0", "success");
    queue.tick().await;
    queue.tick().await;
    assert_eq!(
        backend.submitted.lock()[1]["entrypoint"],
        "default/cleanup.yaml"
    );
    assert_eq!(
        backend.submitted.lock()[1]["external_origin"],
        serde_json::to_value(&origin).unwrap()
    );
    backend.finish("run-1", "success");
    queue.tick().await;
}

#[tokio::test]
async fn external_rule_origin_cannot_move_to_another_target_and_legacy_rules_remain_manual() {
    let (_directory, backend, queue) = setup().await;
    let legacy = serde_json::to_value(&*backend.rules.lock()).unwrap();
    assert!(legacy.get("external_origin").is_none());
    let mut origin = external_origin();
    origin.device_id = "another-device".into();
    backend.rules.lock().external_origin = Some(origin);
    queue.receive(event("wrong-target")).await;
    queue.tick().await;
    assert_eq!(backend.calls.load(Ordering::SeqCst), 0);
    assert!(queue.status(0, "").await["waiting"]
        .as_array()
        .unwrap()
        .is_empty());
    backend.rules.lock().external_origin = None;
    queue.receive(event("legacy-manual")).await;
    queue.tick().await;
    assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    assert!(backend.submitted.lock()[0].get("external_origin").is_none());
    backend.finish("run-0", "success");
    queue.tick().await;
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
async fn audience_featured_prefers_active_then_last_real_result_by_change_time() {
    let (_dir, f, q) = setup().await;
    let base = now() - 10000;
    for id in ["older-insert", "newer-insert", "removed", "waiting"] {
        q.receive(event(id)).await;
    }
    {
        let mut state = q.state.lock().await;
        state.items[0].state = "success".into();
        state.items[0].started_at = Some(base + 10);
        state.items[0].finished_at = Some(base + 100);
        state.items[0].updated_at = base + 90;
        state.items[1].state = "failed".into();
        state.items[1].finished_at = Some(base + 80);
        state.items[1].updated_at = base + 80;
        state.items[1].error = Some("private runtime failure".into());
        state.items[2].state = "removed".into();
        state.items[2].finished_at = Some(base + 1000);
        state.items[2].updated_at = base + 1000;
        state.blocked = Some("private dependency failure".into());
    }
    let projection = q.audience().await;
    assert!(projection["current"].is_null());
    assert_eq!(
        projection["featured"]["number"], 1,
        "finish time beats insertion order"
    );
    assert_eq!(projection["featured"]["state"], "success");
    assert_eq!(projection["featured"]["finished_at"], base + 100);
    assert!(!projection.to_string().contains("private"));
    {
        let mut state = q.state.lock().await;
        state.items[1].updated_at = base + 200;
        state.items[2].state = "acknowledged".into();
    }
    let projection = q.audience().await;
    assert_eq!(
        projection["featured"]["number"], 2,
        "later real result update wins"
    );
    assert_eq!(projection["featured"]["state"], "failed");
    assert!(!projection.to_string().contains("private"));
    let mut chat = event("unmatched");
    chat.payload = json!({"text":"旁观聊天不要显示"});
    q.receive(chat).await;
    let projection = q.audience().await;
    assert_eq!(projection["featured"]["number"], 2);
    assert!(!projection.to_string().contains("旁观聊天不要显示"));
    {
        let mut state = q.state.lock().await;
        state.items[0].state = "running".into();
        state.items[0].finished_at = None;
        state.items[0].updated_at = base + 1;
    }
    f.finish("run-0", "running");
    let projection = q.audience().await;
    assert_eq!(projection["featured"], projection["current"]);
    assert_eq!(
        projection["featured"]["number"], 1,
        "active item wins over a newer terminal update"
    );
}

#[tokio::test]
async fn audience_waiting_includes_the_entire_fifo_without_chat_or_private_fields() {
    let (_dir, f, q) = setup().await;
    f.busy.store(true, Ordering::SeqCst);
    for position in 1..=15 {
        let mut e = event(&format!("request-{position}"));
        e.actor =
            Some(json!({"id":format!("private-id-{position}"),"name":format!("观众{position}")}));
        q.receive(e).await;
    }
    let projection = q.audience().await;
    assert!(projection["featured"].is_null());
    assert!(projection["current"].is_null());
    assert_eq!(projection["waiting_count"], 15);
    let waiting = projection["waiting"].as_array().unwrap();
    assert_eq!(
        waiting.len(),
        15,
        "audience must not truncate the queue to ten rows"
    );
    for (index, item) in waiting.iter().enumerate() {
        assert_eq!(item["position"], index + 1);
        assert_eq!(item["number"], index + 1);
        assert_eq!(item["viewer"], format!("观众{}", index + 1));
    }
    let text = projection.to_string();
    for forbidden in ["request-", "run_id", "entrypoint", "args", "error"] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
    assert_eq!(waiting[0]["open_id"], "private-id-1");
}

#[tokio::test]
async fn audience_fixed_read_url_toggles_and_whitelists_fields() {
    let (dir, f, q) = setup().await;
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
    let connection = SyncMutex::new(super::super::ConnectionStatus::default());
    let links_path = dir.path().join("local-links.json");
    let links = super::super::local_links::LocalLinks::open(&links_path, 8443).unwrap();
    let audience = super::super::audience::Audience::new(
        dir.path().join("audience.json"),
        links.audience_url().to_owned(),
    );
    let initial = audience.read().await.unwrap();
    assert_eq!(initial["enabled"], false);
    assert_eq!(initial["url"], links.audience_url());
    assert_eq!(initial["text"], "");
    assert!(initial["version"].is_null());
    assert_eq!(audience.page().await.status(), 403);
    assert_eq!(audience.snapshot(&q, &connection).await.status(), 403);
    let saved = audience
        .save("发送日常\n按顺序等待".into(), None)
        .await
        .unwrap();
    let opened = audience.open().await.unwrap();
    assert_eq!(opened["enabled"], true);
    assert_eq!(opened["text"], saved["text"]);
    assert_eq!(
        audience.read().await.unwrap(),
        opened,
        "remount restores enabled state and stable URL"
    );
    assert_eq!(audience.open().await.unwrap(), opened, "open is idempotent");
    let page = audience.page().await;
    assert_eq!(page.status(), 200);
    assert_eq!(page.headers()["cache-control"], "no-store");
    assert_eq!(page.headers()["referrer-policy"], "no-referrer");
    assert!(page.headers().contains_key("content-security-policy"));
    async fn json(response: axum::response::Response) -> Value {
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["cache-control"], "no-store");
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    }
    let state = json(audience.snapshot(&q, &connection).await).await;
    assert_eq!(state["current"]["name"], "公开操作");
    assert_eq!(state["featured"], state["current"]);
    assert_eq!(state["text"], "发送日常\n按顺序等待");
    let updated = audience
        .save("新提示\n保留换行".into(), saved["version"].as_str())
        .await
        .unwrap();
    assert_eq!(updated["url"], opened["url"]);
    let state = json(audience.snapshot(&q, &connection).await).await;
    assert_eq!(state["text"], "新提示\n保留换行");
    for forbidden in [
        "secret-event-id",
        "entrypoint",
        "run_id",
        "actor_id",
        "args",
        "default#jump",
        "phone",
        "error",
        "version",
        "url",
    ] {
        assert!(!state.as_object().unwrap().contains_key(forbidden));
    }
    let closed = audience.close().await.unwrap();
    assert_eq!(closed["enabled"], false);
    assert_eq!(
        closed["url"], opened["url"],
        "closing keeps the copyable address"
    );
    assert_eq!(closed["text"], updated["text"]);
    assert_eq!(closed["version"], updated["version"]);
    assert_eq!(audience.read().await.unwrap(), closed);
    assert_eq!(audience.page().await.status(), 403);
    assert_eq!(audience.snapshot(&q, &connection).await.status(), 403);
    assert_eq!(q.status(0, "").await["current"]["state"], "running");
    let reopened = audience.open().await.unwrap();
    assert_eq!(
        reopened["url"], opened["url"],
        "re-enable uses the same durable capability"
    );
    assert_eq!(reopened["text"], updated["text"]);
    audience.shutdown().await;
    assert_eq!(audience.page().await.status(), 403);
    assert_eq!(audience.snapshot(&q, &connection).await.status(), 403);
    let restored_links = super::super::local_links::LocalLinks::open(&links_path, 8443).unwrap();
    let restored = super::super::audience::Audience::new(
        dir.path().join("audience.json"),
        restored_links.audience_url().to_owned(),
    );
    assert_eq!(
        restored.read().await.unwrap(),
        closed,
        "a new instance preserves URL/text but starts disabled"
    );
    assert_eq!(restored.open().await.unwrap()["url"], opened["url"]);
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
async fn failed_timeout_cancellation_keeps_live_picture_until_the_run_finishes() {
    struct Picture {
        restored: AtomicUsize,
    }
    #[async_trait]
    impl DispatchHooks for Picture {
        async fn before_submit(&self, _: &Target) -> Result<()> {
            Ok(())
        }
        fn executing(&self) {}
        async fn idle(&self, _: Option<String>) -> Result<()> {
            self.restored.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    let (_dir, f, q) = setup().await;
    let picture = Arc::new(Picture {
        restored: AtomicUsize::new(0),
    });
    q.set_hooks(picture.clone());
    q.receive(event("first")).await;
    q.receive(event("second")).await;
    q.tick().await;
    {
        let mut s = q.state.lock().await;
        s.items[0].timeout_secs = 1;
        s.items[0].started_at = Some(now() - 2000);
    }
    f.cancel_failure.store(true, Ordering::SeqCst);
    q.tick().await;
    let status = q.status(0, "").await;
    assert_eq!(status["paused"], true);
    assert_eq!(status["current"]["state"], "running");
    assert_eq!(status["waiting"].as_array().unwrap().len(), 1);
    assert_eq!(picture.restored.load(Ordering::SeqCst), 0);
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.finish("run-0", "success");
    q.tick().await;
    assert_eq!(picture.restored.load(Ordering::SeqCst), 1);
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn contains_daily_messages_queue_only_matching_requests_in_arrival_order() {
    let (_dir, f, q) = setup().await;
    {
        let mut rules = f.rules.lock();
        let rule = &mut rules.rules[0];
        rule.name = "直播日常".into();
        rule.operator = "contains".into();
        rule.value = "日常".into();
        rule.entrypoint = "default/live_daily.yaml".into();
    }
    for (id, text) in [
        ("chat", "你好"),
        ("first", "请帮我清理日常"),
        ("second", "日常"),
    ] {
        let mut message = event(id);
        message.payload["text"] = json!(text);
        q.receive(message).await;
    }
    let status = q.status(0, "").await;
    assert_eq!(status["waiting"].as_array().unwrap().len(), 2);
    assert_eq!(
        status["waiting"][0]["event"]["message"]["event_id"],
        "first"
    );
    assert_eq!(
        status["waiting"][1]["event"]["message"]["event_id"],
        "second"
    );
    assert_eq!(
        status["waiting"][0]["entrypoint"],
        "default/live_daily.yaml"
    );
    q.tick().await;
    q.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.finish("run-0", "success");
    q.tick().await;
    q.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        q.status(0, "").await["current"]["event"]["message"]["event_id"],
        "second"
    );
}

#[tokio::test]
async fn automatic_video_folder_and_bound_target_survive_reopening_the_queue() {
    let (dir, f, q) = setup().await;
    q.configure_with_folder(
        "browser-next",
        "default",
        Some(" D:/code/gamer/live-videos "),
    )
    .await
    .unwrap();
    q.configure("browser-next", "default").await.unwrap();
    drop(q);
    let reopened = Queue::open(dir.path().join("queue.json"), f).unwrap();
    let status = reopened.status(0, "").await;
    assert_eq!(
        status["automatic_video_folder"],
        "D:/code/gamer/live-videos"
    );
    assert_eq!(status["target"]["device_id"], "browser-next");
    assert_eq!(status["target"]["package_id"], "default");
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
    let mut gift = events.push("connection", json!({"cmd":"OPEN_LIVEROOM_SEND_GIFT","data":{"room_id":1,"open_id":"gift-viewer","msg_id":"gift-one","gift_id":42,"gift_num":5}})).unwrap();
    gift.identity_namespace = "qa-app".into();
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

#[tokio::test]
async fn stop_admission_fence_wins_after_a_successful_preparation() {
    struct Ready {
        entered: tokio::sync::Notify,
        release: tokio::sync::Semaphore,
    }
    #[async_trait]
    impl DispatchHooks for Ready {
        async fn before_submit(&self, _: &Target) -> Result<()> {
            self.entered.notify_one();
            self.release.acquire().await.unwrap().forget();
            Ok(())
        }
        fn executing(&self) {}
        async fn idle(&self, _: Option<String>) -> Result<()> {
            Ok(())
        }
    }
    let (_dir, f, q) = setup().await;
    let hooks = Arc::new(Ready {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Semaphore::new(0),
    });
    q.set_hooks(hooks.clone());
    q.receive(event("first")).await;
    let copy = q.clone();
    let tick = tokio::spawn(async move { copy.tick().await });
    hooks.entered.notified().await;
    // Queue the dispatcher on the state lock before suspend. The admission
    // atomic must stop it even when it wins that lock ahead of paused=true.
    let state = q.state.lock().await;
    hooks.release.add_permits(1);
    tokio::task::yield_now().await;
    let copy = q.clone();
    let stop = tokio::spawn(async move { copy.suspend(false).await });
    tokio::task::yield_now().await;
    assert!(!q.accepting.load(Ordering::SeqCst));
    drop(state);
    tick.await.unwrap();
    stop.await.unwrap();
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    q.control("stop", &[], "").await.unwrap();
    assert!(q.status(0, "").await["current"].is_null());
}

fn install_cleanup(f: &Fake) {
    f.rules.lock().rules[0].cleanup = Some(CleanupSpec {
        entrypoint: "default/cleanup.yaml".into(),
        args: Map::from_iter([("who".into(), json!("cleanup"))]),
        timeout_secs: 180,
    });
}
async fn setup_cleanup() -> (tempfile::TempDir, Arc<Fake>, Arc<Queue>) {
    let (dir, f, q) = setup().await;
    install_cleanup(&f);
    (dir, f, q)
}
#[derive(Default)]
struct CleanupPicture {
    log: SyncMutex<Vec<&'static str>>,
}
#[async_trait]
impl DispatchHooks for CleanupPicture {
    async fn before_submit(&self, _: &Target) -> Result<()> {
        self.log.lock().push("device");
        Ok(())
    }
    fn executing(&self) {
        self.log.lock().push("executing");
    }
    async fn idle(&self, _: Option<String>) -> Result<()> {
        self.log.lock().push("video");
        Ok(())
    }
}
async fn enter_cleanup(q: &Queue, f: &Fake, main_state: &str) {
    q.receive(event("first")).await;
    q.tick().await;
    f.runs.lock().insert(
        "run-0".into(),
        json!({"run_id":"run-0","state":main_state,
        "error":if main_state == "failed" {json!("daily failed")} else {Value::Null}}),
    );
    q.tick().await;
    assert_eq!(q.status(0, "").await["current"]["phase"], "cleanup");
}

#[tokio::test]
async fn independent_cleanup_preserves_each_main_result_and_frozen_bound_args() {
    for main_state in ["success", "failed"] {
        for cleanup_state in ["success", "failed"] {
            let (_dir, f, q) = setup_cleanup().await;
            enter_cleanup(&q, &f, main_state).await;
            f.rules.lock().rules[0].cleanup.as_mut().unwrap().entrypoint =
                "default/replaced.yaml".into();
            q.tick().await;
            let current = q.status(0, "").await["current"].clone();
            assert_eq!(current["run_id"], "run-0");
            assert_eq!(current["cleanup"]["run_id"], "run-1");
            assert_eq!(current["main_result"]["state"], main_state);
            assert_eq!(f.submitted.lock()[1]["entrypoint"], "default/cleanup.yaml");
            assert_eq!(
                f.submitted.lock()[1]["args"],
                json!({"who":"cleanup","count":1})
            );
            f.runs.lock().insert(
                "run-1".into(),
                json!({"run_id":"run-1","state":cleanup_state,
                "error":if cleanup_state == "failed" {json!("logout failed")} else {Value::Null}}),
            );
            q.tick().await;
            let status = q.status(0, "").await;
            assert!(status["current"].is_null());
            assert_eq!(status["history"][0]["state"], main_state);
            assert_eq!(
                status["history"][0]["error"],
                if main_state == "failed" {
                    json!("daily failed")
                } else {
                    Value::Null
                }
            );
            assert_eq!(status["history"][0]["cleanup"]["state"], cleanup_state);
            assert_eq!(
                status["history"][0]["cleanup_warning"].is_string(),
                cleanup_state == "failed"
            );
            assert_eq!(f.run("run-0").await.unwrap().unwrap()["state"], main_state);
            assert_eq!(status["paused"], false);
            if cleanup_state == "failed" {
                assert_eq!(
                    q.logs(0, 0, "errors", "", &[]).await["rows"]
                        .as_array()
                        .unwrap()
                        .len(),
                    1
                );
            }
        }
    }
}

#[tokio::test]
async fn failed_main_cleanup_and_fifo_wait_for_both_owned_slots_then_restore_video() {
    let (_dir, f, q) = setup_cleanup().await;
    let picture = Arc::new(CleanupPicture::default());
    q.set_hooks(picture.clone());
    q.receive(event("first")).await;
    q.receive(event("second")).await;
    q.tick().await;
    f.finish("run-0", "failed");
    *f.active_override.lock() = Some(json!({"run_id":"run-0","state":"failed"}));
    q.tick().await;
    assert_eq!(q.status(0, "").await["current"]["phase"], "");
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    *f.active_override.lock() = None;
    q.tick().await;
    assert_eq!(
        f.calls.load(Ordering::SeqCst),
        1,
        "persist cleanup intent before submitting"
    );
    q.tick().await;
    f.finish("run-1", "success");
    *f.active_override.lock() = Some(json!({"run_id":"run-1","state":"success"}));
    q.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    assert_eq!(*picture.log.lock(), ["device", "executing"]);
    assert_eq!(q.status(0, "").await["current"]["phase"], "cleanup");
    *f.active_override.lock() = None;
    q.tick().await;
    assert_eq!(*picture.log.lock(), ["device", "executing", "video"]);
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    q.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 3);
    assert_eq!(f.submitted.lock()[2]["entrypoint"], "default#jump");
    assert_eq!(
        *picture.log.lock(),
        ["device", "executing", "video", "device", "executing"]
    );
}

#[tokio::test]
async fn cleanup_busy_waits_without_cancelling_a_manual_run_or_passing_the_viewer() {
    let (_dir, f, q) = setup_cleanup().await;
    enter_cleanup(&q, &f, "failed").await;
    q.receive(event("second")).await;
    *f.active_override.lock() = Some(json!({"run_id":"manual","state":"running"}));
    q.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    *f.active_override.lock() = None;
    f.busy.store(true, Ordering::SeqCst);
    q.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        q.status(0, "").await["current"]["cleanup"]["state"],
        "waiting"
    );
    assert_eq!(
        q.status(0, "").await["waiting"].as_array().unwrap().len(),
        1
    );
    assert!(f.cancelled.lock().is_empty());
    f.busy.store(false, Ordering::SeqCst);
    q.tick().await;
    assert_eq!(f.submitted.lock()[1]["entrypoint"], "default/cleanup.yaml");
}

#[tokio::test]
async fn cleanup_submission_failure_is_a_warning_without_changing_daily_success() {
    let (_dir, f, q) = setup_cleanup().await;
    enter_cleanup(&q, &f, "success").await;
    f.offline.store(true, Ordering::SeqCst);
    q.tick().await;
    let status = q.status(0, "").await;
    assert!(status["current"].is_null());
    assert_eq!(status["history"][0]["state"], "success");
    assert_eq!(status["history"][0]["cleanup"]["state"], "failed");
    assert_eq!(status["history"][0]["cleanup_warning"], "设备离线");
    assert_eq!(status["paused"], false);
}

#[tokio::test]
async fn cancelling_cleanup_targets_its_run_and_preserves_completed_daily_success() {
    let (_dir, f, q) = setup_cleanup().await;
    q.prepare_automatic("phone", "default", "videos")
        .await
        .unwrap();
    q.activate_automatic().await.unwrap();
    enter_cleanup(&q, &f, "success").await;
    q.tick().await;
    q.control("cancel", &[], "").await.unwrap();
    assert_eq!(*f.cancelled.lock(), ["run-1"]);
    f.finish("run-1", "cancelled");
    q.tick().await;
    let status = q.status(0, "").await;
    assert_eq!(status["history"][0]["state"], "success");
    assert_eq!(status["history"][0]["cleanup"]["state"], "cancelled");
    assert_eq!(
        status["paused"], true,
        "explicit cancellation retains recovery gate"
    );
    assert_eq!(f.run("run-0").await.unwrap().unwrap()["state"], "success");
}

#[tokio::test]
async fn stopping_before_cleanup_submission_preserves_success_without_new_input() {
    let (_dir, f, q) = setup_cleanup().await;
    enter_cleanup(&q, &f, "success").await;
    q.receive(event("second")).await;
    q.stop_automatic().await.unwrap();
    let status = q.status(0, "").await;
    let finished = status["history"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["run_id"] == "run-0")
        .unwrap();
    assert_eq!(finished["state"], "success");
    assert_eq!(finished["cleanup"]["state"], "skipped");
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    assert!(f.cancelled.lock().is_empty());
    assert_eq!(status["waiting"], json!([]));
}

#[tokio::test]
async fn stopping_waits_for_cleanup_slot_and_never_cancels_a_new_manual_owner() {
    let (_dir, f, q) = setup_cleanup().await;
    enter_cleanup(&q, &f, "success").await;
    q.tick().await;
    let copy = q.clone();
    let stop = tokio::spawn(async move { copy.stop_automatic().await });
    for _ in 0..100 {
        if !f.cancelled.lock().is_empty() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(*f.cancelled.lock(), ["run-1"]);
    f.finish("run-1", "cancelled");
    *f.active_override.lock() = Some(json!({"run_id":"run-1","state":"cancelled"}));
    tokio::task::yield_now().await;
    assert!(!stop.is_finished());
    *f.active_override.lock() = Some(json!({"run_id":"new-manual","state":"running"}));
    tokio::time::timeout(Duration::from_secs(2), stop)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(*f.cancelled.lock(), ["run-1"]);
    assert_eq!(q.status(0, "").await["history"][0]["state"], "success");
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn main_cancellation_or_stop_success_race_never_submits_new_cleanup() {
    for terminal in ["cancelled", "success"] {
        let (_dir, f, q) = setup_cleanup().await;
        q.receive(event("main")).await;
        q.tick().await;
        q.control("stop", &[], "").await.unwrap();
        f.finish("run-0", terminal);
        q.tick().await;
        q.tick().await;
        assert_eq!(f.calls.load(Ordering::SeqCst), 1);
        let item = q.status(0, "").await["history"][0].clone();
        assert_eq!(item["state"], terminal);
        if terminal == "success" {
            assert_eq!(item["cleanup"]["state"], "skipped");
        }
    }
}

#[tokio::test]
async fn cleanup_timeout_cancels_only_cleanup_and_retains_main_result_until_release() {
    let (_dir, f, q) = setup_cleanup().await;
    f.rules.lock().rules[0]
        .cleanup
        .as_mut()
        .unwrap()
        .timeout_secs = 1;
    enter_cleanup(&q, &f, "success").await;
    q.tick().await;
    q.state.lock().await.items[0]
        .cleanup
        .as_mut()
        .unwrap()
        .started_at = Some(now() - 2000);
    q.tick().await;
    assert_eq!(*f.cancelled.lock(), ["run-1"]);
    f.finish("run-1", "cancelled");
    *f.active_override.lock() = Some(json!({"run_id":"run-1","state":"cancelled"}));
    q.tick().await;
    assert_eq!(q.status(0, "").await["current"]["state"], "cancelling");
    *f.active_override.lock() = None;
    q.tick().await;
    let item = q.status(0, "").await["history"][0].clone();
    assert_eq!(item["state"], "success");
    assert!(item["cleanup_warning"].as_str().unwrap().contains("超时"));
}

#[tokio::test]
async fn cleanup_restart_requires_review_and_checks_the_cleanup_slot_when_resolving() {
    let (dir, f, q) = setup_cleanup().await;
    enter_cleanup(&q, &f, "success").await;
    q.tick().await;
    let item_id = q.status(0, "").await["current"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    drop(q);
    let restored = Queue::open(dir.path().join("queue.json"), f.clone()).unwrap();
    assert_eq!(restored.status(0, "").await["current"]["state"], "review");
    assert!(restored.control("resume", &[], "").await.is_err());
    restored.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    f.finish("run-1", "failed");
    *f.active_override.lock() = Some(json!({"run_id":"run-1","state":"failed"}));
    assert!(restored
        .control("resolve", std::slice::from_ref(&item_id), "")
        .await
        .is_err());
    *f.active_override.lock() = None;
    restored.control("resolve", &[item_id], "").await.unwrap();
    assert_eq!(
        restored.status(0, "").await["history"][0]["state"],
        "success"
    );
    assert_eq!(
        restored.status(0, "").await["history"][0]["cleanup"]["state"],
        "acknowledged"
    );
}

#[tokio::test]
async fn retry_keeps_frozen_cleanup_spec_and_clears_old_result_and_cleanup_identity() {
    let (_dir, f, q) = setup_cleanup().await;
    enter_cleanup(&q, &f, "failed").await;
    q.tick().await;
    f.finish("run-1", "failed");
    q.tick().await;
    let original = q.status(0, "").await["history"][0].clone();
    f.rules.lock().rules[0].cleanup = None;
    q.control(
        "retry",
        &[original["id"].as_str().unwrap().into()],
        "retry-cleanup",
    )
    .await
    .unwrap();
    let retried = q.status(0, "").await["waiting"][0].clone();
    assert_eq!(retried["cleanup_spec"], original["cleanup_spec"]);
    assert_eq!(retried["phase"], "");
    for key in [
        "run_id",
        "main_result",
        "cleanup",
        "cleanup_warning",
        "reconciliation",
    ] {
        assert!(retried[key].is_null(), "{key}");
    }
    assert_eq!(retried["stop_requested"], false);
    assert_eq!(retried["main_timed_out"], false);
    assert_eq!(retried["retry_of"], original["id"]);
}

#[tokio::test]
async fn cleanup_submission_identity_survives_a_persistence_failure_for_stop() {
    let (dir, f, q) = setup_cleanup().await;
    enter_cleanup(&q, &f, "success").await;
    std::fs::remove_file(dir.path().join("queue.json")).unwrap();
    std::fs::create_dir(dir.path().join("queue.json")).unwrap();
    q.tick().await;
    let current = q.status(0, "").await["current"].clone();
    assert_eq!(current["run_id"], "run-0");
    assert_eq!(current["cleanup"]["run_id"], "run-1");
    assert_eq!(q.status(0, "").await["paused"], true);
    assert!(q.control("stop", &[], "").await.is_err());
    assert_eq!(*f.cancelled.lock(), ["run-1"]);
}

#[tokio::test]
async fn missing_cleanup_record_requires_review_and_never_dispatches_next_viewer() {
    let (_dir, f, q) = setup_cleanup().await;
    enter_cleanup(&q, &f, "success").await;
    q.receive(event("second")).await;
    q.tick().await;
    f.runs.lock().remove("run-1");
    q.tick().await;
    q.tick().await;
    assert_eq!(q.status(0, "").await["current"]["state"], "review");
    assert_eq!(
        q.status(0, "").await["current"]["main_result"]["state"],
        "success"
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    let item_id = q.status(0, "").await["current"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        q.control("resolve", &[item_id], "").await.is_err(),
        "missing cleanup identity cannot be acknowledged as if its slot were released"
    );
}

#[tokio::test]
async fn ordinary_main_review_keeps_explicit_acknowledgement_when_old_run_is_unknown() {
    let (_dir, f, q) = setup().await;
    q.receive(event("unknown-old-main")).await;
    q.tick().await;
    f.runs.lock().clear();
    q.tick().await;
    let item_id = q.status(0, "").await["current"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    q.control("resolve", &[item_id], "").await.unwrap();
    let status = q.status(0, "").await;
    assert!(status["current"].is_null());
    assert_eq!(status["history"][0]["state"], "acknowledged");
    assert!(status["history"][0]["main_result"].is_null());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    assert!(f.cancelled.lock().is_empty());
}

#[tokio::test]
async fn reconcile_success_requires_exact_child_evidence_terminal_and_released_original_run() {
    let (_dir, f, q) = setup().await;
    q.receive(event("old-combined-workflow")).await;
    q.tick().await;
    // This API intentionally repairs only pre-billing combined-workflow history.
    q.state.lock().await.items[0].billing = None;
    f.runs.lock().insert(
        "run-0".into(),
        json!({"run_id":"run-0","state":"failed","error":"postdaily reload failed"}),
    );
    q.tick().await;
    let item_id = q.status(0, "").await["history"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let daily = "default/daily.yaml";
    assert!(q.reconcile_success(&item_id, daily).await.is_err());
    assert!(q
        .reconcile_success(&item_id, "other/daily.yaml")
        .await
        .is_err());
    f.completed.lock().insert(format!("run-0:{daily}"), true);
    *f.active_override.lock() = Some(json!({"run_id":"run-0","state":"failed"}));
    assert!(q.reconcile_success(&item_id, daily).await.is_err());
    *f.active_override.lock() = None;
    f.finish("run-0", "running");
    assert!(q.reconcile_success(&item_id, daily).await.is_err());
    f.finish("run-0", "failed");
    let result = q.reconcile_success(&item_id, daily).await.unwrap();
    assert_eq!(result["state"], "success");
    assert_eq!(result["reconciliation"]["completed_entrypoint"], daily);
    assert_eq!(
        result["reconciliation"]["previous_error"],
        "postdaily reload failed"
    );
    assert!(result["cleanup_warning"]
        .as_str()
        .unwrap()
        .contains("postdaily reload failed"));
    assert_eq!(
        f.run("run-0").await.unwrap().unwrap()["state"],
        "failed",
        "Core history is immutable"
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    assert!(f.cancelled.lock().is_empty());
    assert_eq!(
        f.proof_queries.lock().last().unwrap(),
        &json!({"run_id":"run-0","entrypoint":daily})
    );
    let proof_count = f.proof_queries.lock().len();
    let revision = q.status(0, "").await["revision"].clone();
    let repeated = q.reconcile_success(&item_id, daily).await.unwrap();
    assert_eq!(repeated["already_reconciled"], true);
    assert_eq!(repeated["reconciliation"], result["reconciliation"]);
    assert_eq!(q.status(0, "").await["revision"], revision);
    assert_eq!(f.proof_queries.lock().len(), proof_count);
    assert!(
        q.reconcile_success(&item_id, "default/other.yaml")
            .await
            .is_err(),
        "idempotence is not a generic result override"
    );
}

#[tokio::test]
async fn failed_daily_and_failed_cleanup_still_allow_the_next_fifo_to_succeed() {
    let (_dir, f, q) = setup_cleanup().await;
    q.prepare_automatic("phone", "default", "videos")
        .await
        .unwrap();
    q.activate_automatic().await.unwrap();
    enter_cleanup(&q, &f, "failed").await;
    q.receive(event("second")).await;
    q.tick().await;
    f.finish("run-1", "failed");
    q.tick().await;
    assert_eq!(q.status(0, "").await["history"][0]["state"], "failed");
    assert_eq!(q.status(0, "").await["paused"], false);
    q.tick().await;
    assert_eq!(q.status(0, "").await["current"]["run_id"], "run-2");
    f.finish("run-2", "success");
    q.tick().await;
    q.tick().await;
    f.finish("run-3", "success");
    q.tick().await;
    let status = q.status(0, "").await;
    assert_eq!(status["history"][0]["state"], "success");
    assert_eq!(status["history"][1]["state"], "failed");
    assert_eq!(status["history"][1]["cleanup"]["state"], "failed");
    assert!(status["history"][1]["cleanup_warning"].is_string());
    assert_eq!(status["paused"], false);
    assert!(status["current"].is_null());
    assert!(f.cancelled.lock().is_empty());
}

#[tokio::test]
async fn queue_owned_main_timeout_is_failed_and_runs_cleanup_without_user_cancel_pause() {
    let (_dir, f, q) = setup_cleanup().await;
    q.prepare_automatic("phone", "default", "videos")
        .await
        .unwrap();
    q.activate_automatic().await.unwrap();
    q.receive(event("timed-out")).await;
    q.receive(event("next")).await;
    q.tick().await;
    {
        let mut state = q.state.lock().await;
        state.items[0].timeout_secs = 1;
        state.items[0].started_at = Some(now() - 2000);
    }
    q.tick().await;
    assert_eq!(*f.cancelled.lock(), ["run-0"]);
    assert_eq!(q.status(0, "").await["current"]["main_timed_out"], true);
    f.finish("run-0", "cancelled");
    q.tick().await;
    let current = q.status(0, "").await["current"].clone();
    assert_eq!(current["phase"], "cleanup");
    assert_eq!(current["main_result"]["state"], "failed");
    assert!(current["main_result"]["error"]
        .as_str()
        .unwrap()
        .contains("超时"));
    assert_eq!(q.status(0, "").await["paused"], false);
    q.tick().await;
    assert_eq!(f.submitted.lock()[1]["entrypoint"], "default/cleanup.yaml");
    f.finish("run-1", "success");
    q.tick().await;
    assert_eq!(q.status(0, "").await["history"][0]["state"], "failed");
    q.tick().await;
    assert_eq!(q.status(0, "").await["current"]["run_id"], "run-2");
}

#[tokio::test]
async fn successful_main_and_cleanup_keep_device_between_waiting_viewers() {
    let (_dir, f, q) = setup_cleanup().await;
    let picture = Arc::new(CleanupPicture::default());
    q.set_hooks(picture.clone());
    enter_cleanup(&q, &f, "success").await;
    q.receive(event("next")).await;
    q.tick().await;
    f.finish("run-1", "success");
    q.tick().await;
    assert_eq!(*picture.log.lock(), ["device", "executing"]);
    assert!(q.status(0, "").await["current"].is_null());
    q.tick().await;
    assert_eq!(
        *picture.log.lock(),
        ["device", "executing", "device", "executing"]
    );
    assert_eq!(q.status(0, "").await["current"]["run_id"], "run-2");
}

async fn quota_setup() -> (tempfile::TempDir, Arc<Fake>, Arc<Queue>) {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake::new();
    let queue = Queue::open(dir.path().join("queue.json"), fake.clone()).unwrap();
    queue.configure("phone", "default").await.unwrap();
    (dir, fake, queue)
}
async fn account(q: &Queue) -> Value {
    q.accounts_list(0, "viewer").await["users"][0].clone()
}

#[tokio::test]
async fn account_search_matches_uppercase_and_mixed_case_open_ids_without_changing_identity() {
    let (_dir, _f, q) = quota_setup().await;
    for (event_id, open_id) in [("uppercase", "UPPERCASE-ID"), ("mixed", "MixedCase-OpenID")] {
        let mut e = event(event_id);
        e.actor = Some(json!({"id":open_id,"name":"观众"}));
        q.receive(e).await;
        for query in [
            open_id.to_owned(),
            open_id.to_lowercase(),
            open_id.to_uppercase(),
        ] {
            let result = q.accounts_list(0, &query).await;
            assert_eq!(result["total"], 1, "search: {query}");
            assert_eq!(result["users"][0]["open_id"], open_id);
            assert_eq!(result["users"][0]["balance"], 1);
            assert_eq!(result["users"][0]["reserved"], 1);
        }
    }
}
fn install_checkpoint(f: &Fake) {
    f.rules.lock().rules[0].completion = Some(CompletionSpec {
        entrypoint: "default/daily.yaml".into(),
        checkpoint_id: "gold-reward".into(),
    });
}
fn verify_checkpoint(f: &Fake, run_id: &str) {
    f.completed.lock().insert(
        format!("checkpoint:{run_id}:default/daily.yaml:gold-reward"),
        true,
    );
}
fn install_confirmation(f: &Fake) {
    install_cleanup(f);
    install_checkpoint(f);
    f.rules.lock().rules[0].confirmation = Some(ConfirmationSpec {
        timeout_secs: 300,
        cancel_cleanup: CleanupSpec {
            entrypoint: "default/logout-only.yaml".into(),
            args: Map::from_iter([("who".into(), json!("cancel-no-reset"))]),
            timeout_secs: 180,
        },
    });
}
fn command(id: &str, text: &str) -> LiveEvent {
    let mut e = event(id);
    e.payload = json!({"text":text});
    e.received_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    e
}

#[tokio::test]
async fn default_credit_is_reserved_atomically_and_blocks_other_tasks_before_source_switch() {
    let (_dir, f, q) = quota_setup().await;
    let pictures = Arc::new(CleanupPicture::default());
    q.set_hooks(pictures.clone());
    q.receive(event("one")).await;
    q.receive(event("one")).await;
    q.receive(event("no-credit")).await;
    let state = q.status(0, "").await;
    assert_eq!(state["waiting"].as_array().unwrap().len(), 1);
    assert_eq!(account(&q).await["balance"], 1);
    assert_eq!(account(&q).await["reserved"], 1);
    assert_eq!(account(&q).await["available"], 0);
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    assert!(pictures.log.lock().is_empty());
    let id = state["waiting"][0]["id"].as_str().unwrap().to_owned();
    q.control("remove", &[id], "").await.unwrap();
    assert_eq!(account(&q).await["balance"], 1);
    assert_eq!(account(&q).await["reserved"], 0);
    q.receive(event("after-release")).await;
    assert_eq!(
        q.status(0, "").await["waiting"].as_array().unwrap().len(),
        1
    );
}

#[tokio::test]
async fn missing_identity_and_bot_echo_never_create_accounts_or_execution() {
    let (_dir, f, q) = quota_setup().await;
    for (id, actor, namespace) in [
        ("no-open-id", Some(json!({"name":"观众"})), "qa-app"),
        ("no-actor", None, "qa-app"),
        (
            "no-application",
            Some(json!({"id":"viewer","name":"观众"})),
            "",
        ),
    ] {
        let mut e = event(id);
        e.actor = actor;
        e.identity_namespace = namespace.into();
        q.receive(e).await;
    }
    q.receive(command("echo", "   【Gamer】跳")).await;
    q.tick().await;
    assert_eq!(q.accounts_list(0, "").await["total"], 0);
    assert!(q.status(0, "").await["waiting"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cost_and_available_balance_apply_to_concurrent_reservations_and_admin_versions() {
    let (_dir, f, q) = quota_setup().await;
    f.rules.lock().rules[0].cost = 2;
    q.receive(event("insufficient")).await;
    assert!(q.status(0, "").await["waiting"]
        .as_array()
        .unwrap()
        .is_empty());
    let row = account(&q).await;
    let id = row["id"].as_str().unwrap();
    let version = row["version"].as_u64().unwrap();
    let updated = q
        .accounts_update(id, 3, false, version, "grant-three")
        .await
        .unwrap();
    assert_eq!(
        q.accounts_update(id, 3, false, version, "grant-three")
            .await
            .unwrap(),
        updated
    );
    assert!(q
        .accounts_update(id, 4, false, version, "grant-three")
        .await
        .is_err());
    assert!(q
        .accounts_update(id, 4, false, version, "stale-write")
        .await
        .is_err());
    let ((), ()) = tokio::join!(q.receive(event("a")), q.receive(event("b")));
    assert_eq!(
        q.status(0, "").await["waiting"].as_array().unwrap().len(),
        1
    );
    let row = account(&q).await;
    assert_eq!(row["balance"], 3);
    assert_eq!(row["reserved"], 2);
    assert_eq!(row["available"], 1);
    assert!(q
        .accounts_update(id, 1, false, row["version"].as_u64().unwrap(), "under-hold")
        .await
        .is_err());
    q.accounts_save_settings(8, 0).await.unwrap();
    assert!(q.accounts_save_settings(9, 0).await.is_err());
    assert_eq!(
        account(&q).await["balance"],
        3,
        "default applies only to newly observed users"
    );
    let mut another = event("new-user");
    another.actor = Some(json!({"id":"another","name":"另一位"}));
    q.receive(another).await;
    assert_eq!(
        q.accounts_list(0, "another").await["users"][0]["balance"],
        8
    );
}

#[tokio::test]
async fn unlimited_accounts_are_free_and_test_simulations_have_separate_identity() {
    let (_dir, f, q) = quota_setup().await;
    q.receive(event("real")).await;
    let row = account(&q).await;
    q.accounts_update(
        row["id"].as_str().unwrap(),
        1,
        true,
        row["version"].as_u64().unwrap(),
        "unlimited",
    )
    .await
    .unwrap();
    q.receive(event("free")).await;
    q.preview(event("simulation"), true, "test-account")
        .await
        .unwrap();
    let rows = q.accounts_list(0, "viewer").await["users"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(rows.len(), 2);
    assert_ne!(rows[0]["id"], rows[1]["id"]);
    assert_eq!(rows.iter().filter(|r| r["test"] == true).count(), 1);
    // The earlier finite reservation is frozen; making the account unlimited
    // does not change its settlement, and new free items reserve nothing.
    assert_eq!(
        rows.iter().find(|r| r["test"] == false).unwrap()["reserved"],
        1
    );
    q.tick().await;
    f.finish("run-0", "failed");
    q.tick().await;
    q.tick().await;
    f.finish("run-1", "success");
    q.tick().await;
    let real = q.accounts_list(0, "viewer").await["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["test"] == false)
        .unwrap()
        .clone();
    assert_eq!(real["balance"], 1);
    assert_eq!(real["reserved"], 0);
    assert!(real["available"].is_null());
}

#[tokio::test]
async fn checkpoint_success_charges_once_only_after_terminal_slot_release_and_survives_cleanup_failure(
) {
    let (_dir, f, q) = quota_setup().await;
    install_cleanup(&f);
    install_checkpoint(&f);
    q.receive(event("reward")).await;
    q.tick().await;
    verify_checkpoint(&f, "run-0");
    f.finish("run-0", "failed");
    *f.active_override.lock() = Some(json!({"run_id":"run-0","state":"failed"}));
    q.tick().await;
    assert_eq!(account(&q).await["balance"], 1);
    assert_eq!(account(&q).await["reserved"], 1);
    assert!(f.proof_queries.lock().is_empty());
    *f.active_override.lock() = None;
    q.tick().await;
    assert_eq!(
        q.status(0, "").await["current"]["main_result"]["state"],
        "success"
    );
    assert_eq!(account(&q).await["balance"], 0);
    assert_eq!(account(&q).await["reserved"], 0);
    q.tick().await;
    f.finish("run-1", "failed");
    q.tick().await;
    q.tick().await;
    let history = q.status(0, "").await["history"][0].clone();
    assert_eq!(history["state"], "success");
    assert_eq!(history["billing"]["state"], "charged");
    assert!(history["cleanup_warning"].is_string());
    assert_eq!(account(&q).await["balance"], 0);
    assert_eq!(f.proof_queries.lock().len(), 1);
}

#[tokio::test]
async fn no_checkpoint_fails_even_a_successful_core_run_and_releases_reservation() {
    for main_state in ["success", "failed", "cancelled"] {
        let (_dir, f, q) = quota_setup().await;
        install_checkpoint(&f);
        q.receive(event("no-reward")).await;
        q.tick().await;
        f.finish("run-0", main_state);
        q.tick().await;
        let history = q.status(0, "").await["history"][0].clone();
        assert_eq!(
            history["state"],
            if main_state == "cancelled" {
                "cancelled"
            } else {
                "failed"
            }
        );
        assert_eq!(history["billing"]["state"], "released");
        assert_eq!(account(&q).await["balance"], 1);
        assert_eq!(account(&q).await["reserved"], 0);
    }
}

#[tokio::test]
async fn unavailable_checkpoint_journal_requires_review_and_keeps_reserved_credit() {
    let (_dir, f, q) = quota_setup().await;
    install_checkpoint(&f);
    q.receive(event("uncertain-reward")).await;
    q.tick().await;
    f.checkpoint_error.store(true, Ordering::SeqCst);
    f.finish("run-0", "success");
    q.tick().await;
    assert_eq!(q.status(0, "").await["current"]["state"], "review");
    assert_eq!(account(&q).await["balance"], 1);
    assert_eq!(account(&q).await["reserved"], 1);
    q.receive(event("must-not-overbook")).await;
    assert!(q.status(0, "").await["waiting"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn rules_without_checkpoint_use_natural_main_result_for_single_settlement() {
    for state in ["success", "failed"] {
        let (_dir, f, q) = quota_setup().await;
        q.receive(event("legacy-result")).await;
        q.tick().await;
        f.finish("run-0", state);
        q.tick().await;
        q.tick().await;
        assert_eq!(
            account(&q).await["balance"],
            if state == "success" { 0 } else { 1 }
        );
        assert_eq!(account(&q).await["reserved"], 0);
        assert!(f.proof_queries.lock().is_empty());
    }
}

#[tokio::test]
async fn only_same_open_id_application_and_room_can_confirm_an_active_gate() {
    let (_dir, f, q) = quota_setup().await;
    install_confirmation(&f);
    q.receive(command("early", "确认")).await;
    assert_eq!(q.accounts_list(0, "").await["total"], 0);
    q.receive(event("task")).await;
    q.tick().await;
    let first = q
        .begin_confirmation("run-0", "phone", "default", 300)
        .await
        .unwrap();
    assert_eq!(first["status"], "waiting");
    let deadline = first["deadline_at"].as_i64().unwrap();
    assert!((299_000..=300_000).contains(&(deadline - now())));
    assert_eq!(
        q.begin_confirmation("run-0", "phone", "default", 300)
            .await
            .unwrap()["deadline_at"],
        deadline
    );
    for (id, actor, ns, room) in [
        ("other-viewer", "other", "qa-app", "room"),
        ("other-app", "viewer", "other-app", "room"),
        ("other-room", "viewer", "qa-app", "other-room"),
    ] {
        let mut e = command(id, "确认");
        e.actor = Some(json!({"id":actor,"name":"观众"}));
        e.identity_namespace = ns.into();
        e.room_id = room.into();
        q.receive(e).await;
        assert_eq!(
            q.poll_confirmation("run-0", "phone", "default")
                .await
                .unwrap()["status"],
            "waiting"
        );
    }
    assert!(q
        .begin_confirmation("run-0", "other-device", "default", 300)
        .await
        .is_err());
    assert!(q
        .poll_confirmation("run-0", "phone", "other-package")
        .await
        .is_err());
    assert!(q
        .poll_confirmation("not-owned", "phone", "default")
        .await
        .is_err());
    q.receive(command("self", "  确认  ")).await;
    q.receive(command("late-cancel", "取消")).await;
    assert_eq!(
        q.poll_confirmation("run-0", "phone", "default")
            .await
            .unwrap()["status"],
        "confirmed"
    );
    assert_eq!(account(&q).await["balance"], 1);
    assert_eq!(account(&q).await["reserved"], 1);
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    verify_checkpoint(&f, "run-0");
    f.finish("run-0", "success");
    q.tick().await;
    assert_eq!(account(&q).await["balance"], 0);
}

#[tokio::test]
async fn reserved_controls_are_free_even_if_an_ordinary_rule_matches_them() {
    let (_dir, f, q) = quota_setup().await;
    f.rules.lock().rules[0].value = "确认".into();
    q.receive(command("confirm-outside", "确认")).await;
    f.rules.lock().rules[0].value = "取消".into();
    q.receive(command("cancel-outside", "取消")).await;
    assert_eq!(q.accounts_list(0, "").await["total"], 0);
    assert!(q.status(0, "").await["waiting"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn confirmation_cancel_and_timeout_release_credit_then_run_only_logout_cleanup() {
    for decision in ["cancelled", "timeout"] {
        let (_dir, f, q) = quota_setup().await;
        install_confirmation(&f);
        q.prepare_automatic("phone", "default", "videos")
            .await
            .unwrap();
        q.activate_automatic().await.unwrap();
        q.receive(event("task")).await;
        q.tick().await;
        q.begin_confirmation("run-0", "phone", "default", 300)
            .await
            .unwrap();
        if decision == "cancelled" {
            q.receive(command("cancel-self", "取消")).await;
        } else {
            q.state.lock().await.items[0]
                .confirmation
                .as_mut()
                .unwrap()
                .deadline_at = now() - 1;
            assert_eq!(
                q.poll_confirmation("run-0", "phone", "default")
                    .await
                    .unwrap()["status"],
                "timeout"
            );
            q.receive(command("too-late", "确认")).await;
        }
        assert_eq!(account(&q).await["balance"], 1);
        assert_eq!(account(&q).await["reserved"], 0);
        assert!(
            f.cancelled.lock().is_empty(),
            "viewer decisions return normally, without Core cancel"
        );
        // A new task may reserve the released credit but cannot skip the
        // current main terminal and its no-reset logout cleanup.
        q.receive(event("next")).await;
        q.tick().await;
        assert_eq!(f.calls.load(Ordering::SeqCst), 1);
        f.finish("run-0", "success");
        q.tick().await;
        assert_eq!(
            q.status(0, "").await["current"]["main_result"]["state"],
            "cancelled"
        );
        assert!(
            f.proof_queries.lock().is_empty(),
            "declined gates do not require a reward proof"
        );
        q.tick().await;
        assert_eq!(
            f.submitted.lock()[1]["entrypoint"],
            "default/logout-only.yaml"
        );
        assert_eq!(f.submitted.lock()[1]["args"]["who"], "cancel-no-reset");
        f.finish("run-1", "success");
        q.tick().await;
        assert_eq!(q.status(0, "").await["history"][0]["state"], "cancelled");
        assert_eq!(q.status(0, "").await["paused"], false);
        q.tick().await;
        assert_eq!(f.calls.load(Ordering::SeqCst), 3);
    }
}

#[tokio::test]
async fn confirmation_deadline_uses_frozen_minimum_and_does_not_extend_on_reentry() {
    let (_dir, f, q) = quota_setup().await;
    install_confirmation(&f);
    f.rules.lock().rules[0]
        .confirmation
        .as_mut()
        .unwrap()
        .timeout_secs = 30;
    q.receive(event("frozen-gate")).await;
    f.rules.lock().rules[0]
        .confirmation
        .as_mut()
        .unwrap()
        .timeout_secs = 300;
    q.tick().await;
    let result = q
        .begin_confirmation("run-0", "phone", "default", 90)
        .await
        .unwrap();
    let deadline = result["deadline_at"].as_i64().unwrap();
    assert!((29_000..=30_000).contains(&(deadline - now())));
    assert_eq!(
        q.begin_confirmation("run-0", "phone", "default", 300)
            .await
            .unwrap()["deadline_at"],
        deadline
    );
    assert!(q
        .begin_confirmation("run-0", "phone", "default", 301)
        .await
        .is_err());
}

#[tokio::test]
async fn restart_keeps_accounts_reservations_and_deadline_but_never_replays_confirmation() {
    let (dir, f, q) = quota_setup().await;
    install_confirmation(&f);
    q.receive(event("before-restart")).await;
    q.tick().await;
    let gate = q
        .begin_confirmation("run-0", "phone", "default", 300)
        .await
        .unwrap();
    drop(q);
    let restored = Queue::open(dir.path().join("queue.json"), f.clone()).unwrap();
    let current = restored.status(0, "").await["current"].clone();
    assert_eq!(current["state"], "review");
    assert_eq!(current["confirmation"]["deadline_at"], gate["deadline_at"]);
    assert_eq!(account(&restored).await["balance"], 1);
    assert_eq!(account(&restored).await["reserved"], 1);
    restored.receive(command("after-restart", "确认")).await;
    assert_eq!(
        restored.status(0, "").await["current"]["confirmation"]["state"],
        "awaiting_confirmation"
    );
    assert!(restored
        .begin_confirmation("run-0", "phone", "default", 300)
        .await
        .is_err());
    restored.tick().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retry_reserves_again_with_frozen_cost_and_clears_previous_confirmation() {
    let (_dir, f, q) = quota_setup().await;
    install_confirmation(&f);
    q.receive(event("first-attempt")).await;
    q.tick().await;
    q.begin_confirmation("run-0", "phone", "default", 300)
        .await
        .unwrap();
    q.receive(command("no", "取消")).await;
    f.finish("run-0", "success");
    q.tick().await;
    q.tick().await;
    f.finish("run-1", "success");
    q.tick().await;
    let id = q.status(0, "").await["history"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.rules.lock().rules[0].cost = 100;
    q.control("retry", &[id], "retry-billing").await.unwrap();
    let item = q.status(0, "").await["waiting"][0].clone();
    assert_eq!(item["billing"]["cost"], 1);
    assert_eq!(item["billing"]["state"], "reserved");
    assert!(item["confirmation"].is_null());
    assert!(item["run_id"].is_null());
    assert_eq!(
        item["confirmation_spec"]["cancel_cleanup"]["entrypoint"],
        "default/logout-only.yaml"
    );
    assert_eq!(account(&q).await["balance"], 1);
    assert_eq!(account(&q).await["reserved"], 1);
}

struct CapturedNotices {
    queue: std::sync::Weak<Queue>,
    events: SyncMutex<Vec<Value>>,
}
impl NoticeSink for CapturedNotices {
    fn send(&self, event: Value) {
        let q = self.queue.upgrade().unwrap();
        let state = q
            .state
            .try_lock()
            .expect("notifier must run after dropping queue lock");
        let persisted: Value = serde_json::from_slice(&std::fs::read(&q.path).unwrap()).unwrap();
        assert_eq!(persisted["revision"], state.revision);
        self.events.lock().push(event);
    }
}
fn capture_notices(q: &Arc<Queue>) -> Arc<CapturedNotices> {
    let capture = Arc::new(CapturedNotices {
        queue: Arc::downgrade(q),
        events: SyncMutex::new(Vec::new()),
    });
    q.set_notifier(capture.clone());
    capture
}

#[tokio::test]
async fn notices_follow_durable_commit_outside_lock_and_quota_rejection_is_specific() {
    let (_dir, f, q) = quota_setup().await;
    let capture = capture_notices(&q);
    q.receive(event("first")).await;
    q.receive(event("quota-rejected")).await;
    q.receive(event("quota-rejected")).await;
    let notices = capture.events.lock().clone();
    assert_eq!(notices.iter().filter(|n| n["kind"] == "queued").count(), 1);
    let rejected: Vec<_> = notices.iter().filter(|n| n["kind"] == "rejected").collect();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0]["reason"], "quota_unavailable");
    assert_eq!(rejected[0]["test"], false);
    assert_eq!(rejected[0]["open_id"], "viewer");
    assert_eq!(rejected[0]["available"], 0);
    f.rules.lock().rules[0].cooldown_secs = 300;
    q.receive(event("cooldown-rejected")).await;
    f.offline.store(true, Ordering::SeqCst);
    q.receive(event("target-rejected")).await;
    assert_eq!(
        capture
            .events
            .lock()
            .iter()
            .filter(|n| n["kind"] == "rejected")
            .count(),
        1,
        "cooldown and target failures must not claim insufficient quota"
    );
}

#[tokio::test]
async fn persistence_failure_never_publishes_admission_or_mutates_user_balance() {
    let (dir, _f, q) = quota_setup().await;
    let capture = capture_notices(&q);
    std::fs::remove_file(dir.path().join("queue.json")).unwrap();
    std::fs::create_dir(dir.path().join("queue.json")).unwrap();
    q.receive(event("cannot-save")).await;
    assert!(capture.events.lock().is_empty());
    assert_eq!(q.accounts_list(0, "").await["total"], 0);
    assert!(q.status(0, "").await["waiting"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(q.status(0, "").await["paused"], true);
}

#[tokio::test]
async fn stale_or_invalid_message_times_cannot_confirm_but_fresh_seconds_and_milliseconds_can() {
    for precision in ["seconds", "milliseconds"] {
        let (_dir, f, q) = quota_setup().await;
        install_confirmation(&f);
        q.receive(event("task")).await;
        q.tick().await;
        q.begin_confirmation("run-0", "phone", "default", 300)
            .await
            .unwrap();
        let started = q.state.lock().await.items[0]
            .confirmation
            .as_ref()
            .unwrap()
            .started_at;
        for mode in [
            "old-host-receipt",
            "old-platform-seconds",
            "old-platform-milliseconds",
            "invalid-host-receipt",
        ] {
            let mut e = command(mode, "确认");
            match mode {
                "old-host-receipt" => {
                    e.received_at = chrono::DateTime::from_timestamp_millis(started - 1)
                        .unwrap()
                        .to_rfc3339()
                }
                "old-platform-seconds" => e.occurred_at = Some(started / 1000 - 1),
                "old-platform-milliseconds" => e.occurred_at = Some(started - 1),
                _ => e.received_at = "not-a-timestamp".into(),
            }
            q.receive(e).await;
            assert_eq!(
                q.poll_confirmation("run-0", "phone", "default")
                    .await
                    .unwrap()["status"],
                "waiting",
                "{mode}"
            );
        }
        let mut fresh = command("fresh-control", "确认");
        fresh.occurred_at = Some(if precision == "seconds" {
            now() / 1000
        } else {
            now()
        });
        q.receive(fresh).await;
        assert_eq!(
            q.poll_confirmation("run-0", "phone", "default")
                .await
                .unwrap()["status"],
            "confirmed"
        );
        assert_eq!(account(&q).await["balance"], 1);
        assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn simultaneous_confirm_and_cancel_have_one_durable_winner_without_charging() {
    let (_dir, f, q) = quota_setup().await;
    install_confirmation(&f);
    q.receive(event("task")).await;
    q.tick().await;
    q.begin_confirmation("run-0", "phone", "default", 300)
        .await
        .unwrap();
    let ((), ()) = tokio::join!(
        q.receive(command("yes", "确认")),
        q.receive(command("no", "取消"))
    );
    let state = q
        .poll_confirmation("run-0", "phone", "default")
        .await
        .unwrap()["status"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(["confirmed", "cancelled"].contains(&state.as_str()));
    assert_eq!(account(&q).await["balance"], 1);
    assert_eq!(
        account(&q).await["reserved"],
        if state == "confirmed" { 1 } else { 0 }
    );
    assert!(f.cancelled.lock().is_empty());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn checkpoint_review_after_restart_settles_actual_goal_without_replaying_cleanup() {
    for evidence in ["verified", "absent", "unavailable"] {
        let (dir, f, q) = quota_setup().await;
        install_confirmation(&f);
        q.receive(event("before-crash")).await;
        q.tick().await;
        q.begin_confirmation("run-0", "phone", "default", 300)
            .await
            .unwrap();
        q.receive(command("confirmed-before-crash", "确认")).await;
        let item_id = q.status(0, "").await["current"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        drop(q);
        let q = Queue::open(dir.path().join("queue.json"), f.clone()).unwrap();
        f.finish("run-0", "failed");
        if evidence != "absent" {
            verify_checkpoint(&f, "run-0");
        }
        f.checkpoint_error
            .store(evidence == "unavailable", Ordering::SeqCst);
        *f.active_override.lock() = Some(json!({"run_id":"run-0","state":"failed"}));
        assert!(
            q.control("resolve", std::slice::from_ref(&item_id), "")
                .await
                .is_err(),
            "owned slot must actually be released"
        );
        *f.active_override.lock() = None;
        let result = q
            .control("resolve", std::slice::from_ref(&item_id), "")
            .await;
        if evidence == "unavailable" {
            assert!(result.is_err());
            assert_eq!(q.status(0, "").await["current"]["state"], "review");
            assert_eq!(account(&q).await["reserved"], 1);
            f.checkpoint_error.store(false, Ordering::SeqCst);
            q.control("resolve", std::slice::from_ref(&item_id), "")
                .await
                .unwrap();
        } else {
            result.unwrap();
        }
        let history = q.status(0, "").await["history"][0].clone();
        assert_eq!(
            history["state"],
            if evidence == "absent" {
                "acknowledged"
            } else {
                "success"
            }
        );
        assert_eq!(
            history["billing"]["state"],
            if evidence == "absent" {
                "released"
            } else {
                "charged"
            }
        );
        assert_eq!(
            account(&q).await["balance"],
            if evidence == "absent" { 1 } else { 0 }
        );
        assert_eq!(account(&q).await["reserved"], 0);
        assert_eq!(f.calls.load(Ordering::SeqCst), 1);
        assert!(q.control("resolve", &[item_id], "").await.is_err());
        assert_eq!(
            account(&q).await["balance"],
            if evidence == "absent" { 1 } else { 0 }
        );
    }
}

#[tokio::test]
async fn legacy_child_reconciliation_cannot_override_billed_or_checkpoint_tasks() {
    let (_dir, f, q) = quota_setup().await;
    install_checkpoint(&f);
    q.receive(event("not-a-legacy-record")).await;
    q.tick().await;
    f.completed
        .lock()
        .insert("run-0:default/daily.yaml".into(), true);
    f.finish("run-0", "failed");
    q.tick().await;
    let id = q.status(0, "").await["history"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(q
        .reconcile_success(&id, "default/daily.yaml")
        .await
        .is_err());
    assert_eq!(q.status(0, "").await["history"][0]["state"], "failed");
    assert_eq!(account(&q).await["balance"], 1);
}

#[tokio::test]
async fn quota_accounts_outlive_expired_journal_history_and_reload_with_settings() {
    let (dir, f, q) = quota_setup().await;
    q.receive(event("long-lived-account")).await;
    q.tick().await;
    f.finish("run-0", "success");
    q.tick().await;
    q.state.lock().await.items[0].finished_at = Some(now() - 8 * 86400 * 1000);
    q.accounts_save_settings(7, 0).await.unwrap();
    assert!(q.status(0, "").await["history"]
        .as_array()
        .unwrap()
        .is_empty());
    drop(q);
    let q = Queue::open(dir.path().join("queue.json"), f.clone()).unwrap();
    assert_eq!(account(&q).await["balance"], 0);
    assert_eq!(
        q.accounts_settings().await,
        json!({"default_quota":7,"version":1})
    );
    q.receive(event("not-a-new-account")).await;
    assert!(q.status(0, "").await["waiting"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn timeout_notices_keep_one_stable_identity_and_queue_notice_has_fifo_position() {
    let (_dir, f, q) = quota_setup().await;
    install_confirmation(&f);
    let capture = capture_notices(&q);
    q.receive(event("task")).await;
    assert_eq!(capture.events.lock()[0]["position"], 1);
    q.tick().await;
    q.begin_confirmation("run-0", "phone", "default", 300)
        .await
        .unwrap();
    q.state.lock().await.items[0]
        .confirmation
        .as_mut()
        .unwrap()
        .deadline_at = now() - 1;
    q.poll_confirmation("run-0", "phone", "default")
        .await
        .unwrap();
    f.finish("run-0", "success");
    q.tick().await;
    let notices = capture.events.lock();
    let timeouts: Vec<_> = notices.iter().filter(|n| n["kind"] == "timeout").collect();
    assert_eq!(
        timeouts.len(),
        2,
        "gate and main-result commits share the same dedup identity"
    );
    assert_eq!(timeouts[0]["id"], timeouts[1]["id"]);
    assert_eq!(timeouts[0]["position"], 0);
    assert!(!notices.iter().any(|n| n["kind"] == "cancelled"));
}
