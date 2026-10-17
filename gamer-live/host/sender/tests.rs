use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn setup(path: PathBuf) -> Arc<Sender> {
    Sender::open(path).unwrap()
}
fn config() -> Value {
    json!({"enabled":true,"room_id":"123","session_cookie":"session-fixture","csrf_token":"0123456789abcdef0123456789abcdef","expected_version":null,"templates":Config::default().templates})
}
#[test]
fn secrets_persist_redacted_and_failed_updates_do_not_replace_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private/sender.dat");
    let s = setup(path.clone());
    let saved = s.save(config()).unwrap();
    assert!(saved["configured"].as_bool().unwrap());
    assert!(!saved.to_string().contains("session-fixture"));
    #[cfg(windows)]
    assert!(!String::from_utf8_lossy(&std::fs::read(&path).unwrap()).contains("session-fixture"));
    let s = setup(path.clone());
    let mut next = config();
    next["expected_version"] = saved["version"].clone();
    next["session_cookie"] = json!("");
    next["csrf_token"] = json!("");
    let saved = s.save(next).unwrap();
    assert!(saved["has_session"] == true && saved["has_csrf"] == true);
    assert!(s.save(config()).is_err());
    for cookie in ["x;other=y", "x\r\nInjected:yes"] {
        let mut bad = config();
        bad["expected_version"] = saved["version"].clone();
        bad["session_cookie"] = json!(cookie);
        assert!(s.save(bad).is_err());
    }
    assert_eq!(s.read()["version"], saved["version"]);
    let cleared = s.clear(saved["version"].as_str()).unwrap();
    assert_eq!(cleared["configured"], false);
    assert_eq!(cleared["enabled"], false);
}
async fn mock_server(
    responses: Vec<String>,
) -> (
    String,
    Arc<Mutex<Vec<(std::time::Instant, String)>>>,
    JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let out = requests.clone();
    let task = tokio::spawn(async move {
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut all = Vec::new();
            let mut data = [0u8; 4096];
            loop {
                let n = socket.read(&mut data).await.unwrap();
                if n == 0 {
                    break;
                }
                all.extend_from_slice(&data[..n]);
                if let Some(end) = all.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&all[..end]);
                    let len = head
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|s| s.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if all.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            out.lock()
                .push((std::time::Instant::now(), String::from_utf8(all).unwrap()));
            let packet=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",response.len(),response);
            socket.write_all(packet.as_bytes()).await.unwrap();
        }
    });
    (format!("http://{addr}/msg/send"), requests, task)
}
async fn wait_results(s: &Sender, count: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if s.read()["recent"].as_array().unwrap().len() >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn real_http_form_redacts_errors_deduplicates_and_rate_limits() {
    let (endpoint, requests, task) = mock_server(vec![
        r#"{"code":0,"msg":""}"#.into(),
        r#"{"code":-101,"msg":"session-fixture must not leak"}"#.into(),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = setup(dir.path().join("sender.dat"));
    let mutable = Arc::get_mut(&mut s).unwrap();
    mutable.endpoint = Some(endpoint);
    mutable.interval = Duration::from_millis(80);
    s.save(config()).unwrap();
    s.manual("hello", "one").unwrap();
    assert_eq!(s.manual("hello", "one").unwrap()["duplicate"], true);
    assert!(s.manual("different", "one").is_err());
    s.manual("next", "two").unwrap();
    wait_results(&s, 2).await;
    task.await.unwrap();
    let rows = requests.lock();
    assert_eq!(rows.len(), 2);
    assert!(rows[1].0.duration_since(rows[0].0) >= Duration::from_millis(75));
    assert!(rows[0].1.contains("SESSDATA=session-fixture; bili_jct="));
    assert!(
        rows[0].1.contains("roomid=123")
            && rows[0].1.contains("csrf=0123456789abcdef0123456789abcdef")
    );
    assert!(rows[0].1.contains("Gamer"));
    drop(rows);
    let result = s.read();
    assert_eq!(result["recent"][0]["state"], "failed");
    assert!(result["last_error"].as_str().unwrap().contains("-101"));
    assert!(!result.to_string().contains("session-fixture"));
    assert_eq!(result["recent"][1]["state"], "sent");
    s.stop().await;
}
#[tokio::test]
async fn automatic_notices_skip_tests_and_wrong_room_and_never_block_flow() {
    let (endpoint, requests, task) = mock_server(vec![r#"{"code":0,"msg":""}"#.into()]).await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = setup(dir.path().join("sender.dat"));
    Arc::get_mut(&mut s).unwrap().endpoint = Some(endpoint);
    s.save(config()).unwrap();
    let event = json!({"kind":"queued","id":"item:queued","room_id":"123","platform_id":"bilibili","test":true,"viewer":"观众","number":1});
    s.send(event.clone());
    assert_eq!(s.read()["pending"], 0);
    let mut wrong = event.clone();
    wrong["test"] = json!(false);
    wrong["room_id"] = json!("999");
    s.send(wrong);
    assert_eq!(s.read()["pending"], 0);
    let mut valid = event;
    valid["test"] = json!(false);
    s.send(valid);
    wait_results(&s, 1).await;
    task.await.unwrap();
    assert_eq!(requests.lock().len(), 1);
    s.stop().await;
    assert!(s.manual("x", "after-stop").is_err());
}
#[tokio::test]
async fn queue_is_bounded_length_is_explicit_and_no_retry_on_rejection() {
    let dir = tempfile::tempdir().unwrap();
    let s = setup(dir.path().join("sender.dat"));
    s.save(config()).unwrap();
    assert!(s.manual(&"x".repeat(34), "long").is_err());
    // Enqueue synchronously without yielding: no HTTP call can leave the worker.
    for i in 0..MAX_PENDING {
        s.manual("bounded", &format!("{i}")).unwrap();
    }
    assert!(s.manual("overflow", "overflow").is_err());
    s.stop().await;
    assert_eq!(s.read()["pending"], 0);
}
