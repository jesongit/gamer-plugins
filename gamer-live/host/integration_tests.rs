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
        let mut value: toml::Value = toml::from_str(manifest).unwrap();
        let denied = ["media.stream", "run.submit", "run.control"];
        value["permissions"]
            .as_array_mut()
            .unwrap()
            .retain(|permission| !denied.contains(&permission.as_str().unwrap()));
        // This fixture exercises ordinary management-action permissions. Tools
        // requiring the deliberately removed grants cannot be declared by it.
        value["tools"].as_array_mut().unwrap().retain(|tool| {
            !tool["required_permissions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|permission| denied.contains(&permission.as_str().unwrap()))
        });
        toml::to_string(&value).unwrap()
    };
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut out);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.toml", options).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        append_schemas(&mut zip, &manifest);
        zip.start_file("ui/plugin.js", options).unwrap();
        zip.write_all(b"export const sdkVersion=1; export const panels={};")
            .unwrap();
        zip.finish().unwrap();
    }
    out.into_inner()
}
fn append_schemas<W: std::io::Write + std::io::Seek>(zip: &mut zip::ZipWriter<W>, manifest: &str) {
    let value: toml::Value = toml::from_str(manifest).unwrap();
    let mut paths = std::collections::BTreeSet::new();
    for tool in value["tools"].as_array().unwrap() {
        for key in ["input_schema", "output_schema"] {
            if let Some(path) = tool.get(key).and_then(toml::Value::as_str) {
                paths.insert(path);
            }
        }
    }
    for path in paths {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/gamer-live")
            .join(path);
        zip.start_file(path, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&std::fs::read(source).unwrap()).unwrap();
    }
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
    assert!(service
        .call_extension(&id, "stream.switch", json!({"device_id":"device"}))
        .await
        .is_err());
    for action in [
        "queue.configure",
        "queue.test",
        "queue.control",
        "queue.reconcile_success",
        "automatic.start",
        "automatic.stop",
        "automatic.preview",
    ] {
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

#[tokio::test]
async fn audience_management_actions_require_running_and_live_connect_permission() {
    let directory = tempfile::tempdir().unwrap();
    let jobs = Arc::new(Jobs::default());
    let service = ExtensionService::for_data_root(directory.path(), CapabilityRegistry::default())
        .with_builtin_service(jobs.clone());
    let id = ExtensionId::parse(ID).unwrap();
    let denied_archive = {
        let manifest = include_str!("../manifest.toml").replace("\"live.connect\", ", "");
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut out);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("manifest.toml", options).unwrap();
            zip.write_all(manifest.as_bytes()).unwrap();
            append_schemas(&mut zip, &manifest);
            zip.start_file("ui/plugin.js", options).unwrap();
            zip.write_all(b"export const sdkVersion=1; export const panels={};")
                .unwrap();
            zip.finish().unwrap();
        }
        out.into_inner()
    };
    service.install(&denied_archive).await.unwrap();
    service.enable(&id).await.unwrap();
    service.start(&id).await.unwrap();
    for action in [
        "audience.read",
        "audience.save",
        "audience.open",
        "audience.close",
    ] {
        assert!(service
            .call_extension(&id, action, json!({}))
            .await
            .is_err());
    }
    assert_eq!(jobs.calls.load(Ordering::SeqCst), 0);
    let allowed_directory = tempfile::tempdir().unwrap();
    let service =
        ExtensionService::for_data_root(allowed_directory.path(), CapabilityRegistry::default())
            .with_builtin_service(jobs.clone());
    service.install(&archive(true)).await.unwrap();
    service.enable(&id).await.unwrap();
    service.start(&id).await.unwrap();
    for action in [
        "audience.read",
        "audience.save",
        "audience.open",
        "audience.close",
    ] {
        service
            .call_extension(&id, action, json!({}))
            .await
            .unwrap();
    }
    assert_eq!(jobs.calls.load(Ordering::SeqCst), 4);
    service.disable(&id).await.unwrap();
    assert!(service
        .call_extension(&id, "audience.read", json!({}))
        .await
        .is_err());
}

#[tokio::test]
async fn reconcile_success_requires_running_and_run_control_permission() {
    assert_eq!(
        permissions(ID, "queue.reconcile_success"),
        Some(&[Permission::RunControl][..])
    );
    let denied_directory = tempfile::tempdir().unwrap();
    let jobs = Arc::new(Jobs::default());
    let denied =
        ExtensionService::for_data_root(denied_directory.path(), CapabilityRegistry::default())
            .with_builtin_service(jobs.clone());
    denied.install(&archive(false)).await.unwrap();
    let id = ExtensionId::parse(ID).unwrap();
    denied.enable(&id).await.unwrap();
    denied.start(&id).await.unwrap();
    assert!(denied
        .call_extension(
            &id,
            "queue.reconcile_success",
            json!({"item_id":"q","completed_entrypoint":"hsr/daily.yaml"})
        )
        .await
        .is_err());
    assert_eq!(jobs.calls.load(Ordering::SeqCst), 0);
    let directory = tempfile::tempdir().unwrap();
    let allowed = ExtensionService::for_data_root(directory.path(), CapabilityRegistry::default())
        .with_builtin_service(jobs.clone());
    allowed.install(&archive(true)).await.unwrap();
    assert!(allowed
        .call_extension(&id, "queue.reconcile_success", json!({}))
        .await
        .is_err());
    allowed.enable(&id).await.unwrap();
    allowed.start(&id).await.unwrap();
    allowed
        .call_extension(
            &id,
            "queue.reconcile_success",
            json!({"item_id":"q","completed_entrypoint":"hsr/daily.yaml"}),
        )
        .await
        .unwrap();
    assert_eq!(jobs.calls.load(Ordering::SeqCst), 1);
    allowed.disable(&id).await.unwrap();
    assert!(allowed
        .call_extension(&id, "queue.reconcile_success", json!({}))
        .await
        .is_err());
}

#[tokio::test]
async fn script_switch_only_allows_authorized_yaml_callers_and_running_live() {
    struct CallerRegistrar;
    #[async_trait::async_trait]
    impl crate::extensions::service::TimerRunnerRegistrar for CallerRegistrar {
        async fn extension_started(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn extension_stopped(&self, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn executes_without_instance(&self, _: &str) -> bool {
            true
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let jobs = Arc::new(Jobs::default());
    let service = ExtensionService::for_data_root(temp.path(), CapabilityRegistry::default())
        .with_builtin_service(jobs.clone())
        .with_runner_registrar(Arc::new(CallerRegistrar));
    let live = ExtensionId::parse(ID).unwrap();
    service.install(&archive(true)).await.unwrap();
    service.enable(&live).await.unwrap();
    service.start(&live).await.unwrap();
    fn caller_archive(id: &str, granted: bool) -> Vec<u8> {
        let permission = if granted { "media.stream" } else { "log.write" };
        let manifest = format!("manifest_version=2\nid=\"{id}\"\nversion=\"1.0.0\"\nname=\"fixture\"\nentry=\"plugin.wasm\"\npermissions=[\"{permission}\"]\n[execution]\nkind=\"wasm\"\n");
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut out);
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("manifest.toml", options).unwrap();
            zip.write_all(manifest.as_bytes()).unwrap();
            zip.start_file("plugin.wasm", options).unwrap();
            zip.write_all(b"\0asm\x01\0\0\0").unwrap();
            zip.finish().unwrap();
        }
        out.into_inner()
    }
    // Caller admission uses installed, enabled manifest permissions, even for
    // the controlled script bridge that does not execute a caller guest here.
    for (id, granted, allowed) in [("gamer-yaml", true, true), ("other-caller", true, false)] {
        let caller = ExtensionId::parse(id).unwrap();
        service.install(&caller_archive(id, granted)).await.unwrap();
        service.enable(&caller).await.unwrap();
        service.start(&caller).await.unwrap();
        let context = service.plugin_call_context(&caller).await.unwrap();
        let result = service
            .call_extension_from_plugin(
                &context,
                &live,
                "stream.switch",
                json!({"device_id":"device"}),
            )
            .await;
        assert_eq!(result.is_ok(), allowed);
    }
    assert_eq!(jobs.calls.load(Ordering::SeqCst), 1);
    let caller = ExtensionId::parse("gamer-yaml").unwrap();
    service.disable(&live).await.unwrap();
    let context = service.plugin_call_context(&caller).await.unwrap();
    assert!(service
        .call_extension_from_plugin(
            &context,
            &live,
            "stream.switch",
            json!({"device_id":"device"})
        )
        .await
        .is_err());
}

struct ConfirmationCallerRegistrar;
#[async_trait::async_trait]
impl crate::extensions::service::TimerRunnerRegistrar for ConfirmationCallerRegistrar {
    async fn extension_started(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn extension_stopped(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn executes_without_instance(&self, _: &str) -> bool {
        true
    }
}

fn confirmation_caller_archive(id: &str, permission: &str) -> Vec<u8> {
    let manifest = format!("manifest_version=2\nid=\"{id}\"\nversion=\"1.0.0\"\nname=\"confirmation caller fixture\"\nentry=\"plugin.wasm\"\npermissions=[\"{permission}\"]\n[execution]\nkind=\"wasm\"\n");
    let mut out = std::io::Cursor::new(Vec::new());
    {
        let mut zip = zip::ZipWriter::new(&mut out);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("manifest.toml", options).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.start_file("plugin.wasm", options).unwrap();
        zip.write_all(b"\0asm\x01\0\0\0").unwrap();
        zip.finish().unwrap();
    }
    out.into_inner()
}

#[tokio::test]
async fn confirmation_internal_actions_reject_public_admin_calls_and_forged_owner() {
    let directory = tempfile::tempdir().unwrap();
    let jobs = Arc::new(Jobs::default());
    let service = ExtensionService::for_data_root(directory.path(), CapabilityRegistry::default())
        .with_builtin_service(jobs.clone());
    let live = ExtensionId::parse(ID).unwrap();
    service.install(&archive(true)).await.unwrap();
    service.enable(&live).await.unwrap();
    service.start(&live).await.unwrap();
    for action in ["confirmation.begin", "confirmation.poll"] {
        for values in [
            json!({"run_id":"current-main", "device_id":"browser", "package_id":"hsr", "timeout_secs":300}),
            json!({"run_id":"current-main", "device_id":"browser", "package_id":"hsr", "timeout_secs":300,
                "caller":"gamer-yaml", "plugin_id":"gamer-yaml", "token":"forged-fixture-token",
                "context":{"caller":"gamer-yaml","run_id":"current-main"}}),
        ] {
            assert!(
                matches!(
                    service.call_extension(&live, action, values).await,
                    Err(ExtensionError::CallRejected(_))
                ),
                "{action}"
            );
        }
    }
    assert_eq!(
        jobs.calls.load(Ordering::SeqCst),
        0,
        "an authenticated admin request cannot fabricate plugin caller provenance"
    );
    assert!(crate::extensions::native_public_actions(&live)
        .iter()
        .all(|entry| !matches!(
            entry["action"].as_str(),
            Some("confirmation.begin" | "confirmation.poll")
        )));
}

#[tokio::test]
async fn confirmation_internal_calls_require_yaml_permission_and_current_lifecycle_context() {
    for (caller_id, permission, allowed) in [
        ("gamer-yaml", "runtime.sleep", true),
        ("gamer-yaml", "log.write", false),
        ("other-caller", "runtime.sleep", false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let jobs = Arc::new(Jobs::default());
        let service =
            ExtensionService::for_data_root(directory.path(), CapabilityRegistry::default())
                .with_builtin_service(jobs.clone())
                .with_runner_registrar(Arc::new(ConfirmationCallerRegistrar));
        let live = ExtensionId::parse(ID).unwrap();
        let caller = ExtensionId::parse(caller_id).unwrap();
        service.install(&archive(true)).await.unwrap();
        service.enable(&live).await.unwrap();
        service.start(&live).await.unwrap();
        service
            .install(&confirmation_caller_archive(caller_id, permission))
            .await
            .unwrap();
        assert!(service.plugin_call_context(&caller).await.is_err());
        service.enable(&caller).await.unwrap();
        service.start(&caller).await.unwrap();
        let context = service.plugin_call_context(&caller).await.unwrap();
        let values = json!({"run_id":"fixture-run", "device_id":"browser", "package_id":"hsr", "timeout_secs":300});
        for action in ["confirmation.begin", "confirmation.poll"] {
            assert_eq!(
                service
                    .call_extension_from_plugin(&context, &live, action, values.clone())
                    .await
                    .is_ok(),
                allowed,
                "{caller_id}/{permission}/{action}"
            );
        }
        assert_eq!(
            jobs.calls.load(Ordering::SeqCst),
            if allowed { 2 } else { 0 }
        );
        if !allowed {
            continue;
        }
        service.disable(&caller).await.unwrap();
        assert!(service
            .call_extension_from_plugin(&context, &live, "confirmation.poll", values.clone())
            .await
            .is_err());
        service.enable(&caller).await.unwrap();
        service.start(&caller).await.unwrap();
        assert!(
            service
                .call_extension_from_plugin(&context, &live, "confirmation.begin", values.clone())
                .await
                .is_err(),
            "a prior process instance context must stay invalid after restart"
        );
        let fresh = service.plugin_call_context(&caller).await.unwrap();
        service
            .call_extension_from_plugin(&fresh, &live, "confirmation.poll", values.clone())
            .await
            .unwrap();
        assert_eq!(jobs.calls.load(Ordering::SeqCst), 3);
        service.disable(&live).await.unwrap();
        for action in ["confirmation.begin", "confirmation.poll"] {
            assert!(service
                .call_extension_from_plugin(&fresh, &live, action, values.clone())
                .await
                .is_err());
        }
        assert_eq!(jobs.calls.load(Ordering::SeqCst), 3);
    }
}
