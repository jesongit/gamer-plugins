use super::*;

fn png() -> Vec<u8> {
    let image = image::DynamicImage::ImageLuma8(image::GrayImage::from_fn(9, 9, |x, y| {
        image::Luma([(x * 13 + y * 7) as u8])
    }));
    let mut buffer = std::io::Cursor::new(vec![]);
    image
        .write_to(&mut buffer, image::ImageFormat::Png)
        .unwrap();
    buffer.into_inner()
}
fn query(script: &str) -> Value {
    json!({"package_id":"default","script_id":script})
}
async fn draft(service: &AuthoringService, script: &str) -> Document {
    service
        .inner
        .authoring_call("authoring.create", query(script))
        .await
        .unwrap();
    service.inner.authoring_call("authoring.save", json!({"package_id":"default","script_id":script,"expected_version":"","yaml":"# test draft\nrun: [{log: private}]\n"})).await.unwrap();
    service.inner.authoring_read("default", script).unwrap()
}
fn record(id: &str, script: &str, runner: &str) -> crate::run_manager::RunRecord {
    crate::run_manager::RunRecord {
        run_id: id.into(),
        device_id: "fixture-device".into(),
        runner_id: runner.into(),
        entrypoint: format!("default/{script}"),
        content_package: Some("default".into()),
        script_id: script.into(),
        source: crate::run_manager::RunSource::Manual,
        task_id: None,
        scheduled_at: None,
        state: crate::run_manager::RunState::Success,
        started_at: chrono::Utc::now(),
        finished_at: Some(chrono::Utc::now()),
        error: None,
    }
}
fn frozen(db: &crate::store::Db, id: &str, doc: &Document) {
    db.trace.register(id, None);
    db.trace.snapshot(id,json!({"_authoring_document":doc.id,"_script_revision":doc.head(),
        "_source_files":{format!("automations/{}",doc.script_id):doc.versions.last().unwrap().yaml}})).unwrap();
    db.trace
        .submit(
            crate::runtime_trace::CapturedEvidence {
                frame: Arc::new(crate::matcher::DecodedFrame::from_rgb(
                    image::RgbImage::from_pixel(3, 3, image::Rgb([17, 23, 31])),
                )),
                captured_at: chrono::Utc::now().to_rfc3339(),
                source: None,
            },
            json!({"run_id":id,"kind":"consumed","trace_enabled":true}),
            false,
            None,
        )
        .unwrap();
    db.trace.flush();
    db.save_run_record(&record(id, &doc.script_id, "gamer-yaml-version"))
        .unwrap();
}

#[tokio::test]
async fn deleting_draft_preserves_public_history_templates_shared_material_and_explicit_import() {
    let (root, service, _) = super::super::tests::fixture();
    let mut doc = draft(&service, "daily.yaml").await;
    doc.versions[0]
        .generated_templates
        .insert("shared.png".into(), png());
    doc.versions[0]
        .material_refs
        .push(json!({"plugin_id":"gamer-video","sample_id":"shared"}));
    service.inner.authoring_write("default", &doc).unwrap();
    service
        .inner
        .authoring_call(
            "authoring.publish",
            json!({"package_id":"default","script_id":"daily.yaml","expected_version":"0.1"}),
        )
        .await
        .unwrap();
    service.inner.authoring_call("authoring.save",json!({"package_id":"default","script_id":"daily.yaml","expected_version":"1.0","yaml":"# another private\nrun: [{log: changed}]\n"})).await.unwrap();
    let formal = revisions::snapshot(&service.inner.packages, "default").unwrap();
    let public_history = revisions::history(&service.inner.packages, "default").unwrap();
    let mut sibling = draft(&service, "other.yaml").await;
    sibling.versions[0]
        .generated_templates
        .insert("shared.png".into(), png());
    sibling.versions[0]
        .material_refs
        .push(json!({"plugin_id":"gamer-video","sample_id":"shared"}));
    service.inner.authoring_write("default", &sibling).unwrap();
    let sibling_bytes = std::fs::read(
        service
            .inner
            .authoring_path("default", "other.yaml")
            .unwrap(),
    )
    .unwrap();
    let video = root
        .path()
        .join("plugin-data/gamer-video/samples/shared.gamersample");
    std::fs::create_dir_all(video.parent().unwrap()).unwrap();
    std::fs::write(&video, b"shared teaching bundle").unwrap();
    let yaml_sample = service
        .inner
        .packages
        .plugin_dir("default", super::super::super::YAML_EXTENSION_ID)
        .unwrap()
        .join("samples/shared.gamersample");
    std::fs::create_dir_all(yaml_sample.parent().unwrap()).unwrap();
    std::fs::write(&yaml_sample, b"other shared material").unwrap();
    let preview = service
        .inner
        .authoring_call("authoring.delete_preview", query("daily.yaml"))
        .await
        .unwrap();
    assert_eq!(preview["version_count"], 2);
    assert_eq!(preview["generated_template_count"], 1);
    assert_eq!(preview["busy"], false);
    assert!(service
        .inner
        .authoring_call(
            "authoring.delete",
            json!({"package_id":"default","script_id":"daily.yaml","expected_version":"0.1"})
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("script_version_conflict"));
    service
        .inner
        .authoring_call(
            "authoring.delete",
            json!({"package_id":"default","script_id":"daily.yaml","expected_version":"1.1"}),
        )
        .await
        .unwrap();
    assert!(!service
        .inner
        .authoring_path("default", "daily.yaml")
        .unwrap()
        .exists());
    assert_eq!(std::fs::read(video).unwrap(), b"shared teaching bundle");
    assert_eq!(
        std::fs::read(&yaml_sample).unwrap(),
        b"other shared material"
    );
    let now = revisions::snapshot(&service.inner.packages, "default").unwrap();
    for (path, bytes) in formal {
        assert_eq!(now[&path], bytes, "{path}");
    }
    assert_eq!(
        revisions::history(&service.inner.packages, "default").unwrap()["revisions"],
        public_history["revisions"]
    );
    assert_eq!(
        service
            .inner
            .authoring_read("default", "other.yaml")
            .unwrap()
            .id,
        sibling.id
    );
    assert_eq!(
        std::fs::read(
            service
                .inner
                .authoring_path("default", "other.yaml")
                .unwrap()
        )
        .unwrap(),
        sibling_bytes
    );
    let listed = service
        .inner
        .authoring_call("authoring.list", json!({"package_id":"default"}))
        .await
        .unwrap();
    assert_eq!(listed["scripts"].as_array().unwrap().len(), 1);
    assert!(service
        .inner
        .authoring_call("authoring.get", query("daily.yaml"))
        .await
        .unwrap_err()
        .to_string()
        .contains("draft_not_found"));
    assert!(service.inner.authoring_call("authoring.save",json!({"package_id":"default","script_id":"daily.yaml","expected_version":"","yaml":"run: []"})).await.unwrap_err().to_string().contains("draft_not_found"));
    let imported = service
        .inner
        .authoring_call("authoring.import", query("daily.yaml"))
        .await
        .unwrap();
    assert_eq!(imported["script"]["published_version"], "1.0");
    assert_eq!(imported["script"]["versions"].as_array().unwrap().len(), 1);
    assert!(service
        .inner
        .authoring_call("authoring.import", query("daily.yaml"))
        .await
        .unwrap_err()
        .to_string()
        .contains("draft_exists"));
    let saved=service.inner.authoring_call("authoring.save",json!({"package_id":"default","script_id":"daily.yaml","expected_version":"1.0","yaml":"# use public template\nrun: [{find: {template: shared.png}}]\n"})).await.unwrap();
    assert_eq!(saved["script"]["head_version"], "1.1");
    assert_eq!(saved["script"]["versions"][1]["diagnostics"], json!([]));
    service
        .inner
        .authoring_call(
            "authoring.publish",
            json!({"package_id":"default","script_id":"daily.yaml","expected_version":"1.1"}),
        )
        .await
        .unwrap();
    assert_eq!(
        service
            .inner
            .authoring_read("default", "daily.yaml")
            .unwrap()
            .published_version
            .as_deref(),
        Some("2.0")
    );
    // A logically damaged draft cannot overwrite an existing public archive
    // with fewer immutable published snapshots.
    let archive_path = service
        .inner
        .authoring_published_path("default", "daily.yaml")
        .unwrap();
    let archive_bytes = std::fs::read(&archive_path).unwrap();
    let formal_after_publish = revisions::snapshot(&service.inner.packages, "default").unwrap();
    let mut damaged = service
        .inner
        .authoring_read("default", "daily.yaml")
        .unwrap();
    damaged.versions.retain(|version| version.version != "1.0");
    service.inner.authoring_write("default", &damaged).unwrap();
    assert!(service
        .inner
        .authoring_call(
            "authoring.delete",
            json!({"package_id":"default","script_id":"daily.yaml","expected_version":"2.0"})
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("published_history_conflict"));
    assert_eq!(std::fs::read(archive_path).unwrap(), archive_bytes);
    assert_eq!(
        revisions::snapshot(&service.inner.packages, "default").unwrap(),
        formal_after_publish
    );
}

#[tokio::test]
async fn deleting_owned_terminal_runs_isolates_public_other_drafts_retained_and_task_records() {
    let (root, service, _) = super::super::tests::fixture();
    let db = Arc::new(
        crate::store::Store::open(&crate::config::Config {
            data_dir: root.path().into(),
            ..Default::default()
        })
        .unwrap(),
    );
    let service = service.with_diagnostics(db.clone());
    let doc = draft(&service, "daily.yaml").await;
    let other = draft(&service, "other.yaml").await;
    frozen(&db, "private-run", &doc);
    frozen(&db, "retained-run", &doc);
    frozen(&db, "task-run", &doc);
    frozen(&db, "other-run", &other);
    db.save_run_record(&record("public-run", "daily.yaml", "gamer-yaml"))
        .unwrap();
    let mut task = record("task-run", "daily.yaml", "gamer-yaml-version");
    task.task_id = Some("scheduled-task".into());
    task.source = crate::run_manager::RunSource::Scheduled;
    db.save_run_record(&task).unwrap();
    db.trace.retain("retained-run").unwrap();
    db.append_run_event(
        "private-run".into(),
        json!({"ev":"detail","data":"private"}),
    )
    .await
    .unwrap();
    let preview = service
        .inner
        .authoring_call("authoring.delete_preview", query("daily.yaml"))
        .await
        .unwrap();
    assert_eq!(preview["private_runs"], json!(["private-run"]));
    assert_eq!(preview["protected_runs"].as_array().unwrap().len(), 2);
    let deleted = service
        .inner
        .authoring_call(
            "authoring.delete",
            json!({"package_id":"default","script_id":"daily.yaml","expected_version":"0.1"}),
        )
        .await
        .unwrap();
    assert_eq!(deleted["removed_private_runs"], 1);
    assert_eq!(deleted["removed_temporary_traces"], 1);
    assert!(db.stored_run("private-run".into()).await.unwrap().is_none());
    assert_eq!(
        db.run_event_page("private-run".into(), 0).await.unwrap()["events"],
        json!([])
    );
    assert!(!root.path().join("runtime-traces/private-run").exists());
    for id in ["retained-run", "task-run", "other-run", "public-run"] {
        assert!(db.stored_run(id.into()).await.unwrap().is_some(), "{id}");
    }
    assert_eq!(db.trace.page("retained-run", 0, 1)["status"], "retained");
}

#[tokio::test]
async fn missing_or_corrupt_ownership_evidence_never_guesses_public_records_as_private() {
    let (root, service, _) = super::super::tests::fixture();
    let db = Arc::new(
        crate::store::Store::open(&crate::config::Config {
            data_dir: root.path().into(),
            ..Default::default()
        })
        .unwrap(),
    );
    let service = service.with_diagnostics(db.clone());
    let doc = draft(&service, "daily.yaml").await;
    db.save_run_record(&record("unknown-run", "daily.yaml", "gamer-yaml-version"))
        .unwrap();
    let preview = service
        .inner
        .authoring_call("authoring.delete_preview", query("daily.yaml"))
        .await
        .unwrap();
    assert_eq!(preview["private_run_count"], 0);
    service
        .inner
        .authoring_call(
            "authoring.delete",
            json!({"package_id":"default","script_id":"daily.yaml","expected_version":doc.head()}),
        )
        .await
        .unwrap();
    assert!(db.stored_run("unknown-run".into()).await.unwrap().is_some());
    let path = service
        .inner
        .authoring_path("default", "broken.yaml")
        .unwrap();
    std::fs::write(&path, b"not a draft document").unwrap();
    assert!(service
        .inner
        .authoring_call(
            "authoring.delete",
            json!({"package_id":"default","script_id":"broken.yaml","expected_version":""})
        )
        .await
        .is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"not a draft document");
    // A valid JSON document in the wrong hashed file must not migrate its UUID
    // into, or otherwise overwrite, a different draft.
    let mut impostor = draft(&service, "other.yaml").await;
    let other_path = service
        .inner
        .authoring_path("default", "other.yaml")
        .unwrap();
    let other_bytes = std::fs::read(&other_path).unwrap();
    impostor.id.clear();
    std::fs::write(
        service
            .inner
            .authoring_path("default", "broken.yaml")
            .unwrap(),
        serde_json::to_vec(&impostor).unwrap(),
    )
    .unwrap();
    assert!(service
        .inner
        .authoring_call("authoring.get", query("broken.yaml"))
        .await
        .unwrap_err()
        .to_string()
        .contains("draft_identity_invalid"));
    assert!(service
        .inner
        .authoring_call(
            "authoring.delete",
            json!({"package_id":"default","script_id":"broken.yaml","expected_version":"0.1"})
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("draft_identity_invalid"));
    assert_eq!(std::fs::read(other_path).unwrap(), other_bytes);
}

#[tokio::test]
async fn explicit_keep_runs_and_active_jobs_reject_or_preserve_without_partial_draft_removal() {
    let (root, service, _) = super::super::tests::fixture();
    let db = Arc::new(
        crate::store::Store::open(&crate::config::Config {
            data_dir: root.path().into(),
            ..Default::default()
        })
        .unwrap(),
    );
    let service = service.with_diagnostics(db.clone());
    let mut doc = draft(&service, "daily.yaml").await;
    frozen(&db, "keep-run", &doc);
    doc.job = Some(json!({"id":"busy-model","state":"generating"}));
    service.inner.authoring_write("default", &doc).unwrap();
    service.inner.jobs.lock().insert(
        "busy-model".into(),
        Job {
            cancel: Arc::new(AtomicBool::new(false)),
        },
    );
    assert_eq!(
        service
            .inner
            .authoring_call("authoring.delete_preview", query("daily.yaml"))
            .await
            .unwrap()["busy"],
        true
    );
    assert!(service
        .inner
        .authoring_call(
            "authoring.delete",
            json!({"package_id":"default","script_id":"daily.yaml","expected_version":"0.1"})
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("script_busy"));
    assert!(service
        .inner
        .authoring_path("default", "daily.yaml")
        .unwrap()
        .exists());
    service.inner.jobs.lock().clear();
    let deleted=service.inner.authoring_call("authoring.delete",json!({"package_id":"default","script_id":"daily.yaml","expected_version":"0.1","remove_private_runs":false})).await.unwrap();
    assert_eq!(deleted["removed_private_runs"], 0);
    assert!(db.stored_run("keep-run".into()).await.unwrap().is_some());
    assert!(root.path().join("runtime-traces/keep-run").exists());
}

#[tokio::test]
async fn legacy_document_identity_is_stable_and_new_import_never_claims_previous_epoch_runs() {
    let (root, service, _) = super::super::tests::fixture();
    let db = Arc::new(
        crate::store::Store::open(&crate::config::Config {
            data_dir: root.path().into(),
            ..Default::default()
        })
        .unwrap(),
    );
    let service = service.with_diagnostics(db.clone());
    draft(&service, "daily.yaml").await;
    service
        .inner
        .authoring_call(
            "authoring.publish",
            json!({"package_id":"default","script_id":"daily.yaml","expected_version":"0.1"}),
        )
        .await
        .unwrap();
    service.inner.authoring_call("authoring.save",json!({"package_id":"default","script_id":"daily.yaml","expected_version":"1.0","yaml":"# legacy private source\nrun: [{log: private}]\n"})).await.unwrap();
    let mut legacy = service
        .inner
        .authoring_read("default", "daily.yaml")
        .unwrap();
    legacy.versions.last_mut().unwrap().created_at = "2020-01-01T00:00:00Z".into();
    let mut value = serde_json::to_value(&legacy).unwrap();
    for field in ["id", "legacy_run_binding", "private_run_ids"] {
        value.as_object_mut().unwrap().remove(field);
    }
    std::fs::write(
        service
            .inner
            .authoring_path("default", "daily.yaml")
            .unwrap(),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    for (id, year) in [("old-legitimate", "2021"), ("previous-epoch", "2019")] {
        db.trace.register(id, None);
        db.trace.snapshot(id,json!({"_script_revision":"1.1","_source_files":{"automations/daily.yaml":legacy.versions.last().unwrap().yaml}})).unwrap();
        let mut run = record(id, "daily.yaml", "gamer-yaml-version");
        run.started_at = chrono::DateTime::parse_from_rfc3339(&format!("{year}-02-01T00:00:00Z"))
            .unwrap()
            .with_timezone(&chrono::Utc);
        db.save_run_record(&run).unwrap();
    }
    let preview = service
        .inner
        .authoring_call("authoring.delete_preview", query("daily.yaml"))
        .await
        .unwrap();
    assert_eq!(preview["private_runs"], json!(["old-legitimate"]));
    let identity = service
        .inner
        .authoring_read("default", "daily.yaml")
        .unwrap()
        .id;
    assert!(!identity.is_empty());
    service
        .inner
        .authoring_call("authoring.get", query("daily.yaml"))
        .await
        .unwrap();
    assert_eq!(
        service
            .inner
            .authoring_read("default", "daily.yaml")
            .unwrap()
            .id,
        identity
    );
    service.inner.authoring_call("authoring.delete",json!({"package_id":"default","script_id":"daily.yaml","expected_version":"1.1","remove_private_runs":false})).await.unwrap();
    service
        .inner
        .authoring_call("authoring.import", query("daily.yaml"))
        .await
        .unwrap();
    let imported = service
        .inner
        .authoring_read("default", "daily.yaml")
        .unwrap();
    assert_ne!(imported.id, identity);
    assert!(!imported.legacy_run_binding);
    service.inner.authoring_call("authoring.save",json!({"package_id":"default","script_id":"daily.yaml","expected_version":"1.0","yaml":legacy.versions.last().unwrap().yaml})).await.unwrap();
    // Force identical source, version and time bounds: only the new draft epoch
    // must prevent claiming the preceding legacy document's retained records.
    let mut collision = service
        .inner
        .authoring_read("default", "daily.yaml")
        .unwrap();
    collision.versions.last_mut().unwrap().created_at = "2020-01-01T00:00:00Z".into();
    service
        .inner
        .authoring_write("default", &collision)
        .unwrap();
    assert_eq!(
        service
            .inner
            .authoring_call("authoring.delete_preview", query("daily.yaml"))
            .await
            .unwrap()["private_run_count"],
        0
    );
    assert!(db
        .stored_run("old-legitimate".into())
        .await
        .unwrap()
        .is_some());
}

struct PendingExecutor(Arc<tokio::sync::Notify>);
impl crate::run_manager::RunExecutor for PendingExecutor {
    fn prepare<'a>(
        &'a self,
        _: &'a crate::core::RunContext,
        _: &'a crate::core::RunRequest,
    ) -> futures_util::future::BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.0.notified().await;
            Ok(())
        })
    }
    fn execute<'a>(
        &'a self,
        _: &'a crate::core::RunContext,
        _: &'a crate::core::RunRequest,
        _: bool,
        _: Arc<AtomicBool>,
    ) -> futures_util::future::BoxFuture<'a, Result<Vec<(String, String)>>> {
        Box::pin(async { Ok(vec![]) })
    }
    fn acquire(&self, _: &crate::core::RunContext) -> Result<Box<dyn crate::core::ActivityLease>> {
        Ok(Box::new(crate::core::NoopLease))
    }
}

#[tokio::test]
async fn same_draft_gate_covers_pending_submit_and_starting_run_blocks_deletion_without_snapshot() {
    let (_root, service, _) = super::super::tests::fixture();
    let unblock = Arc::new(tokio::sync::Notify::new());
    let runs = Arc::new(RunManager::new(Arc::new(PendingExecutor(unblock.clone()))));
    let service = AuthoringService::new(service.inner.packages.clone(), runs.clone());
    draft(&service, "daily.yaml").await;
    let key = "default/daily.yaml".to_string();
    let gate = service
        .inner
        .authoring_gates
        .lock()
        .get(&key)
        .unwrap()
        .clone();
    let guard = gate.lock().await;
    service.inner.trials.pending_trials.lock().insert(
        "pending-fixture".into(),
        TrialInput {
            program: json!({}),
            templates: BTreeMap::new(),
        },
    );
    let inner = service.inner.clone();
    let deleting = tokio::spawn(async move {
        inner
            .authoring_call(
                "authoring.delete",
                json!({"package_id":"default","script_id":"daily.yaml","expected_version":"0.1"}),
            )
            .await
    });
    tokio::task::yield_now().await;
    assert!(
        !deleting.is_finished(),
        "pending request window must be serialized"
    );
    let request = crate::core::RunRequest::for_app(
        crate::core::AppContext::for_test("synthetic-device", "default").unwrap(),
        "gamer-yaml-version",
        "default/daily.yaml",
        crate::core::RunPayload::empty(),
    )
    .unwrap();
    let run = runs
        .submit(
            crate::run_manager::StartRequest {
                request,
                source: crate::run_manager::RunSource::Manual,
                task_id: None,
                scheduled_at: None,
                realtime_logs: false,
            },
            None,
        )
        .unwrap();
    drop(guard);
    assert!(deleting
        .await
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("script_busy"));
    let preview = service
        .inner
        .authoring_call("authoring.delete_preview", query("daily.yaml"))
        .await
        .unwrap();
    assert_eq!(preview["active_private_run_count"], 1);
    assert_eq!(preview["busy"], true);
    assert!(service
        .inner
        .authoring_path("default", "daily.yaml")
        .unwrap()
        .exists());
    assert!(runs
        .delete_terminal_records(
            &crate::store::Store::open(&crate::config::Config {
                data_dir: _root.path().into(),
                ..Default::default()
            })
            .unwrap(),
            &[run.run_id.clone()]
        )
        .unwrap_err()
        .to_string()
        .contains("run_active"));
    runs.cancel(&run.run_id);
    unblock.notify_one();
    runs.wait_terminal(&run.run_id).await;
    service.inner.trials.pending_trials.lock().clear();
}
