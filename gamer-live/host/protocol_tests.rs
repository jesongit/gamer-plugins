use super::*;
use std::io::Write;

fn credentials(mode: &str) -> Credentials {
    Credentials {
        mode: mode.into(),
        access_key: "key".into(),
        access_secret: "secret".into(),
        app_id: if mode == "open_live" { "1" } else { "" }.into(),
        identity_code: if mode == "open_live" { "identity" } else { "" }.into(),
        access_token: if mode == "oauth" { "token" } else { "" }.into(),
    }
}

#[test]
fn compressed_batches_and_unknown_versions_are_bounded() {
    let p = packet(
        5,
        br#"{"cmd":"OPEN_LIVEROOM_LIKE","data":{"like_count":3}}"#,
    );
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&p).unwrap();
    z.write_all(&p).unwrap();
    let mut compressed = packet(5, &z.finish().unwrap());
    compressed[6..8].copy_from_slice(&2u16.to_be_bytes());
    assert_eq!(decode(&compressed).unwrap().len(), 2);
    compressed[6..8].copy_from_slice(&3u16.to_be_bytes());
    assert!(decode(&compressed).is_err());
}

#[tokio::test]
async fn signed_http_uses_empty_oauth_body_and_never_returns_remote_secret_text() {
    use axum::{http::HeaderMap, routing::post, Json, Router};
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let app = Router::new().route(
        "/test",
        post(move |headers: HeaderMap, body: String| {
            let sink = sink.clone();
            async move {
                sink.lock().push((headers, body));
                Json(json!({"code":123,"message":"secret=do-not-leak","data":{}}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let c = Client {
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
        credentials: credentials("oauth"),
        base: format!("http://{addr}"),
    };
    let error = c.post("/test", Value::Null).await.unwrap_err().to_string();
    assert!(error.contains("123"));
    assert!(!error.contains("do-not-leak"));
    let rows = captured.lock();
    let (headers, body) = &rows[0];
    assert!(body.is_empty());
    assert_eq!(
        headers["x-bili-content-md5"],
        "d41d8cd98f00b204e9800998ecf8427e"
    );
    assert_eq!(headers["access-token"], "token");
    assert_eq!(headers["x-bili-signature-version"], "2.0");
    let expected = signed_headers(
        &c.credentials,
        body,
        headers["x-bili-timestamp"].to_str().unwrap(),
        headers["x-bili-signature-nonce"].to_str().unwrap(),
    )
    .unwrap();
    assert_eq!(headers["authorization"], expected["authorization"]);
    drop(rows);
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn websocket_authentication_heartbeat_and_event_flow() {
    use axum::{routing::post, Json, Router};
    let heartbeats = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let h = heartbeats.clone();
    let app = Router::new().route(
        "/v2/app/heartbeat",
        post(move || {
            let h = h.clone();
            async move {
                h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Json(json!({"code":0,"data":{}}))
            }
        }),
    );
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let http_addr = http.local_addr().unwrap();
    let http_task = tokio::spawn(async move { axum::serve(http, app).await.unwrap() });
    let ws = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ws_addr = ws.local_addr().unwrap();
    let ws_task = tokio::spawn(async move {
        let (tcp, _) = ws.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let auth = socket.next().await.unwrap().unwrap().into_data();
        assert_eq!(u32::from_be_bytes(auth[8..12].try_into().unwrap()), 7);
        assert_eq!(&auth[16..], b"fixture-auth");
        socket
            .send(Message::Binary(packet(8, br#"{"code":0}"#)))
            .await
            .unwrap();
        let heartbeat = socket.next().await.unwrap().unwrap().into_data();
        assert_eq!(u32::from_be_bytes(heartbeat[8..12].try_into().unwrap()), 2);
        socket
            .send(Message::Binary(packet(3, &0u32.to_be_bytes())))
            .await
            .unwrap();
        let dm=packet(5,br#"{"cmd":"LIVE_OPEN_PLATFORM_DM","data":{"open_id":"viewer","msg_id":"one","room_id":12,"msg":"hello"}}"#);
        let mut batch = dm.clone();
        batch.extend(dm);
        socket.send(Message::Binary(batch)).await.unwrap();
        socket.close(None).await.unwrap();
    });
    let client = Client {
        http: reqwest::Client::builder().no_proxy().build().unwrap(),
        credentials: credentials("open_live"),
        base: format!("http://{http_addr}"),
    };
    let status = Arc::new(Mutex::new(ConnectionStatus::default()));
    let events = Arc::new(Mutex::new(EventBuffer::default()));
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        listen(
            &client,
            "fixture",
            &format!("ws://{ws_addr}"),
            "fixture-auth",
            &status,
            &events,
        ),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    ws_task.await.unwrap();
    assert_eq!(status.lock().state, "connected");
    assert_eq!(heartbeats.load(std::sync::atomic::Ordering::SeqCst), 1);
    let page = events.lock().page(0);
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    assert_eq!(page["events"][0]["payload"]["text"], "hello");
    http_task.abort();
    let _ = http_task.await;
}
