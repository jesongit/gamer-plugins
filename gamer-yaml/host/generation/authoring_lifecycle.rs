//! Draft-owned lifecycle only. Published resources, revision transactions and
//! shared teaching material are separate owners and never cleanup targets.
use super::*;
#[cfg(test)]
#[path = "authoring_lifecycle_tests.rs"]
mod tests;

const PRESERVED: &[&str] = &[
    "published_scripts",
    "published_templates",
    "published_history",
    "shared_materials",
    "retained_traces",
    "unscoped_global_logs",
];

impl AuthoringState {
    fn authoring_published_path(&self, package: &str, script: &str) -> Result<PathBuf> {
        self.packages.manifest(package)?;
        // Keep the public anchor under the existing package-scoped history
        // owner, so a legitimate whole-package deletion clears its identity too.
        Ok(self
            .packages
            .data_root()
            .join("extension-data/gamer-yaml/revisions")
            .join(package)
            .join("authoring-published")
            .join(format!("{:x}.json", Sha256::digest(script.as_bytes()))))
    }

    fn authoring_archive_public(&self, package: &str, doc: &Document) -> Result<()> {
        let mut public = doc.clone();
        public.versions.retain(|version| version.published);
        public.job = None;
        public.private_run_ids.clear();
        if public.versions.is_empty() {
            return Ok(());
        }
        let path = self.authoring_published_path(package, &doc.script_id)?;
        if path.exists() {
            let archived: Document = serde_json::from_slice(&std::fs::read(&path)?)?;
            ensure!(
                archived.script_id == doc.script_id,
                "authoring.published_history_invalid"
            );
            for version in archived.versions {
                let same = public
                    .versions
                    .iter()
                    .find(|current| current.version == version.version);
                ensure!(
                    same.is_some_and(|current| serde_json::to_value(current).ok()
                        == serde_json::to_value(&version).ok()),
                    "authoring.published_history_conflict: 已发布版本快照不可移除或改写"
                );
            }
        }
        std::fs::create_dir_all(path.parent().context("published history parent")?)?;
        crate::core::fs::atomic_write(&path, &serde_json::to_vec(&public)?)
    }

    async fn authoring_deletion_preview(&self, package: &str, doc: &Document) -> Result<Value> {
        let entrypoint = format!("{package}/{}", doc.script_id);
        let mut records = BTreeMap::new();
        let journal = self.diagnostics.lock().clone();
        if let Some(db) = journal {
            for record in db
                .run_records_for_scope(
                    "gamer-yaml-version".into(),
                    entrypoint.clone(),
                    package.into(),
                )
                .await?
            {
                records.insert(required(&record, "run_id")?.to_string(), record);
            }
        }
        for record in self
            ._runs
            .records_for_scope("gamer-yaml-version", &entrypoint, package)
        {
            records.insert(record.run_id.clone(), serde_json::to_value(record)?);
        }
        let db = self.diagnostics.lock().clone();
        let mut private_runs = vec![];
        let mut protected_runs = vec![];
        let mut unverified_runs = vec![];
        let mut active_count = 0;
        for (id, record) in records {
            // Starting submissions may not have a source snapshot yet. A live
            // slot on this exact private runner/script still blocks deletion.
            if !matches!(
                record["state"].as_str(),
                Some("success" | "failed" | "cancelled")
            ) || record["finished_at"].as_str().is_none()
                || self
                    ._runs
                    .active_for_device(record["device_id"].as_str().unwrap_or(""))
                    .is_some_and(|run| run.run_id == id)
            {
                active_count += 1;
                continue;
            }
            let trace = db
                .as_ref()
                .map(|db| db.trace.page(&id, 0, 1))
                .unwrap_or(Value::Null);
            let snapshot = &trace["snapshot"];
            let source_version = doc.versions.iter().find(|version| {
                !version.published
                    && snapshot["_script_revision"] == version.version
                    && snapshot["_source_files"][format!("automations/{}", doc.script_id)].as_str()
                        == Some(version.yaml.as_str())
            });
            let exact_source = source_version.is_some();
            let legacy_time_bound = source_version.is_some_and(|version| {
                chrono::DateTime::parse_from_rfc3339(&version.created_at)
                    .ok()
                    .zip(
                        record["started_at"]
                            .as_str()
                            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok()),
                    )
                    .is_some_and(|(created, started)| started >= created)
            });
            let identity = snapshot["_authoring_document"].as_str();
            let owned = if let Some(identity) = identity {
                !doc.id.is_empty() && identity == doc.id && exact_source
            } else {
                (doc.legacy_run_binding && exact_source && legacy_time_bound)
                    || doc.private_run_ids.contains(&id)
            };
            if !owned {
                unverified_runs.push(id);
                continue;
            }
            if record["source"] != "manual"
                || !record["task_id"].is_null()
                || db.as_ref().is_some_and(|db| db.trace.is_retained(&id))
            {
                protected_runs.push(id);
            } else {
                private_runs.push(id);
            }
        }
        let generating = doc.job.as_ref().is_some_and(|job| {
            self.jobs
                .lock()
                .contains_key(job["id"].as_str().unwrap_or(""))
        });
        let mut generated = BTreeSet::new();
        for version in doc.versions.iter().filter(|version| !version.published) {
            for (name, bytes) in &version.generated_templates {
                generated.insert((name, format!("{:x}", Sha256::digest(bytes))));
            }
        }
        Ok(json!({"script_id":doc.script_id,"head_version":doc.head(),
            "version_count":doc.versions.iter().filter(|version| !version.published).count(),
            "generated_template_count":generated.len(),
            "private_bytes":std::fs::metadata(self.authoring_path(package,&doc.script_id)?)?.len(),
            "private_run_count":private_runs.len(),"private_runs":private_runs,
            "protected_runs":protected_runs,"active_private_run_count":active_count,
            "unverified_runs":unverified_runs,
            "busy":generating || active_count>0,"preserved":PRESERVED}))
    }

    pub(super) async fn authoring_lifecycle(
        self: &Arc<Self>,
        action: &str,
        package: &str,
        script: &str,
        args: &Value,
        admission: &Option<Admission>,
    ) -> Result<Value> {
        let path = self.authoring_path(package, script)?;
        if action == "authoring.import" {
            ensure!(
                !path.exists(),
                "authoring.draft_exists: 已有工作稿，请打开或先删除"
            );
            let formal = self
                .packages
                .read_text(
                    package,
                    super::super::YAML_EXTENSION_ID,
                    &format!("automations/{script}"),
                )?
                .context("authoring.published_not_found: 正式脚本不存在")?;
            let public_path = self.authoring_published_path(package, script)?;
            let mut doc = if public_path.exists() {
                let doc: Document = serde_json::from_slice(&std::fs::read(public_path)?)?;
                ensure!(
                    doc.script_id == script && doc.versions.iter().all(|version| version.published),
                    "authoring.published_history_invalid"
                );
                doc
            } else {
                self.authoring_read(package, script)?
            };
            doc.id = uuid::Uuid::new_v4().to_string();
            doc.legacy_run_binding = false;
            doc.private_run_ids.clear();
            doc.job = None;
            let current_hash = format!("{:x}", Sha256::digest(formal.content.as_bytes()));
            // Resource publication history is authoritative even if an older
            // draft archive predates the most recent public transaction.
            let history = revisions::history(&self.packages, package)?;
            let prefix = format!("{script}@");
            let history_major = history["revisions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|revision| revision["kind"] == "published")
                .filter_map(|revision| revision["candidate_id"].as_str()?.strip_prefix(&prefix))
                .filter_map(|revision| revision.split_once('.')?.0.parse::<u32>().ok())
                .max()
                .unwrap_or(1);
            let archived_major = doc
                .published_version
                .as_deref()
                .map(number)
                .map(|version| version.0)
                .unwrap_or(0);
            let major = history_major.max(archived_major);
            doc.published_version = Some(revision(major, 0));
            if doc.versions.is_empty() {
                // Publication transactions are the durable numbering anchor;
                // imports themselves never publish or rewrite those snapshots.
                let version = revision(major, 0);
                doc.published_version = Some(version.clone());
                doc.versions.push(Version {
                    extra: Map::new(),
                    version,
                    yaml: formal.content.clone(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    instruction: String::new(),
                    source: "library".into(),
                    published: true,
                    diagnostics: json!([]),
                    usage: Value::Null,
                    material_refs: vec![],
                    template_refs: vec![],
                    frozen_functions: None,
                    generated_templates: BTreeMap::new(),
                    crop_checks: BTreeMap::new(),
                });
            } else if doc
                .versions
                .last()
                .is_some_and(|version| version.yaml != formal.content)
            {
                // A resource-editor change is an editable import; immutable
                // published snapshots in the archive retain their exact bytes.
                doc.versions.push(Version {
                    extra: Map::new(),
                    version: doc.minor(),
                    yaml: formal.content.clone(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    instruction: "导入当前正式脚本".into(),
                    source: "library".into(),
                    published: false,
                    diagnostics: json!([]),
                    usage: Value::Null,
                    material_refs: vec![],
                    template_refs: vec![],
                    frozen_functions: None,
                    generated_templates: BTreeMap::new(),
                    crop_checks: BTreeMap::new(),
                });
            }
            doc.published_resource_hash = Some(current_hash);
            admit(admission)?;
            self.authoring_write(package, &doc)?;
            return Ok(json!({"script":self.authoring_view(&doc)}));
        }
        ensure!(path.exists(), "authoring.draft_not_found");
        let mut doc = self.authoring_read(package, script)?;
        ensure!(doc.script_id == script, "authoring.draft_identity_invalid");
        if doc.id.is_empty() {
            doc.id = uuid::Uuid::new_v4().to_string();
            admit(admission)?;
            self.authoring_write(package, &doc)?;
        }
        let preview = self.authoring_deletion_preview(package, &doc).await?;
        if action == "authoring.delete_preview" {
            return Ok(preview);
        }
        ensure!(
            args["expected_version"].as_str() == Some(doc.head()),
            "script_version_conflict: 请刷新版本后重试"
        );
        ensure!(
            preview["busy"] == false,
            "authoring.script_busy: 生成或私有试跑尚未结束"
        );
        let remove_runs = args
            .get("remove_private_runs")
            .map(|value| {
                value
                    .as_bool()
                    .context("remove_private_runs must be boolean")
            })
            .transpose()?
            .unwrap_or(true);
        self.authoring_archive_public(package, &doc)?;
        let mut removed_runs = 0;
        let mut removed_traces = 0;
        if remove_runs {
            let ids: Vec<String> = serde_json::from_value(preview["private_runs"].clone())?;
            if !ids.is_empty() {
                let db = self
                    .diagnostics
                    .lock()
                    .clone()
                    .context("run journal unavailable")?;
                // Preserve the approved identities until every cleanup succeeds;
                // an IO failure must not lose ownership needed for a retry.
                doc.private_run_ids.extend(ids.iter().cloned());
                doc.private_run_ids.sort();
                doc.private_run_ids.dedup();
                admit(admission)?;
                self.authoring_write(package, &doc)?;
                for id in &ids {
                    removed_traces += usize::from(db.trace.delete_completed_temporary(id)?);
                }
                removed_runs = self._runs.delete_terminal_records(&db, &ids)?;
            }
        }
        // This single hashed JSON file owns all private revisions and embedded
        // generated images. Shared sample references disappear with the file;
        // their source ZIPs, recordings and explicit retained copies stay put.
        admit(admission)?;
        std::fs::remove_file(path)?;
        Ok(
            json!({"deleted":true,"script_id":script,"removed_private_runs":removed_runs,
            "removed_temporary_traces":removed_traces,"preserved":PRESERVED}),
        )
    }
}
