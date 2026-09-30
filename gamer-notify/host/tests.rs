use super::*;
use axum::{
    extract::State as AxumState,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use std::sync::atomic::AtomicUsize;

async fn mock_server(
    data: Value,
    status: StatusCode,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let state = (data, status, hits.clone());
    let router = Router::new()
        .route(
            "/api/v1/notify",
            post(
                |AxumState((data, status, hits)): AxumState<(
                    Value,
                    StatusCode,
                    Arc<AtomicUsize>,
                )>,
                 headers: HeaderMap,
                 Json(body): Json<Value>| async move {
                    assert_eq!(headers["authorization"], "Bearer fixture-notify-key");
                    assert!(headers["idempotency-key"]
                        .to_str()
                        .unwrap()
                        .starts_with("gamer-"));
                    assert_eq!(body["content"], "测试正文");
                    hits.fetch_add(1, Ordering::SeqCst);
                    (status, Json(data))
                },
            ),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (endpoint, hits, handle)
}
async fn save_channel(service: &NotifyService) {
    service.dispatch("channels.save", json!({"channel":{"id":"wechat","name":"我的微信","kind":"wecomlink","enabled":true,"key":"fixture-notify-key"},"expected_version":null})).await.unwrap();
}
async fn wait_terminal(service: &NotifyService, id: &str) -> Record {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(record) = service
                .state
                .records
                .lock()
                .iter()
                .find(|r| r.id == id && !["queued", "sending"].contains(&r.status.as_str()))
                .cloned()
            {
                return record;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[test]
fn utf8_limits_and_result_semantics_are_enforced() {
    assert!(validate_message("", &"中".repeat(682)).is_ok());
    assert!(validate_message("", &"中".repeat(683)).is_err());
    assert!(validate_message("标题", &"x".repeat(2042)).is_err());
    assert!(validate_message("", "  \n").is_err());
    assert_eq!(
        response_status(200, &json!({"status":"partial"})).0,
        "partial"
    );
    assert_eq!(
        response_status(202, &json!({"status":"pending"})).0,
        "pending"
    );
    assert_eq!(response_status(200, &json!({"ok":true})).0, "unknown");
    assert_eq!(response_status(504, &json!({})).0, "unknown");
    assert_eq!(response_status(500, &json!({})).0, "unknown");
}

#[tokio::test]
async fn credentials_survive_reopen_without_public_exposure_and_edits_require_version() {
    let root = tempfile::tempdir().unwrap();
    let service = NotifyService::new(root.path()).unwrap();
    save_channel(&service).await;
    let public = service.dispatch("channels.read", json!({})).await.unwrap();
    assert!(!public.to_string().contains("fixture-notify-key"));
    assert_eq!(public["channels"][0]["has_key"], true);
    assert!(service
        .dispatch(
            "channels.delete",
            json!({"id":"wechat","expected_version":null})
        )
        .await
        .is_err());
    service.dispatch("channels.save", json!({"expected_version":public["version"], "channel":{"id":"wechat","name":"更名","kind":"wecomlink","enabled":true,"key":""}})).await.unwrap();
    let reopened = NotifyService::new(root.path()).unwrap();
    assert_eq!(
        Settings::load(&reopened.state.private_path)
            .unwrap()
            .resolve(Some("wechat"))
            .unwrap()
            .key,
        "fixture-notify-key"
    );
    #[cfg(windows)]
    assert!(
        !String::from_utf8_lossy(&std::fs::read(&service.state.private_path).unwrap())
            .contains("fixture-notify-key")
    );
}

#[tokio::test]
async fn submission_is_nonblocking_and_replayed_task_events_send_once() {
    let (endpoint, hits, server) = mock_server(
        json!({"id":"remote-1","status":"sent","wecom":{"errcode":0}}),
        StatusCode::OK,
    )
    .await;
    let root = tempfile::tempdir().unwrap();
    let service = NotifyService::open(root.path(), endpoint).unwrap();
    save_channel(&service).await;
    let values = json!({"channel":"wechat","title":"标题","content":"测试正文","source":"task","source_id":"run:one"});
    let first = service.submit(values.clone()).unwrap();
    assert_eq!(first["accepted"], true);
    assert_eq!(first["record"]["status"], "queued");
    let second = service.submit(values).unwrap();
    assert_eq!(first["record"]["id"], second["record"]["id"]);
    assert_eq!(second["replayed"], true);
    let record = wait_terminal(&service, first["record"]["id"].as_str().unwrap()).await;
    assert_eq!(record.status, "sent");
    assert_eq!(record.remote_id.as_deref(), Some("remote-1"));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn partial_and_unknown_are_not_retried_and_service_errors_do_not_expose_key() {
    for (status, data, expected) in [
        (
            StatusCode::OK,
            json!({"id":"r","status":"partial","wecom":{"errcode":0}}),
            "partial",
        ),
        (
            StatusCode::GATEWAY_TIMEOUT,
            json!({"id":"r","status":"unknown"}),
            "unknown",
        ),
        (
            StatusCode::UNAUTHORIZED,
            json!({"error":{"message":"bad fixture-notify-key"}}),
            "failed",
        ),
    ] {
        let (endpoint, hits, server) = mock_server(data, status).await;
        let root = tempfile::tempdir().unwrap();
        let service = NotifyService::open(root.path(), endpoint).unwrap();
        save_channel(&service).await;
        let response = service
            .submit(json!({"channel":"wechat","content":"测试正文"}))
            .unwrap();
        let record = wait_terminal(&service, response["record"]["id"].as_str().unwrap()).await;
        assert_eq!(record.status, expected);
        assert!(!record.message.contains("fixture-notify-key"));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        server.abort();
    }
}

#[tokio::test]
async fn missing_disabled_channels_and_oversized_messages_are_recorded_without_delivery() {
    let root = tempfile::tempdir().unwrap();
    let service = NotifyService::new(root.path()).unwrap();
    let missing = service.submit(json!({"content":"正文"})).unwrap();
    assert_eq!(missing["accepted"], false);
    assert_eq!(missing["record"]["status"], "skipped");
    save_channel(&service).await;
    let oversized = service
        .submit(json!({"channel":"wechat","content":"中".repeat(683)}))
        .unwrap();
    assert_eq!(oversized["accepted"], false);
    assert_eq!(oversized["record"]["status"], "failed");
    let public = service.dispatch("channels.read", json!({})).await.unwrap();
    service.dispatch("channels.save", json!({"expected_version":public["version"],"channel":{"id":"wechat","name":"微信","kind":"wecomlink","enabled":false,"key":""}})).await.unwrap();
    let disabled = service
        .submit(json!({"channel":"wechat","content":"正文"}))
        .unwrap();
    assert_eq!(disabled["record"]["status"], "skipped");
    assert_eq!(service.state.pending.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn restart_marks_inflight_unknown_and_does_not_replay_queued_notifications() {
    let root = tempfile::tempdir().unwrap();
    let service = NotifyService::new(root.path()).unwrap();
    let mut records = service.state.records.lock();
    for status in ["sending", "queued"] {
        records.push(Record {
            id: status.into(),
            channel_id: None,
            channel_name: None,
            title: "".into(),
            content: "正文".into(),
            source: "task".into(),
            source_id: Some(status.into()),
            created_at: Utc::now(),
            status: status.into(),
            message: "".into(),
            remote_id: None,
            wecom_code: None,
        });
    }
    service.state.persist(&mut records).unwrap();
    drop(records);
    let reopened = NotifyService::new(root.path()).unwrap();
    let records = reopened.state.records.lock();
    assert_eq!(records[0].status, "unknown");
    assert_eq!(records[1].status, "skipped");
    assert_eq!(reopened.state.pending.load(Ordering::Acquire), 0);
}

fn archive(permission: bool) -> Vec<u8> {
    use std::io::Write;
    let manifest = format!("manifest_version=2\nid=\"gamer-notify\"\nversion=\"0.1.0\"\nname=\"通知\"\npermissions=[{}]\n[execution]\nkind=\"builtin\"\nbuiltin_id=\"gamer-notify\"\n[host_api]\nruntime=\"^1.0\"\n", if permission { "\"notify.send\"" } else { "" });
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    writer
        .start_file("manifest.toml", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(manifest.as_bytes()).unwrap();
    writer.finish().unwrap().into_inner()
}

#[tokio::test]
async fn lifecycle_gates_calls_and_uninstall_preserves_global_channels() {
    use crate::{
        capabilities::CapabilityRegistry,
        extensions::{ExtensionId, ExtensionService},
    };
    let root = tempfile::tempdir().unwrap();
    let notify = Arc::new(NotifyService::new(root.path()).unwrap());
    let service = ExtensionService::for_data_root(root.path(), CapabilityRegistry::default())
        .with_builtin_service(notify.clone());
    let id = ExtensionId::parse(ID).unwrap();
    assert!(service
        .call_extension(&id, "channels.read", json!({}))
        .await
        .is_err());
    let installed = service.install(&archive(true)).await.unwrap();
    service.enable(&id).await.unwrap();
    service.start(&id).await.unwrap();
    service.call_extension(&id, "channels.save", json!({"channel":{"id":"wechat","name":"微信","kind":"wecomlink","enabled":true,"key":"fixture-notify-key"},"expected_version":null})).await.unwrap();
    service.disable(&id).await.unwrap();
    assert!(service
        .call_extension(&id, SEND, json!({"content":"正文"}))
        .await
        .is_err());
    service
        .uninstall(&id, installed.active_version())
        .await
        .unwrap();
    assert!(notify.state.private_path.exists());
    service.install(&archive(true)).await.unwrap();
    service.enable(&id).await.unwrap();
    service.start(&id).await.unwrap();
    assert_eq!(
        service
            .call_extension(&id, "channels.read", json!({}))
            .await
            .unwrap()["channels"][0]["has_key"],
        true
    );
    service.disable(&id).await.unwrap();
    service
        .uninstall(&id, installed.active_version())
        .await
        .unwrap();
    service.install(&archive(false)).await.unwrap();
    service.enable(&id).await.unwrap();
    service.start(&id).await.unwrap();
    assert!(service
        .call_extension(&id, SEND, json!({"content":"正文"}))
        .await
        .is_err());
}

#[tokio::test]
async fn result_query_uses_remote_id_and_failed_query_keeps_original_status() {
    use axum::routing::get;
    let router = Router::new()
        .route(
            "/api/v1/notify",
            post(|| async { Json(json!({"id":"remote-1","status":"pending"})) }),
        )
        .route(
            "/api/v1/notifications/remote-1",
            get(|headers: HeaderMap| async move {
                assert_eq!(headers["authorization"], "Bearer fixture-notify-key");
                (StatusCode::NOT_FOUND, Json(json!({"error":"expired"})))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let root = tempfile::tempdir().unwrap();
    let notify = NotifyService::open(root.path(), endpoint).unwrap();
    save_channel(&notify).await;
    let submitted = notify
        .submit(json!({"channel":"wechat","content":"正文"}))
        .unwrap();
    let id = submitted["record"]["id"].as_str().unwrap();
    assert_eq!(wait_terminal(&notify, id).await.status, "pending");
    let queried = notify.query(json!({"id":id})).await.unwrap();
    assert_eq!(queried["record"]["status"], "pending");
    assert!(queried["record"]["message"]
        .as_str()
        .unwrap()
        .contains("HTTP 404"));
    server.abort();
}

#[tokio::test]
async fn worker_outlives_temporary_script_capability_runtime() {
    let (endpoint, hits, server) = mock_server(json!({"status":"sent"}), StatusCode::OK).await;
    let root = tempfile::tempdir().unwrap();
    let notify = Arc::new(NotifyService::open(root.path(), endpoint).unwrap());
    save_channel(&notify).await;
    let worker = notify.clone();
    let submitted = tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(worker.dispatch(SEND, json!({"channel":"wechat","content":"测试正文"})))
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(
        wait_terminal(&notify, submitted["record"]["id"].as_str().unwrap())
            .await
            .status,
        "sent"
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
async fn task_policy_delivers_through_global_channels_without_package_resources() {
    use crate::{
        capabilities::CapabilityRegistry,
        core::AppContext,
        extensions::{ExtensionId, ExtensionService},
        timer_core::{Task, TaskResult, TaskSchedule},
    };
    let (endpoint, hits, server) = mock_server(json!({"status":"sent"}), StatusCode::OK).await;
    let root = tempfile::tempdir().unwrap();
    let notify = Arc::new(NotifyService::open(root.path(), endpoint).unwrap());
    save_channel(&notify).await;
    let extensions = Arc::new(
        ExtensionService::for_data_root(root.path(), CapabilityRegistry::default())
            .with_builtin_service(notify.clone()),
    );
    let id = ExtensionId::parse(ID).unwrap();
    extensions.install(&archive(true)).await.unwrap();
    extensions.enable(&id).await.unwrap();
    extensions.start(&id).await.unwrap();
    let db = Arc::new(
        crate::store::Store::open(&crate::config::Config {
            data_dir: root.path().to_path_buf(),
            ..Default::default()
        })
        .unwrap(),
    );
    let mut task = Task::new(
        "task",
        "完成任务",
        AppContext::for_test("device", "com.game").unwrap(),
        "runner",
        "entry",
        json!({}),
        TaskSchedule::new("cron", json!({"expression":"0 8 * * *"})).unwrap(),
    )
    .unwrap();
    task.extensions = json!({ID:{"enabled":true,"results":{"success":{"enabled":true,"channels":["wechat","wechat"],"title":"{{task.name}}：{{result}}","content":"测试正文"}}}});
    let hook = super::task::result_hook(Arc::downgrade(&extensions), db);
    hook(
        task,
        TaskResult {
            event_id: "run:task-test".into(),
            run_id: Some("task-test".into()),
            state: "success".into(),
            error: None,
            finished_at: Utc::now(),
            elapsed_secs: 1,
        },
    );
    let record = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(record) = notify
                .state
                .records
                .lock()
                .iter()
                .find(|r| r.status == "sent")
                .cloned()
            {
                break record;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(record.title, "完成任务：成功");
    assert_eq!(record.source, "task");
    assert_eq!(record.source_id.as_deref(), Some("run:task-test"));
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    server.abort();
}

#[tokio::test]
#[ignore = "显式设置 GAMER_NOTIFY_TEST_KEY，向企微连发送一条真实测试通知"]
async fn live_wecomlink_smoke() {
    let key = std::env::var("GAMER_NOTIFY_TEST_KEY").expect("缺少测试密钥");
    let root = tempfile::tempdir().unwrap();
    let service = NotifyService::new(root.path()).unwrap();
    service.dispatch("channels.save", json!({"channel":{"id":"smoke","name":"测试","kind":"wecomlink","enabled":true,"key":key},"expected_version":null})).await.unwrap();
    let submitted = service.submit(json!({"channel":"smoke","title":"Gamer 通知插件测试","content":"通知助手接入验证：这是一条测试通知。","source":"test"})).unwrap();
    let id = submitted["record"]["id"].as_str().unwrap();
    let record = tokio::time::timeout(Duration::from_secs(65), async {
        loop {
            if let Some(record) = service
                .state
                .records
                .lock()
                .iter()
                .find(|r| r.id == id && !["queued", "sending"].contains(&r.status.as_str()))
                .cloned()
            {
                break record;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    println!("真实通知结果：{}", record.status);
    assert_eq!(record.status, "sent", "{}", record.message);
}
