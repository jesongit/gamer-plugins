use super::*;
use crate::core::fs::atomic_write;
use crate::resources::ResourceHandler;

impl MemoryStore {
    /// Exact references used by the persistent import queue. Linking these to
    /// user definitions makes deletion suppression cover their derived jobs.
    pub fn source_references(
        &self,
        pkg: &str,
        source_id: &str,
        revision: u64,
    ) -> Result<Vec<Value>> {
        let _guard = self.gate.lock();
        self.packages.with_package_read(pkg,||{
            let(source,_)=self.read_source(pkg,source_id,Some(revision))?;
            ensure!(!source.deleted,"memory.source_deleted");
            Ok(index::chunks(&source_memory(&source),24*1024).into_iter().map(|chunk|json!({"id":source.id,"revision":source.revision,"section":chunk.section,"excerpt":chunk.text})).collect())
        })
    }
    fn job_dir(&self, pkg: &str) -> PathBuf {
        self.root.join("private/import-jobs").join(pkg)
    }
    fn read_job(&self, pkg: &str, job_id: &str) -> Result<ImportJob> {
        validate_scope_id("import job id", job_id)?;
        let text = std::fs::read_to_string(self.job_dir(pkg).join(format!("{job_id}.json")))
            .context("memory.import_job_not_found")?;
        let job: ImportJob = serde_json::from_str(&text)?;
        ensure!(job.package == pkg, "memory.import_job_scope_mismatch");
        Ok(job)
    }
    fn save_job(&self, job: &ImportJob) -> Result<()> {
        std::fs::create_dir_all(self.job_dir(&job.package))?;
        atomic_write(
            &self.job_dir(&job.package).join(format!("{}.json", job.id)),
            &serde_json::to_vec_pretty(job)?,
        )
    }
    fn make_job(&self, pkg: &str, source: &Source) -> Result<ImportJob> {
        let job_id = format!("{}-r{}", source.id, source.revision);
        if let Ok(job) = self.read_job(pkg, &job_id) {
            return Ok(job);
        }
        let pseudo = source_memory(source);
        let chunks = index::chunks(&pseudo, 24 * 1024)
            .into_iter()
            .map(|c| ImportChunk {
                id: c.id,
                section: c.section,
                text: c.text,
                state: "pending".into(),
                claim_id: None,
                claimed_at: None,
                outcome: None,
            })
            .collect::<Vec<_>>();
        let timestamp = now();
        let job = ImportJob {
            id: job_id,
            package: pkg.into(),
            source_id: source.id.clone(),
            source_revision: source.revision,
            title: source.title.clone(),
            status: "pending".into(),
            total: chunks.len(),
            processed: 0,
            counts: [
                "created", "updated", "merged", "retained", "skipped", "failed",
            ]
            .iter()
            .map(|s| ((*s).into(), 0))
            .collect(),
            created_at: timestamp.clone(),
            updated_at: timestamp,
            error: None,
            chunks,
            limits: super::super::Limits::default(),
            usage: super::super::Usage {
                total_tokens: Some(0),
                ..Default::default()
            },
            open_requests: BTreeSet::new(),
            usage_receipts: BTreeMap::new(),
            unknown_usage: false,
        };
        self.save_job(&job)?;
        Ok(job)
    }
    /// Queue discovery also covers source files installed by the archive seam.
    /// Jobs stay local: an imported package does not replay exported job progress.
    fn discover_jobs(&self, pkg: &str) -> Result<()> {
        self.packages.with_package_read(pkg, || {
            for e in self.packages.list(pkg, PLUGIN, "memory-sources")? {
                if let Some(text) = e.content {
                    if let Ok(source) = serde_json::from_str::<Source>(&text) {
                        if source.format_version == FORMAT && !source.deleted {
                            let source = self.read_source(pkg, &source.id, None)?.0;
                            self.make_job(pkg, &source)?;
                        }
                    }
                }
            }
            Ok(())
        })
    }
    pub fn pending_imports(&self, pkg: &str) -> Result<Vec<ImportJob>> {
        let _guard = self.gate.lock();
        self.discover_jobs(pkg)?;
        let mut jobs = self.all_jobs(pkg)?;
        jobs.retain(|j| matches!(j.status.as_str(), "pending" | "running"));
        Ok(jobs)
    }
    fn all_jobs(&self, pkg: &str) -> Result<Vec<ImportJob>> {
        let dir = self.job_dir(pkg);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut jobs = Vec::new();
        for e in std::fs::read_dir(dir)? {
            let e = e?;
            if e.path().extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let job: ImportJob = serde_json::from_slice(&std::fs::read(e.path())?)?;
            if job.package == pkg {
                jobs.push(job)
            }
        }
        jobs.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(jobs)
    }
    pub(super) fn jobs(&self, pkg: &str, args: &Value) -> Result<Value> {
        let _guard = self.gate.lock();
        self.discover_jobs(pkg)?;
        let jobs = self.all_jobs(pkg)?;
        let total = jobs.len();
        let items: Vec<_> = jobs
            .into_iter()
            .rev()
            .take(bounded_limit(args, 30, 100))
            .map(|j| job_summary(&j))
            .collect();
        Ok(json!({"items":items,"total":total}))
    }
    /// Claim is persisted, expiring after 10 minutes so a process crash can resume.
    /// Retry with the same claim identity returns the same uncompleted chunk.
    pub fn claim_import_chunk(
        &self,
        pkg: &str,
        job_id: &str,
        claim_id: &str,
    ) -> Result<Option<Value>> {
        ensure!(
            !claim_id.is_empty() && claim_id.len() <= 160,
            "memory.claim_id_invalid"
        );
        let _guard = self.gate.lock();
        self.packages.with_package_read(pkg,||{
            let mut job=self.read_job(pkg,job_id)?;
            if !matches!(job.status.as_str(),"pending"|"running"){return Ok(None)}
            let source=self.read_source(pkg,&job.source_id,Some(job.source_revision))?.0;
            let current=self.read_source(pkg,&job.source_id,None)?.0;
            if current.deleted||current.revision!=job.source_revision {job.status="cancelled".into();job.error=Some("source_changed_or_deleted".into());self.save_job(&job)?;return Ok(None)}
            let now_ms=Utc::now().timestamp_millis();
            let selected=job.chunks.iter().position(|c|c.state=="claimed"&&c.claim_id.as_deref()==Some(claim_id))
                .or_else(||job.chunks.iter().position(|c|c.state=="pending"||(c.state=="claimed"&&c.claimed_at.unwrap_or(0)+600_000<now_ms)));
            let Some(i)=selected else{return Ok(None)};
            let c=&mut job.chunks[i];c.state="claimed".into();c.claim_id=Some(claim_id.into());c.claimed_at=Some(now_ms);
            let chunk=c.clone();job.status="running".into();job.updated_at=now();self.save_job(&job)?;
            Ok(Some(json!({"job_id":job.id,"source_id":source.id,"source_revision":source.revision,"source_title":source.title,
                "game_version":source.game_version,"source_url":source.source_url,"chunk":chunk,"claim_id":claim_id,
                "source_reference":{"id":source.id,"revision":source.revision,"section":chunk.section,"excerpt":chunk.text}})))
        })
    }
    /// Call only after the target memory commit returns a durable receipt.
    /// An outcome records the target operation, so retry never repeats the merge.
    pub fn complete_import_chunk(
        &self,
        pkg: &str,
        job_id: &str,
        chunk_id: &str,
        claim_id: &str,
        outcome: Value,
    ) -> Result<Value> {
        let _guard = self.gate.lock();
        self.packages.with_package_read(pkg, || {
            let mut job = self.read_job(pkg, job_id)?;
            let chunk = job
                .chunks
                .iter_mut()
                .find(|c| c.id == chunk_id)
                .context("memory.import_chunk_not_found")?;
            if chunk.state == "completed" {
                ensure!(
                    chunk.outcome.as_ref() == Some(&outcome),
                    "memory.import_outcome_conflict"
                );
                return Ok(job_summary(&job));
            }
            // Do not turn a withdrawn/paused guide into a completed operation.
            ensure!(
                self.import_job_active_unlocked(pkg, job_id)?,
                "memory.import_not_active"
            );
            ensure!(
                chunk.state == "claimed" && chunk.claim_id.as_deref() == Some(claim_id),
                "memory.import_claim_conflict"
            );
            let disposition = self.validate_import_outcome_unlocked(pkg, &outcome)?;
            chunk.state = "completed".into();
            chunk.outcome = Some(outcome);
            chunk.claim_id = None;
            chunk.claimed_at = None;
            job.processed += 1;
            *job.counts.entry(disposition.to_owned()).or_default() += 1;
            if job.processed == job.total && job.status != "cancelled" {
                job.status = "completed".into();
            }
            job.updated_at = now();
            self.save_job(&job)?;
            Ok(job_summary(&job))
        })
    }
    /// Preflight gives the model a normal tool error so it can correct a bad
    /// target. Final completion repeats the same checks under its commit gate.
    pub(in super::super) fn validate_import_outcome(
        &self,
        pkg: &str,
        job_id: &str,
        outcome: &Value,
    ) -> Result<()> {
        let _guard = self.gate.lock();
        self.packages.with_package_read(pkg, || {
            ensure!(
                self.import_job_active_unlocked(pkg, job_id)?,
                "memory.import_not_active"
            );
            self.validate_import_outcome_unlocked(pkg, outcome)
                .map(|_| ())
        })
    }
    fn validate_import_outcome_unlocked(&self, pkg: &str, outcome: &Value) -> Result<String> {
        let disposition = required(outcome, "disposition")?.to_owned();
        ensure!(
            ["created", "updated", "merged", "retained", "skipped", "failed"]
                .contains(&disposition.as_str()),
            "memory.import_disposition_invalid"
        );
        if matches!(disposition.as_str(), "created" | "updated" | "merged") {
            let target_op = required(outcome, "operation_id")?;
            let entry = self
                .packages
                .read_text(
                    pkg,
                    PLUGIN,
                    &format!("memory-operations/{}.json", hash(target_op)),
                )?
                .context("memory.import_target_not_committed")?;
            let receipt: Value = serde_json::from_str(&entry.content)?;
            ensure!(
                receipt["result"]["saved"] == true,
                "memory.import_target_not_committed"
            );
            self.ensure_import_guide_target(pkg, required(&receipt["result"], "id")?)?;
            if let Some(target) = outcome.get("id").and_then(Value::as_str) {
                ensure!(
                    receipt["result"]["id"] == target,
                    "memory.import_target_mismatch"
                );
            }
        } else if disposition == "retained" {
            self.ensure_import_guide_target(pkg, required(outcome, "id")?)?;
        }
        Ok(disposition)
    }
    pub fn fail_import_chunk(
        &self,
        pkg: &str,
        job_id: &str,
        chunk_id: &str,
        claim_id: &str,
        error: &str,
    ) -> Result<Value> {
        let _guard = self.gate.lock();
        let mut job = self.read_job(pkg, job_id)?;
        let c = job
            .chunks
            .iter_mut()
            .find(|c| c.id == chunk_id)
            .context("memory.import_chunk_not_found")?;
        ensure!(
            c.claim_id.as_deref() == Some(claim_id),
            "memory.import_claim_conflict"
        );
        c.state = "pending".into();
        c.claim_id = None;
        c.claimed_at = None;
        if !matches!(job.status.as_str(), "paused" | "cancelled") {
            job.status = "failed".into();
            job.error = Some(error.chars().take(1000).collect());
        }
        job.updated_at = now();
        self.save_job(&job)?;
        Ok(job_summary(&job))
    }
    pub fn release_import_chunk(
        &self,
        pkg: &str,
        job_id: &str,
        chunk_id: &str,
        claim_id: &str,
        _reason: &str,
    ) -> Result<Value> {
        let _guard = self.gate.lock();
        self.packages.with_package_read(pkg, || {
            let mut job = self.read_job(pkg, job_id)?;
            let c = job
                .chunks
                .iter_mut()
                .find(|c| c.id == chunk_id)
                .context("memory.import_chunk_not_found")?;
            if c.state == "completed" {
                return Ok(job_summary(&job));
            }
            ensure!(
                c.claim_id.as_deref() == Some(claim_id),
                "memory.import_claim_conflict"
            );
            c.state = "pending".into();
            c.claim_id = None;
            c.claimed_at = None;
            if job.status == "running" {
                job.status = "pending".into();
            }
            job.updated_at = now();
            self.save_job(&job)?;
            Ok(job_summary(&job))
        })
    }
    pub(super) fn import(&self, pkg: &str, args: &Value, op: &str, fp: &str) -> Result<Value> {
        let limits = args
            .get("limits")
            .map(|v| serde_json::from_value::<super::super::Limits>(v.clone()))
            .transpose()?;
        if let Some(limits) = &limits {
            limits.validate()?;
        }
        let filename = required(args, "filename")?;
        ensure!(
            filename.len() <= 200
                && !filename.contains(['/', '\\'])
                && !filename.chars().any(char::is_control),
            "memory.import_filename_invalid"
        );
        let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
        ensure!(
            ["md", "txt"].contains(&ext.as_str()),
            "memory.import_format_md_or_txt_required"
        );
        let text = required(args, "text")?;
        ensure!(text.len() <= MAX_TEXT, "memory.import_too_large_1mib");
        let game_version = args
            .get("game_version")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        ensure!(
            game_version.len() <= 120 && !game_version.is_empty(),
            "memory.game_version_invalid"
        );
        let source_id = format!("source-{}", &hash(&format!("{game_version}\0{text}"))[..24]);
        if let Ok((source, _)) = self.read_source(pkg, &source_id, None) {
            ensure!(
                !source.deleted,
                "memory.source_deleted_user_restore_required"
            );
            let job = self.make_job(pkg, &source)?;
            let result = json!({"source_id":source.id,"source_revision":source.revision,"job_id":job.id,"status":job.status,"chunks":job.total,"duplicate":true});
            self.save_receipt(pkg, op, fp, &result)?;
            return Ok(result);
        }
        let timestamp = now();
        let source = Source {
            format_version: FORMAT,
            id: source_id,
            title: args
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or(filename)
                .into(),
            filename: filename.into(),
            format: if ext == "md" { "markdown" } else { "text" }.into(),
            text: text.into(),
            game_version: game_version.into(),
            source_url: args
                .get("source_url")
                .and_then(Value::as_str)
                .map(str::to_owned),
            revision: 1,
            created_at: timestamp.clone(),
            updated_at: timestamp,
            deleted: false,
            content_hash: hash(text),
            applied_operations: BTreeMap::new(),
        };
        self.write_source_raw(pkg, &source)?;
        self.write_json_new(
            pkg,
            &format!("memory-source-revisions/{}/1.json", source.id),
            &source,
        )?;
        self.write_json_new(pkg, &format!("memory-sources/{}.json", source.id), &source)?;
        let mut job = self.make_job(pkg, &source)?;
        if let Some(limits) = limits {
            job.limits = limits;
            self.save_job(&job)?;
        }
        let result = json!({"source_id":source.id,"source_revision":1,"job_id":job.id,"status":job.status,"chunks":job.total,"duplicate":false});
        self.save_receipt(pkg, op, fp, &result)?;
        Ok(result)
    }
    fn read_source(
        &self,
        pkg: &str,
        source_id: &str,
        rev: Option<u64>,
    ) -> Result<(Source, ResourceEntry)> {
        validate_scope_id("source id", source_id)?;
        if let Some(revision) = rev {
            let (current, _) = self.read_source(pkg, source_id, None)?;
            ensure!(
                revision == 1
                    || revision == current.revision
                    || current
                        .applied_operations
                        .values()
                        .any(|a| a.revision == revision),
                "memory.source_revision_not_committed"
            );
        }
        let p = match rev {
            Some(r) => format!("memory-source-revisions/{source_id}/{r}.json"),
            None => format!("memory-sources/{source_id}.json"),
        };
        let e = self
            .packages
            .read_text(pkg, PLUGIN, &p)?
            .context("memory.source_not_found")?;
        let mut s: Source = serde_json::from_str(&e.content)?;
        if s.text.is_empty() {
            s.text = self
                .packages
                .read_text(pkg, PLUGIN, &raw_path(&s))?
                .context("memory.source_raw_not_found")?
                .content;
        }
        ensure!(
            s.format_version == FORMAT
                && s.id == source_id
                && s.text.len() <= MAX_TEXT
                && s.content_hash == hash(&s.text),
            "memory.invalid_source"
        );
        Ok((s, e))
    }
    fn write_source_raw(&self, pkg: &str, source: &Source) -> Result<()> {
        let resource = raw_path(source);
        if let Some(old) = self.packages.read_text(pkg, PLUGIN, &resource)? {
            ensure!(
                old.content == source.text,
                "memory.source_revision_immutable"
            );
        } else {
            self.packages
                .write_text(pkg, PLUGIN, &resource, &source.text, None, false)?;
        }
        Ok(())
    }
    pub(super) fn source_get(&self, pkg: &str, args: &Value) -> Result<Value> {
        self.packages.with_package_read(pkg,||{
            let(s,e)=self.read_source(pkg,id(args)?,args.get("revision").and_then(Value::as_u64))?;let(current,_)=self.read_source(pkg,&s.id,None)?;
            let mut source=serde_json::to_value(&s)?;source["text"]=json!(s.text);
            if let Some(origin)=self.packages.read_text(pkg,PLUGIN,&format!("memory-source-origins/{}.json",s.id))?{source["import_provenance"]=serde_json::from_str(&origin.content)?;}
            Ok(json!({"source":source,"version":e.version(),"current_revision":current.revision,"current_deleted":current.deleted,"changed_since_reference":s.revision!=current.revision}))
        })
    }
    pub(super) fn source_mutate(
        &self,
        pkg: &str,
        args: &Value,
        name: &str,
        op: &str,
        fp: &str,
        delegated: bool,
    ) -> Result<Value> {
        ensure!(delegated, "memory.user_delegation_required");
        let (s, e) = self.read_source(pkg, id(args)?, None)?;
        ensure!(
            required(args, "expected_version")? == e.version(),
            "version_conflict"
        );
        let mut source = s;
        source.revision += 1;
        source.updated_at = now();
        source.applied_operations.insert(
            op.into(),
            Applied {
                input: fp.into(),
                revision: source.revision,
            },
        );
        if name == "memory_source_delete" {
            source.deleted = true;
        } else {
            ensure!(!source.deleted, "memory.source_deleted");
            let text = required(args, "text")?;
            ensure!(text.len() <= MAX_TEXT, "memory.import_too_large_1mib");
            source.text = text.into();
            source.content_hash = hash(text);
            if let Some(title) = args.get("title").and_then(Value::as_str) {
                ensure!(
                    !title.trim().is_empty() && title.len() <= 600,
                    "memory.title_invalid"
                );
                source.title = title.into();
            }
        }
        self.write_source_raw(pkg, &source)?;
        self.write_json_new(
            pkg,
            &format!(
                "memory-source-revisions/{}/{}.json",
                source.id, source.revision
            ),
            &source,
        )?;
        let entry = self.packages.write_text(
            pkg,
            PLUGIN,
            &format!("memory-sources/{}.json", source.id),
            &serde_json::to_string_pretty(&source)?,
            Some(&e.version()),
            false,
        )?;
        for mut job in self.all_jobs(pkg)? {
            if job.source_id == source.id
                && job.source_revision < source.revision
                && job.status != "completed"
            {
                job.status = "cancelled".into();
                job.error = Some("source_changed_or_deleted".into());
                self.save_job(&job)?;
            }
        }
        let mut affected = Vec::new();
        for (mut m, version) in self.snapshot(pkg)?.0 {
            if !m
                .sources
                .iter()
                .any(|v| v["id"] == source.id || v["source_id"] == source.id)
            {
                continue;
            }
            affected.push(m.id.clone());
            // The origin is changed; protected memory remains visible as a conflict,
            // rather than silently changing a user-owned status.
            if m.protected_fields.contains("validation")
                || m.protected_fields.contains("body")
                || !self.source_review_required(pkg, &m)?
            {
                continue;
            }
            if m.status == "active" {
                m.validation = "pending".into();
                m.revision += 1;
                m.updated_at = now();
                m.reason = "source changed; revalidation required".into();
                m.actor = "source_update".into();
                let child_op = format!("{op}:review:{}", m.id);
                let child_fp = hash(&format!("{fp}:{}", m.id));
                self.commit(pkg, m, Some(&version), &child_op, &child_fp)?;
            }
        }
        let job = if source.deleted {
            None
        } else {
            Some(self.make_job(pkg, &source)?)
        };
        let result = json!({"saved":true,"source_id":source.id,"source_revision":source.revision,"version":entry.version(),"deleted":source.deleted,"affected_memories":affected,"job":job.as_ref().map(job_summary)});
        self.save_receipt(pkg, op, fp, &result)?;
        Ok(result)
    }
    pub(super) fn job_control(
        &self,
        pkg: &str,
        args: &Value,
        name: &str,
        op: &str,
        fp: &str,
    ) -> Result<Value> {
        let job_id = required(args, "job_id")?;
        let mut job = self.read_job(pkg, job_id)?;
        ensure!(job.status != "completed", "memory.import_already_completed");
        match name {
            "memory_import_pause" => job.status = "paused".into(),
            "memory_import_cancel" => job.status = "cancelled".into(),
            _ => {
                ensure!(job.status != "cancelled", "memory.import_cancelled");
                if let Some(limits) = args.get("limits") {
                    let limits: super::super::Limits = serde_json::from_value(limits.clone())?;
                    limits.validate()?;
                    job.limits = limits;
                }
                job.status = "pending".into();
                job.error = None;
            }
        }
        job.updated_at = now();
        self.save_job(&job)?;
        let result = job_summary(&job);
        self.save_receipt(pkg, op, fp, &result)?;
        Ok(result)
    }
    pub fn import_job_active(&self, pkg: &str, job_id: &str) -> Result<bool> {
        let _guard = self.gate.lock();
        self.packages
            .with_package_read(pkg, || self.import_job_active_unlocked(pkg, job_id))
    }
    pub(super) fn import_job_active_unlocked(&self, pkg: &str, job_id: &str) -> Result<bool> {
        let job = self.read_job(pkg, job_id)?;
        if !matches!(job.status.as_str(), "pending" | "running") {
            return Ok(false);
        }
        let source = self.read_source(pkg, &job.source_id, None)?.0;
        Ok(!source.deleted && source.revision == job.source_revision)
    }
    pub fn import_job_record(&self, pkg: &str, job_id: &str) -> Result<ImportJob> {
        let _guard = self.gate.lock();
        self.packages
            .with_package_read(pkg, || self.read_job(pkg, job_id))
    }
    pub(in super::super) fn import_source_text(&self, pkg: &str, job_id: &str) -> Result<String> {
        let _guard = self.gate.lock();
        self.packages.with_package_read(pkg, || {
            let job = self.read_job(pkg, job_id)?;
            Ok(self
                .read_source(pkg, &job.source_id, Some(job.source_revision))?
                .0
                .text)
        })
    }
    pub(super) fn source_conflicts(&self, pkg: &str, m: &Memory) -> Result<Vec<Value>> {
        let mut conflicts = Vec::new();
        for reference in &m.sources {
            let Some(source_id) = reference
                .get("source_id")
                .or_else(|| reference.get("id"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            if validate_scope_id("source id", source_id).is_err() {
                continue;
            }
            if self
                .packages
                .read_text(pkg, PLUGIN, &format!("memory-sources/{source_id}.json"))?
                .is_none()
            {
                continue;
            }
            let (source, _) = self.read_source(pkg, source_id, None)?;
            let cited = reference.get("revision").and_then(Value::as_u64);
            if source.deleted || cited.is_some_and(|r| r != source.revision) {
                conflicts.push(json!({"source_id":source_id,"cited_revision":cited,"current_revision":source.revision,"deleted":source.deleted,"reason":"source_changed_requires_review"}));
            }
        }
        Ok(conflicts)
    }
    pub(super) fn source_review_required(&self, pkg: &str, m: &Memory) -> Result<bool> {
        if self.source_conflicts(pkg, m)?.is_empty() {
            return Ok(false);
        }
        for reference in &m.sources {
            let source_id = reference
                .get("source_id")
                .or_else(|| reference.get("id"))
                .and_then(Value::as_str);
            if let Some(source_id) = source_id {
                if validate_scope_id("source id", source_id).is_ok() {
                    if let Ok((source, _)) = self.read_source(pkg, source_id, None) {
                        if !source.deleted
                            && reference.get("revision").and_then(Value::as_u64)
                                == Some(source.revision)
                        {
                            return Ok(false);
                        }
                        continue;
                    }
                }
            }
            // A separately cited URL/excerpt can still support the record. It
            // is not silently replaced by the changed local guide source.
            if reference
                .get("url")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
                && reference
                    .get("excerpt")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub fn record_import_request(
        &self,
        pkg: &str,
        job_id: &str,
        request_id: &str,
    ) -> Result<Value> {
        ensure!(
            !request_id.is_empty() && request_id.len() <= 300,
            "memory.request_id_invalid"
        );
        let _guard = self.gate.lock();
        self.packages.with_package_read(pkg, || {
            let mut job = self.read_job(pkg, job_id)?;
            if job.open_requests.contains(request_id) || job.usage_receipts.contains_key(request_id)
            {
                return Ok(job_summary(&job));
            }
            ensure!(
                matches!(job.status.as_str(), "pending" | "running"),
                "memory.import_not_active"
            );
            if let Some(reason) = import_budget(&job) {
                job.status = "paused".into();
                job.error = Some(reason.clone());
                self.save_job(&job)?;
                bail!("memory.import_budget: {reason}")
            }
            job.usage.turns = job.usage.turns.saturating_add(1);
            job.open_requests.insert(request_id.into());
            job.usage.total_tokens = None;
            job.usage.has_unknown_tokens = true;
            job.updated_at = now();
            self.save_job(&job)?;
            Ok(job_summary(&job))
        })
    }
    pub fn record_import_usage(
        &self,
        pkg: &str,
        job_id: &str,
        request_id: &str,
        tokens: Option<u64>,
        seconds: f64,
        failed: bool,
    ) -> Result<Value> {
        ensure!(
            seconds.is_finite() && seconds >= 0.0,
            "memory.usage_seconds_invalid"
        );
        let fp = hash(&json!({"tokens":tokens,"seconds":seconds,"failed":failed}).to_string());
        let _guard = self.gate.lock();
        self.packages.with_package_read(pkg, || {
            let mut job = self.read_job(pkg, job_id)?;
            if let Some(prior) = job.usage_receipts.get(request_id) {
                ensure!(prior == &fp, "memory.usage_receipt_conflict");
                return Ok(job_summary(&job));
            }
            ensure!(
                job.open_requests.remove(request_id),
                "memory.usage_request_not_recorded"
            );
            if let Some(tokens) = tokens {
                job.usage.known_tokens = job.usage.known_tokens.saturating_add(tokens);
            } else {
                job.unknown_usage = true;
            }
            job.usage.active_seconds += seconds;
            job.usage.consecutive_failures = if failed {
                job.usage.consecutive_failures.saturating_add(1)
            } else {
                0
            };
            job.usage.has_unknown_tokens = job.unknown_usage || !job.open_requests.is_empty();
            job.usage.total_tokens =
                (!job.usage.has_unknown_tokens).then_some(job.usage.known_tokens);
            job.usage_receipts.insert(request_id.into(), fp);
            job.updated_at = now();
            self.save_job(&job)?;
            Ok(job_summary(&job))
        })
    }
    pub fn record_import_action(
        &self,
        pkg: &str,
        job_id: &str,
        operation_id: &str,
    ) -> Result<Value> {
        ensure!(
            !operation_id.is_empty() && operation_id.len() <= 300,
            "memory.action_id_invalid"
        );
        let key = format!("action:{operation_id}");
        let _guard = self.gate.lock();
        self.packages.with_package_read(pkg, || {
            let mut job = self.read_job(pkg, job_id)?;
            if job.usage_receipts.contains_key(&key) {
                return Ok(job_summary(&job));
            }
            ensure!(
                self.import_job_active_unlocked(pkg, job_id)?,
                "memory.import_not_active"
            );
            if job.limits.max_actions > 0 && job.usage.actions >= job.limits.max_actions {
                job.status = "paused".into();
                job.error = Some("budget_actions: 工具调用次数达到预算".into());
                self.save_job(&job)?;
                bail!("memory.import_budget_actions")
            }
            job.usage.actions = job.usage.actions.saturating_add(1);
            job.usage_receipts.insert(key, "counted".into());
            job.updated_at = now();
            self.save_job(&job)?;
            Ok(job_summary(&job))
        })
    }
    pub fn record_import_elapsed(
        &self,
        pkg: &str,
        job_id: &str,
        activity_id: &str,
        seconds: f64,
    ) -> Result<Value> {
        ensure!(
            !activity_id.is_empty()
                && activity_id.len() <= 300
                && seconds.is_finite()
                && seconds >= 0.0,
            "memory.elapsed_identity_invalid"
        );
        let key = format!("elapsed:{activity_id}");
        let fp = hash(&seconds.to_string());
        let _guard = self.gate.lock();
        self.packages.with_package_read(pkg, || {
            let mut job = self.read_job(pkg, job_id)?;
            if let Some(prior) = job.usage_receipts.get(&key) {
                ensure!(prior == &fp, "memory.elapsed_receipt_conflict");
                return Ok(job_summary(&job));
            }
            job.usage.active_seconds += seconds;
            job.usage_receipts.insert(key, fp);
            job.updated_at = now();
            self.save_job(&job)?;
            Ok(job_summary(&job))
        })
    }
}
fn job_summary(j: &ImportJob) -> Value {
    json!({"id":j.id,"source_id":j.source_id,"source_revision":j.source_revision,"title":j.title,"status":j.status,"total":j.total,"processed":j.processed,"counts":j.counts,"created_at":j.created_at,"updated_at":j.updated_at,"error":j.error,"limits":j.limits,"usage":j.usage})
}
fn import_budget(j: &ImportJob) -> Option<String> {
    let l = &j.limits;
    let u = &j.usage;
    if l.max_turns > 0 && u.turns >= l.max_turns {
        Some("budget_turns: 模型轮数达到预算".into())
    } else if l.max_actions > 0 && u.actions >= l.max_actions {
        Some("budget_actions: 工具调用次数达到预算".into())
    } else if l.max_seconds > 0 && u.active_seconds >= l.max_seconds as f64 {
        Some("budget_seconds: 执行时间达到预算".into())
    } else if l.max_tokens > 0 && u.known_tokens >= l.max_tokens {
        Some("budget_tokens: 已知token达到预算；未知用量不计为零".into())
    } else if l.max_tokens > 0 && u.has_unknown_tokens {
        Some(
            "budget_tokens_unknown: 供应商未返回完整token用量，有限预算无法继续计量；设0无限可继续"
                .into(),
        )
    } else if l.max_failures > 0 && u.consecutive_failures >= l.max_failures {
        Some("budget_failures: 连续失败达到预算".into())
    } else {
        None
    }
}
pub(super) fn source_memory(s: &Source) -> Memory {
    Memory {
        format_version: FORMAT,
        id: s.id.clone(),
        title: s.title.clone(),
        body: s.text.clone(),
        kind: "procedure".into(),
        tags: Vec::new(),
        applicability: String::new(),
        game_version: s.game_version.clone(),
        validation: "pending".into(),
        status: "active".into(),
        revision: s.revision,
        protected_fields: BTreeSet::new(),
        sources: Vec::new(),
        created_at: s.created_at.clone(),
        updated_at: s.updated_at.clone(),
        reason: "guide import".into(),
        actor: "import".into(),
        operation_id: String::new(),
        operation_fingerprint: String::new(),
        applied_operations: BTreeMap::new(),
    }
}

impl ResourceHandler for MemoryStore {
    fn after_package_delete(&self, package: &str) -> Result<()> {
        self.cleanup_deleted_package(package)
    }
    fn prepare_package_replace(
        &self,
        package: &str,
        current: Option<&Path>,
        incoming: &Path,
    ) -> Result<()> {
        validate_scope_id("package id", package)?;
        std::fs::create_dir_all(incoming)?;
        let source_remap = preserve_colliding_archive_sources(current, incoming)?;
        let mut imported_markers = Vec::new();
        let marker_dir = incoming.join("memory-tombstones");
        if marker_dir.exists() {
            for entry in std::fs::read_dir(&marker_dir)? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let bytes = std::fs::read(entry.path())?;
                if let Ok(mut marker) = serde_json::from_slice::<Tombstone>(&bytes) {
                    if marker.format_version == FORMAT {
                        extend_source_aliases(&mut marker, &source_remap.fingerprints);
                        // Incoming deletion intent suppresses recreation but cannot
                        // delete a coincidentally named local record.
                        marker.id=format!("import-marker-{}",hash(&json!({"title_hash":marker.title_hash,"body_hash":marker.body_hash,"source_fingerprints":marker.source_fingerprints,"permanent":marker.permanent}).to_string()));
                        imported_markers.push(marker);
                        continue;
                    }
                }
                let preserved = incoming
                    .join("memory-unrecognized")
                    .join(format!("{:x}.json", Sha256::digest(&bytes)));
                std::fs::create_dir_all(preserved.parent().unwrap())?;
                atomic_write(&preserved, &bytes)?;
            }
        }
        // Convert every incoming memory into a preserved guide source before
        // installing it. It cannot replace a local instruction before AI merging.
        let originals = incoming.join("memories");
        if originals.exists() {
            for e in std::fs::read_dir(&originals)? {
                let e = e?;
                if !e.file_type()?.is_file() {
                    continue;
                }
                let bytes = std::fs::read(e.path())?;
                let parsed = std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|text| serde_json::from_str::<Memory>(text).ok())
                    .filter(|m| validate_memory(m).is_ok() && bytes.len() <= MAX_TEXT);
                let Some(mut record) = parsed else {
                    let preserved = incoming
                        .join("memory-unrecognized")
                        .join(format!("{:x}.json", Sha256::digest(&bytes)));
                    std::fs::create_dir_all(preserved.parent().unwrap())?;
                    atomic_write(&preserved, &bytes)?;
                    continue;
                };
                let text = String::from_utf8(bytes).expect("validated UTF-8");
                remap_source_references(&mut record.sources, &source_remap.ids);
                let source_id = format!(
                    "archive-{}",
                    &hash(
                        &json!({"original":hash(&text),"mapped_sources":record.sources})
                            .to_string()
                    )[..24]
                );
                let timestamp = now();
                let guide=format!("# {}\n\n适用版本：{}\n适用条件：{}\n来源验证状态：{}（不代表本机已验证）\n原稿引用：{}\n\n{}",record.title,record.game_version,record.applicability,record.validation,serde_json::to_string(&record.sources)?,record.body);
                ensure!(guide.len() <= MAX_TEXT, "memory.archive_source_too_large");
                let source = Source {
                    format_version: FORMAT,
                    id: source_id.clone(),
                    title: record.title,
                    filename: format!("{}.json", record.id),
                    format: "markdown".into(),
                    text: guide.clone(),
                    game_version: record.game_version,
                    source_url: None,
                    revision: 1,
                    created_at: timestamp.clone(),
                    updated_at: timestamp,
                    deleted: false,
                    content_hash: hash(&guide),
                    applied_operations: BTreeMap::new(),
                };
                if matches!(record.status.as_str(), "disabled" | "deleted") {
                    let references:Vec<Value>=index::chunks(&source_memory(&source),24*1024).into_iter().map(|chunk|json!({"id":source.id,"revision":source.revision,"section":chunk.section,"excerpt":chunk.text})).collect();
                    let mut fingerprints = source_fingerprints(&record.sources);
                    fingerprints.extend(source_fingerprints(&references));
                    imported_markers.push(Tombstone {
                        format_version: FORMAT,
                        id: format!("import-inactive-{source_id}"),
                        title_hash: hash(&source.title.trim().to_lowercase()),
                        body_hash: hash(record.body.trim()),
                        permanent: false,
                        updated_at: now(),
                        operation_id: format!("archive-inactive:{source_id}"),
                        operation_fingerprint: hash(&text),
                        source_fingerprints: fingerprints,
                    });
                }
                let raw = incoming.join(raw_path(&source));
                std::fs::create_dir_all(raw.parent().unwrap())?;
                atomic_write(&raw, guide.as_bytes())?;
                let original = incoming
                    .join("memory-source-originals")
                    .join(format!("{source_id}.json"));
                std::fs::create_dir_all(original.parent().unwrap())?;
                atomic_write(&original, text.as_bytes())?;
                let provenance = incoming
                    .join("memory-source-origins")
                    .join(format!("{source_id}.json"));
                std::fs::create_dir_all(provenance.parent().unwrap())?;
                atomic_write(
                    &provenance,
                    &serde_json::to_vec_pretty(
                        &json!({"format_version":FORMAT,"original_kind":"memory","original_id":record.id,"mapped_id":source_id,"original_resource":format!("memory-source-originals/{source_id}.json"),"source_id_map":source_remap.ids,"mapped_sources":record.sources}),
                    )?,
                )?;
                let history = incoming.join("memory-revisions").join(&record.id);
                if history.exists() {
                    copy_tree(
                        &history,
                        &incoming
                            .join("memory-source-original-revisions")
                            .join(&source_id),
                    )?;
                }
                for resource in [
                    format!("memory-sources/{source_id}.json"),
                    format!("memory-source-revisions/{source_id}/1.json"),
                ] {
                    let p = incoming.join(resource);
                    std::fs::create_dir_all(p.parent().unwrap())?;
                    atomic_write(&p, &serde_json::to_vec_pretty(&source)?)?;
                }
            }
        }
        for dir in [
            "memories",
            "memory-revisions",
            "memory-tombstones",
            "memory-operations",
            "memory-operation-intents",
            "memory-dictionary.json",
        ] {
            let target = incoming.join(dir);
            if target.is_dir() {
                std::fs::remove_dir_all(&target)?
            } else if target.exists() {
                std::fs::remove_file(&target)?
            }
            if let Some(current) = current {
                let source = current.join(dir);
                if source.exists() {
                    copy_tree(&source, &target)?;
                }
            }
        }
        for mut marker in imported_markers {
            let target = incoming
                .join("memory-tombstones")
                .join(format!("{}.json", marker.id));
            std::fs::create_dir_all(target.parent().unwrap())?;
            if target.exists() {
                if let Ok(old) = serde_json::from_slice::<Tombstone>(&std::fs::read(&target)?) {
                    marker.source_fingerprints.extend(old.source_fingerprints);
                }
            }
            atomic_write(&target, &serde_json::to_vec_pretty(&marker)?)?;
        }
        // Keep local source revisions as well; incoming stable IDs are content-
        // derived. A same-path disagreement must preserve local authority.
        if let Some(current) = current {
            for dir in [
                "memory-sources",
                "memory-source-revisions",
                "memory-source-text",
                "memory-source-originals",
                "memory-source-original-revisions",
                "memory-source-origins",
                "memory-unrecognized",
            ] {
                let source = current.join(dir);
                if source.exists() {
                    copy_tree(&source, &incoming.join(dir))?;
                }
            }
        }
        // Local deletion intent also covers namespaced copies of the exact
        // original guide, so preserving a branch cannot resurrect a deletion.
        let tombstones = incoming.join("memory-tombstones");
        if tombstones.exists() {
            for entry in std::fs::read_dir(&tombstones)? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let mut marker: Tombstone = serde_json::from_slice(&std::fs::read(entry.path())?)?;
                if extend_source_aliases(&mut marker, &source_remap.fingerprints) {
                    atomic_write(&entry.path(), &serde_json::to_vec_pretty(&marker)?)?;
                }
            }
        }
        Ok(())
    }
}
impl MemoryStore {
    pub(super) fn purge_corrupt_cache(&self, package: &str) -> Result<()> {
        validate_scope_id("package id", package)?;
        let owned_root = self.root.join("cache/memory-index-corrupt");
        let target = owned_root.join(package);
        if target.exists() {
            safe_remove(&target, &owned_root)?;
        }
        Ok(())
    }
    pub fn cleanup_deleted_package(&self, package: &str) -> Result<()> {
        validate_scope_id("package id", package)?;
        let cache_root = self.root.join("cache/memory-index");
        for suffix in ["sqlite", "sqlite-wal", "sqlite-shm", "sqlite-journal"] {
            let target = cache_root.join(format!("{package}.{suffix}"));
            if target.exists() {
                safe_remove(&target, &cache_root)?;
            }
        }
        for owned_root in [
            self.root.join("private/import-jobs"),
            self.root.join("cache/memory-index-corrupt"),
        ] {
            let target = owned_root.join(package);
            if target.exists() {
                safe_remove(&target, &owned_root)?;
            }
        }
        Ok(())
    }
}
fn safe_remove(target: &Path, owned_root: &Path) -> Result<()> {
    let resolved = std::fs::canonicalize(target)?;
    let boundary = std::fs::canonicalize(owned_root)?;
    ensure!(
        resolved.starts_with(&boundary) && resolved != boundary,
        "memory.cache_cleanup_path_escape"
    );
    ensure!(
        !std::fs::symlink_metadata(target)?.file_type().is_symlink(),
        "memory.cache_cleanup_symlink"
    );
    if target.is_dir() {
        std::fs::remove_dir_all(target)?;
    } else {
        std::fs::remove_file(target)?;
    }
    Ok(())
}
fn raw_path(source: &Source) -> String {
    format!(
        "memory-source-text/{}/{}.{}",
        source.id,
        source.revision,
        if source.format == "markdown" {
            "md"
        } else {
            "txt"
        }
    )
}
#[derive(Default)]
struct ArchiveSourceRemap {
    ids: BTreeMap<String, String>,
    fingerprints: BTreeMap<String, BTreeSet<String>>,
}
fn remap_source_references(references: &mut [Value], ids: &BTreeMap<String, String>) {
    for reference in references {
        for key in ["id", "source_id"] {
            if let Some(mapped) = reference
                .get(key)
                .and_then(Value::as_str)
                .and_then(|id| ids.get(id))
                .cloned()
            {
                reference[key] = json!(mapped);
            }
        }
    }
}
fn extend_source_aliases(
    marker: &mut Tombstone,
    aliases: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    let extra: Vec<String> = marker
        .source_fingerprints
        .iter()
        .filter_map(|old| aliases.get(old))
        .flatten()
        .cloned()
        .collect();
    let before = marker.source_fingerprints.len();
    marker.source_fingerprints.extend(extra);
    before != marker.source_fingerprints.len()
}
fn read_staged_source(root: &Path, file: &Path, expected: &str) -> Result<Source> {
    let mut source: Source = serde_json::from_slice(&std::fs::read(file)?)?;
    validate_scope_id("source id", &source.id)?;
    ensure!(
        source.id == expected
            && source.format_version == FORMAT
            && source.revision > 0
            && matches!(source.format.as_str(), "markdown" | "text"),
        "memory.invalid_archive_source"
    );
    if source.text.is_empty() {
        source.text = std::fs::read_to_string(root.join(raw_path(&source)))?;
    }
    ensure!(
        source.text.len() <= MAX_TEXT && source.content_hash == hash(&source.text),
        "memory.invalid_archive_source_hash"
    );
    Ok(source)
}
fn tree_files(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    ensure!(
        !std::fs::symlink_metadata(root)?.file_type().is_symlink(),
        "memory.archive_symlink_rejected"
    );
    if root.is_file() {
        return Ok(vec![root.to_path_buf()]);
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root)? {
        files.extend(tree_files(&entry?.path())?);
    }
    files.sort();
    Ok(files)
}
fn preserve_colliding_archive_sources(
    current: Option<&Path>,
    incoming: &Path,
) -> Result<ArchiveSourceRemap> {
    let mut remap = ArchiveSourceRemap::default();
    let Some(current) = current else {
        return Ok(remap);
    };
    let directory = incoming.join("memory-sources");
    if !directory.exists() {
        return Ok(remap);
    }
    let files = tree_files(&directory)?;
    for file in files {
        if file.parent() != Some(directory.as_path())
            || file.extension().and_then(|s| s.to_str()) != Some("json")
        {
            continue;
        }
        let source_id = file
            .file_stem()
            .and_then(|s| s.to_str())
            .context("memory.archive_source_filename")?
            .to_string();
        validate_scope_id("source id", &source_id)?;
        let mut bundle = vec![file.clone()];
        for folder in ["memory-source-revisions", "memory-source-text"] {
            bundle.extend(tree_files(&incoming.join(folder).join(&source_id))?);
        }
        bundle.sort();
        let mut identity = Sha256::new();
        let mut conflict = false;
        for entry in &bundle {
            let relative = entry.strip_prefix(incoming)?;
            let bytes = std::fs::read(entry)?;
            identity.update(relative.to_string_lossy().as_bytes());
            identity.update([0]);
            identity.update(Sha256::digest(&bytes));
            let local = current.join(relative);
            if local.is_file() && std::fs::read(&local)? != bytes {
                conflict = true;
            }
        }
        if !conflict {
            continue;
        }
        let mapped = format!(
            "archive-source-{}",
            &format!("{:x}", identity.finalize())[..24]
        );
        remap.ids.insert(source_id.clone(), mapped.clone());
        let original = incoming
            .join("memory-source-originals")
            .join(format!("{mapped}.json"));
        copy_tree(&file, &original)?;
        let original_history = incoming.join("memory-source-revisions").join(&source_id);
        if original_history.exists() {
            copy_tree(
                &original_history,
                &incoming
                    .join("memory-source-original-revisions")
                    .join(&mapped),
            )?;
        }
        let raw_history = incoming.join("memory-source-text").join(&source_id);
        if raw_history.exists() {
            copy_tree(
                &raw_history,
                &incoming.join("memory-source-text").join(&mapped),
            )?;
        }
        let mut valid_revisions = BTreeMap::new();
        let source = read_staged_source(incoming, &file, &source_id);
        if let Ok(source) = &source {
            valid_revisions.insert(source.revision, source.clone());
        }
        if original_history.exists() {
            for entry in tree_files(&original_history)? {
                if let Ok(source) = read_staged_source(incoming, &entry, &source_id) {
                    if entry
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.parse::<u64>().ok())
                        == Some(source.revision)
                    {
                        valid_revisions.entry(source.revision).or_insert(source);
                    }
                }
            }
        }
        for (revision, original_source) in &valid_revisions {
            let mut translated = original_source.clone();
            translated.id = mapped.clone();
            translated.applied_operations = translated
                .applied_operations
                .into_iter()
                .map(|(op, applied)| (format!("archive:{mapped}:{op}"), applied))
                .collect();
            let raw = incoming.join(raw_path(&translated));
            std::fs::create_dir_all(raw.parent().unwrap())?;
            atomic_write(&raw, translated.text.as_bytes())?;
            let target = incoming
                .join("memory-source-revisions")
                .join(&mapped)
                .join(format!("{revision}.json"));
            std::fs::create_dir_all(target.parent().unwrap())?;
            atomic_write(&target, &serde_json::to_vec_pretty(&translated)?)?;
            if source
                .as_ref()
                .is_ok_and(|current| current.revision == *revision)
            {
                atomic_write(
                    &incoming
                        .join("memory-sources")
                        .join(format!("{mapped}.json")),
                    &serde_json::to_vec_pretty(&translated)?,
                )?;
            }
            for chunk in index::chunks(&source_memory(original_source), 24 * 1024) {
                let old = json!({"id":source_id,"revision":revision,"section":chunk.section,"excerpt":chunk.text});
                let mut new = old.clone();
                new["id"] = json!(mapped);
                let aliases = source_fingerprints(&[new]);
                for original in source_fingerprints(&[old]) {
                    remap
                        .fingerprints
                        .entry(original)
                        .or_default()
                        .extend(aliases.clone());
                }
            }
        }
        let origin = incoming
            .join("memory-source-origins")
            .join(format!("{mapped}.json"));
        std::fs::create_dir_all(origin.parent().unwrap())?;
        atomic_write(
            &origin,
            &serde_json::to_vec_pretty(
                &json!({"format_version":FORMAT,"original_kind":"guide_source","original_id":source_id,"mapped_id":mapped,"original_resource":format!("memory-source-originals/{mapped}.json"),"original_history":format!("memory-source-original-revisions/{mapped}"),"preserved_revisions":valid_revisions.keys().copied().collect::<Vec<_>>(),"source_id_map":remap.ids,"unrecognized":source.is_err()}),
            )?,
        )?;
        // An unsupported colliding source is preserved as original bytes but
        // never parsed as a local source or automatically queued.
        if source.is_err() {
            let unknown = incoming.join("memory-unrecognized").join(&mapped);
            copy_tree(&file, &unknown.join("source.json"))?;
            if raw_history.exists() {
                copy_tree(&raw_history, &unknown.join("text"))?;
            }
            if original_history.exists() {
                copy_tree(&original_history, &unknown.join("revisions"))?;
            }
        }
        for target in [&file, &original_history, &raw_history] {
            if target.exists() {
                safe_remove(target, incoming)?;
            }
        }
    }
    Ok(remap)
}
fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    ensure!(
        !std::fs::symlink_metadata(source)?.file_type().is_symlink(),
        "memory.archive_symlink_rejected"
    );
    if source.is_file() {
        std::fs::create_dir_all(target.parent().unwrap())?;
        std::fs::copy(source, target)?;
        return Ok(());
    }
    std::fs::create_dir_all(target)?;
    for e in std::fs::read_dir(source)? {
        let e = e?;
        copy_tree(&e.path(), &target.join(e.file_name()))?;
    }
    Ok(())
}
