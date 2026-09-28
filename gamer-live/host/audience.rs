//! Loopback-only, revocable read capability for the livehime capture window.
use super::{queue::Queue, ConnectionStatus};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use parking_lot::Mutex;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Clone)]
struct View {
    queue: Arc<Queue>,
    connection: Arc<Mutex<ConnectionStatus>>,
    token: String,
    host: String,
    revoked: Arc<AtomicBool>,
}
pub struct Window {
    pub url: String,
    task: tokio::task::JoinHandle<()>,
    revoked: Arc<AtomicBool>,
}
impl Drop for Window {
    fn drop(&mut self) {
        self.revoked.store(true, Ordering::SeqCst);
        self.task.abort();
    }
}
impl Window {
    pub async fn open(
        queue: Arc<Queue>,
        connection: Arc<Mutex<ConnectionStatus>>,
    ) -> anyhow::Result<Self> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let host = listener.local_addr()?.to_string();
        let token = uuid::Uuid::new_v4().simple().to_string();
        let url = format!("http://{host}/#{token}");
        let revoked = Arc::new(AtomicBool::new(false));
        let view = View {
            queue,
            connection,
            token,
            host,
            revoked: revoked.clone(),
        };
        let router = Router::new()
            .route("/", get(page))
            .route("/state", get(snapshot))
            .with_state(view);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self { url, task, revoked })
    }
}
fn local_request(headers: &HeaderMap, host: &str) -> bool {
    headers.get("host").and_then(|h| h.to_str().ok()) == Some(host)
        && headers
            .get("origin")
            .is_none_or(|o| o.to_str().ok() == Some(format!("http://{host}").as_str()))
}
fn protect(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert("cache-control", "no-store".parse().unwrap());
    headers.insert("referrer-policy", "no-referrer".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    headers.insert("content-security-policy", "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'".parse().unwrap());
    response
}
async fn page(State(view): State<View>, headers: HeaderMap) -> Response {
    if !local_request(&headers, &view.host) {
        return StatusCode::FORBIDDEN.into_response();
    }
    protect(Html(include_str!("audience.html")).into_response())
}
async fn snapshot(State(view): State<View>, headers: HeaderMap) -> Response {
    if view.revoked.load(Ordering::SeqCst)
        || !local_request(&headers, &view.host)
        || headers.get("authorization").and_then(|h| h.to_str().ok())
            != Some(format!("Bearer {}", view.token).as_str())
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut value = view.queue.audience().await;
    if view.revoked.load(Ordering::SeqCst) {
        return StatusCode::FORBIDDEN.into_response();
    }
    value["connection"] = serde_json::json!(view.connection.lock().state);
    protect(Json(value).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_foreign_origins_and_dns_rebinding() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "127.0.0.1:1234".parse().unwrap());
        assert!(local_request(&headers, "127.0.0.1:1234"));
        headers.insert("origin", "https://example.com".parse().unwrap());
        assert!(!local_request(&headers, "127.0.0.1:1234"));
        headers.remove("origin");
        headers.insert("host", "evil.example:1234".parse().unwrap());
        assert!(!local_request(&headers, "127.0.0.1:1234"));
    }
}
