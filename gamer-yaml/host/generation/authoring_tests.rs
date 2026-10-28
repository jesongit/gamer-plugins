use super::*;

struct NoExecutor;
impl crate::run_manager::RunExecutor for NoExecutor {
    fn prepare<'a>(
        &'a self,
        _: &'a crate::core::RunContext,
        _: &'a crate::core::RunRequest,
    ) -> futures_util::future::BoxFuture<'a, Result<()>> {
        Box::pin(async { anyhow::bail!("synthetic authoring fixture must not prepare a target") })
    }
    fn acquire(&self, _: &crate::core::RunContext) -> Result<Box<dyn crate::core::ActivityLease>> {
        anyhow::bail!("synthetic authoring fixture must not acquire a target")
    }
    fn execute<'a>(
        &'a self,
        _: &'a crate::core::RunContext,
        _: &'a crate::core::RunRequest,
        _: bool,
        _: Arc<AtomicBool>,
    ) -> futures_util::future::BoxFuture<'a, Result<Vec<(String, String)>>> {
        Box::pin(async { anyhow::bail!("synthetic authoring fixture must not execute a target") })
    }
}
pub(super) fn fixture() -> (tempfile::TempDir, AuthoringService, Value) {
    let root = tempfile::tempdir().unwrap();
    let packages = Arc::new(
        PackageStore::open(&crate::config::Config {
            data_dir: root.path().into(),
            ..Default::default()
        })
        .unwrap(),
    );
    packages.ensure_default_package().unwrap();
    super::super::resources::register_resource_handlers(&packages);
    let service = AuthoringService::new(packages, Arc::new(RunManager::new(Arc::new(NoExecutor))));
    (root, service, Value::Null)
}

#[tokio::test]
async fn admitted_save_rechecks_after_waiting_for_the_draft_and_before_persistence() {
    let (_root, service, _) = fixture();
    service
        .call(
            "authoring.create",
            json!({"package_id":"default","script_id":"locked.yaml"}),
        )
        .await
        .unwrap();
    let path = service
        .inner
        .authoring_path("default", "locked.yaml")
        .unwrap();
    let original = std::fs::read(&path).unwrap();
    let gate = service.inner.authoring_gates.lock()["default/locked.yaml"].clone();
    let held = gate.lock().await;
    let active = Arc::new(AtomicBool::new(true));
    let reached = Arc::new(tokio::sync::Notify::new());
    let observed = active.clone();
    let entered = reached.clone();
    let check: Admission = Arc::new(move || {
        entered.notify_one();
        ensure!(observed.load(Ordering::Acquire), "credential revoked");
        Ok(())
    });
    let pending = service.clone();
    let task = tokio::spawn(async move {
        pending.call_admitted("authoring.save",json!({"package_id":"default","script_id":"locked.yaml","expected_version":"","yaml":"run: []"}),check).await
    });
    reached.notified().await;
    active.store(false, Ordering::Release);
    drop(held);
    assert!(task
        .await
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("credential revoked"));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let checks = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = checks.clone();
    let check: Admission = Arc::new(move || {
        ensure!(
            observed.fetch_add(1, Ordering::SeqCst) < 2,
            "revoked before commit"
        );
        Ok(())
    });
    assert!(service.call_admitted("authoring.save",json!({"package_id":"default","script_id":"locked.yaml","expected_version":"","yaml":"run: []"}),check).await.unwrap_err().to_string().contains("revoked before commit"));
    assert!(checks.load(Ordering::SeqCst) >= 3);
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[tokio::test]
async fn script_revisions_publish_exact_selected_content_and_keep_minor_edits_private() {
    let (_root, service, _input) = fixture();
    let query = json!({"package_id":"default","script_id":"日常.yaml"});
    let created = service
        .inner
        .authoring_call("authoring.create", query.clone())
        .await
        .unwrap();
    assert_eq!(created["script"]["versions"].as_array().unwrap().len(), 0);
    let first=service.inner.authoring_call("authoring.save",json!({"package_id":"default","script_id":"日常.yaml","expected_version":"","yaml":"# exact source\nrun: [{log: first}]\n"})).await.unwrap();
    assert_eq!(first["script"]["head_version"], "0.1");
    assert!(service
        .inner
        .packages
        .read_text(
            "default",
            super::super::YAML_EXTENSION_ID,
            "automations/日常.yaml"
        )
        .unwrap()
        .is_none());
    let published=service.inner.authoring_call("authoring.publish",json!({"package_id":"default","script_id":"日常.yaml","expected_version":"0.1","version":"0.1"})).await.unwrap();
    assert_eq!(published["script"]["published_version"], "1.0");
    let formal = service
        .inner
        .packages
        .read_text(
            "default",
            super::super::YAML_EXTENSION_ID,
            "automations/日常.yaml",
        )
        .unwrap()
        .unwrap();
    assert_eq!(formal.content, "# exact source\nrun: [{log: first}]\n");
    let next=service.inner.authoring_call("authoring.save",json!({"package_id":"default","script_id":"日常.yaml","expected_version":"1.0","yaml":"run: [{log: second}]\n"})).await.unwrap();
    assert_eq!(next["script"]["head_version"], "1.1");
    assert_eq!(
        service
            .inner
            .packages
            .read_text(
                "default",
                super::super::YAML_EXTENSION_ID,
                "automations/日常.yaml"
            )
            .unwrap()
            .unwrap()
            .content,
        formal.content
    );
    assert!(service.inner.authoring_call("authoring.save",json!({"package_id":"default","script_id":"日常.yaml","expected_version":"1.0","yaml":"run: []"})).await.is_err());
    let saved=service.inner.authoring_call("authoring.publish",json!({"package_id":"default","script_id":"日常.yaml","expected_version":"1.1","version":"1.1"})).await.unwrap();
    assert_eq!(saved["script"]["published_version"], "2.0");
    assert_eq!(
        saved["script"]["versions"][0]["yaml"],
        "# exact source\nrun: [{log: first}]\n"
    );
    let old_records = service
        .inner
        .packages
        .data_root()
        .join("extension-data/gamer-yaml/candidates/default");
    assert!(
        !old_records.exists()
            || !old_records.read_dir().unwrap().any(|p| p
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "json"))
    );
}

#[tokio::test]
async fn pinned_publication_rejects_drift_and_revocation_before_any_formal_write() {
    let (_root, service, _) = fixture();
    service
        .call(
            "authoring.create",
            json!({"package_id":"default","script_id":"pinned.yaml"}),
        )
        .await
        .unwrap();
    let source = "# immutable source\nrun: [{log: pinned}]\n";
    service.call("authoring.save",json!({"package_id":"default","script_id":"pinned.yaml","expected_version":"","yaml":source})).await.unwrap();
    let correct = format!("{:x}", Sha256::digest(source.as_bytes()));
    let mut args = json!({"package_id":"default","script_id":"pinned.yaml","version":"0.1","expected_version":"0.1","expected_source_sha256":correct,"expected_resource_sha256":null});
    args["expected_source_sha256"] = json!("0".repeat(64));
    assert!(service
        .call("authoring.publish", args.clone())
        .await
        .is_err());
    args["expected_source_sha256"] = json!(correct);
    args["expected_resource_sha256"] = json!("0".repeat(64));
    assert!(service
        .call("authoring.publish", args.clone())
        .await
        .is_err());
    args["expected_resource_sha256"] = Value::Null;
    let admission: Admission = Arc::new(|| anyhow::bail!("credential_invalid: synthetic revoke"));
    assert!(service
        .call_admitted("authoring.publish", args.clone(), admission)
        .await
        .is_err());
    assert!(service
        .inner
        .packages
        .read_text(
            "default",
            super::super::YAML_EXTENSION_ID,
            "automations/pinned.yaml"
        )
        .unwrap()
        .is_none());
    let published = service
        .call("authoring.publish", args.clone())
        .await
        .unwrap();
    assert_eq!(published["script"]["head_version"], "1.0");
    assert_eq!(published["script"]["versions"].as_array().unwrap().len(), 2);
    assert_eq!(
        service
            .inner
            .packages
            .read_text(
                "default",
                super::super::YAML_EXTENSION_ID,
                "automations/pinned.yaml"
            )
            .unwrap()
            .unwrap()
            .content,
        source
    );
    assert!(service.call("authoring.publish", args).await.is_err());
}

#[tokio::test]
async fn invalid_source_is_an_editable_revision_but_cannot_publish_or_start_a_device() {
    let (_root, service, _input) = fixture();
    service
        .inner
        .authoring_call(
            "authoring.create",
            json!({"package_id":"default","script_id":"test.yaml"}),
        )
        .await
        .unwrap();
    let draft=service.inner.authoring_call("authoring.save",json!({"package_id":"default","script_id":"test.yaml","expected_version":"","yaml":"version: 2\nrun: []\n"})).await.unwrap();
    assert_eq!(draft["script"]["head_version"], "0.1");
    assert!(!draft["script"]["versions"][0]["diagnostics"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(service.inner.authoring_call("authoring.publish",json!({"package_id":"default","script_id":"test.yaml","expected_version":"0.1","version":"0.1"})).await.is_err());
    assert!(service.inner.authoring_call("authoring.generate",json!({"package_id":"default","script_id":"test.yaml","expected_version":"0.1","instruction":"rewrite","samples":[],"templates":[]})).await.is_err());
    let after = service
        .inner
        .authoring_call(
            "authoring.get",
            json!({"package_id":"default","script_id":"test.yaml"}),
        )
        .await
        .unwrap();
    assert_eq!(after["script"]["versions"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn static_checks_reject_native_argument_errors_and_missing_literal_templates() {
    let (_root, service, _input) = fixture();
    for source in [
        "run: [{find: {template: missing.png}}]",
        "run: [{find: {template: missing.png, timeout: 2s}}]",
        "run: [{tap: {x: bad, y: 0.2}}]",
    ] {
        let diagnostics =
            service
                .inner
                .authoring_diagnostics(source, &BTreeMap::new(), &BTreeMap::new());
        assert!(!diagnostics.as_array().unwrap().is_empty(), "{source}");
    }
}

#[tokio::test]
async fn a_generating_script_rejects_manual_saves_and_publication() {
    let (_root, service, _input) = fixture();
    let mut doc = service
        .inner
        .authoring_read("default", "busy.yaml")
        .unwrap();
    doc.job = Some(json!({"id":"authoring-test-job","state":"generating"}));
    service.inner.authoring_write("default", &doc).unwrap();
    service.inner.jobs.lock().insert(
        "authoring-test-job".into(),
        Job {
            cancel: Arc::new(AtomicBool::new(false)),
        },
    );
    let result = service.inner.authoring_call("authoring.save", json!({"package_id":"default","script_id":"busy.yaml","expected_version":"","yaml":"run: []"})).await;
    assert!(result.unwrap_err().to_string().contains("script_busy"));
    service.inner.jobs.lock().clear();
    let interrupted = service
        .inner
        .authoring_call(
            "authoring.get",
            json!({"package_id":"default","script_id":"busy.yaml"}),
        )
        .await
        .unwrap();
    assert_eq!(interrupted["script"]["job"]["state"], "interrupted");
}

#[tokio::test]
async fn standalone_authoring_preserves_unknown_history_metadata_and_legacy_template_bytes() {
    let (root, service, _) = fixture();
    let script = "history.yaml";
    let source = "# historical AI source, exact formatting\nrun: [{log: original}]\n";
    let png = {
        let mut bytes = std::io::Cursor::new(vec![]);
        image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(3, 3, image::Luma([137])))
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    };
    use base64::Engine;
    let historical = json!({
        "id":"durable-document-id", "legacy_run_binding":false, "private_run_ids":["past-run"],
        "script_id":script, "published_version":null, "published_resource_hash":null,
        "job":{"id":"past-job","state":"generating","supplier_receipt":{"id":"old"}},
        "future_document_metadata":{"tag":"keep","nested":[1,{"value":true}]},
        "versions":[{"version":"0.7","yaml":source,"created_at":"2026-01-01T00:00:00Z",
            "instruction":"old instruction","source":"ai","published":false,
            "diagnostics":[],"usage":{"known_tokens":12,"unknown":true},
            "material_refs":[],"generated_templates":{"legacy.png":base64::engine::general_purpose::STANDARD.encode(&png)},
            "crop_checks":{"legacy.png":{"status":"historical"}},
            "future_version_metadata":{"codec":"original","supplier":["keep"]}}]
    });
    let path = root
        .path()
        .join("extension-data/gamer-yaml/authoring/default")
        .join(format!("{:x}.json", Sha256::digest(script.as_bytes())));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&historical).unwrap()).unwrap();
    let original_bytes = std::fs::read(&path).unwrap();
    let opened = service
        .call(
            "authoring.get",
            json!({"package_id":"default","script_id":script}),
        )
        .await
        .unwrap();
    assert_eq!(opened["script"]["job"]["state"], "interrupted");
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original_bytes,
        "reading stale jobs must not rewrite history"
    );
    assert!(opened["script"]["versions"][0]
        .get("generated_templates")
        .is_none());
    assert!(service
        .call(
            "authoring.generate",
            json!({"package_id":"default","script_id":script})
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("unknown authoring action"));
    let edited = service
        .call(
            "authoring.save",
            json!({"package_id":"default","script_id":script,
        "expected_version":"0.7","yaml":"# external edit\nrun: [{log: edited}]\n"}),
        )
        .await
        .unwrap();
    assert_eq!(edited["script"]["head_version"], "0.8");
    let after: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        after["versions"][0], historical["versions"][0],
        "all historical fields must survive a save"
    );
    for field in ["future_document_metadata", "job", "id", "private_run_ids"] {
        assert_eq!(after[field], historical[field], "{field}");
    }
    let published = service
        .call(
            "authoring.publish",
            json!({"package_id":"default","script_id":script,
        "expected_version":"0.8","version":"0.7"}),
        )
        .await
        .unwrap();
    assert_eq!(published["script"]["published_version"], "1.0");
    assert_eq!(
        service
            .inner
            .packages
            .read_binary(
                "default",
                super::super::YAML_EXTENSION_ID,
                "templates/legacy.png"
            )
            .unwrap()
            .unwrap(),
        png
    );
    assert_eq!(
        service
            .inner
            .packages
            .read_text(
                "default",
                super::super::YAML_EXTENSION_ID,
                "automations/history.yaml"
            )
            .unwrap()
            .unwrap()
            .content,
        source
    );
    let reopened =
        AuthoringService::new(service.inner.packages.clone(), service.inner._runs.clone());
    let history = reopened
        .call(
            "authoring.get",
            json!({"package_id":"default","script_id":script}),
        )
        .await
        .unwrap();
    assert_eq!(history["script"]["versions"].as_array().unwrap().len(), 3);
    assert_eq!(
        history["script"]["versions"][0]["usage"],
        historical["versions"][0]["usage"]
    );
}

#[tokio::test]
async fn legacy_document_reads_do_not_assign_identity_or_rewrite_original_bytes() {
    let (_root, service, _) = fixture();
    let query = json!({"package_id":"default","script_id":"legacy.yaml"});
    service
        .call("authoring.create", query.clone())
        .await
        .unwrap();
    service.call("authoring.save",json!({"package_id":"default","script_id":"legacy.yaml","expected_version":"","yaml":"run: []"})).await.unwrap();
    let path = service
        .inner
        .authoring_path("default", "legacy.yaml")
        .unwrap();
    let mut historical: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for field in ["id", "legacy_run_binding", "private_run_ids"] {
        historical.as_object_mut().unwrap().remove(field);
    }
    let original = serde_json::to_vec_pretty(&historical).unwrap();
    std::fs::write(&path, &original).unwrap();
    service.call("authoring.get", query).await.unwrap();
    service
        .call(
            "authoring.validate",
            json!({"package_id":"default","script_id":"legacy.yaml","expected_version":"0.1"}),
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), original);
}
