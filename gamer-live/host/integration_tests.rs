use super::*;
use crate::{
    capabilities::CapabilityRegistry,
    extensions::{ExtensionId, ExtensionService, ExtensionState},
};
use std::{
    io::Write,
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Jobs {
    calls: AtomicUsize,
    stops: AtomicUsize,
}
#[async_trait::async_trait]
impl BuiltinService for Jobs {
    fn extension_id(&self) -> &str {
        ID
    }
    async fn call(&self, _: &str, _: Value) -> ExtensionResult<Value> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(json!({"ok":true}))
    }
    async fn stop(&self) {
        self.stops.fetch_add(1, Ordering::SeqCst);
    }
}
fn archive(permissions: bool) -> Vec<u8> {
    let manifest = include_str!("../manifest.toml");
    let manifest = if permissions {
        manifest.to_owned()
    } else {
        manifest
            .replace("\"media.stream\", \"live.connect\"", "\"live.connect\"")
            .replace(", \"run.submit\", \"run.control\"", "")
    };
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut out);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.toml", options).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.start_file("ui/plugin.js", options).unwrap();
        zip.write_all(b"export const sdkVersion=1; export const panels={};")
            .unwrap();
        zip.finish().unwrap();
    }
    out.into_inner()
}
#[tokio::test]
async fn calls_require_running_and_permissions_and_disable_drains_jobs() {
    let temp = tempfile::tempdir().unwrap();
    let jobs = Arc::new(Jobs::default());
    let service = ExtensionService::for_data_root(temp.path(), CapabilityRegistry::default())
        .with_builtin_service(jobs.clone());
    let id = ExtensionId::parse(ID).unwrap();
    service.install(&archive(false)).await.unwrap();
    assert!(service
        .call_extension(&id, "stream.start", json!({}))
        .await
        .is_err());
    service.enable(&id).await.unwrap();
    service.start(&id).await.unwrap();
    assert!(service
        .call_extension(&id, "stream.start", json!({}))
        .await
        .is_err());
    assert_eq!(jobs.calls.load(Ordering::SeqCst), 0);
    for action in ["queue.configure", "queue.test", "queue.control"] {
        assert!(service
            .call_extension(&id, action, json!({}))
            .await
            .is_err());
    }
    assert_eq!(jobs.calls.load(Ordering::SeqCst), 0);
    service
        .call_extension(&id, "events.read", json!({}))
        .await
        .unwrap();
    assert_eq!(jobs.calls.load(Ordering::SeqCst), 1);
    let snapshot = service.disable(&id).await.unwrap();
    assert_eq!(snapshot.state(), ExtensionState::Disabled);
    assert_eq!(jobs.stops.load(Ordering::SeqCst), 1);
    assert!(service
        .call_extension(&id, "events.read", json!({}))
        .await
        .is_err());
    service.enable(&id).await.unwrap();
    service.start(&id).await.unwrap();
    service.shutdown_builtin_service().await;
    assert_eq!(jobs.stops.load(Ordering::SeqCst), 2);
    assert!(service
        .call_extension(&id, "events.read", json!({}))
        .await
        .is_err());
}
