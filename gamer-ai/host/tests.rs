use super::*;
use crate::{
    capabilities::adapters, config::Config, core::AppContext, extensions::service::BuiltinService,
    store::Store,
};
use std::io::Write;

struct NoExecutor;
impl RunExecutor for NoExecutor {
    fn prepare<'a>(&'a self, _: &'a RunContext, _: &'a RunRequest) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("unexpected default executor") })
    }
    fn execute<'a>(
        &'a self,
        _: &'a RunContext,
        _: &'a RunRequest,
        _: bool,
        _: Arc<AtomicBool>,
    ) -> BoxFuture<'a, Result<Vec<(String, String)>>> {
        Box::pin(async { anyhow::bail!("unexpected default executor") })
    }
    fn acquire(&self, _: &RunContext) -> Result<Box<dyn ActivityLease>> {
        unreachable!()
    }
}
async fn fixture() -> (tempfile::TempDir, Arc<AiService>, Arc<ExtensionService>) {
    let root = tempfile::tempdir().unwrap();
    let cfg = crate::config::Config {
        data_dir: root.path().into(),
        // This process-local opt-in only selects the isolated test browser;
        // it does not alter production config or global browser detection.
        browser_path: std::env::var("GAMER_AI_TEST_BROWSER")
            .unwrap_or_default()
            .trim()
            .to_string(),
        ..Default::default()
    };
    let db = Arc::new(Store::open(&cfg).unwrap());
    let packages = Arc::new(PackageStore::open(&cfg).unwrap());
    packages.ensure_default_package().unwrap();
    let devices = Arc::new(DeviceManager::new(db.clone(), cfg));
    let runs = Arc::new(RunManager::new(Arc::new(NoExecutor)));
    let scheduler = Arc::new(Scheduler::new(db.clone()));
    let capabilities =
        adapters::build_registry(devices.clone(), packages.clone(), db, runs.clone());
    let ai = Arc::new(
        AiService::new(
            Runtime {
                devices,
                packages,
                runs: runs.clone(),
                scheduler,
                capabilities: capabilities.clone(),
            },
            root.path(),
        )
        .unwrap(),
    );
    runs.register_executor(ID, ai.executor());
    let extensions = Arc::new(
        ExtensionService::for_data_root(root.path(), capabilities)
            .with_builtin_service(ai.clone())
            .with_runner_registrar(ai.registrar()),
    );
    ai.attach(&extensions);
    let mut bytes = Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut bytes);
        for (name, data) in [
            ("manifest.toml", include_str!("../manifest.toml")),
            ("ui/plugin.js", "export const sdkVersion=1;"),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }
    let id = ExtensionId::parse(ID).unwrap();
    extensions.install(bytes.get_ref()).await.unwrap();
    extensions.enable(&id).await.unwrap();
    extensions.start(&id).await.unwrap();
    (root, ai, extensions)
}
use std::io::Cursor;
fn record() -> SessionRecord {
    SessionRecord {
        session_id: "s".into(),
        run_id: "r".into(),
        device_id: "d".into(),
        android_package: None,
        content_package: "default".into(),
        goal: "goal".into(),
        mode: "api".into(),
        state: "running".into(),
        generation: 1,
        reason: None,
        pause_reason: None,
        messages: vec![UserMessage::new("goal")],
        limits: Limits::default(),
        usage: Usage::default(),
        events: vec![],
    }
}
fn unlimited_limits() -> Limits {
    Limits {
        max_turns: 0,
        max_actions: 0,
        max_seconds: 0,
        max_tokens: 0,
        max_failures: 0,
    }
}
fn session_record(record: SessionRecord, lease: Option<ControlLease>) -> Arc<Session> {
    Arc::new(Session {
        record: Mutex::new(record),
        lease: AsyncMutex::new(lease),
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
    })
}
async fn controlled_session(ai: &AiService) -> Arc<Session> {
    let mut r = record();
    let lease = ai
        .state
        .runtime
        .devices
        .controls
        .claim(&r.device_id, &r.session_id)
        .await
        .unwrap();
    r.generation = lease.generation;
    let session = session_record(r, Some(lease));
    ai.state.sessions.lock().insert("s".into(), session.clone());
    session
}
async fn controlled_browser_session(ai: &AiService) -> Arc<Session> {
    let target = crate::browser::BrowserTarget {
        id: "browser-unlimited-budget".into(),
        name: "synthetic budget target".into(),
        url: "http://localhost/".into(),
        profile_id: "unlimited-budget".into(),
        width: 640,
        height: 480,
    };
    ai.state
        .runtime
        .devices
        .browsers
        .db
        .save_browser_target(target.clone())
        .unwrap();
    let lease = ai
        .state
        .runtime
        .devices
        .controls
        .claim(&target.id, "s")
        .await
        .unwrap();
    let mut record = record();
    record.device_id = target.id;
    record.generation = lease.generation;
    let session = session_record(record, Some(lease));
    ai.state.sessions.lock().insert("s".into(), session.clone());
    session
}

async fn model_loop_fixture(
    limits: Limits,
    delay: Duration,
    fail: bool,
) -> (
    tempfile::TempDir,
    Arc<AiService>,
    Arc<ExtensionService>,
    Arc<Session>,
    tokio::task::JoinHandle<()>,
) {
    use axum::{http::StatusCode, routing::post, Json, Router};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new().route(
        "/responses",
        post(move |Json(body): Json<Value>| async move {
            assert!(body["input"].to_string().contains("input_image"));
            tokio::time::sleep(delay).await;
            if fail {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error":{"message":"synthetic temporary failure"}})),
                )
            } else {
                (
                    StatusCode::OK,
                    Json(json!({"status":"completed","output":[{
                        "type":"function_call","id":"finish","call_id":"finish",
                        "name":"session_finish","arguments":"{\"message\":\"synthetic goal completed\"}"
                    }],"usage":{"total_tokens":4}})),
                )
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (root, ai, extensions) = fixture().await;
    let saved = ai.state.settings.read().unwrap();
    ai.dispatch(
        "settings.save",
        json!({"expected_version":saved["version"],"base_url":base,
        "model":"synthetic","protocol":"responses","api_key":"synthetic-local-only",
        "request_timeout_secs":5}),
    )
    .await
    .unwrap();
    let session = controlled_browser_session(&ai).await;
    session.record.lock().limits = limits;
    *session.active_since.lock() = Some(Instant::now());
    // Seed a genuine synthetic image receipt so the production model loop
    // exercises request timing and failures without starting a browser or ADB.
    let mut png = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([1, 2, 3, 255]),
    ))
    .write_to(&mut png, image::ImageFormat::Png)
    .unwrap();
    let generation = session.record.lock().generation;
    let capture = mcp::ToolResult::image(
        png.get_ref(),
        "image/png",
        json!({"frame_id":"synthetic-frame","generation":generation,"width":1,"height":1}),
    )
    .value();
    session.results.lock().insert(
        format!("{generation}:capture:{generation}"),
        json!({"fingerprint":serde_json::to_string(&json!(["screen_capture",{}])).unwrap(),"result":capture}),
    );
    (root, ai, extensions, session, server)
}
#[test]
fn budgets_are_bounded_and_tokens_unknown_are_not_zero() {
    let mut r = record();
    assert!(budget_reason(&r).is_none());
    r.usage.actions = r.limits.max_actions;
    assert!(budget_reason(&r).is_some());
    r.usage.actions = 0;
    r.usage.total_tokens = Some(r.limits.max_tokens);
    assert!(budget_reason(&r).is_some());
    assert!(Limits {
        max_seconds: 9,
        ..Limits::default()
    }
    .validate()
    .is_err());
    assert!(constant_eq("abc", "abc"));
    assert!(!constant_eq("abc", "abd"));
}

#[test]
fn every_zero_budget_is_unlimited_and_nonzero_ranges_stay_bounded() {
    let mut r = record();
    r.limits = unlimited_limits();
    r.limits.validate().unwrap();
    r.usage = Usage {
        turns: u32::MAX,
        actions: u32::MAX,
        active_seconds: 100_000.0,
        total_tokens: Some(u64::MAX),
        known_tokens: u64::MAX,
        has_unknown_tokens: true,
        consecutive_failures: u32::MAX,
    };
    ensure_budget_available(&r).unwrap();
    assert!(activity_budget_remaining(&r).is_none());
    for (field, invalid) in [
        ("max_turns", 501),
        ("max_actions", 2001),
        ("max_seconds", 9),
        ("max_seconds", 7201),
        ("max_tokens", 2047),
        ("max_tokens", 2_000_001),
        ("max_failures", 21),
    ] {
        let mut value = serde_json::to_value(unlimited_limits()).unwrap();
        value[field] = json!(invalid);
        assert!(serde_json::from_value::<Limits>(value)
            .unwrap()
            .validate()
            .is_err());
    }
    for (field, maximum, expected) in [
        ("max_turns", 1, "budget_turns"),
        ("max_actions", 1, "budget_actions"),
        ("max_seconds", 10, "budget_seconds"),
        ("max_tokens", 2048, "budget_tokens"),
    ] {
        let mut value = serde_json::to_value(unlimited_limits()).unwrap();
        value[field] = json!(maximum);
        r.limits = serde_json::from_value(value).unwrap();
        r.limits.validate().unwrap();
        assert_eq!(budget_reason(&r).unwrap().code, expected);
    }
}

#[tokio::test]
async fn all_zero_budgets_admit_a_real_tool_and_saturate_usage() {
    let (_root, ai, _extensions) = fixture().await;
    let session = controlled_browser_session(&ai).await;
    {
        let mut r = session.record.lock();
        r.limits = unlimited_limits();
        r.usage.actions = u32::MAX;
        r.usage.active_seconds = 100_000.0;
        r.usage.known_tokens = u64::MAX;
        r.events.push(Event {
            seq: u64::MAX,
            at: "now".into(),
            kind: "test".into(),
            message: String::new(),
            data: json!({}),
        });
    }
    let generation = session.record.lock().generation;
    let result = ai
        .state
        .tool(
            &session,
            "wait",
            json!({"duration_ms":1}),
            "unlimited-wait",
            generation,
        )
        .await
        .unwrap();
    assert_ne!(result["isError"], true);
    assert_eq!(session.record.lock().usage.actions, u32::MAX);
    assert_eq!(session.record.lock().usage.known_tokens, u64::MAX);
    assert_eq!(session.record.lock().events.last().unwrap().seq, u64::MAX);
    assert!(session
        .record
        .lock()
        .events
        .iter()
        .any(|event| event.kind == "tool"
            && event.data["phase"] == "result"
            && event.data["ok"] == true));
    session.record.lock().limits.max_actions = 1;
    assert!(ai
        .state
        .tool(
            &session,
            "wait",
            json!({"duration_ms":1}),
            "finite-wait",
            generation
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("工具调用达到预算"));
    ai.state.stop(&session, "cancelled").await.unwrap();
}

#[tokio::test]
async fn unlimited_failures_keep_the_model_loop_running_with_pause_and_stop_available() {
    let (_root, ai, _extensions, session, server) =
        model_loop_fixture(unlimited_limits(), Duration::ZERO, true).await;
    let original = session.record.lock().generation;
    let runner = {
        let state = ai.state.clone();
        let session = session.clone();
        tokio::spawn(async move { state.run(&session, Arc::new(AtomicBool::new(false))).await })
    };
    let began = Instant::now();
    tokio::time::timeout(Duration::from_secs(8), async {
        while session.record.lock().usage.consecutive_failures < 5 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        began.elapsed() >= Duration::from_millis(1800),
        "Failed requests must retain retry backoff"
    );
    assert_eq!(session.record.lock().state, "running");
    assert_eq!(session.record.lock().generation, original);
    assert!(session.record.lock().pause_reason.is_none());
    ai.state.pause(&session, "用户暂停".into()).await.unwrap();
    assert_eq!(session.record.lock().state, "paused");
    assert!(
        ai.state
            .runtime
            .devices
            .controls
            .status("browser-unlimited-budget")
            .manual_allowed
    );
    ai.state.stop(&session, "cancelled").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), runner)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(session.record.lock().state, "finished");
    assert!(ai
        .state
        .runtime
        .devices
        .browsers
        .session("browser-unlimited-budget")
        .is_err());
    server.abort();
}

#[tokio::test]
async fn unlimited_seconds_wait_for_the_model_and_execute_its_tool() {
    let (_root, ai, _extensions, session, server) =
        model_loop_fixture(unlimited_limits(), Duration::from_millis(150), false).await;
    {
        let mut r = session.record.lock();
        r.usage.turns = u32::MAX;
        r.usage.known_tokens = u64::MAX;
    }
    let began = Instant::now();
    tokio::time::timeout(
        Duration::from_secs(3),
        ai.state.run(&session, Arc::new(AtomicBool::new(false))),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(began.elapsed() >= Duration::from_millis(100));
    let r = session.record.lock();
    assert_eq!(r.state, "finished");
    assert_eq!(r.usage.turns, u32::MAX);
    assert_eq!(r.usage.known_tokens, u64::MAX);
    assert_eq!(r.usage.actions, 1);
    assert!(!r
        .events
        .iter()
        .any(|event| event.data["code"] == "budget_seconds"));
    assert!(ai
        .state
        .runtime
        .devices
        .browsers
        .session("browser-unlimited-budget")
        .is_err());
    server.abort();
}

#[tokio::test]
async fn finite_seconds_still_interrupt_a_pending_model_request() {
    let mut limits = unlimited_limits();
    limits.max_seconds = 10;
    let (_root, ai, _extensions, session, server) =
        model_loop_fixture(limits, Duration::from_secs(2), false).await;
    session.record.lock().usage.active_seconds = 9.0;
    *session.active_since.lock() = Some(Instant::now());
    let runner = {
        let state = ai.state.clone();
        let session = session.clone();
        tokio::spawn(async move { state.run(&session, Arc::new(AtomicBool::new(false))).await })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while session.record.lock().state != "paused" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        session.record.lock().pause_reason.as_ref().unwrap().code,
        "budget_seconds"
    );
    assert_eq!(session.record.lock().usage.turns, 1);
    assert_eq!(session.record.lock().usage.actions, 0);
    ai.state.stop(&session, "cancelled").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), runner)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.abort();
}

#[tokio::test]
async fn finite_failures_still_pause_when_other_budgets_are_unlimited() {
    let mut limits = unlimited_limits();
    limits.max_failures = 2;
    let (_root, ai, _extensions, session, server) =
        model_loop_fixture(limits, Duration::ZERO, true).await;
    let runner = {
        let state = ai.state.clone();
        let session = session.clone();
        tokio::spawn(async move { state.run(&session, Arc::new(AtomicBool::new(false))).await })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while session.record.lock().state != "paused" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        session.record.lock().pause_reason.as_ref().unwrap().code,
        "model_request_failed"
    );
    assert_eq!(session.record.lock().usage.consecutive_failures, 2);
    assert_eq!(session.record.lock().usage.turns, 2);
    ai.state.stop(&session, "cancelled").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), runner)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    server.abort();
}

#[test]
fn unlimited_tokens_preserve_usage_and_other_hard_budgets() {
    let mut r = record();
    r.limits.max_tokens = 0;
    r.limits.validate().unwrap();
    r.usage.known_tokens = 900_000;
    r.usage.has_unknown_tokens = true;
    r.usage.total_tokens = None;
    assert!(budget_reason(&r).is_none());
    ensure_budget_available(&r).unwrap();
    assert_eq!(r.usage.known_tokens, 900_000);
    r.usage.actions = r.limits.max_actions;
    assert_eq!(budget_reason(&r).unwrap().code, "budget_actions");
    r.usage.actions = 0;
    r.usage.active_seconds = r.limits.max_seconds as f64;
    assert_eq!(budget_reason(&r).unwrap().code, "budget_seconds");
}

#[test]
fn finite_token_budget_reports_the_known_lower_bound_when_total_is_unknown() {
    let mut r = record();
    r.usage.known_tokens = 105_396;
    r.usage.has_unknown_tokens = true;
    let reason = budget_reason(&r).unwrap();
    assert_eq!(reason.code, "budget_tokens");
    assert_eq!(reason.source, "budget");
    assert!(reason.detail.contains("至少 105396"));
    assert!(reason.detail.contains("100000"));
    assert!(reason.detail.contains("总量未知"));
    let error = ensure_budget_available(&r).unwrap_err().to_string();
    assert!(error.contains("budget_tokens"));
    assert!(error.contains("上限"));
    assert!(error.contains("不会清零"));
    assert!(optional_limits(&json!({"limits":{"max_tokens":0}})).is_err());
    r.limits.max_tokens = 0;
    assert_eq!(
        optional_limits(&json!({"limits":r.limits}))
            .unwrap()
            .unwrap()
            .max_tokens,
        0
    );
}

#[tokio::test]
async fn exhausted_budget_resume_does_not_flash_running_or_change_generation() {
    let (_root, ai, _extensions) = fixture().await;
    let session = controlled_session(&ai).await;
    {
        let mut record = session.record.lock();
        record.usage.known_tokens = record.limits.max_tokens;
        record.usage.consecutive_failures = 3;
    }
    let reason = budget_reason(&session.record.lock()).unwrap();
    let generation = session.record.lock().generation;
    ai.state
        .pause_automatic(&session, generation, reason)
        .await
        .unwrap();
    let paused = session.record.lock().generation;
    let error = ai.state.resume(&session, None).await.unwrap_err();
    assert!(error.to_string().contains("budget_tokens"));
    let mut insufficient = session.record.lock().limits.clone();
    insufficient.max_tokens = 2048;
    assert!(ai
        .state
        .resume(&session, Some(insufficient))
        .await
        .unwrap_err()
        .to_string()
        .contains("budget_tokens"));
    let record = session.record.lock();
    assert_eq!(record.state, "paused");
    assert_eq!(record.generation, paused);
    assert_eq!(record.usage.consecutive_failures, 3);
    assert_eq!(record.limits.max_tokens, 100_000);
    assert_eq!(record.pause_reason.as_ref().unwrap().code, "budget_tokens");
    assert_eq!(record.events.last().unwrap().data["state"], "paused");
    assert!(ai.state.runtime.devices.controls.status("d").manual_allowed);
}

#[tokio::test]
async fn new_message_waits_for_admitted_input_before_becoming_available_to_manual() {
    let (_root, ai, _extensions) = fixture().await;
    let session = controlled_session(&ai).await;
    let lease = session.lease.lock().await.clone().unwrap();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let controls = ai.state.runtime.devices.controls.clone();
    let operation = tokio::spawn(async move {
        controls
            .execute(&lease, async move {
                entered_tx.send(()).unwrap();
                release_rx.await.unwrap();
                Ok(())
            })
            .await
            .unwrap();
    });
    entered_rx.await.unwrap();
    let sender = {
        let ai = ai.clone();
        let session = session.clone();
        tokio::spawn(async move {
            ai.state
                .message(&session, "先打开设置，再调整画质", false, None)
                .await
                .unwrap()
        })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while session.record.lock().state != "pausing" {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!sender.is_finished());
    assert_eq!(session.record.lock().messages.len(), 1);
    assert!(!ai.state.runtime.devices.controls.status("d").manual_allowed);
    release_tx.send(()).unwrap();
    operation.await.unwrap();
    let response = sender.await.unwrap();
    assert_eq!(response["session"]["state"], "paused");
    assert_eq!(response["resumed"], false);
    assert_eq!(response["session"]["messages"].as_array().unwrap().len(), 2);
    assert!(ai.state.runtime.devices.controls.status("d").manual_allowed);
    let record = session.record.lock();
    assert_eq!(record.events.last().unwrap().kind, "user");
    assert_eq!(
        record.events.last().unwrap().data["message_id"],
        response["message"]["id"]
    );
}

#[tokio::test]
async fn old_generation_automatic_pause_cannot_pause_a_resumed_owner() {
    let (_root, ai, _extensions) = fixture().await;
    let session = controlled_session(&ai).await;
    let original = session.lease.lock().await.clone().unwrap();
    let held = session.transition.lock().await;
    let late = {
        let ai = ai.clone();
        let session = session.clone();
        let generation = original.generation;
        tokio::spawn(async move {
            ai.state
                .pause_automatic(
                    &session,
                    generation,
                    PauseReason::new(
                        "model_request_failed",
                        "model",
                        "旧请求失败",
                        "旧代次错误",
                        "检查连接",
                        true,
                    ),
                )
                .await
                .unwrap()
        })
    };
    let controls = &ai.state.runtime.devices.controls;
    let paused = controls.pause(&original, async { Ok(()) }).await.unwrap();
    let resumed = controls.resume(&paused, async { Ok(()) }).await.unwrap();
    session.record.lock().generation = resumed.generation;
    *session.lease.lock().await = Some(resumed.clone());
    drop(held);
    late.await.unwrap();
    assert_eq!(session.record.lock().state, "running");
    assert_eq!(session.record.lock().generation, resumed.generation);
    assert!(session.record.lock().pause_reason.is_none());
    assert!(!controls.status("d").manual_allowed);
}

#[tokio::test]
async fn old_finish_waiting_for_lease_cannot_borrow_the_resumed_generation() {
    let (_root, ai, _extensions) = fixture().await;
    let target = crate::browser::BrowserTarget {
        id: "browser-lease-race".into(),
        name: "lease race".into(),
        url: "http://localhost/".into(),
        profile_id: "lease-race".into(),
        width: 640,
        height: 480,
    };
    ai.state
        .runtime
        .devices
        .browsers
        .db
        .save_browser_target(target.clone())
        .unwrap();
    let controls = &ai.state.runtime.devices.controls;
    let original = controls.claim(&target.id, "s").await.unwrap();
    let mut record = record();
    record.device_id = target.id.clone();
    record.generation = original.generation;
    let session = session_record(record, Some(original.clone()));
    let mut held = session.lease.lock().await;
    let late = {
        let ai = ai.clone();
        let session = session.clone();
        let generation = original.generation;
        tokio::spawn(async move {
            ai.state
                .tool(
                    &session,
                    "session_finish",
                    json!({"message":"旧代次的结束指令"}),
                    "old-finish",
                    generation,
                )
                .await
        })
    };
    // On the single-thread test runtime, the worker runs synchronously after
    // operation admission until its next await: our deliberately held lease.
    tokio::time::timeout(Duration::from_secs(2), async {
        while session.operation.try_lock().is_ok() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!late.is_finished());
    let paused = controls.pause(&original, async { Ok(()) }).await.unwrap();
    let resumed = controls.resume(&paused, async { Ok(()) }).await.unwrap();
    session.record.lock().generation = resumed.generation;
    *held = Some(resumed.clone());
    drop(held);
    let error = late.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("stale_generation"));
    assert!(
        !session.ending.load(Ordering::Acquire),
        "A stale session_finish must not end the resumed owner"
    );
    assert_eq!(session.record.lock().state, "running");
    assert_eq!(session.record.lock().generation, resumed.generation);
    assert_eq!(session.record.lock().usage.actions, 0);
    assert_eq!(
        controls.status(&target.id).phase,
        crate::core::control::ControlPhase::Running
    );
    assert!(!controls.status(&target.id).manual_allowed);
    // No browser process or device session was created for this race test.
    assert!(ai
        .state
        .runtime
        .devices
        .browsers
        .session(&target.id)
        .is_err());
}

#[tokio::test]
async fn accepted_message_has_a_receipt_even_when_resume_configuration_is_missing() {
    let (_root, ai, _extensions) = fixture().await;
    let session = controlled_session(&ai).await;
    ai.state.pause(&session, "用户暂停".into()).await.unwrap();
    let response = ai
        .state
        .message(&session, "改为点击左侧菜单", true, None)
        .await
        .unwrap();
    assert_eq!(response["resumed"], false);
    assert!(response["resume_error"].is_string());
    assert_eq!(session.record.lock().state, "paused");
    assert_eq!(session.record.lock().messages.len(), 2);
    let history = generation_history(&session.record.lock());
    assert!(history
        .iter()
        .any(|item| item["content"][0]["text"] == "改为点击左侧菜单"));
    session.record.lock().mode = "mcp".into();
    assert!(ai
        .state
        .message(&session, "不应写入", false, None)
        .await
        .is_err());
    assert_eq!(session.record.lock().messages.len(), 2);
    session.record.lock().mode = "api".into();
    session.record.lock().messages = (0..64)
        .map(|_| UserMessage::new("不可丢弃的指令"))
        .collect();
    assert!(ai
        .state
        .message(&session, "第65条", false, None)
        .await
        .is_err());
    assert_eq!(session.record.lock().messages.len(), 64);
    ai.state.stop(&session, "cancelled").await.unwrap();
    assert_eq!(session.record.lock().state, "finished");
    assert!(ai
        .state
        .message(&session, "已结束不能追加", false, None)
        .await
        .is_err());
    assert_eq!(session.record.lock().messages.len(), 64);
}

#[test]
fn new_generation_rebuild_keeps_every_instruction_and_only_public_receipts() {
    let mut r = record();
    r.messages.push(UserMessage::new("现在先打开设置"));
    r.events.push(Event {
        seq: 1,
        at: "now".into(),
        kind: "assistant".into(),
        message: "已打开菜单".into(),
        data: json!({}),
    });
    r.events.push(Event {
        seq: 2,
        at: "now".into(),
        kind: "tool".into(),
        message: "已执行 input_tap".into(),
        data: json!({"phase":"result","ok":true,"result":{"ok":true}}),
    });
    let history = generation_history(&r);
    let serialized = json!(history).to_string();
    assert!(serialized.contains("已打开菜单"));
    assert!(serialized.contains("已执行 input_tap"));
    assert!(serialized.contains("现在先打开设置"));
    assert!(serialized.contains("goal"));
    assert!(
        !serialized.contains("function_call"),
        "Rebuilt summaries must not create unpaired tool calls"
    );
}

#[test]
fn stale_observation_and_results_cannot_borrow_new_generation_cancellation_or_failures() {
    let session = session_record(record(), None);
    let original = session.generation_cancel(1).unwrap();
    session.cancel_generation();
    session.record.lock().generation = 2;
    *session.cancelled.lock() = Arc::new(AtomicBool::new(false));
    assert!(original.load(Ordering::Acquire));
    assert!(session.generation_cancel(1).is_none());
    assert!(!session
        .generation_cancel(2)
        .unwrap()
        .load(Ordering::Acquire));
    session.record.lock().usage.consecutive_failures = 2;
    assert!(session.update_failures(1, true).is_none());
    assert!(session.update_failures(1, false).is_none());
    assert_eq!(session.record.lock().usage.consecutive_failures, 2);
    assert_eq!(session.update_failures(2, false), Some(3));
    assert_eq!(session.update_failures(2, true), Some(0));
}

#[test]
fn history_keeps_recent_images_and_preserves_tool_receipts() {
    let mut history = vec![
        json!({"role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,old"}]}),
        json!({"type":"function_call_output","call_id":"c","output":[{"type":"image","data":"new","mimeType":"image/png"}]}),
    ];
    retain_recent_images(&mut history, 1);
    assert_eq!(history[0]["content"][0]["type"], "input_text");
    assert_eq!(history[1]["call_id"], "c");
    assert_eq!(history[1]["output"][0]["type"], "image");
}

#[test]
fn missing_usage_keeps_total_unknown_and_preserves_known_lower_bound() {
    let mut usage = Usage::default();
    record_usage(&mut usage, Some(&json!({"total_tokens":10})));
    assert_eq!(usage.total_tokens, Some(10));
    record_usage(&mut usage, None);
    record_usage(&mut usage, Some(&json!({"total_tokens":20})));
    assert_eq!(usage.total_tokens, None);
    assert_eq!(usage.known_tokens, 30);
    assert!(usage.has_unknown_tokens);
}

#[tokio::test]
async fn lifecycle_stop_cancels_all_targets_before_waiting_for_one_release() {
    let (_root, ai, _extensions) = fixture().await;
    let mut created = Vec::new();
    for id in ["a", "b"] {
        let lease = ai
            .state
            .runtime
            .devices
            .controls
            .claim(id, id)
            .await
            .unwrap();
        let mut r = record();
        r.session_id = id.into();
        r.device_id = id.into();
        r.generation = lease.generation;
        let session = Arc::new(Session {
            record: Mutex::new(r),
            lease: AsyncMutex::new(Some(lease)),
            transition: AsyncMutex::new(()),
            operation: AsyncMutex::new(()),
            cancelled: Mutex::new(Arc::new(AtomicBool::new(false))),
            ending: AtomicBool::new(false),
            wake: Notify::new(),
            deadline: Mutex::new(Instant::now() + Duration::from_secs(120)),
            active_since: Mutex::new(Some(Instant::now())),
            frame: Mutex::new(None),
            binding: Mutex::new(None),
            results: Mutex::new(BTreeMap::new()),
        });
        ai.state.sessions.lock().insert(id.into(), session.clone());
        created.push(session);
    }
    let held = created[0].transition.lock().await;
    let stopping = {
        let state = ai.state.clone();
        tokio::spawn(async move { state.stop_all().await })
    };
    tokio::time::timeout(Duration::from_secs(2), async {
        while !created[1].ending.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!stopping.is_finished());
    assert!(created[1].cancelled.lock().load(Ordering::Acquire));
    let generation = created[1].record.lock().generation;
    assert!(ai
        .state
        .tool(
            &created[1],
            "wait",
            json!({"duration_ms":1}),
            "late",
            generation
        )
        .await
        .is_err());
    drop(held);
    stopping.await.unwrap();
    for session in created {
        assert_eq!(session.record.lock().state, "finished");
        assert!(
            ai.state
                .runtime
                .devices
                .controls
                .status(&session.record.lock().device_id)
                .manual_allowed
        );
    }
}

#[tokio::test]
async fn tokens_are_target_scoped_private_revocable_and_lifecycle_bound() {
    let (root, ai, extensions) = fixture().await;
    let target = crate::browser::BrowserTarget {
        id: "browser-test".into(),
        name: "test".into(),
        url: "http://localhost/".into(),
        profile_id: "test".into(),
        width: 640,
        height: 480,
    };
    ai.state
        .runtime
        .devices
        .browsers
        .db
        .save_browser_target(target)
        .unwrap();
    let created = ai
        .dispatch(
            "mcp.tokens.create",
            json!({"device_id":"browser-test","content_package":"default"}),
        )
        .await
        .unwrap();
    let secret = created["token"].as_str().unwrap();
    let result = ai
        .state
        .mcp(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            secret,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(result["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .all(|t| !t["name"].as_str().unwrap().starts_with("input_")));
    let stored = std::fs::read(
        root.path()
            .join("extension-data/gamer-ai/private/tokens.dat"),
    )
    .unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains(secret));
    assert!(ai
        .state
        .runtime
        .packages
        .list_packages()
        .unwrap()
        .iter()
        .all(|p| p.id != "gamer-ai"));
    ai.dispatch("mcp.tokens.revoke", json!({"token_id":created["token_id"]}))
        .await
        .unwrap();
    assert!(ai.state.token(secret).is_err());
    extensions
        .disable(&ExtensionId::parse(ID).unwrap())
        .await
        .unwrap();
    assert!(ai.state.authorize(None).is_err());
    assert!(ai
        .state
        .runtime
        .scheduler
        .runners()
        .iter()
        .all(|r| r.runner_id != ID));
}

/// Uses an isolated local page, browser profile and synthetic model. No ADB or
/// real game/account is touched. Opt in on machines with Chrome/Edge installed.
/// Set GAMER_AI_TEST_BROWSER to an explicit Chrome/Edge executable for this
/// test process when the system's automatically detected browser cannot run headless.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Chrome/Edge (optional GAMER_AI_TEST_BROWSER path); local AI gameplay integration"]
async fn local_browser_mcp_pause_resume_and_model_gameplay_roundtrip() {
    use axum::{
        response::Html,
        routing::{get, post},
        Json, Router,
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let turn = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let turn_copy = turn.clone();
    let stall = Arc::new(AtomicBool::new(false));
    let stall_copy = stall.clone();
    let app=Router::new().route("/",get(||async {Html("<!doctype html><style>body{margin:0;background:#123456}button{position:absolute;left:20px;top:20px;width:120px;height:60px}</style><button onclick='document.body.style.background=\"#abcdef\";window.clicks=(window.clicks||0)+1'>Play</button>")})).route("/responses",post(move|Json(body):Json<Value>|{let turn=turn_copy.clone();let stall=stall_copy.clone();async move {
        if stall.load(Ordering::Acquire) {tokio::time::sleep(Duration::from_secs(20)).await;}
        let index=turn.fetch_add(1,Ordering::SeqCst);let history=body["input"].to_string();assert!(history.contains("input_image"),"model must receive real image input");
        fn find_frame(value:&Value)->Option<String>{match value {Value::Object(object)=>{if let Some(id)=object.get("frame_id").and_then(Value::as_str){return Some(id.into());}object.values().rev().find_map(find_frame)},Value::Array(values)=>values.iter().rev().find_map(find_frame),Value::String(s)=>serde_json::from_str::<Value>(s).ok().and_then(|v|find_frame(&v)),_=>None}}
        let (name,args)=match index {0=>("screen_capture",json!({})),1=>("input_tap",json!({"frame_id":find_frame(&body).expect("frame metadata"),"x":70,"y":45})),2=>("screen_capture",json!({})),_=>("session_finish",json!({"message":"测试目标已完成"}))};
        Json(json!({"status":"completed","output":[{"type":"function_call","id":format!("fc{index}"),"call_id":format!("c{index}"),"name":name,"arguments":args.to_string()}],"usage":{"input_tokens":10,"output_tokens":10,"total_tokens":20}}))
    }}));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (_root, ai, extensions) = fixture().await;
    let target = crate::browser::BrowserTarget {
        id: "browser-ai-test".into(),
        name: "AI test".into(),
        url: format!("{base}/"),
        profile_id: "ai-test".into(),
        width: 640,
        height: 480,
    };
    ai.state
        .runtime
        .devices
        .browsers
        .db
        .save_browser_target(target.clone())
        .unwrap();
    let saved = ai.state.settings.read().unwrap();
    ai.dispatch("settings.save",json!({"expected_version":saved["version"],"base_url":base,"model":"test","protocol":"responses","api_key":"synthetic-local-key","request_timeout_secs":30})).await.unwrap();
    // Read-only observation requires neither a control run nor an API key.
    crate::targets::prepare(&ai.state.runtime.devices, &target.id)
        .await
        .unwrap();
    let token = ai
        .dispatch(
            "mcp.tokens.create",
            json!({"device_id":target.id,"content_package":"default","control":true}),
        )
        .await
        .unwrap();
    let secret = token["token"].as_str().unwrap();
    let capture = ai
        .state
        .mcp_tool(
            &ai.state.token(secret).unwrap(),
            "screen_capture",
            json!({}),
            "observe",
        )
        .await
        .unwrap();
    assert!(capture["content"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["type"] == "image"));
    let started=ai.dispatch("session.start",json!({"device_id":target.id,"content_package":"default","mode":"mcp","goal":"外部测试"})).await.unwrap();
    let session = ai
        .state
        .session(started["session_id"].as_str().unwrap())
        .unwrap();
    wait_state(&session, "running").await;
    let generation = session.record.lock().generation;
    let run_id = session.record.lock().run_id.clone();
    let access = ai.state.token(secret).unwrap();
    let capture = ai
        .state
        .mcp_tool(
            &access,
            "screen_capture",
            json!({"session_id":started["session_id"],"generation":generation}),
            "mcp-capture",
        )
        .await
        .unwrap();
    let args = json!({"session_id":started["session_id"],"generation":generation,"operation_id":"one-tap","frame_id":capture["structuredContent"]["frame_id"],"x":70,"y":45});
    let rpc = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"input_tap","arguments":args}});
    let first = ai.state.mcp(rpc.clone(), secret).await.unwrap().unwrap();
    let actions = session.record.lock().usage.actions;
    let retried = ai.state.mcp(rpc, secret).await.unwrap().unwrap();
    assert_eq!(first, retried);
    assert_eq!(session.record.lock().usage.actions, actions);
    assert_ne!(first["result"]["isError"], true);
    let mut changed = args;
    changed["x"] = json!(80);
    let mismatch=ai.state.mcp(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"input_tap","arguments":changed}}),secret).await.unwrap().unwrap();
    assert_eq!(mismatch["result"]["isError"], true);
    let readonly = ai
        .dispatch(
            "mcp.tokens.create",
            json!({"device_id":target.id,"content_package":"default","control":false}),
        )
        .await
        .unwrap();
    ai.dispatch(
        "mcp.tokens.revoke",
        json!({"token_id":readonly["token_id"]}),
    )
    .await
    .unwrap();
    assert_eq!(session.record.lock().state, "running");
    let old_deadline = Instant::now() + Duration::from_secs(50);
    *session.deadline.lock() = old_deadline;
    let rejected=ai.state.mcp_tool(&access,"wait",json!({"session_id":started["session_id"],"generation":generation+1,"operation_id":"stale","duration_ms":1}),"stale-request").await;
    assert!(rejected.is_err());
    assert_eq!(*session.deadline.lock(), old_deadline);
    *session.deadline.lock() = Instant::now() + Duration::from_secs(120);
    assert!(ai
        .state
        .runtime
        .devices
        .controls
        .manual(&target.id, async {
            panic!("manual must not enter");
            #[allow(unreachable_code)]
            Ok(())
        })
        .await
        .is_err());
    ai.state.pause(&session, "测试暂停".into()).await.unwrap();
    assert!(ai
        .state
        .runtime
        .runs
        .active_for_device(&target.id)
        .is_some());
    assert_eq!(session.record.lock().run_id, run_id);
    ai.state
        .runtime
        .devices
        .controls
        .manual(&target.id, async { Ok(()) })
        .await
        .unwrap();
    // Token accounting remains cumulative. Explicitly changing the paused
    // limit to zero permits continuation without erasing unknown usage.
    {
        let mut record = session.record.lock();
        record.usage.known_tokens = record.limits.max_tokens + 100;
        record.usage.has_unknown_tokens = true;
        record.usage.total_tokens = None;
        record.usage.consecutive_failures = 3;
    }
    assert!(ai
        .state
        .resume(&session, None)
        .await
        .unwrap_err()
        .to_string()
        .contains("budget_tokens"));
    assert_eq!(session.record.lock().state, "paused");
    let mut unlimited = session.record.lock().limits.clone();
    unlimited.max_tokens = 0;
    ai.state.resume(&session, Some(unlimited)).await.unwrap();
    assert_eq!(session.record.lock().usage.known_tokens, 100_100);
    assert!(session.record.lock().usage.has_unknown_tokens);
    assert_eq!(session.record.lock().usage.consecutive_failures, 0);
    assert_eq!(session.record.lock().limits.max_tokens, 0);
    assert_ne!(session.record.lock().generation, generation);
    assert!(ai
        .state
        .tool(
            &session,
            "wait",
            json!({"duration_ms":1}),
            "stale",
            generation
        )
        .await
        .is_err());
    ai.state.stop(&session, "cancelled").await.unwrap();
    assert!(
        ai.state
            .runtime
            .devices
            .controls
            .status(&target.id)
            .manual_allowed
    );
    // An active controller revocation and inactivity expiry drain the same
    // barrier as a user pause, and never release the reserved Core run slot.
    for reason in ["revoked", "expired"] {
        let started = ai.dispatch("session.start", json!({"device_id":target.id,"content_package":"default","mode":"mcp","goal":reason})).await.unwrap();
        let controlled = ai
            .state
            .session(started["session_id"].as_str().unwrap())
            .unwrap();
        wait_state(&controlled, "running").await;
        if reason == "revoked" {
            ai.dispatch("mcp.tokens.revoke", json!({"token_id":token["token_id"]}))
                .await
                .unwrap();
        } else {
            *controlled.deadline.lock() = Instant::now();
        }
        wait_state(&controlled, "paused").await;
        assert!(
            ai.state
                .runtime
                .devices
                .controls
                .status(&target.id)
                .manual_allowed
        );
        assert!(ai
            .state
            .runtime
            .runs
            .active_for_device(&target.id)
            .is_some());
        ai.state.stop(&controlled, "cancelled").await.unwrap();
    }
    // Disabling a plugin with a live MCP run finishes it before unregistering
    // its Runner. Re-enabling restores the Runner without replaying inputs.
    let started = ai.dispatch("session.start", json!({"device_id":target.id,"content_package":"default","mode":"mcp","goal":"disable cleanup"})).await.unwrap();
    let controlled = ai
        .state
        .session(started["session_id"].as_str().unwrap())
        .unwrap();
    wait_state(&controlled, "running").await;
    let id = ExtensionId::parse(ID).unwrap();
    extensions.disable(&id).await.unwrap();
    assert_eq!(controlled.record.lock().state, "finished");
    assert!(ai
        .state
        .runtime
        .runs
        .active_for_device(&target.id)
        .is_none());
    assert!(
        ai.state
            .runtime
            .devices
            .controls
            .status(&target.id)
            .manual_allowed
    );
    extensions.enable_and_start(&id, None, None).await.unwrap();
    let started=ai.dispatch("session.start",json!({"device_id":target.id,"content_package":"default","mode":"api","goal":"点击 Play，然后检查背景颜色"})).await.unwrap();
    let session = ai
        .state
        .session(started["session_id"].as_str().unwrap())
        .unwrap();
    wait_state(&session, "finished").await;
    assert_eq!(session.record.lock().reason.as_deref(), Some("completed"));
    assert!(turn.load(Ordering::SeqCst) >= 4);
    assert!(session
        .record
        .lock()
        .events
        .iter()
        .any(|e| e.kind == "tool" && e.data["tool"] == "input_tap"));
    let (png, _) = ai
        .state
        .runtime
        .devices
        .browsers
        .session(&target.id)
        .unwrap()
        .capture()
        .await
        .unwrap();
    assert_eq!(
        image::load_from_memory(&png)
            .unwrap()
            .to_rgb8()
            .get_pixel(500, 400)
            .0,
        [0xab, 0xcd, 0xef]
    );
    assert!(
        ai.state
            .runtime
            .devices
            .controls
            .status(&target.id)
            .manual_allowed
    );
    // The remaining activity budget bounds a stalled model HTTP request,
    // rather than waiting for the much longer configured API timeout.
    stall.store(true, Ordering::Release);
    let began = Instant::now();
    let started = ai.dispatch("session.start", json!({"device_id":target.id,"content_package":"default","mode":"api","goal":"budget timeout","limits":{"max_seconds":10}})).await.unwrap();
    let stalled = ai
        .state
        .session(started["session_id"].as_str().unwrap())
        .unwrap();
    wait_state(&stalled, "paused").await;
    assert!(began.elapsed() < Duration::from_secs(15));
    assert_eq!(
        stalled.record.lock().reason.as_deref(),
        Some("活动时长达到预算")
    );
    assert_eq!(stalled.record.lock().usage.consecutive_failures, 0);
    assert!(
        ai.state
            .runtime
            .devices
            .controls
            .status(&target.id)
            .manual_allowed
    );
    let paused_generation = stalled.record.lock().generation;
    let queued = ai
        .dispatch(
            "session.message",
            json!({"session_id":started["session_id"],"message":"先确认菜单位置，保持暂停"}),
        )
        .await
        .unwrap();
    assert_eq!(queued["resumed"], false);
    assert_eq!(stalled.record.lock().state, "paused");
    assert_eq!(stalled.record.lock().generation, paused_generation);
    let before = stalled.record.lock().usage.active_seconds;
    let mut continued = stalled.record.lock().limits.clone();
    continued.max_seconds = 600;
    continued.max_tokens = 0;
    stall.store(false, Ordering::Release);
    let resumed = ai.dispatch("session.message", json!({"session_id":started["session_id"],"message":"确认后结束本次测试","resume":true,"limits":continued})).await.unwrap();
    assert_eq!(resumed["resumed"], true);
    assert_ne!(stalled.record.lock().generation, paused_generation);
    assert!(stalled.record.lock().usage.active_seconds >= before);
    assert_eq!(stalled.record.lock().messages.len(), 3);
    wait_state(&stalled, "finished").await;
    ai.state.stop(&stalled, "cancelled").await.unwrap();
    assert!(ai
        .dispatch(
            "session.message",
            json!({"session_id":started["session_id"],"message":"结束后不能操作"})
        )
        .await
        .is_err());
    ai.state
        .runtime
        .devices
        .browsers
        .close(&target.id, &ai.state.runtime.runs)
        .await
        .unwrap();
    server.abort();
}
async fn wait_state(session: &Arc<Session>, state: &str) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while session.record.lock().state != state {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        let r = session.record.lock().clone();
        panic!("expected {state}, got {} {:?}", r.state, r.reason)
    });
}
