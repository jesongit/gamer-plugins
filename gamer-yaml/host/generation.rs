//! Candidate orchestration belongs to automation; the optional AI service only proposes.
use super::{
    offline_validation::{self, ValidationRequest},
    revisions,
};
use crate::{
    extensions::{
        service::BuiltinService, ExtensionError, ExtensionId, ExtensionResult, ExtensionService,
    },
    resources::PackageStore,
    run_manager::RunManager,
};
use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use base64::Engine;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
    time::{Duration, Instant},
};

pub(crate) const ACTIONS: &[&str] = &[
    "generation.readiness",
    "generation.create",
    "generation.start",
    "generation.get",
    "generation.template",
    "generation.list",
    "generation.edit",
    "generation.apply_proposal",
    "generation.validate",
    "generation.retry",
    "generation.cancel",
    "generation.save",
    "generation.history",
    "generation.rollback",
    "automation.validate_source",
];
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Limits {
    pub max_attempts: u32,
    pub max_failures: u32,
    pub max_seconds: u64,
    pub max_tokens: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            max_failures: 3,
            max_seconds: 180,
            max_tokens: 40000,
        }
    }
}
impl Limits {
    fn validate(&self) -> Result<()> {
        ensure!(
            (1..=500).contains(&self.max_attempts)
                && (1..=20).contains(&self.max_failures)
                && (10..=7200).contains(&self.max_seconds)
                && (2048..=2_000_000).contains(&self.max_tokens),
            "invalid generation limits"
        );
        Ok(())
    }

    fn from_ai_defaults(model: &Value) -> Result<Self> {
        let defaults = &model["default_limits"];
        let rounds = defaults["max_turns"]
            .as_u64()
            .context("AI round budget missing")?;
        let failures = defaults["max_failures"]
            .as_u64()
            .context("AI failure budget missing")?;
        let limits = Self {
            max_attempts: u32::try_from(rounds)?,
            max_failures: u32::try_from(failures)?,
            max_seconds: defaults["max_seconds"]
                .as_u64()
                .context("AI time budget missing")?,
            max_tokens: defaults["max_tokens"]
                .as_u64()
                .context("AI token budget missing")?,
        };
        limits.validate()?;
        Ok(limits)
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Crop {
    pub name: String,
    pub sample_id: String,
    pub frame_id: String,
    pub rect: [u32; 4],
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileInput {
    pub path: String,
    pub base64: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Sample {
    pub manifest: Value,
    pub files: Vec<FileInput>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Candidate {
    pub id: String,
    pub package_id: String,
    pub name: String,
    pub goal: String,
    pub state: String,
    pub revision: u64,
    pub yaml: String,
    pub templates: Vec<Crop>,
    pub sample_ids: Vec<String>,
    pub report: Option<Value>,
    #[serde(default)]
    pub pending_proposal: Option<Value>,
    pub attempts: u32,
    pub known_tokens: u64,
    #[serde(default)]
    pub active_seconds: f64,
    pub unknown_usage: bool,
    pub reason: Option<String>,
    pub explanation: String,
    pub base_version: String,
    pub model_version: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub limits: Limits,
    pub args: serde_json::Map<String, Value>,
    #[serde(default)]
    pub execution_settings: super::settings::Settings,
}
#[derive(Clone, Serialize, Deserialize)]
struct Stored {
    candidate: Candidate,
    samples: Vec<Sample>,
    base: BTreeMap<String, Vec<u8>>,
}
#[derive(Serialize, Deserialize)]
struct FrozenSnapshot {
    samples: Vec<Sample>,
    #[serde(with = "base64_map")]
    base: BTreeMap<String, Vec<u8>>,
}
#[derive(Serialize, Deserialize)]
struct CandidateEnvelope {
    candidate: Candidate,
    base_yaml: String,
    base_exists: bool,
    verification_scope: Value,
}
mod base64_map {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        map: &BTreeMap<String, Vec<u8>>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        map.iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    base64::engine::general_purpose::STANDARD.encode(v),
                )
            })
            .collect::<BTreeMap<_, _>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<BTreeMap<String, Vec<u8>>, D::Error> {
        BTreeMap::<String, String>::deserialize(deserializer)?
            .into_iter()
            .map(|(k, v)| {
                base64::engine::general_purpose::STANDARD
                    .decode(v)
                    .map(|v| (k, v))
                    .map_err(serde::de::Error::custom)
            })
            .collect()
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    package_id: String,
    name: String,
    goal: String,
    samples: Vec<Value>,
    #[serde(default)]
    yaml: String,
    #[serde(default)]
    templates: Vec<Crop>,
    #[serde(default)]
    limits: Option<Limits>,
    #[serde(default)]
    args: serde_json::Map<String, Value>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    yaml: String,
    #[serde(default)]
    templates: Vec<Crop>,
    #[serde(default)]
    explanation: String,
}
struct Job {
    cancel: Arc<AtomicBool>,
    request_id: String,
}
pub(crate) struct GenerationService {
    inner: Arc<Inner>,
}
type ModelCanceller = Arc<dyn Fn(&str) + Send + Sync>;
struct Inner {
    packages: Arc<PackageStore>,
    _runs: Arc<RunManager>,
    extensions: Mutex<Weak<ExtensionService>>,
    jobs: Mutex<BTreeMap<String, Job>>,
    gate: Mutex<()>,
    diagnostics: Mutex<Option<crate::store::Db>>,
    ffmpeg_path: Mutex<String>,
    cancel_model: Mutex<Option<ModelCanceller>>,
}
impl GenerationService {
    pub(crate) fn new(packages: Arc<PackageStore>, runs: Arc<RunManager>) -> Self {
        Self {
            inner: Arc::new(Inner {
                packages,
                _runs: runs,
                extensions: Mutex::new(Weak::new()),
                jobs: Mutex::new(BTreeMap::new()),
                gate: Mutex::new(()),
                diagnostics: Mutex::new(None),
                ffmpeg_path: Mutex::new("ffmpeg".into()),
                cancel_model: Mutex::new(None),
            }),
        }
    }
    pub(crate) fn with_ffmpeg_path(self, path: impl Into<String>) -> Self {
        *self.inner.ffmpeg_path.lock() = path.into();
        self
    }
    pub(crate) fn with_diagnostics(self, db: crate::store::Db) -> Self {
        *self.inner.diagnostics.lock() = Some(db);
        self
    }
    pub(crate) fn attach_model_canceller(&self, cancel: ModelCanceller) {
        *self.inner.cancel_model.lock() = Some(cancel);
    }
    pub(crate) fn recover_all(&self) -> Result<()> {
        for package in self.inner.packages.list_packages()? {
            revisions::recover(&self.inner.packages, &package.id)?;
        }
        Ok(())
    }
    pub(crate) fn attach(&self, extensions: &Arc<ExtensionService>) {
        *self.inner.extensions.lock() = Arc::downgrade(extensions);
    }
}
#[async_trait]
impl BuiltinService for GenerationService {
    fn extension_id(&self) -> &str {
        super::YAML_EXTENSION_ID
    }
    async fn call(&self, action: &str, values: Value) -> ExtensionResult<Value> {
        if ACTIONS.contains(&action) {
            self.inner
                .dispatch(action, values)
                .await
                .map_err(|e| ExtensionError::CallRejected(e.to_string()))
        } else {
            super::actions::native_call_action(
                super::YAML_EXTENSION_ID,
                action,
                &values,
                self.inner.packages.data_root(),
            )
            .unwrap_or_else(|| Err(ExtensionError::CallRejected("unknown YAML action".into())))
        }
    }
    async fn stop(&self) {
        for job in self.inner.jobs.lock().values() {
            job.cancel.store(true, Ordering::Release);
            if let Some(cancel) = self.inner.cancel_model.lock().as_ref() {
                cancel(&job.request_id);
            }
        }
    }
}
fn required<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("missing {k}"))
}
fn script_name(name: &str) -> Result<String> {
    let name = name.trim();
    ensure!(!name.is_empty() && name.len() <= 200, "invalid script name");
    let name = if name.ends_with(".yaml") {
        name.to_owned()
    } else {
        format!("{name}.yaml")
    };
    crate::resources::sanitize_rel_path(&name)?;
    ensure!(
        !name
            .split('/')
            .next_back()
            .unwrap_or("")
            .starts_with("_function"),
        "generation target cannot be a function library"
    );
    Ok(name)
}
fn crop_name(name: &str) -> Result<String> {
    let name = if name.ends_with(".png") {
        name.to_string()
    } else {
        format!("{name}.png")
    };
    crate::resources::sanitize_rel_path(&name)?;
    ensure!(
        !name.contains('/') && !name.contains('\\') && name.len() <= 128,
        "template must have a short scoped PNG name"
    );
    Ok(name)
}
impl Inner {
    fn root(&self, package: &str) -> Result<PathBuf> {
        self.packages.manifest(package)?;
        Ok(self
            .packages
            .data_root()
            .join("extension-data/gamer-yaml/candidates")
            .join(package))
    }
    fn file(&self, package: &str, id: &str) -> Result<PathBuf> {
        ensure!(uuid::Uuid::parse_str(id).is_ok(), "invalid candidate id");
        Ok(self.root(package)?.join(format!("{id}.json")))
    }
    fn read_envelope(&self, package: &str, id: &str) -> Result<CandidateEnvelope> {
        let p = self.file(package, id)?;
        ensure!(
            std::fs::metadata(&p)?.len() <= 8 * 1024 * 1024,
            "candidate metadata exceeds limit"
        );
        let envelope: CandidateEnvelope = serde_json::from_slice(&std::fs::read(p)?)?;
        ensure!(
            envelope.candidate.package_id == package && envelope.candidate.id == id,
            "candidate scope mismatch"
        );
        Ok(envelope)
    }
    fn read(&self, package: &str, id: &str) -> Result<Stored> {
        let envelope = self.read_envelope(package, id)?;
        let path = self.root(package)?.join(format!("{id}.snapshot"));
        ensure!(
            std::fs::metadata(&path)?.len() <= 280 * 1024 * 1024,
            "candidate snapshot exceeds limit"
        );
        let frozen: FrozenSnapshot = serde_json::from_slice(&std::fs::read(path)?)?;
        Ok(Stored {
            candidate: envelope.candidate,
            samples: frozen.samples,
            base: frozen.base,
        })
    }
    fn write(&self, s: &Stored) -> Result<()> {
        let root = self.root(&s.candidate.package_id)?;
        std::fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        }
        let path = format!("automations/{}", s.candidate.name);
        let base = s.base.get(&path);
        let envelope = CandidateEnvelope {
            candidate: s.candidate.clone(),
            base_yaml: base
                .and_then(|b| std::str::from_utf8(b).ok())
                .unwrap_or("")
                .to_string(),
            base_exists: base.is_some(),
            verification_scope: verification_metadata(s)?,
        };
        let bytes = serde_json::to_vec(&envelope)?;
        ensure!(
            bytes.len() <= 8 * 1024 * 1024,
            "candidate report metadata exceeds8MiB"
        );
        let snapshot = root.join(format!("{}.snapshot", s.candidate.id));
        let created_snapshot = !snapshot.exists();
        if created_snapshot {
            let frozen = FrozenSnapshot {
                samples: s.samples.clone(),
                base: s.base.clone(),
            };
            let frozen_bytes = serde_json::to_vec(&frozen)?;
            ensure!(
                frozen_bytes.len() <= 280 * 1024 * 1024,
                "candidate snapshot exceeds limit"
            );
            crate::core::fs::atomic_write(&snapshot, &frozen_bytes)?;
        }
        if let Err(error) = crate::core::fs::atomic_write(
            &self.file(&s.candidate.package_id, &s.candidate.id)?,
            &bytes,
        ) {
            if created_snapshot {
                let _ = std::fs::remove_file(&snapshot);
            }
            return Err(error);
        }
        Ok(())
    }
    fn active(&self, id: &str) -> bool {
        self.jobs.lock().contains_key(id)
    }
    async fn ai(&self, action: &str, values: Value) -> Result<Value> {
        let service = self
            .extensions
            .lock()
            .upgrade()
            .context("extension host unavailable")?;
        let caller = service
            .plugin_call_context(&ExtensionId::parse(super::YAML_EXTENSION_ID)?)
            .await?;
        Ok(service
            .call_extension_from_plugin(&caller, &ExtensionId::parse("gamer-ai")?, action, values)
            .await?)
    }
    async fn dispatch(self: &Arc<Self>, action: &str, v: Value) -> Result<Value> {
        if action == "generation.readiness" {
            return Ok(match self.ai("automation.readiness", json!({})).await {
                Ok(model) => json!({"ready":model["ready"],"reason":model["reason"],"model":model}),
                Err(e) => json!({"ready":false,"reason":e.to_string(),"model":null}),
            });
        }
        let package = required(&v, "package_id")?.to_string();
        let _package_activity = self.packages.acquire_activity(&package)?;
        match action {
            "automation.validate_source" => {
                let yaml = required(&v, "yaml")?;
                let diagnostics = if v["kind"] == "function_library" {
                    super::syntax::parse_function_library(yaml).err()
                } else {
                    super::syntax::parse_script(yaml).err()
                };
                Ok(
                    json!({"valid":diagnostics.is_none(),"diagnostics":diagnostics.unwrap_or_default()}),
                )
            }
            "generation.start" | "generation.create" => {
                let mut request: Create = serde_json::from_value(v)?;
                if let Some(limits) = &request.limits {
                    limits.validate()?;
                }
                ensure!(request.yaml.len() <= 512 * 1024, "yaml exceeds512KiB");
                ensure!(
                    serde_json::to_vec(&request.args)?.len() <= 64 * 1024,
                    "args exceed64KiB"
                );
                request.name = script_name(&request.name)?;
                ensure!(
                    !request.goal.trim().is_empty() && request.goal.len() <= 8000,
                    "goal required (max 8000 bytes)"
                );
                let mut samples = Vec::new();
                for input in request.samples {
                    let value = if input.get("sample_id").is_some() {
                        let id = required(&input, "sample_id")?;
                        crate::resources::validate_scope_id("sample id", id)?;
                        let plugin = input["plugin_id"].as_str().unwrap_or("gamer-video");
                        ensure!(
                            matches!(plugin, "gamer-video" | "gamer-yaml"),
                            "sample source not allowed"
                        );
                        let bytes = self
                            .packages
                            .read_binary(&package, plugin, &format!("samples/{id}.gamersample"))?
                            .context("selected sample missing")?;
                        crate::extensions::video::sample::read_bundle(&bytes)?
                    } else {
                        input
                    };
                    samples.push(serde_json::from_value::<Sample>(value)?);
                }
                validate_samples(&samples)?;
                let model = if action == "generation.start" {
                    let model = self.ai("automation.readiness", json!({})).await?;
                    ensure!(model["ready"] == true, "AI not ready: {}", model["reason"]);
                    if request.limits.is_none() {
                        request.limits = Some(Limits::from_ai_defaults(&model)?);
                    }
                    Some(required(&model, "model_version")?.to_string())
                } else {
                    None
                };
                let _guard = self.gate.lock();
                revisions::recover(&self.packages, &package)?;
                let mut base = revisions::snapshot(&self.packages, &package)?;
                let base_version = revisions::hash(&base);
                base.retain(|path, _| {
                    path.starts_with("automations/") || path.starts_with("templates/")
                });
                ensure!(
                    base.values().map(Vec::len).sum::<usize>() <= 64 * 1024 * 1024,
                    "candidate base exceeds 64 MiB"
                );
                let id = uuid::Uuid::new_v4().to_string();
                let now = chrono::Utc::now().to_rfc3339();
                let candidate = Candidate {
                    id: id.clone(),
                    package_id: package.clone(),
                    name: request.name,
                    goal: request.goal,
                    state: "draft".into(),
                    revision: 1,
                    yaml: request.yaml,
                    templates: request.templates,
                    sample_ids: samples
                        .iter()
                        .map(|s| s.manifest["id"].as_str().unwrap().to_string())
                        .collect(),
                    report: None,
                    pending_proposal: None,
                    attempts: 0,
                    known_tokens: 0,
                    active_seconds: 0.0,
                    unknown_usage: false,
                    reason: None,
                    explanation: String::new(),
                    base_version,
                    model_version: model,
                    created_at: now.clone(),
                    updated_at: now,
                    limits: request.limits.unwrap_or_default(),
                    args: request.args,
                    execution_settings: super::settings::load(self.packages.data_root())?,
                };
                let stored = Stored {
                    candidate,
                    samples,
                    base,
                };
                crop_templates(&stored)?;
                self.write(&stored)?;
                if action == "generation.start" {
                    self.launch(&package, &id, true)?;
                }
                Ok(json!({"candidate":self.read(&package,&id)?.candidate}))
            }
            "generation.list" => {
                let _guard = self.gate.lock();
                let root = self.root(&package)?;
                let mut candidates = vec![];
                if root.exists() {
                    for file in std::fs::read_dir(root)? {
                        let file = file?;
                        if file.path().extension().is_some_and(|e| e == "json") {
                            if let Ok(mut s) = serde_json::from_slice::<CandidateEnvelope>(
                                &std::fs::read(file.path())?,
                            ) {
                                if !self.active(&s.candidate.id)
                                    && matches!(
                                        s.candidate.state.as_str(),
                                        "generating" | "validating"
                                    )
                                {
                                    s.candidate.state = "failed".into();
                                    s.candidate.reason =
                                        Some("interrupted: server restarted".into());
                                    crate::core::fs::atomic_write(
                                        &file.path(),
                                        &serde_json::to_vec(&s)?,
                                    )?;
                                }
                                if self.active(&s.candidate.id)
                                    && matches!(s.candidate.state.as_str(), "passed" | "failed")
                                {
                                    s.candidate.state = "validating".into();
                                }
                                candidates.push(s.candidate);
                            }
                        }
                    }
                }
                candidates.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                Ok(json!({"candidates":candidates}))
            }
            "generation.history" => revisions::history(&self.packages, &package),
            "generation.rollback" => {
                let _guard = self.gate.lock();
                let revision = revisions::rollback(
                    &self.packages,
                    &package,
                    required(&v, "revision_id")?,
                    required(&v, "expected_version")?,
                )?;
                Ok(json!({"version":revision.version,"revision":revision}))
            }
            _ => {
                let id = required(&v, "candidate_id")?.to_string();
                if action == "generation.cancel" {
                    self.read(&package, &id)?;
                    let request = self.jobs.lock().get(&id).map(|job| {
                        job.cancel.store(true, Ordering::Release);
                        job.request_id.clone()
                    });
                    {
                        let _guard = self.gate.lock();
                        let mut s = self.read(&package, &id)?;
                        if s.candidate.state == "generating" && s.candidate.attempts > 0 {
                            s.candidate.unknown_usage = true;
                        }
                        s.candidate.state = "cancelled".into();
                        s.candidate.reason = Some(
                            if s.candidate.unknown_usage {
                                "user_cancelled: in-flight model may have incurred unreported usage"
                            } else {
                                "user_cancelled"
                            }
                            .into(),
                        );
                        s.candidate.report = None;
                        s.candidate.revision += 1;
                        s.candidate.updated_at = chrono::Utc::now().to_rfc3339();
                        self.write(&s)?;
                    }
                    if let Some(request) = request {
                        let _ = self
                            .ai("automation.cancel", json!({"request_id":request}))
                            .await;
                    }
                    return Ok(json!({"candidate":self.read(&package,&id)?.candidate}));
                }
                let _guard = self.gate.lock();
                if action == "generation.get" {
                    let mut envelope = self.read_envelope(&package, &id)?;
                    if !self.active(&id)
                        && matches!(
                            envelope.candidate.state.as_str(),
                            "generating" | "validating"
                        )
                    {
                        envelope.candidate.state = "failed".into();
                        envelope.candidate.reason = Some("interrupted: server restarted".into());
                    }
                    if envelope.candidate.state != "saved"
                        && envelope.candidate.report.is_some()
                        && (revisions::version(&self.packages, &package)?
                            != envelope.candidate.base_version
                            || super::settings::load(self.packages.data_root())?
                                != envelope.candidate.execution_settings)
                    {
                        envelope.candidate.report = None;
                        envelope.candidate.state = "draft".into();
                        envelope.candidate.reason = Some(
                            "base_resources_or_settings_changed: create a fresh candidate before saving".into(),
                        );
                        envelope.verification_scope["validation_status"] = Value::Null;
                    }
                    // Completion also publishes the final budget ledger and
                    // removes the job. Never expose a terminal receipt earlier.
                    if self.active(&id)
                        && matches!(envelope.candidate.state.as_str(), "passed" | "failed")
                    {
                        envelope.candidate.state = "validating".into();
                    }
                    return Ok(serde_json::to_value(envelope)?);
                }
                let mut s = self.read(&package, &id)?;
                match action {
                    "generation.template" => {
                        let name = crop_name(required(&v, "name")?)?;
                        let preview = if v["pending"].as_bool() == Some(true) {
                            let p = s
                                .candidate
                                .pending_proposal
                                .as_ref()
                                .context("no pending proposal")?;
                            let proposal: Proposal = serde_json::from_value(p["proposal"].clone())?;
                            let mut preview = s.clone();
                            preview.candidate.templates = proposal.templates;
                            preview
                        } else {
                            s.clone()
                        };
                        let templates = all_templates(&preview)?;
                        let bytes = templates.get(&name).context("template missing")?;
                        let image = decode_image(bytes)?;
                        return Ok(
                            json!({"name":name,"mime_type":"image/png","base64":base64::engine::general_purpose::STANDARD.encode(bytes),"width":image.width(),"height":image.height()}),
                        );
                    }
                    "generation.get" => {
                        if s.candidate.state != "saved"
                            && s.candidate.report.is_some()
                            && revisions::version(&self.packages, &package)?
                                != s.candidate.base_version
                        {
                            s.candidate.report = None;
                            s.candidate.state = "draft".into();
                            s.candidate.reason = Some(
                                "base_resources_or_settings_changed: create a fresh candidate before saving"
                                    .into(),
                            );
                            self.write(&s)?;
                        }
                    }
                    "generation.apply_proposal" => {
                        ensure!(
                            !self.active(&id) && s.candidate.state != "saved",
                            "candidate_busy_or_saved"
                        );
                        check_revision(&s, &v)?;
                        let proposed = s
                            .candidate
                            .pending_proposal
                            .as_ref()
                            .context("no pending proposal")?;
                        ensure!(
                            proposed["id"].as_str() == Some(required(&v, "proposal_id")?)
                                && proposed["base_revision"].as_u64() == Some(s.candidate.revision),
                            "proposal_stale"
                        );
                        let proposal: Proposal =
                            serde_json::from_value(proposed["proposal"].clone())?;
                        s.candidate.yaml = proposal.yaml;
                        s.candidate.templates = proposal.templates;
                        s.candidate.explanation = proposal.explanation;
                        crop_templates(&s)?;
                        s.candidate.pending_proposal = None;
                        s.candidate.revision += 1;
                        s.candidate.report = None;
                        s.candidate.state = "draft".into();
                        s.candidate.reason = None;
                        s.candidate.updated_at = chrono::Utc::now().to_rfc3339();
                        self.write(&s)?;
                    }
                    "generation.edit" => {
                        ensure!(
                            !self.active(&id),
                            "candidate_busy: cancel and wait before editing"
                        );
                        check_revision(&s, &v)?;
                        ensure!(
                            s.candidate.state != "saved",
                            "saved candidate immutable; create a new candidate"
                        );
                        s.candidate.pending_proposal = None;
                        s.candidate.yaml = required(&v, "yaml")?.to_string();
                        ensure!(s.candidate.yaml.len() <= 512 * 1024, "yaml too large");
                        if let Some(t) = v.get("templates") {
                            s.candidate.templates = serde_json::from_value(t.clone())?;
                        }
                        if let Some(args) = v.get("args") {
                            s.candidate.args = serde_json::from_value(args.clone())?;
                            ensure!(
                                serde_json::to_vec(&s.candidate.args)?.len() <= 64 * 1024,
                                "args exceed64KiB"
                            );
                        }
                        crop_templates(&s)?;
                        s.candidate.revision += 1;
                        s.candidate.report = None;
                        s.candidate.state = "draft".into();
                        s.candidate.reason = None;
                        s.candidate.updated_at = chrono::Utc::now().to_rfc3339();
                        self.write(&s)?;
                    }
                    "generation.validate" | "generation.retry" => {
                        self.launch(&package, &id, action == "generation.retry")?;
                        s = self.read(&package, &id)?;
                    }
                    "generation.save" => {
                        ensure!(!self.active(&id), "candidate_busy");
                        check_revision(&s, &v)?;
                        if v["mode"] == "draft" {
                            s.candidate.state = "draft".into();
                            self.write(&s)?;
                        } else {
                            ensure!(v["mode"] == "validated", "mode must be draft or validated");
                            let report = s
                                .candidate
                                .report
                                .as_ref()
                                .context("current validation required")?;
                            let request = validation_request(&s)?;
                            ensure!(
                                report["status"] == "passed"
                                    && report["candidate_sha256"]
                                        == offline_validation::candidate_sha256(&request)?,
                                "validation_stale_or_failed"
                            );
                            ensure!(
                                report["samples"]
                                    .as_array()
                                    .is_some_and(|r| r.len() == s.samples.len()
                                        && r.iter().all(|r| r["status"] == "passed")),
                                "all selected samples must pass"
                            );
                            let expected = required(&v, "expected_version")?;
                            ensure!(
                                expected == s.candidate.base_version,
                                "candidate_base_version_conflict"
                            );
                            let mut changes = BTreeMap::new();
                            changes.insert(
                                format!("automations/{}", s.candidate.name),
                                s.candidate.yaml.as_bytes().to_vec(),
                            );
                            for (name, bytes) in crop_templates(&s)? {
                                changes.insert(format!("templates/{name}"), bytes);
                            }
                            let metadata = verification_metadata(&s)?;
                            let revision = super::settings::with_snapshot(
                                self.packages.data_root(),
                                &s.candidate.execution_settings,
                                || {
                                    revisions::commit_with_metadata(
                                        &self.packages,
                                        &package,
                                        expected,
                                        &changes,
                                        &id,
                                        metadata,
                                    )
                                },
                            )?;
                            s.candidate.state = "saved".into();
                            s.candidate.updated_at = chrono::Utc::now().to_rfc3339();
                            let warning=self.write(&s).err().map(|error|format!("正式资源和版本记录已提交，候选状态暂存失败，请查看版本历史：{error}"));
                            return Ok(
                                json!({"candidate":s.candidate,"version":revision.version,"revision":revision,"warning":warning}),
                            );
                        }
                    }
                    _ => anyhow::bail!("unknown generation action"),
                }
                let path = format!("automations/{}", s.candidate.name);
                let base = s.base.get(&path);
                let base_yaml = base
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .unwrap_or("")
                    .to_string();
                Ok(
                    json!({"verification_scope":verification_metadata(&s)?,"candidate":s.candidate,"base_yaml":base_yaml,"base_exists":base.is_some()}),
                )
            }
        }
    }
    fn launch(self: &Arc<Self>, package: &str, id: &str, generate: bool) -> Result<()> {
        let mut jobs = self.jobs.lock();
        ensure!(!jobs.contains_key(id), "candidate_busy");
        ensure!(
            jobs.len() < 2,
            "generation_busy: at most2 generation/validation jobs may run; wait or cancel one"
        );
        let mut s = self.read(package, id)?;
        if generate {
            ensure!(!s.candidate.unknown_usage,"usage_unknown: prior model request may have incurred unreported charges; automatic retry disabled");
            ensure!(
                s.candidate.known_tokens < s.candidate.limits.max_tokens,
                "generation_token_budget"
            );
            ensure!(
                s.candidate.attempts < s.candidate.limits.max_attempts,
                "attempt_budget"
            );
            ensure!(
                s.candidate.active_seconds < s.candidate.limits.max_seconds as f64,
                "generation_time_budget"
            );
        }
        ensure!(
            s.candidate.state != "saved",
            "saved candidate is immutable; create a new candidate"
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let request_id = format!("{id}:{}", uuid::Uuid::new_v4());
        s.candidate.state = if generate { "generating" } else { "validating" }.into();
        s.candidate.reason = None;
        s.candidate.report = None;
        s.candidate.updated_at = chrono::Utc::now().to_rfc3339();
        self.write(&s)?;
        jobs.insert(
            id.into(),
            Job {
                cancel: cancel.clone(),
                request_id: request_id.clone(),
            },
        );
        let this = self.clone();
        let package = package.to_string();
        let id = id.to_string();
        tokio::spawn(async move {
            let started = Instant::now();
            let result = this
                .work(&package, &id, generate, cancel.clone(), request_id)
                .await;
            {
                let _guard = this.gate.lock();
                if let Ok(mut s) = this.read(&package, &id) {
                    if generate {
                        s.candidate.active_seconds += started.elapsed().as_secs_f64();
                    }
                    if let Err(ref error) = result {
                        if s.candidate.state != "cancelled" {
                            if cancel.load(Ordering::Acquire)
                                && s.candidate.state == "generating"
                                && s.candidate.attempts > 0
                            {
                                s.candidate.unknown_usage = true;
                            }
                            s.candidate.state = if cancel.load(Ordering::Acquire) {
                                "cancelled"
                            } else {
                                "failed"
                            }
                            .into();
                            s.candidate.reason = Some(error.to_string());
                            s.candidate.updated_at = chrono::Utc::now().to_rfc3339();
                        }
                    }
                    if generate || result.is_err() {
                        let _ = this.write(&s);
                    }
                }
                this.jobs.lock().remove(&id);
            }
        });
        Ok(())
    }
    async fn work(
        &self,
        package: &str,
        id: &str,
        generate: bool,
        cancel: Arc<AtomicBool>,
        request_id: String,
    ) -> Result<()> {
        let _activity = self.packages.acquire_activity(package)?;
        ensure!(!cancel.load(Ordering::Acquire), "CANCELLED");
        let started = Instant::now();
        let mut initial = self.read(package, id)?;
        if generate && initial.candidate.model_version.is_none() {
            let model = self.ai("automation.readiness", json!({})).await?;
            ensure!(model["ready"] == true, "AI not ready: {}", model["reason"]);
            initial.candidate.limits = Limits::from_ai_defaults(&model)?;
            initial.candidate.model_version = Some(required(&model, "model_version")?.to_string());
            let _guard = self.gate.lock();
            ensure!(!cancel.load(Ordering::Acquire), "CANCELLED");
            self.write(&initial)?;
        }
        let limits = initial.candidate.limits.clone();
        let seconds = if generate {
            limits
                .max_seconds
                .saturating_sub(initial.candidate.active_seconds.ceil() as u64)
        } else {
            limits.max_seconds
        };
        ensure!(seconds > 0, "generation_time_budget");
        if generate {
            let samples = initial
                .samples
                .iter()
                .map(|sample| {
                    Ok(offline_validation::SampleInput {
                        manifest: sample.manifest.clone(),
                        files: decode_files(sample)?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let ffmpeg_path = self.ffmpeg_path.lock().clone();
            match tokio::time::timeout(
                Duration::from_secs(seconds),
                offline_validation::preflight_samples(samples, cancel.clone(), ffmpeg_path),
            )
            .await
            {
                Ok(result) => result?,
                Err(_) => {
                    cancel.store(true, Ordering::Release);
                    anyhow::bail!("sample_preflight_time_budget");
                }
            }
        }
        let mut previous = None;
        let mut unchanged = 0u32;
        let attempts = if generate {
            limits
                .max_attempts
                .saturating_sub(initial.candidate.attempts)
        } else {
            1
        };
        for attempt in 0..attempts {
            ensure!(!cancel.load(Ordering::Acquire), "CANCELLED");
            ensure!(
                started.elapsed().as_secs() < seconds,
                "generation_time_budget"
            );
            let mut s = self.read(package, id)?;
            if generate {
                ensure!(
                    !s.candidate.unknown_usage,
                    "usage_unknown: 无法确认累计费用，停止自动重试"
                );
                ensure!(
                    s.candidate.known_tokens < limits.max_tokens,
                    "generation_token_budget"
                );
                let readiness = self.ai("automation.readiness", json!({})).await?;
                ensure!(
                    readiness["ready"] == true,
                    "AI not ready: {}",
                    readiness["reason"]
                );
                ensure!(
                    readiness["contract_version"] == 1,
                    "unsupported AI generation contract version"
                );
                let model = required(&readiness, "model_version")?.to_string();
                if let Some(expected) = s.candidate.model_version.as_ref() {
                    ensure!(expected == &model, "model_version_conflict");
                }
                s.candidate.model_version = Some(model.clone());
                s.candidate.attempts += 1;
                s.candidate.state = "generating".into();
                {
                    let _guard = self.gate.lock();
                    ensure!(!cancel.load(Ordering::Acquire), "CANCELLED");
                    self.write(&s)?;
                }
                let request_id = format!("{request_id}:{attempt}");
                if let Some(job) = self.jobs.lock().get_mut(id) {
                    job.request_id = request_id.clone();
                }
                let request = prompt_request(
                    &s,
                    &model,
                    &request_id,
                    seconds.saturating_sub(started.elapsed().as_secs()),
                    limits.max_tokens - s.candidate.known_tokens,
                )?;
                let response = tokio::select! {result=self.model_turn(request,&request_id,&cancel)=>match result {
                    Ok(response)=>response,
                    Err(error)=>{let _guard=self.gate.lock();if !cancel.load(Ordering::Acquire){s.candidate.unknown_usage=true;self.write(&s)?;}return Err(error);}
                },_=cancelled(&cancel)=>{let _=self.ai("automation.cancel",json!({"request_id":request_id})).await;anyhow::bail!("CANCELLED");}};
                ensure!(!cancel.load(Ordering::Acquire), "CANCELLED");
                let total = response["usage"]["total_tokens"].as_u64().or_else(|| {
                    response["usage"]["input_tokens"]
                        .as_u64()?
                        .checked_add(response["usage"]["output_tokens"].as_u64()?)
                });
                s.candidate.known_tokens =
                    s.candidate.known_tokens.saturating_add(total.unwrap_or(0));
                s.candidate.unknown_usage |= total.is_none();
                {
                    let _guard = self.gate.lock();
                    ensure!(!cancel.load(Ordering::Acquire), "CANCELLED");
                    self.write(&s)?;
                }
                let proposal: Proposal = serde_json::from_value(response["proposal"].clone())
                    .context("candidate proposal invalid")?;
                ensure!(
                    proposal.yaml.len() <= 512 * 1024 && proposal.explanation.len() <= 32000,
                    "proposal too large"
                );
                s.candidate.yaml = proposal.yaml;
                s.candidate.templates = proposal.templates;
                s.candidate.explanation = proposal.explanation;
                s.candidate.revision += 1;
                s.candidate.report = None;
                crop_templates(&s)?;
            }
            ensure!(!cancel.load(Ordering::Acquire), "CANCELLED");
            s.candidate.state = "validating".into();
            s.candidate.updated_at = chrono::Utc::now().to_rfc3339();
            {
                let _guard = self.gate.lock();
                ensure!(!cancel.load(Ordering::Acquire), "CANCELLED");
                self.write(&s)?;
            }
            let request = validation_request(&s)?;
            let fingerprint = offline_validation::candidate_sha256(&request)?;
            if previous.as_ref() == Some(&fingerprint) {
                unchanged += 1;
            } else {
                unchanged = 0;
            }
            previous = Some(fingerprint);
            let remaining = Duration::from_secs(seconds).saturating_sub(started.elapsed());
            let ffmpeg_path = self.ffmpeg_path.lock().clone();
            let report = match tokio::time::timeout(
                remaining,
                offline_validation::validate_candidate_with_media(
                    request,
                    cancel.clone(),
                    ffmpeg_path,
                ),
            )
            .await
            {
                Ok(r) => r?,
                Err(_) => {
                    cancel.store(true, Ordering::Release);
                    anyhow::bail!("generation_time_budget");
                }
            };
            ensure!(!cancel.load(Ordering::Acquire), "CANCELLED");
            s.candidate.report = Some(serde_json::to_value(report)?);
            let passed = s.candidate.report.as_ref().unwrap()["status"] == "passed";
            let complete = passed
                || !generate
                || unchanged > 0
                || attempt + 1 >= attempts
                || attempt + 1 >= limits.max_failures;
            s.candidate.state = if passed {
                "passed"
            } else if complete {
                "failed"
            } else {
                "generating"
            }
            .into();
            s.candidate.updated_at = chrono::Utc::now().to_rfc3339();
            if !passed {
                s.candidate.reason = Some(
                    if unchanged > 0 {
                        "no_progress"
                    } else if attempt + 1 >= attempts {
                        "attempt_budget"
                    } else if attempt + 1 >= limits.max_failures {
                        "failure_budget"
                    } else {
                        "validation_failed"
                    }
                    .into(),
                );
            }
            {
                let _guard = self.gate.lock();
                ensure!(!cancel.load(Ordering::Acquire), "CANCELLED");
                self.write(&s)?;
            }
            if complete {
                return Ok(());
            }
        }
        Ok(())
    }
    async fn model_turn(&self, request: Value, id: &str, cancel: &AtomicBool) -> Result<Value> {
        let seconds = request["max_seconds"].as_u64().unwrap_or(60);
        let deadline = Instant::now() + Duration::from_secs(seconds + 2);
        self.ai("automation.generate", request).await?;
        loop {
            if Instant::now() >= deadline {
                if let Some(stop) = self.cancel_model.lock().as_ref() {
                    stop(id);
                }
                anyhow::bail!("generation_time_budget");
            }
            if cancel.load(Ordering::Acquire) {
                if let Some(stop) = self.cancel_model.lock().as_ref() {
                    stop(id);
                }
                anyhow::bail!("CANCELLED");
            }
            let result = self
                .ai("automation.result", json!({"request_id":id}))
                .await?;
            match result["state"].as_str() {
                Some("completed") => return Ok(result["result"].clone()),
                Some("failed") => anyhow::bail!(
                    "{}",
                    result["error"].as_str().unwrap_or("model request failed")
                ),
                Some("running") => tokio::time::sleep(Duration::from_millis(75)).await,
                _ => anyhow::bail!("invalid model job state"),
            }
        }
    }
    async fn diagnostic_call(
        &self,
        package: &str,
        script: &str,
        candidate: Option<&str>,
        name: &str,
        args: Value,
        expected_revision: Option<u64>,
    ) -> Result<Value> {
        // There is intentionally no save/apply/rollback operation in this bridge.
        let script = script_name(script)?;
        match name {
            "automation_read_script" => {
                if let Some(entry) = self.packages.read_text(
                    package,
                    super::YAML_EXTENSION_ID,
                    &format!("automations/{script}"),
                )? {
                    return Ok(serde_json::to_value(entry)?);
                }
                let id = candidate.context(
                    "formal script missing; attach a candidate to inspect unsaved source",
                )?;
                let s = self.read(package, id)?;
                ensure!(s.candidate.name == script, "candidate/script mismatch");
                Ok(
                    json!({"path":format!("automations/{script}"),"content":s.candidate.yaml,"version":s.candidate.revision,"source":"candidate","formal_exists":false}),
                )
            }
            "automation_read_candidate" => {
                let id = candidate.context("user must attach a candidate")?;
                Ok(json!({"candidate":self.read(package,id)?.candidate}))
            }
            "automation_read_samples" => {
                let id = candidate.context("user must attach a candidate")?;
                let s = self.read(package, id)?;
                Ok(json!({"samples":s.samples.iter().map(|s|&s.manifest).collect::<Vec<_>>()}))
            }
            "automation_read_sample_frame" => {
                let id = candidate.context("user must attach a candidate")?;
                let s = self.read(package, id)?;
                let sample = s
                    .samples
                    .iter()
                    .find(|s| s.manifest["id"].as_str() == args["sample_id"].as_str())
                    .context("sample not selected")?;
                let frame = sample.manifest["frames"]
                    .as_array()
                    .context("frames missing")?
                    .iter()
                    .find(|f| f["id"].as_str() == args["frame_id"].as_str())
                    .context("frame not selected")?;
                let bytes = sample_file(sample, required(frame, "path")?)?;
                Ok(
                    json!({"sample_id":sample.manifest["id"],"frame":frame,"content":[{"type":"image","mimeType":"image/png","data":base64::engine::general_purpose::STANDARD.encode(bytes)}]}),
                )
            }
            "automation_read_template" => {
                let raw = required(&args, "name")?;
                let name = if raw.to_ascii_lowercase().ends_with(".png") {
                    raw.to_string()
                } else {
                    format!("{raw}.png")
                };
                crate::resources::sanitize_rel_path(&name)?;
                ensure!(name.len() <= 512, "template path too long");
                let (resolved_name, bytes) = if let Some(id) = candidate {
                    let s = self.read(package, id)?;
                    let cropped = crop_templates(&s)?;
                    let original = s
                        .base
                        .get(&format!("automations/{script}"))
                        .and_then(|b| std::str::from_utf8(b).ok())
                        .unwrap_or("");
                    let selected_crop = cropped.keys().any(|crop| {
                        crop == &name || super::resources::template_short_name(crop) == name
                    });
                    ensure!(
                        selected_crop || template_referenced(original, &name),
                        "template not referenced by user-selected source or selected crop"
                    );
                    let templates = all_templates(&s)?;
                    let resolved = crate::resources::resolve_short_relative_path(
                        &name,
                        templates.keys().map(String::as_str),
                    )?;
                    let bytes = templates
                        .get(&resolved)
                        .context("template missing")?
                        .clone();
                    (resolved, bytes)
                } else {
                    self.packages.with_package_read(package, || {
                        let source = self
                            .packages
                            .read_text(
                                package,
                                super::YAML_EXTENSION_ID,
                                &format!("automations/{script}"),
                            )?
                            .context("script missing")?;
                        ensure!(
                            template_referenced(&source.content, &name),
                            "template not referenced by user-selected script"
                        );
                        let path = self.packages.resolve_short_path(
                            package,
                            super::YAML_EXTENSION_ID,
                            &format!("templates/{name}"),
                        )?;
                        let template_root = self
                            .packages
                            .plugin_dir(package, super::YAML_EXTENSION_ID)?
                            .join("templates");
                        let relative = path
                            .strip_prefix(&template_root)
                            .context("resolved template outside scoped directory")?
                            .to_str()
                            .context("template path invalid")?
                            .replace('\\', "/");
                        ensure!(
                            std::fs::metadata(&path)?.len() <= 8 * 1024 * 1024,
                            "template image too large"
                        );
                        let bytes = std::fs::read(&path)?;
                        Ok((relative, bytes))
                    })?
                };
                ensure!(bytes.len() <= 8 * 1024 * 1024, "template image too large");
                decode_image(&bytes)?;
                Ok(
                    json!({"name":name,"resolved_name":resolved_name,"content":[{"type":"image","mimeType":"image/png","data":base64::engine::general_purpose::STANDARD.encode(bytes)}]}),
                )
            }
            "automation_propose" => {
                let id = candidate.context("user must attach a candidate")?;
                let _guard = self.gate.lock();
                ensure!(!self.active(id), "candidate_busy");
                let mut s = self.read(package, id)?;
                ensure!(
                    expected_revision == Some(s.candidate.revision),
                    "automation_context_stale: candidate revision changed"
                );
                ensure!(s.candidate.name == script, "candidate/script mismatch");
                ensure!(
                    s.candidate.state != "saved",
                    "saved candidate immutable; create a new candidate"
                );
                let proposal: Proposal = serde_json::from_value(args)?;
                ensure!(
                    proposal.yaml.len() <= 512 * 1024 && proposal.explanation.len() <= 32000,
                    "proposal too large"
                );
                let mut preview = s.clone();
                preview.candidate.yaml = proposal.yaml.clone();
                preview.candidate.templates = proposal.templates.clone();
                crop_templates(&preview)?;
                s.candidate.pending_proposal = Some(
                    json!({"id":uuid::Uuid::new_v4().to_string(),"base_revision":s.candidate.revision,"created_at":chrono::Utc::now().to_rfc3339(),"proposal":proposal}),
                );
                self.write(&s)?;
                Ok(
                    json!({"candidate":s.candidate,"requires_user_apply":true,"requires_user_save":true}),
                )
            }

            _ => anyhow::bail!("tool not in automation scope"),
        }
    }
}
fn check_revision(s: &Stored, v: &Value) -> Result<()> {
    ensure!(
        v["expected_revision"].as_u64() == Some(s.candidate.revision),
        "candidate_revision_conflict"
    );
    Ok(())
}
async fn cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
fn decode_files(sample: &Sample) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    for file in &sample.files {
        crate::resources::sanitize_rel_path(&file.path)?;
        ensure!(!files.contains_key(&file.path), "duplicate sample file");
        files.insert(
            file.path.clone(),
            base64::engine::general_purpose::STANDARD.decode(&file.base64)?,
        );
    }
    Ok(files)
}
fn validate_samples(samples: &[Sample]) -> Result<()> {
    ensure!(
        !samples.is_empty() && samples.len() <= 12,
        "select 1..12 samples"
    );
    let mut ids = BTreeSet::new();
    let mut total = 0usize;
    let mut images = 0usize;
    let mut asset_bytes = 0usize;
    for sample in samples {
        let m = &sample.manifest;
        ensure!(
            m["schema_version"] == 1 && m["status"] == "complete" && m["goal"]["confirmed"] == true,
            "sample is unsupported or incomplete; review evidence before generation"
        );
        let id = required(m, "id")?;
        ensure!(ids.insert(id), "duplicate sample id");
        let files = decode_files(sample)?;
        asset_bytes = asset_bytes
            .checked_add(files.values().map(Vec::len).sum::<usize>())
            .context("sample asset size overflow")?;
        ensure!(
            asset_bytes <= 128 * 1024 * 1024,
            "selected evidence assets exceed128MiB"
        );
        crate::extensions::video::sample::validate_bundle(m, &files)?;
        for frame in m["frames"].as_array().context("sample frames missing")? {
            let path = required(frame, "path")?;
            let bytes = files.get(path).context("sample frame missing")?;
            total += bytes.len();
            images += 1;
            ensure!(
                total <= 128 * 1024 * 1024 && images <= 10000,
                "selected frame evidence exceeds storage bounds"
            );
            ensure!(
                format!("{:x}", Sha256::digest(bytes)) == required(frame, "sha256")?,
                "sample frame checksum mismatch"
            );
            let image = decode_image(bytes)?;
            ensure!(
                Some(image.width() as u64) == frame["width"].as_u64()
                    && Some(image.height() as u64) == frame["height"].as_u64(),
                "sample frame dimensions mismatch"
            );
        }
        ensure!(
            m["frames"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["id"] == m["start"]["frame_id"])
                && m["frames"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|f| f["id"] == m["end"]["frame_id"]),
            "sample START/END evidence missing"
        );
    }
    Ok(())
}
fn crop_templates(s: &Stored) -> Result<BTreeMap<String, Vec<u8>>> {
    ensure!(
        s.candidate.templates.len() <= 64,
        "template budget exceeded"
    );
    let mut result = BTreeMap::new();
    let mut crop_pixels = 0u64;
    let mut crop_bytes = 0usize;
    for crop in &s.candidate.templates {
        let name = crop_name(&crop.name)?;
        ensure!(!result.contains_key(&name), "duplicate template name");
        let sample = s
            .samples
            .iter()
            .find(|s| s.manifest["id"] == crop.sample_id)
            .context("template sample not selected")?;
        let frame = sample.manifest["frames"]
            .as_array()
            .context("frames missing")?
            .iter()
            .find(|f| f["id"] == crop.frame_id)
            .context("template frame not selected")?;
        let image = decode_image(&sample_file(sample, required(frame, "path")?)?)?;
        let [x, y, w, h] = crop.rect;
        ensure!(
            w > 0
                && h > 0
                && x.checked_add(w).is_some_and(|v| v <= image.width())
                && y.checked_add(h).is_some_and(|v| v <= image.height()),
            "template crop outside original frame"
        );
        crop_pixels = crop_pixels
            .checked_add((w as u64) * (h as u64))
            .context("template pixel count overflow")?;
        ensure!(
            crop_pixels <= 64 * 1024 * 1024,
            "template crops exceed64 million pixels"
        );
        let mut bytes = Cursor::new(Vec::new());
        image
            .crop_imm(x, y, w, h)
            .write_to(&mut bytes, image::ImageFormat::Png)?;
        let bytes = bytes.into_inner();
        crop_bytes = crop_bytes
            .checked_add(bytes.len())
            .context("template bytes overflow")?;
        ensure!(crop_bytes <= 32 * 1024 * 1024, "template crops exceed32MiB");
        let short = super::resources::template_short_name(&name);
        for (existing, old) in s
            .base
            .iter()
            .filter_map(|(path, bytes)| path.strip_prefix("templates/").map(|name| (name, bytes)))
        {
            if old == &bytes || super::resources::template_short_name(existing) != short {
                continue;
            }
            // A new exact filename can shadow an existing region-suffixed
            // template through short-name resolution. Treat that as replacement.
            for (path, source) in &s.base {
                if path.starts_with("automations/")
                    && path.ends_with(".yaml")
                    && path != &format!("automations/{}", s.candidate.name)
                    && std::str::from_utf8(source).map_or(true, |source| {
                        template_referenced(source, &name)
                            || template_referenced(source, existing)
                            || uncertain_template_reference(source)
                    })
                {
                    anyhow::bail!("shared_template_conflict: {name} would change shared template {existing} used by {path}; choose a new unique template name");
                }
            }
        }
        result.insert(name, bytes);
    }
    Ok(result)
}
fn all_templates(s: &Stored) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut templates = BTreeMap::new();
    for (path, bytes) in &s.base {
        if let Some(name) = path.strip_prefix("templates/") {
            if name.ends_with(".png") {
                templates.insert(name.into(), bytes.clone());
            }
        }
    }
    templates.extend(crop_templates(s)?);
    Ok(templates)
}
fn functions(s: &Stored) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for (path, bytes) in &s.base {
        if path.starts_with("automations/")
            && path
                .split('/')
                .next_back()
                .unwrap_or("")
                .starts_with("_function")
            && path.ends_with(".yaml")
        {
            result.insert(path.clone(), String::from_utf8(bytes.clone())?);
        }
    }
    Ok(result)
}

fn validation_request(s: &Stored) -> Result<ValidationRequest> {
    let samples = s
        .samples
        .iter()
        .map(|sample| {
            Ok(offline_validation::SampleInput {
                manifest: sample.manifest.clone(),
                files: decode_files(sample)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ValidationRequest {
        yaml: s.candidate.yaml.clone(),
        templates: all_templates(s)?,
        samples,
        args: s.candidate.args.clone(),
        functions_sources: functions(s)?,
        settings: s.candidate.execution_settings.clone(),
    })
}

fn prompt_request(
    s: &Stored,
    model: &str,
    request: &str,
    seconds: u64,
    remaining_tokens: u64,
) -> Result<Value> {
    // Freeze all evidence for validation, but send a bounded deterministic image
    // subset. START/END of every selected sample are mandatory; selected middle
    // frames are spread over its timeline rather than biased to the first sample.
    let mut quota = (96 / s.samples.len()).max(2);
    let (images, selected, total_frames) = loop {
        let mut images = Vec::new();
        let mut selected = Vec::new();
        let mut bytes = 0usize;
        let mut total_frames = 0usize;
        for sample in &s.samples {
            let frames = sample.manifest["frames"]
                .as_array()
                .context("frames missing")?;
            total_frames += frames.len();
            let count = frames.len().min(quota);
            let mut indices = BTreeSet::new();
            if count == 1 {
                indices.insert(0);
            } else {
                for i in 0..count {
                    indices.insert(i * (frames.len() - 1) / (count - 1));
                }
            }
            for index in indices {
                let frame = &frames[index];
                let file = sample
                    .files
                    .iter()
                    .find(|file| file.path == frame["path"].as_str().unwrap_or(""))
                    .context("frame missing")?;
                bytes = bytes.saturating_add(file.base64.len() * 3 / 4);
                images.push(json!({"label":format!("sample={} frame={} timeline_us={} role={} original={}x{}",sample.manifest["id"],frame["id"],frame["timeline_us"],frame["role"],frame["width"],frame["height"]),"data_url":format!("data:image/png;base64,{}",file.base64)}));
                selected.push(json!({"sample_id":sample.manifest["id"],"frame_id":frame["id"]}));
            }
        }
        if bytes <= 32 * 1024 * 1024 {
            break (images, selected, total_frames);
        }
        ensure!(quota>2,"sample START/END images exceed model input32MiB; choose fewer samples or configure smaller evidence");
        quota = (quota / 2).max(2);
    };
    let context = json!({"goal":s.candidate.goal,"samples":s.samples.iter().map(|s|&s.manifest).collect::<Vec<_>>(),"current_yaml":s.candidate.yaml,"current_templates":s.candidate.templates,"existing_template_names":s.base.keys().filter_map(|path|path.strip_prefix("templates/")).collect::<Vec<_>>(),"validation":s.candidate.report.as_ref().map(compact_report),"args":s.candidate.args,"execution_settings":s.candidate.execution_settings,"functions_sources":functions(s)?,"dsl":DSL_GUIDE,"native_functions":super::native_funcs::native_functions().iter().map(super::native_funcs::native_schema_json).collect::<Vec<_>>(),"image_selection":{"selected":selected,"total_frames":total_frames,"omitted_count":total_frames.saturating_sub(images.len()),"policy":"all samples retain START/END, middle frames uniformly sampled under 96 images/32MiB; all original evidence remains in offline validation"}});
    // Estimate input allowance; supplier image-token accounting can differ.
    // Requests/output tokens are hard-bounded; unknown usage stops correction.
    let estimated_input =
        (context.to_string().len() as u64 / 2).saturating_add(images.len() as u64 * 1024);
    ensure!(
        remaining_tokens > estimated_input + 256,
        "generation_token_budget: selected evidence exceeds remaining estimated token budget"
    );
    Ok(
        json!({"request_id":request,"package_id":s.candidate.package_id,"candidate_id":s.candidate.id,"expected_model_version":model,"context":context,"images":images,"max_seconds":seconds.clamp(1,600),"max_output_tokens":(remaining_tokens-estimated_input).clamp(256,16384)}),
    )
}
const DSL_GUIDE:&str="Canonical YAML: version: 2; targets: {claim: {template: claim.png, threshold: 0.8, region: [x,y,w,h]}, done: {template: done.png}}; run: [{id: claim_step, wait: claim, timeout: 10s, then: [{tap: claim}]}, {optional: {find: confirm, timeout: 0ms, then: [{tap: confirm}]}}, {finish: done, timeout: 10s}]. tap bare target uses last named observation. as:button then tap:$button also valid. wait default10s, optional default0ms, only absence skips. Existing functions/calls, if/repeat/break/return/vars/params retained. trace:false/true and fail:reason supported. All stable id values lowercase identifiers.  Named visual targets refer to PNG templates cropped from selected sample frames. Required waits only observe; actions must explicitly tap returned fresh observations. Optional waits use a bounded timeout. Every loop is bounded. Task success must explicitly assert its visual completion target, never just end of script. Native function schemas are authoritative; do not invent functions or DSL fields.";

fn decode_image(bytes: &[u8]) -> Result<image::DynamicImage> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Attachment {
    script_id: String,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    candidate_id: Option<String>,
    #[serde(default)]
    device_id: Option<String>,
}
#[async_trait]
impl crate::extensions::ai::automation::AutomationBridge for GenerationService {
    async fn bind(
        &self,
        package: &str,
        attachment: Value,
    ) -> Result<crate::extensions::ai::automation::AutomationScope> {
        self.inner.ensure_running()?;
        self.inner.packages.manifest(package)?;
        let selected: Attachment = serde_json::from_value(attachment)?;
        let script = script_name(&selected.script_id)?;
        let source = self.inner.packages.read_text(
            package,
            super::YAML_EXTENSION_ID,
            &format!("automations/{script}"),
        )?;
        let candidate_revision = if let Some(id) = selected.candidate_id.as_deref() {
            let envelope = self.inner.read_envelope(package, id)?;
            ensure!(
                envelope.candidate.name == script,
                "candidate/script mismatch"
            );
            Some(envelope.candidate.revision)
        } else {
            None
        };
        ensure!(
            source.is_some() || selected.candidate_id.is_some(),
            "selected script missing"
        );
        let mut scope = crate::extensions::ai::automation::AutomationScope {
            context_id: uuid::Uuid::new_v4().to_string(),
            package_id: package.into(),
            script_id: script,
            script_version: source.map(|s| s.version()),
            candidate_id: selected.candidate_id,
            candidate_revision,
            run_id: selected.run_id,
            device_id: selected.device_id,
        };
        if scope.run_id.is_some() {
            let record = self.inner.scoped_run(&scope).await?;
            scope.device_id = Some(required(&record, "device_id")?.to_string());
        }
        Ok(scope)
    }
    async fn call(
        &self,
        scope: &crate::extensions::ai::automation::AutomationScope,
        name: &str,
        args: Value,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        self.inner.ensure_running()?;
        ensure!(!cancel.load(Ordering::Acquire), "CANCELLED");
        let current = self.inner.packages.read_text(
            &scope.package_id,
            super::YAML_EXTENSION_ID,
            &format!("automations/{}", scope.script_id),
        )?;
        ensure!(
            current.map(|s| s.version()) == scope.script_version,
            "automation_context_stale: script changed; reattach the selected version"
        );
        if let Some(id) = scope.candidate_id.as_ref() {
            let current = self.inner.read_envelope(&scope.package_id, id)?;
            check_candidate_scope(scope, &current.candidate)?;
        }
        if name == "automation_validate" {
            let id = scope
                .candidate_id
                .as_ref()
                .context("user must attach a candidate")?;
            {
                let _guard = self.inner.gate.lock();
                self.inner.launch(&scope.package_id, id, false)?;
            }
            while self.inner.active(id) {
                if cancel.load(Ordering::Acquire) {
                    if let Some(job) = self.inner.jobs.lock().get(id) {
                        job.cancel.store(true, Ordering::Release);
                    }
                    anyhow::bail!("CANCELLED");
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            ensure!(!cancel.load(Ordering::Acquire), "CANCELLED");
            let envelope = self.inner.read_envelope(&scope.package_id, id)?;
            return envelope.candidate.report.context(
                envelope
                    .candidate
                    .reason
                    .unwrap_or_else(|| "validation did not finish".into()),
            );
        }
        if name == "automation_list_runs" {
            let db = self
                .inner
                .diagnostics
                .lock()
                .clone()
                .context("run journal unavailable")?;
            let device = scope
                .device_id
                .clone()
                .context("select a device to find related runs")?;
            let runs = db
                .run_history(
                    device,
                    Some(format!("{}/{}", scope.package_id, scope.script_id)),
                    args["before"].as_str().map(str::to_owned),
                )
                .await?;
            return Ok(json!({"runs":runs,"requires_user_selection":true}));
        }
        if matches!(
            name,
            "automation_read_run"
                | "automation_read_events"
                | "automation_read_trace"
                | "automation_read_image"
        ) {
            let record = self.inner.scoped_run(scope).await?;
            let run = scope.run_id.as_ref().unwrap();
            let db = self
                .inner
                .diagnostics
                .lock()
                .clone()
                .context("run journal unavailable")?;
            return match name {
                "automation_read_run" => Ok(
                    json!({"run":record,"trace":db.trace.page(run,0,6),"current_script_version":scope.script_version}),
                ),
                "automation_read_events" => {
                    let mut page = db
                        .run_event_page(run.clone(), args["after"].as_i64().unwrap_or(0))
                        .await?;
                    if let Some(events) = page["events"].as_array_mut() {
                        if events.len() > 50 {
                            events.truncate(50);
                            let next = events.last().and_then(|v| v["id"].as_i64());
                            page["next"] = json!(next);
                            page["has_more"] = json!(true);
                        }
                    }
                    Ok(page)
                }
                "automation_read_trace" => {
                    Ok(db.trace.page(run, args["after"].as_u64().unwrap_or(0), 12))
                }
                "automation_read_image" => {
                    let id = required(&args, "image_id")?;
                    let bytes = db
                        .trace
                        .read_image(run, id, args["template"].as_bool().unwrap_or(false))?
                        .context(
                            "image unavailable or expired; do not claim a current screenshot",
                        )?;
                    Ok(
                        json!({"run_id":run,"image_id":id,"content":[{"type":"image","mimeType":"image/png","data":base64::engine::general_purpose::STANDARD.encode(bytes)}]}),
                    )
                }
                _ => unreachable!(),
            };
        }
        self.inner
            .diagnostic_call(
                &scope.package_id,
                &scope.script_id,
                scope.candidate_id.as_deref(),
                name,
                args,
                scope.candidate_revision,
            )
            .await
    }
}
impl Inner {
    fn ensure_running(&self) -> Result<()> {
        let extensions = self
            .extensions
            .lock()
            .upgrade()
            .context("extension host unavailable")?;
        let snapshot = extensions.snapshot_for(&ExtensionId::parse(super::YAML_EXTENSION_ID)?)?;
        ensure!(
            snapshot.state() == crate::extensions::ExtensionState::Running,
            "automation plugin disabled"
        );
        ensure!(
            snapshot
                .manifest()
                .permissions()
                .allows(crate::extensions::Permission::ResourceRead),
            "resource.read permission missing"
        );
        Ok(())
    }
    async fn scoped_run(
        &self,
        scope: &crate::extensions::ai::automation::AutomationScope,
    ) -> Result<Value> {
        let id = scope.run_id.as_ref().context("user must select a run")?;
        let db = self
            .diagnostics
            .lock()
            .clone()
            .context("run journal unavailable")?;
        let record = if let Some(record) = self._runs.get_run(id) {
            serde_json::to_value(record)?
        } else {
            db.stored_run(id.clone()).await?.context("run not found")?
        };
        check_run_scope(scope, &record)?;
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct NoExecutor;
    impl crate::run_manager::RunExecutor for NoExecutor {
        fn prepare<'a>(
            &'a self,
            _: &'a crate::core::RunContext,
            _: &'a crate::core::RunRequest,
        ) -> futures_util::future::BoxFuture<'a, Result<()>> {
            Box::pin(async { anyhow::bail!("no device execution in generation tests") })
        }
        fn execute<'a>(
            &'a self,
            _: &'a crate::core::RunContext,
            _: &'a crate::core::RunRequest,
            _: bool,
            _: Arc<AtomicBool>,
        ) -> futures_util::future::BoxFuture<'a, Result<Vec<(String, String)>>> {
            Box::pin(async { anyhow::bail!("no device execution in generation tests") })
        }
        fn acquire(
            &self,
            _: &crate::core::RunContext,
        ) -> Result<Box<dyn crate::core::ActivityLease>> {
            anyhow::bail!("no device leases in generation tests")
        }
    }
    #[test]
    fn generation_inherits_ai_conversation_defaults_and_failure_bound() {
        let defaults = serde_json::to_value(crate::extensions::ai::Limits::default()).unwrap();
        let mut model = json!({"default_limits":defaults});
        let limits = Limits::from_ai_defaults(&model).unwrap();
        assert_eq!(limits.max_seconds, 600);
        assert_eq!(limits.max_tokens, 100_000);
        assert_eq!(limits.max_attempts, 40);
        assert_eq!(limits.max_failures, 3);
        model["default_limits"]["max_turns"] = json!(2);
        assert_eq!(Limits::from_ai_defaults(&model).unwrap().max_attempts, 2);
        model["default_limits"]["max_seconds"] = json!(0);
        assert!(Limits::from_ai_defaults(&model).is_err());
    }

    fn fixture() -> (tempfile::TempDir, GenerationService, Value) {
        let root = tempfile::tempdir().unwrap();
        let config = crate::config::Config {
            data_dir: root.path().into(),
            ..Default::default()
        };
        let store = Arc::new(PackageStore::open(&config).unwrap());
        store.ensure_default_package().unwrap();
        super::super::resources::register_resource_handlers(&store);
        super::super::settings::dispatch(super::super::settings::SAVE_SETTINGS,&json!({"expected":super::super::settings::Settings::default(),"settings":{"default_timeout_secs":10,"before_click_ms":0,"after_click_ms":0}}),root.path()).unwrap();
        let service =
            GenerationService::new(store, Arc::new(RunManager::new(Arc::new(NoExecutor))));
        let (yaml, _templates, samples) = offline_validation::validation_fixture_samples();
        let inputs=samples.iter().map(|s|json!({"manifest":s.manifest,"files":s.files.iter().map(|(p,b)|json!({"path":p,"base64":base64::engine::general_purpose::STANDARD.encode(b)})).collect::<Vec<_>>()})).collect::<Vec<_>>();
        let crops = json!([{"name":"claim.png","sample_id":"with-confirm","frame_id":"f0","rect":[20,15,9,9]},{"name":"confirm.png","sample_id":"with-confirm","frame_id":"f2","rect":[20,15,9,9]},{"name":"done.png","sample_id":"with-confirm","frame_id":"f4","rect":[20,15,9,9]}]);
        (
            root,
            service,
            json!({"package_id":"default","name":"test.yaml","goal":"done pattern visible","samples":inputs,"yaml":yaml,"templates":crops}),
        )
    }
    #[tokio::test]
    async fn exhausted_ai_budget_blocks_more_requests_but_keeps_manual_validation() {
        let (_root, service, input) = fixture();
        let created = service
            .inner
            .dispatch("generation.create", input)
            .await
            .unwrap();
        let id = created["candidate"]["id"].as_str().unwrap();
        let mut stored = service.inner.read("default", id).unwrap();
        stored.candidate.model_version = Some("fixed-model".into());
        stored.candidate.limits = Limits::from_ai_defaults(&json!({
            "default_limits":crate::extensions::ai::Limits::default()
        }))
        .unwrap();
        stored.candidate.attempts = 40;
        service.inner.write(&stored).unwrap();
        assert!(service
            .inner
            .launch("default", id, true)
            .unwrap_err()
            .to_string()
            .contains("attempt_budget"));
        stored.candidate.attempts = 0;
        stored.candidate.active_seconds = 600.0;
        service.inner.write(&stored).unwrap();
        assert!(service
            .inner
            .launch("default", id, true)
            .unwrap_err()
            .to_string()
            .contains("generation_time_budget"));
        stored.candidate.active_seconds = 0.0;
        stored.candidate.known_tokens = 100_000;
        service.inner.write(&stored).unwrap();
        assert!(service
            .inner
            .launch("default", id, true)
            .unwrap_err()
            .to_string()
            .contains("generation_token_budget"));
        assert!(!service.inner.active(id));
        service.inner.launch("default", id, false).unwrap();
        assert_eq!(settled(&service, id).await["candidate"]["state"], "passed");
    }

    #[tokio::test]
    async fn terminal_candidate_receipt_waits_for_job_and_final_usage() {
        let (_root, service, input) = fixture();
        let created = service
            .inner
            .dispatch("generation.create", input)
            .await
            .unwrap();
        let id = created["candidate"]["id"].as_str().unwrap();
        let mut stored = service.inner.read("default", id).unwrap();
        stored.candidate.state = "failed".into();
        service.inner.write(&stored).unwrap();
        service.inner.jobs.lock().insert(
            id.into(),
            Job {
                cancel: Arc::new(AtomicBool::new(false)),
                request_id: "finishing".into(),
            },
        );
        let values = json!({"package_id":"default","candidate_id":id});
        assert_eq!(
            service
                .inner
                .dispatch("generation.get", values.clone())
                .await
                .unwrap()["candidate"]["state"],
            "validating"
        );
        assert_eq!(
            service
                .inner
                .dispatch("generation.list", json!({"package_id":"default"}))
                .await
                .unwrap()["candidates"][0]["state"],
            "validating"
        );
        stored.candidate.active_seconds = 12.0;
        service.inner.write(&stored).unwrap();
        service.inner.jobs.lock().remove(id);
        let terminal = service
            .inner
            .dispatch("generation.get", values)
            .await
            .unwrap();
        assert_eq!(terminal["candidate"]["state"], "failed");
        assert_eq!(terminal["candidate"]["active_seconds"], 12.0);
    }

    async fn settled(service: &GenerationService, id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let r = service
                    .inner
                    .dispatch(
                        "generation.get",
                        json!({"package_id":"default","candidate_id":id}),
                    )
                    .await
                    .unwrap();
                if !service.inner.active(id) {
                    break r;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn real_validator_save_gate_edits_invalidate_and_full_rollback() {
        let (_root, service, input) = fixture();
        let created = service
            .inner
            .dispatch("generation.create", input.clone())
            .await
            .unwrap();
        let id = created["candidate"]["id"].as_str().unwrap();
        let save = json!({"package_id":"default","candidate_id":id,"expected_revision":1,"expected_version":created["candidate"]["base_version"],"mode":"validated"});
        assert!(service
            .inner
            .dispatch("generation.save", save)
            .await
            .is_err());
        service
            .inner
            .dispatch(
                "generation.validate",
                json!({"package_id":"default","candidate_id":id}),
            )
            .await
            .unwrap();
        let verified = settled(&service, id).await;
        assert_eq!(
            verified["candidate"]["report"]["status"], "passed",
            "{verified}"
        );
        assert_eq!(
            verified["candidate"]["report"]["samples"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let edited=service.inner.dispatch("generation.edit",json!({"package_id":"default","candidate_id":id,"expected_revision":1,"yaml":input["yaml"]})).await.unwrap();
        assert!(edited["candidate"]["report"].is_null());
        assert_eq!(edited["candidate"]["revision"], 2);
        service
            .inner
            .dispatch(
                "generation.validate",
                json!({"package_id":"default","candidate_id":id}),
            )
            .await
            .unwrap();
        let verified = settled(&service, id).await;
        assert_eq!(verified["candidate"]["state"], "passed", "{verified}");
        let saved=service.inner.dispatch("generation.save",json!({"package_id":"default","candidate_id":id,"expected_revision":2,"expected_version":created["candidate"]["base_version"],"mode":"validated"})).await.unwrap();
        assert!(service
            .inner
            .packages
            .read_text(
                "default",
                super::super::YAML_EXTENSION_ID,
                "automations/test.yaml"
            )
            .unwrap()
            .is_some());
        assert!(service
            .inner
            .packages
            .read_binary(
                "default",
                super::super::YAML_EXTENSION_ID,
                "templates/done.png"
            )
            .unwrap()
            .is_some());
        let undone=service.inner.dispatch("generation.rollback",json!({"package_id":"default","revision_id":saved["revision"]["id"],"expected_version":saved["version"]})).await.unwrap();
        assert_eq!(undone["version"], created["candidate"]["base_version"]);
        assert!(service
            .inner
            .packages
            .read_text(
                "default",
                super::super::YAML_EXTENSION_ID,
                "automations/test.yaml"
            )
            .unwrap()
            .is_none());
        assert!(service
            .inner
            .packages
            .read_binary(
                "default",
                super::super::YAML_EXTENSION_ID,
                "templates/done.png"
            )
            .unwrap()
            .is_none());
    }
    #[tokio::test]
    async fn proposal_is_pending_until_user_apply_and_changes_require_revalidation() {
        let (_root, service, input) = fixture();
        let created = service
            .inner
            .dispatch("generation.create", input.clone())
            .await
            .unwrap();
        let id = created["candidate"]["id"].as_str().unwrap();
        let changed = "version: 2\nrun:\n  - fail: proposed\n";
        let staged = service
            .inner
            .diagnostic_call(
                "default",
                "test.yaml",
                Some(id),
                "automation_propose",
                json!({"yaml":changed,"templates":[],"explanation":"test proposal"}),
                Some(1),
            )
            .await
            .unwrap();
        assert_eq!(staged["candidate"]["yaml"], input["yaml"]);
        assert_eq!(staged["candidate"]["revision"], 1);
        let applied=service.inner.dispatch("generation.apply_proposal",json!({"package_id":"default","candidate_id":id,"expected_revision":1,"proposal_id":staged["candidate"]["pending_proposal"]["id"]})).await.unwrap();
        assert_eq!(applied["candidate"]["yaml"], changed);
        assert_eq!(applied["candidate"]["revision"], 2);
        assert!(applied["candidate"]["report"].is_null());
        let scope = crate::extensions::ai::automation::AutomationScope {
            context_id: "selected-before-edit".into(),
            package_id: "default".into(),
            script_id: "test.yaml".into(),
            script_version: None,
            candidate_id: Some(id.to_owned()),
            candidate_revision: Some(1),
            run_id: None,
            device_id: None,
        };
        assert!(check_candidate_scope(
            &scope,
            &service
                .inner
                .read_envelope("default", id)
                .unwrap()
                .candidate
        )
        .is_err());
        for action in [
            "generation.save",
            "generation.rollback",
            "generation.edit",
            "generation.apply_proposal",
        ] {
            assert!(super::super::actions::native_action_expected_caller(
                super::super::YAML_EXTENSION_ID,
                action
            )
            .is_none());
        }
    }
    #[tokio::test]
    async fn diagnostic_template_reads_use_same_frozen_and_current_short_paths() {
        let (_root, service, mut input) = fixture();
        let (_, templates, _) = offline_validation::validation_fixture_samples();
        let png = &templates["claim.png"];
        let source =
            "version: 2\ntargets: {button: {template: ui/button.png}}\nrun: [{finish: button}]\n";
        service
            .inner
            .packages
            .write_text(
                "default",
                super::super::YAML_EXTENSION_ID,
                "automations/scoped.yaml",
                source,
                None,
                false,
            )
            .unwrap();
        for path in [
            "templates/ui/button#100_100_200_200.png",
            "templates/other/button#300_300_400_400.png",
        ] {
            service
                .inner
                .packages
                .write_binary(
                    "default",
                    super::super::YAML_EXTENSION_ID,
                    path,
                    png,
                    None,
                    false,
                )
                .unwrap();
        }
        input["name"] = json!("scoped.yaml");
        input["yaml"] = json!(source);
        let created = service
            .inner
            .dispatch("generation.create", input.clone())
            .await
            .unwrap();
        let id = created["candidate"]["id"].as_str().unwrap();
        for candidate in [None, Some(id)] {
            let result = service
                .inner
                .diagnostic_call(
                    "default",
                    "scoped.yaml",
                    candidate,
                    "automation_read_template",
                    json!({"name":"ui/button.png"}),
                    Some(1),
                )
                .await
                .unwrap();
            assert_eq!(result["resolved_name"], "ui/button#100_100_200_200.png");
            assert_eq!(
                result["content"][0]["data"],
                base64::engine::general_purpose::STANDARD.encode(png)
            );
            for name in ["other/button.png", "button.png", "ui/../other/button.png"] {
                assert!(
                    service
                        .inner
                        .diagnostic_call(
                            "default",
                            "scoped.yaml",
                            candidate,
                            "automation_read_template",
                            json!({"name":name}),
                            Some(1)
                        )
                        .await
                        .is_err(),
                    "{name}"
                );
            }
        }
        service
            .inner
            .packages
            .write_binary(
                "default",
                super::super::YAML_EXTENSION_ID,
                "templates/ui/button#200_200_300_300.png",
                png,
                None,
                false,
            )
            .unwrap();
        let current = service
            .inner
            .diagnostic_call(
                "default",
                "scoped.yaml",
                None,
                "automation_read_template",
                json!({"name":"ui/button.png"}),
                None,
            )
            .await
            .unwrap_err();
        assert!(current.to_string().contains("多个"));
        let ambiguous = service
            .inner
            .dispatch("generation.create", input)
            .await
            .unwrap();
        let ambiguous_id = ambiguous["candidate"]["id"].as_str().unwrap();
        assert!(service
            .inner
            .diagnostic_call(
                "default",
                "scoped.yaml",
                Some(ambiguous_id),
                "automation_read_template",
                json!({"name":"ui/button.png"}),
                Some(1)
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("多个"));
        // The original frozen namespace remains uniquely resolvable.
        assert!(service
            .inner
            .diagnostic_call(
                "default",
                "scoped.yaml",
                Some(id),
                "automation_read_template",
                json!({"name":"ui/button.png"}),
                Some(1)
            )
            .await
            .is_ok());
        service
            .inner
            .packages
            .write_binary(
                "default",
                super::super::YAML_EXTENSION_ID,
                "templates/ui/button.png",
                png,
                None,
                false,
            )
            .unwrap();
        let exact = service
            .inner
            .diagnostic_call(
                "default",
                "scoped.yaml",
                None,
                "automation_read_template",
                json!({"name":"ui/button.png"}),
                None,
            )
            .await
            .unwrap();
        assert_eq!(exact["resolved_name"], "ui/button.png");
    }
    #[tokio::test]
    async fn changed_production_settings_invalidate_report_and_reject_save() {
        let (_root, service, input) = fixture();
        let created = service
            .inner
            .dispatch("generation.create", input)
            .await
            .unwrap();
        let id = created["candidate"]["id"].as_str().unwrap();
        service
            .inner
            .dispatch(
                "generation.validate",
                json!({"package_id":"default","candidate_id":id}),
            )
            .await
            .unwrap();
        let verified = settled(&service, id).await;
        assert_eq!(verified["candidate"]["state"], "passed");
        let old = super::super::settings::load(service.inner.packages.data_root()).unwrap();
        let mut changed = old.clone();
        changed.after_click_ms = 300;
        super::super::settings::dispatch(
            super::super::settings::SAVE_SETTINGS,
            &json!({"expected":old,"settings":changed}),
            service.inner.packages.data_root(),
        )
        .unwrap();
        let current = service
            .inner
            .dispatch(
                "generation.get",
                json!({"package_id":"default","candidate_id":id}),
            )
            .await
            .unwrap();
        assert!(current["candidate"]["report"].is_null());
        assert!(service.inner.dispatch("generation.save",json!({"package_id":"default","candidate_id":id,"expected_revision":1,"expected_version":created["candidate"]["base_version"],"mode":"validated"})).await.unwrap_err().to_string().contains("settings_version_conflict"));
        assert!(service
            .inner
            .packages
            .read_text(
                "default",
                super::super::YAML_EXTENSION_ID,
                "automations/test.yaml"
            )
            .unwrap()
            .is_none());
    }
    #[tokio::test]
    async fn delete_recreate_does_not_resurrect_private_candidates_or_history() {
        let (_root, service, input) = fixture();
        let created = service
            .inner
            .dispatch("generation.create", input)
            .await
            .unwrap();
        let id = created["candidate"]["id"].as_str().unwrap();
        let other = service
            .inner
            .packages
            .data_root()
            .join("extension-data/gamer-yaml/candidates/other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("keep"), b"unchanged").unwrap();
        let history = service
            .inner
            .packages
            .data_root()
            .join("extension-data/gamer-yaml/revisions/default");
        std::fs::create_dir_all(&history).unwrap();
        std::fs::write(history.join("record"), b"private").unwrap();
        let activity = service.inner.packages.acquire_activity("default").unwrap();
        assert!(service.inner.packages.delete_package("default").is_err());
        drop(activity);
        service.inner.packages.delete_package("default").unwrap();
        service.inner.packages.ensure_default_package().unwrap();
        assert!(service.inner.read("default", id).is_err());
        assert!(!history.exists());
        assert!(other.join("keep").exists());
        assert!(service
            .inner
            .dispatch("generation.list", json!({"package_id":"default"}))
            .await
            .unwrap()["candidates"]
            .as_array()
            .unwrap()
            .is_empty());
    }
    #[tokio::test]
    async fn immutable_evidence_and_out_of_bounds_crops_reject_before_model() {
        let (_root, service, mut input) = fixture();
        input["samples"][0]["manifest"]["goal"]["description"] = json!("changed without hash");
        assert!(service
            .inner
            .dispatch("generation.create", input)
            .await
            .is_err());
        let (_root, service, mut input) = fixture();
        input["templates"][0]["rect"] = json!([999, 0, 5, 5]);
        assert!(service
            .inner
            .dispatch("generation.create", input)
            .await
            .is_err());
    }
}

fn compact_report(report: &Value) -> Value {
    json!({"status":report["status"],"candidate_sha256":report["candidate_sha256"],"diagnostics":report["diagnostics"],"samples":report["samples"].as_array().into_iter().flatten().map(|s|json!({"sample_id":s["sample_id"],"status":s["status"],"diagnostics":s["diagnostics"],"consumed_actions":s["consumed_actions"],"recent_events":s["events"].as_array().map(|events|events.iter().rev().take(16).cloned().collect::<Vec<_>>())})).collect::<Vec<_>>()})
}

fn template_referenced(source: &str, name: &str) -> bool {
    fn matches(value: &serde_yaml::Value, name: &str, short: &str) -> bool {
        match value {
            serde_yaml::Value::String(s) => s == name || s == short,
            serde_yaml::Value::Sequence(values) => values.iter().any(|v| matches(v, name, short)),
            _ => false,
        }
    }
    fn visit(value: &serde_yaml::Value, name: &str, short: &str) -> bool {
        match value {
            serde_yaml::Value::Mapping(map) => {
                let template_default = map
                    .get(serde_yaml::Value::String("type".into()))
                    .and_then(serde_yaml::Value::as_str)
                    == Some("template")
                    && map
                        .get(serde_yaml::Value::String("default".into()))
                        .is_some_and(|value| matches(value, name, short));
                template_default
                    || map.iter().any(|(key, value)| {
                        (matches!(
                            key.as_str(),
                            Some(
                                "template"
                                    | "templates"
                                    | "obstacles"
                                    | "find"
                                    | "find_any"
                                    | "wait_find"
                                    | "tap_template"
                                    | "wait_disappear"
                            )
                        ) && matches(value, name, short))
                            || visit(value, name, short)
                    })
            }
            serde_yaml::Value::Sequence(values) => values.iter().any(|v| visit(v, name, short)),
            _ => false,
        }
    }
    serde_yaml::from_str::<serde_yaml::Value>(source)
        .is_ok_and(|value| visit(&value, name, &super::resources::template_short_name(name)))
}

fn verification_metadata(s: &Stored) -> Result<Value> {
    let defaults_match = super::syntax::parse_script(&s.candidate.yaml)
        .ok()
        .and_then(|script| {
            let defaults = super::task_params::bind_entry_args(
                "candidate",
                &script.params,
                &serde_json::Map::new(),
                true,
            )
            .ok()?;
            let tested = super::task_params::bind_entry_args(
                "candidate",
                &script.params,
                &s.candidate.args,
                true,
            )
            .ok()?;
            Some(defaults.resolved == tested.resolved)
        });
    Ok(
        json!({"schema_version":1,"candidate_id":s.candidate.id,"candidate_revision":s.candidate.revision,"script":s.candidate.name,"goal":s.candidate.goal,
        "samples":s.samples.iter().map(|sample|json!({"id":sample.manifest["id"],"content_sha256":sample.manifest["content_sha256"]})).collect::<Vec<_>>(),
        "tested_args":s.candidate.args,"execution_settings":s.candidate.execution_settings,"default_args_match":defaults_match,"other_args_verified":false,"templates":s.candidate.templates,
        "functions_sha256":format!("{:x}",Sha256::digest(serde_json::to_vec(&functions(s)?)?)),
        "candidate_sha256":s.candidate.report.as_ref().map(|report|&report["candidate_sha256"]),"validation_status":s.candidate.report.as_ref().map(|report|&report["status"]),
        "model_version":s.candidate.model_version,"verification_scope":"Only the selected immutable recordings and exact tested arguments; other branches/arguments and real-device/model behavior are not established by offline validation."}),
    )
}

fn sample_file(sample: &Sample, path: &str) -> Result<Vec<u8>> {
    let file = sample
        .files
        .iter()
        .find(|file| file.path == path)
        .context("selected sample file missing")?;
    Ok(base64::engine::general_purpose::STANDARD.decode(&file.base64)?)
}

fn check_run_scope(
    scope: &crate::extensions::ai::automation::AutomationScope,
    record: &Value,
) -> Result<()> {
    ensure!(
        scope.run_id.as_deref() == record["run_id"].as_str(),
        "run identity mismatch"
    );
    ensure!(
        record["runner_id"] == super::YAML_EXTENSION_ID
            && record["entrypoint"] == format!("{}/{}", scope.package_id, scope.script_id),
        "run does not belong to selected automation/package"
    );
    if let Some(device) = scope.device_id.as_ref() {
        ensure!(
            record["device_id"] == *device,
            "run does not belong to selected target"
        );
    }
    Ok(())
}
#[cfg(test)]
mod scope_tests {
    use super::*;
    #[test]
    fn run_scope_never_uses_global_latest_or_other_package_device() {
        let scope = crate::extensions::ai::automation::AutomationScope {
            context_id: "trusted".into(),
            package_id: "p".into(),
            script_id: "a.yaml".into(),
            script_version: None,
            candidate_id: None,
            candidate_revision: None,
            run_id: Some("r".into()),
            device_id: Some("d".into()),
        };
        let run =
            json!({"run_id":"r","runner_id":"gamer-yaml","entrypoint":"p/a.yaml","device_id":"d"});
        assert!(check_run_scope(&scope, &run).is_ok());
        for (key, value) in [
            ("run_id", "newest-global"),
            ("entrypoint", "other/a.yaml"),
            ("device_id", "another-device"),
            ("runner_id", "gamer-ai"),
        ] {
            let mut wrong = run.clone();
            wrong[key] = json!(value);
            assert!(check_run_scope(&scope, &wrong).is_err(), "{key}");
        }
        let mut absent = scope;
        absent.run_id = None;
        assert!(check_run_scope(&absent, &run).is_err());
    }
    #[test]
    fn template_scope_requires_semantic_reference_not_log_or_description() {
        assert!(!template_referenced(
            "version: 2\nname: private.png\nrun: [{log: private.png}]",
            "private.png"
        ));
        assert!(template_referenced(
            "version: 2\ntargets: {done: {template: done.png}}\nrun: [{finish: done}]",
            "done.png"
        ));
        // Production short-name resolution retains the extension; an
        // extensionless "button" cannot resolve a region-suffixed PNG.
        assert!(template_referenced(
            "version: 2\nrun: [{find: button.png}]",
            "button#100_100_200_200.png"
        ));
        assert!(!template_referenced(
            "version: 2\nrun: [{find: button}]",
            "button#100_100_200_200.png"
        ));
    }
    #[test]
    fn model_cannot_self_approve_or_change_fixed_samples() {
        for extra in [
            "approved",
            "user_approved",
            "samples",
            "goal",
            "package_id",
            "path",
        ] {
            let mut proposal =
                json!({"yaml":"version: 2\nrun: []","templates":[],"explanation":"proposal"});
            proposal[extra] = json!(true);
            assert!(
                serde_json::from_value::<Proposal>(proposal).is_err(),
                "{extra}"
            );
        }
    }
}

fn uncertain_template_reference(source: &str) -> bool {
    fn dynamic(value: &serde_yaml::Value) -> bool {
        match value {
            serde_yaml::Value::String(s) => s.starts_with('$'),
            serde_yaml::Value::Sequence(values) => values.iter().any(dynamic),
            serde_yaml::Value::Mapping(map) => map.values().any(dynamic),
            _ => false,
        }
    }
    fn visit(value: &serde_yaml::Value) -> bool {
        match value {
            serde_yaml::Value::Mapping(map) => map.iter().any(|(key, value)| {
                (matches!(
                    key.as_str(),
                    Some(
                        "template"
                            | "templates"
                            | "obstacles"
                            | "find"
                            | "find_any"
                            | "wait_find"
                            | "tap_template"
                            | "wait_disappear"
                    )
                ) && dynamic(value))
                    || visit(value)
            }),
            serde_yaml::Value::Sequence(values) => values.iter().any(visit),
            _ => false,
        }
    }
    serde_yaml::from_str::<serde_yaml::Value>(source).map_or(true, |value| visit(&value))
}

fn check_candidate_scope(
    scope: &crate::extensions::ai::automation::AutomationScope,
    candidate: &Candidate,
) -> Result<()> {
    ensure!(
        scope.candidate_id.as_deref() == Some(&candidate.id)
            && scope.candidate_revision == Some(candidate.revision)
            && scope.package_id == candidate.package_id
            && scope.script_id == candidate.name,
        "automation_context_stale: candidate changed; reattach the current candidate revision"
    );
    Ok(())
}
