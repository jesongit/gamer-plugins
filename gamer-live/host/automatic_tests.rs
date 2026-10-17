use super::super::{
    events::LiveEvent,
    queue::{Backend, SubmitError},
    rules::{Rule, RuleSet},
};
use super::*;
use serde_json::Map;
use std::{
    collections::BTreeMap,
    sync::atomic::{AtomicBool, AtomicUsize},
};
use tokio::sync::{Notify, Semaphore};

struct FakeBackend {
    calls: AtomicUsize,
    cancels: AtomicUsize,
    offline: AtomicBool,
    conflict: AtomicBool,
    external: AtomicBool,
    cleanup: AtomicBool,
    runs: Mutex<BTreeMap<String, Value>>,
}
impl FakeBackend {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            cancels: AtomicUsize::new(0),
            offline: AtomicBool::new(false),
            conflict: AtomicBool::new(false),
            external: AtomicBool::new(false),
            cleanup: AtomicBool::new(false),
            runs: Mutex::new(BTreeMap::new()),
        })
    }
    fn finish(&self, id: &str, state: &str) {
        self.runs
            .lock()
            .insert(id.into(), json!({"run_id":id,"state":state}));
    }
}
#[async_trait]
impl Backend for FakeBackend {
    fn target(&self, device: &str, package: &str) -> Result<Target> {
        Ok(Target {
            device_id: device.into(),
            package_id: package.into(),
            android_package: None,
            package_stamp: "same".into(),
        })
    }
    fn check(&self, _: &Target) -> Result<()> {
        ensure!(!self.offline.load(Ordering::SeqCst), "设备离线");
        Ok(())
    }
    fn rules(&self, package: &str) -> Result<(RuleSet, Option<String>)> {
        Ok((
            RuleSet {
                schema_version: 1,
                external_origin: None,
                rules: vec![Rule {
                    id: "one".into(),
                    name: "动作".into(),
                    public_name: "动作".into(),
                    enabled: true,
                    kind: "message".into(),
                    operator: "equals".into(),
                    value: "开始".into(),
                    min_count: 1,
                    entrypoint: format!("{package}/play.yaml"),
                    args: BTreeMap::new(),
                    cooldown_secs: 0,
                    timeout_secs: 0,
                    cleanup: None,
                    cost: 1,
                    confirmation: None,
                    completion: None,
                }],
            },
            None,
        ))
    }
    fn save_rules(&self, _: &str, _: &RuleSet, _: Option<&str>) -> Result<String> {
        Ok("version".into())
    }
    fn describe(&self, _: &str) -> Result<Value> {
        Ok(json!({"schema":[]}))
    }
    async fn submit(
        &self,
        _: &Target,
        _: &str,
        _: &Map<String, Value>,
    ) -> std::result::Result<String, SubmitError> {
        if self.conflict.load(Ordering::SeqCst) {
            return Err(SubmitError::Busy);
        }
        let id = format!("run-{}", self.calls.fetch_add(1, Ordering::SeqCst));
        self.finish(&id, "running");
        Ok(id)
    }
    async fn run(&self, id: &str) -> Result<Option<Value>> {
        Ok(self.runs.lock().get(id).cloned())
    }
    async fn cancel(&self, id: &str) -> Result<()> {
        self.cancels.fetch_add(1, Ordering::SeqCst);
        self.finish(id, "stopping");
        Ok(())
    }
    fn active(&self, _: &str) -> Option<Value> {
        if self.external.load(Ordering::SeqCst) {
            return Some(json!({"run_id":"manual","state":"running"}));
        }
        self.runs
            .lock()
            .values()
            .rev()
            .find(|v| {
                ["running", "stopping"].contains(&v["state"].as_str().unwrap_or(""))
                    || self.cleanup.load(Ordering::SeqCst)
            })
            .cloned()
    }
}
struct FakeMedia {
    log: Mutex<Vec<String>>,
    alive: AtomicBool,
    fail_start: AtomicBool,
    fail_video: AtomicBool,
    slow_device: AtomicBool,
    slow_video: AtomicBool,
    slow_start: AtomicBool,
    busy_status: AtomicBool,
    entered: Notify,
    proceed: Semaphore,
}
impl FakeMedia {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            log: Mutex::new(vec![]),
            alive: AtomicBool::new(false),
            fail_start: AtomicBool::new(false),
            fail_video: AtomicBool::new(false),
            slow_device: AtomicBool::new(false),
            slow_video: AtomicBool::new(false),
            slow_start: AtomicBool::new(false),
            busy_status: AtomicBool::new(false),
            entered: Notify::new(),
            proceed: Semaphore::new(0),
        })
    }
    async fn delay(&self, slow: bool) {
        if slow {
            self.entered.notify_one();
            self.proceed.acquire().await.unwrap().forget();
        }
    }
}
#[async_trait]
impl Media for FakeMedia {
    async fn start(&self, _: OutputRequest) -> Result<()> {
        self.delay(self.slow_start.load(Ordering::SeqCst)).await;
        ensure!(!self.fail_start.load(Ordering::SeqCst), "文件夹无法播放");
        self.alive.store(true, Ordering::SeqCst);
        self.log.lock().push("start-video".into());
        Ok(())
    }
    async fn device(&self, _: &str) -> Result<()> {
        self.delay(self.slow_device.load(Ordering::SeqCst)).await;
        self.log.lock().push("device".into());
        Ok(())
    }
    async fn video(&self) -> Result<()> {
        self.delay(self.slow_video.load(Ordering::SeqCst)).await;
        ensure!(!self.fail_video.load(Ordering::SeqCst), "视频恢复失败");
        self.log.lock().push("video".into());
        Ok(())
    }
    async fn stop(&self) {
        self.log.lock().push("stop".into());
        self.alive.store(false, Ordering::SeqCst);
    }
    async fn status(&self) -> Option<Value> {
        if self.busy_status.load(Ordering::SeqCst) {
            return None;
        }
        self.reliable_status().await
    }
    async fn reliable_status(&self) -> Option<Value> {
        self.alive
            .load(Ordering::SeqCst)
            .then(|| json!({"state":"running","source_url":"same-url"}))
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    queue: Arc<Queue>,
    backend: Arc<FakeBackend>,
    media: Arc<FakeMedia>,
    auto: Arc<Automatic>,
}
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let backend = FakeBackend::new();
    let queue = Queue::open(dir.path().join("queue.json"), backend.clone()).unwrap();
    let media = FakeMedia::new();
    let auto = Arc::new(Automatic {
        state: Mutex::new(Status::default()),
        epoch: AtomicU64::new(0),
        lifecycle: AsyncMutex::new(()),
        queue: Arc::downgrade(&queue),
        media: media.clone(),
    });
    queue.set_hooks(auto.clone());
    Fixture {
        _dir: dir,
        queue,
        backend,
        media,
        auto,
    }
}
fn settings() -> Value {
    json!({"device_id":"phone","package_id":"default","video_folder":"D:/videos","mode":"local"})
}
fn event(id: &str) -> LiveEvent {
    LiveEvent {
        schema_version: 1,
        seq: 0,
        platform_id: "test".into(),
        connection_id: "same".into(),
        identity_namespace: "qa-app".into(),
        room_id: "room".into(),
        event_id: Some(id.into()),
        kind: "message".into(),
        occurred_at: None,
        received_at: String::new(),
        actor: Some(json!({"id":format!("viewer-{id}"),"name":"测试观众"})),
        payload: json!({"text":"开始"}),
        platform_data: Value::Null,
    }
}
async fn wait_until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !ready() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn fifo_stays_on_device_between_items_and_restores_only_after_cleanup() {
    let f = fixture();
    f.auto.start(settings()).await.unwrap();
    assert_eq!(
        f.backend.calls.load(Ordering::SeqCst),
        0,
        "video must not reserve a Core run"
    );
    f.queue.receive(event("first")).await;
    f.queue.receive(event("second")).await;
    f.queue.tick().await;
    f.backend.finish("run-0", "success");
    f.queue.tick().await;
    assert!(!f.media.log.lock().contains(&"video".into()));
    f.queue.tick().await;
    assert_eq!(f.backend.calls.load(Ordering::SeqCst), 2);
    f.backend.finish("run-1", "success");
    f.backend.cleanup.store(true, Ordering::SeqCst);
    f.queue.tick().await;
    assert!(!f.media.log.lock().contains(&"video".into()));
    f.backend.cleanup.store(false, Ordering::SeqCst);
    f.queue.tick().await;
    assert_eq!(
        f.media.log.lock().iter().filter(|s| *s == "video").count(),
        1
    );
    assert_eq!(f.auto.status().await.unwrap()["state"], "video");
}

#[tokio::test]
async fn cancelled_workflows_preserve_waiting_viewers_until_explicit_recovery() {
    for outcome in ["cancelled"] {
        let f = fixture();
        f.auto.start(settings()).await.unwrap();
        f.queue.receive(event("first")).await;
        f.queue.receive(event("second")).await;
        f.queue.tick().await;
        f.backend.finish("run-0", outcome);
        f.backend.cleanup.store(true, Ordering::SeqCst);
        f.queue.tick().await;
        assert_eq!(f.queue.status(0, "").await["current"]["state"], "running");
        assert!(!f.media.log.lock().contains(&"video".into()));
        f.backend.cleanup.store(false, Ordering::SeqCst);
        f.queue.tick().await;
        let status = f.auto.status().await.unwrap();
        assert_eq!(status["state"], "paused");
        assert_eq!(status["queue"]["paused"], true);
        assert_eq!(
            status["queue"]["blocked"],
            "执行未完成，请检查目标状态后恢复队列"
        );
        assert!(status["queue"]["current"].is_null());
        assert_eq!(status["queue"]["waiting"].as_array().unwrap().len(), 1);
        assert_eq!(status["queue"]["history"][0]["state"], outcome);
        assert!(f.media.log.lock().contains(&"video".into()));
        f.queue.tick().await;
        assert_eq!(f.backend.calls.load(Ordering::SeqCst), 1);
        f.queue.control("resume", &[], "").await.unwrap();
        f.queue.tick().await;
        assert_eq!(f.backend.calls.load(Ordering::SeqCst), 2);
    }
}

#[tokio::test]
async fn failed_workflow_restores_video_before_continuing_fifo_and_preserves_its_error() {
    let f = fixture();
    f.auto.start(settings()).await.unwrap();
    for id in ["first", "second", "third"] {
        f.queue.receive(event(id)).await;
    }
    f.queue.tick().await;
    f.backend.runs.lock().insert(
        "run-0".into(),
        json!({"run_id":"run-0","state":"failed","error":"测试日常步骤失败"}),
    );
    f.backend.cleanup.store(true, Ordering::SeqCst);
    f.queue.tick().await;
    assert_eq!(f.queue.status(0, "").await["current"]["state"], "running");
    assert_eq!(f.backend.calls.load(Ordering::SeqCst), 1);
    assert!(!f.media.log.lock().contains(&"video".into()));

    f.backend.cleanup.store(false, Ordering::SeqCst);
    f.media.slow_video.store(true, Ordering::SeqCst);
    let q = f.queue.clone();
    let restore = tokio::spawn(async move { q.tick().await });
    f.media.entered.notified().await;
    let status = f.queue.status(0, "").await;
    assert!(status["current"].is_null());
    assert_eq!(status["waiting"].as_array().unwrap().len(), 2);
    assert_eq!(status["paused"], false);
    assert!(status["blocked"].is_null());
    assert_eq!(status["history"][0]["state"], "failed");
    assert_eq!(status["history"][0]["error"], "测试日常步骤失败");
    let q = f.queue.clone();
    let next = tokio::spawn(async move { q.tick().await });
    tokio::task::yield_now().await;
    assert!(
        !next.is_finished(),
        "dispatch must wait for video restoration"
    );
    assert_eq!(f.backend.calls.load(Ordering::SeqCst), 1);
    f.media.slow_video.store(false, Ordering::SeqCst);
    f.media.proceed.add_permits(1);
    restore.await.unwrap();
    next.await.unwrap();
    assert_eq!(
        *f.media.log.lock(),
        ["start-video", "device", "video", "device"]
    );
    let status = f.auto.status().await.unwrap();
    assert_eq!(status["state"], "executing");
    assert!(status["error"].is_null());
    assert_eq!(
        status["queue"]["current"]["event"]["message"]["event_id"],
        "second"
    );

    f.backend.finish("run-1", "success");
    f.queue.tick().await;
    f.queue.tick().await;
    assert_eq!(f.backend.calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        f.queue.status(0, "").await["current"]["event"]["message"]["event_id"],
        "third"
    );
    f.backend.finish("run-2", "success");
    f.queue.tick().await;
    let status = f.auto.status().await.unwrap();
    assert_eq!(status["state"], "video");
    assert!(status["error"].is_null());
    assert_eq!(status["queue"]["paused"], false);
    assert!(status["queue"]["blocked"].is_null());
    assert_eq!(status["queue"]["waiting"], json!([]));
    assert_eq!(status["queue"]["history"][0]["state"], "success");
    assert_eq!(status["queue"]["history"][1]["state"], "success");
    assert_eq!(status["queue"]["history"][2]["state"], "failed");
    assert_eq!(status["queue"]["history"][2]["error"], "测试日常步骤失败");
    let reopened = Queue::open(f._dir.path().join("queue.json"), f.backend.clone()).unwrap();
    let saved = reopened.status(0, "failed").await;
    assert_eq!(saved["history"][0]["error"], "测试日常步骤失败");
    assert_eq!(saved["paused"], false);
    assert!(saved["blocked"].is_null());
}

#[tokio::test]
async fn failed_workflow_still_blocks_when_restoring_video_fails() {
    let f = fixture();
    f.auto.start(settings()).await.unwrap();
    f.queue.receive(event("first")).await;
    f.queue.receive(event("second")).await;
    f.queue.tick().await;
    f.media.fail_video.store(true, Ordering::SeqCst);
    f.backend.finish("run-0", "failed");
    f.queue.tick().await;
    let status = f.auto.status().await.unwrap();
    assert_eq!(status["state"], "paused");
    assert_eq!(status["error"], "视频恢复失败");
    assert_eq!(status["queue"]["blocked"], "视频恢复失败");
    assert_eq!(status["queue"]["paused"], true);
    assert_eq!(status["queue"]["history"][0]["state"], "failed");
    assert_eq!(status["queue"]["waiting"].as_array().unwrap().len(), 1);
    f.queue.tick().await;
    assert_eq!(f.backend.calls.load(Ordering::SeqCst), 1);
    f.media.fail_video.store(false, Ordering::SeqCst);
    f.queue.control("resume", &[], "").await.unwrap();
    f.queue.tick().await;
    assert_eq!(f.backend.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn slow_source_preparation_keeps_admission_open_and_stop_prevents_submission() {
    let f = fixture();
    f.auto.start(settings()).await.unwrap();
    f.media.slow_device.store(true, Ordering::SeqCst);
    f.queue.receive(event("first")).await;
    let q = f.queue.clone();
    let tick = tokio::spawn(async move { q.tick().await });
    f.media.entered.notified().await;
    tokio::time::timeout(
        std::time::Duration::from_millis(100),
        f.queue.receive(event("second")),
    )
    .await
    .unwrap();
    assert_eq!(
        f.queue.status(0, "").await["waiting"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let a = f.auto.clone();
    let stop = tokio::spawn(async move { a.stop().await });
    wait_until(|| f.auto.state.lock().state == "stopping").await;
    assert!(!stop.is_finished());
    f.media.proceed.add_permits(1);
    tick.await.unwrap();
    stop.await.unwrap().unwrap();
    assert_eq!(f.backend.calls.load(Ordering::SeqCst), 0);
    f.queue.receive(event("after-stop")).await;
    f.queue.tick().await;
    assert_eq!(f.queue.status(0, "").await["waiting"], json!([]));
    assert!(f.queue.control("resume", &[], "").await.is_err());
    f.queue.new_connection().await.unwrap();
    f.queue.receive(event("reconnect")).await;
    assert_eq!(f.queue.status(0, "").await["waiting"], json!([]));
}

#[tokio::test]
async fn busy_submission_keeps_fifo_and_dependency_failure_pauses_with_video() {
    let f = fixture();
    f.auto.start(settings()).await.unwrap();
    f.queue.receive(event("first")).await;
    f.backend.conflict.store(true, Ordering::SeqCst);
    f.queue.tick().await;
    assert_eq!(
        f.queue.status(0, "").await["waiting"][0]["event"]["message"]["event_id"],
        "first"
    );
    assert_eq!(f.auto.status().await.unwrap()["state"], "video");
    f.backend.conflict.store(false, Ordering::SeqCst);
    f.backend.offline.store(true, Ordering::SeqCst);
    f.queue.tick().await;
    assert_eq!(f.auto.status().await.unwrap()["state"], "paused");
    assert_eq!(f.backend.calls.load(Ordering::SeqCst), 0);
    f.backend.offline.store(false, Ordering::SeqCst);
    f.queue.control("resume", &[], "").await.unwrap();
    f.queue.tick().await;
    assert_eq!(f.backend.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_initial_preparation_restores_the_ordinary_queue() {
    let f = fixture();
    f.queue.configure("original", "default").await.unwrap();
    let previous = f.queue.status(0, "").await;
    f.media.fail_start.store(true, Ordering::SeqCst);
    assert!(f.auto.start(settings()).await.is_err());
    let current = f.queue.status(0, "").await;
    assert_eq!(current["target"], previous["target"]);
    assert_eq!(current["paused"], previous["paused"]);
    f.queue.receive(event("ordinary")).await;
    f.queue.tick().await;
    assert_eq!(f.backend.calls.load(Ordering::SeqCst), 1);
    assert!(f.auto.ensure_manual().is_ok());
}

#[tokio::test]
async fn stopped_status_restores_saved_folder_and_successful_start_remembers_changes() {
    let f = fixture();
    f.queue
        .configure_with_folder("phone", "default", Some("D:/saved-videos"))
        .await
        .unwrap();
    let stopped = f.auto.status().await.unwrap();
    assert_eq!(stopped["state"], "stopped");
    assert_eq!(stopped["video_folder"], "D:/saved-videos");
    assert_eq!(stopped["queue"]["target"]["device_id"], "phone");
    f.media.fail_start.store(true, Ordering::SeqCst);
    assert!(f.auto.start(settings()).await.is_err());
    assert_eq!(
        f.auto.status().await.unwrap()["video_folder"],
        "D:/saved-videos"
    );
    f.media.fail_start.store(false, Ordering::SeqCst);
    f.auto.start(settings()).await.unwrap();
    f.auto.stop().await.unwrap();
    assert_eq!(f.auto.status().await.unwrap()["video_folder"], "D:/videos");
    assert_eq!(
        f.queue.status(0, "").await["automatic_video_folder"],
        "D:/videos"
    );
}

#[tokio::test]
async fn stop_waits_for_its_run_cleanup_and_does_not_cancel_manual_competitors() {
    let f = fixture();
    f.auto.start(settings()).await.unwrap();
    f.queue.receive(event("first")).await;
    f.queue.tick().await;
    f.backend.cleanup.store(true, Ordering::SeqCst);
    let a = f.auto.clone();
    let stop = tokio::spawn(async move { a.stop().await });
    wait_until(|| f.backend.cancels.load(Ordering::SeqCst) == 1).await;
    f.backend.finish("run-0", "cancelled");
    tokio::time::sleep(std::time::Duration::from_millis(70)).await;
    assert!(!stop.is_finished());
    assert!(f.media.alive.load(Ordering::SeqCst));
    f.backend.cleanup.store(false, Ordering::SeqCst);
    f.backend.external.store(true, Ordering::SeqCst);
    stop.await.unwrap().unwrap();
    assert_eq!(f.backend.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(f.auto.status().await.unwrap()["state"], "stopped");
    assert!(f.backend.external.load(Ordering::SeqCst));
}

#[tokio::test]
async fn stop_during_initial_video_preparation_cannot_activate_the_queue() {
    let f = fixture();
    f.media.slow_start.store(true, Ordering::SeqCst);
    let a = f.auto.clone();
    let start = tokio::spawn(async move { a.start(settings()).await });
    f.media.entered.notified().await;
    let a = f.auto.clone();
    let stop = tokio::spawn(async move { a.stop().await });
    wait_until(|| f.auto.state.lock().state == "stopping").await;
    f.media.proceed.add_permits(1);
    assert!(start.await.unwrap().is_err());
    stop.await.unwrap().unwrap();
    f.queue.receive(event("late")).await;
    f.queue.tick().await;
    assert_eq!(f.backend.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.auto.status().await.unwrap()["state"], "stopped");
}

#[tokio::test]
async fn duplicate_stop_cannot_close_a_new_session_queued_between_the_callers() {
    let f = fixture();
    f.auto.start(settings()).await.unwrap();
    let lifecycle = f.auto.lifecycle.lock().await;
    let a = f.auto.clone();
    let first = tokio::spawn(async move { a.stop().await });
    wait_until(|| f.auto.state.lock().state == "stopping").await;
    let stop_epoch = f.auto.epoch.load(Ordering::SeqCst);
    let a = f.auto.clone();
    let next = tokio::spawn(async move { a.start(settings()).await });
    tokio::task::yield_now().await;
    let a = f.auto.clone();
    let duplicate = tokio::spawn(async move { a.stop().await });
    tokio::task::yield_now().await;
    assert_eq!(f.auto.epoch.load(Ordering::SeqCst), stop_epoch);
    drop(lifecycle);
    first.await.unwrap().unwrap();
    next.await.unwrap().unwrap();
    duplicate.await.unwrap().unwrap();
    assert_eq!(
        f.media.log.lock().iter().filter(|s| *s == "stop").count(),
        1
    );
    assert!(f.media.alive.load(Ordering::SeqCst));
    f.queue.receive(event("new-session")).await;
    f.queue.tick().await;
    assert_eq!(f.backend.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_busy_fast_snapshot_does_not_report_a_dead_output_to_idle_dispatch() {
    let f = fixture();
    f.auto.start(settings()).await.unwrap();
    f.queue.receive(event("first")).await;
    f.queue.tick().await;
    f.media.busy_status.store(true, Ordering::SeqCst);
    assert!(f.auto.status().await.unwrap()["stream"].is_null());
    f.backend.finish("run-0", "success");
    f.queue.tick().await;
    assert_eq!(f.auto.status().await.unwrap()["state"], "video");
    assert_eq!(f.queue.status(0, "").await["paused"], false);
}

#[tokio::test]
async fn storage_failure_during_preparation_keeps_the_first_item_retryable() {
    let f = fixture();
    f.auto.start(settings()).await.unwrap();
    f.media.slow_device.store(true, Ordering::SeqCst);
    f.queue.receive(event("first")).await;
    let q = f.queue.clone();
    let tick = tokio::spawn(async move { q.tick().await });
    f.media.entered.notified().await;
    let path = f._dir.path().join("queue.json");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    f.queue.receive(event("second")).await;
    assert_eq!(f.queue.status(0, "").await["paused"], true);
    f.media.slow_device.store(false, Ordering::SeqCst);
    f.media.proceed.add_permits(1);
    tick.await.unwrap();
    let status = f.queue.status(0, "").await;
    assert!(status["current"].is_null());
    assert_eq!(
        status["waiting"][0]["event"]["message"]["event_id"],
        "first"
    );
    assert_eq!(f.auto.status().await.unwrap()["state"], "paused");
    std::fs::remove_dir(&path).unwrap();
    f.queue.control("resume", &[], "").await.unwrap();
    f.queue.tick().await;
    assert_eq!(f.backend.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn plugin_shutdown_closes_resources_even_when_interactive_stop_needs_review() {
    let f = fixture();
    f.auto.start(settings()).await.unwrap();
    f.queue.receive(event("first")).await;
    f.queue.tick().await;
    f.backend.runs.lock().clear();
    f.queue.tick().await;
    assert_eq!(f.queue.status(0, "").await["current"]["state"], "review");
    assert!(f.auto.stop().await.is_err());
    assert!(f.media.alive.load(Ordering::SeqCst));
    f.auto.shutdown().await;
    assert!(!f.media.alive.load(Ordering::SeqCst));
    assert!(f.auto.ensure_manual().is_ok());
    assert_eq!(f.queue.status(0, "").await["current"]["state"], "review");
    assert_eq!(f.backend.cancels.load(Ordering::SeqCst), 0);
    let id = f.queue.status(0, "").await["current"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.queue.control("resolve", &[id], "").await.unwrap();
    f.auto.start(settings()).await.unwrap();
    assert_eq!(f.auto.status().await.unwrap()["state"], "video");
}
