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
        limits: Limits::default(),
        usage: Usage::default(),
        events: vec![],
    }
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
        max_seconds: 0,
        ..Limits::default()
    }
    .validate()
    .is_err());
    assert!(constant_eq("abc", "abc"));
    assert!(!constant_eq("abc", "abd"));
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
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires installed Chrome/Edge; local AI gameplay integration"]
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
    ai.state.resume(&session).await.unwrap();
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
    ai.state.stop(&stalled, "cancelled").await.unwrap();
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
