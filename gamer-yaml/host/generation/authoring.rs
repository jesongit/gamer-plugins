//! Deterministic script drafts, versions, material checks and publication.
//! Model generation is owned by the external Agent; existing history is retained.
use super::{
    authoring_support::{base64_map, decode_files, required, script_name, Job, Sample},
    offline_validation::{self, ValidationRequest},
    private_trial::{PrivateTrialExecutor, PrivateTrialRuntime, TrialInput},
    revisions, syntax,
};
use crate::{resources::PackageStore, run_manager::RunManager};
use anyhow::{ensure, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
#[path = "authoring_lifecycle.rs"]
mod lifecycle;
#[cfg(test)]
#[path = "authoring_tests.rs"]
mod tests;

pub(crate) const ACTIONS: &[&str] = &[
    "authoring.list",
    "authoring.get",
    "authoring.create",
    "authoring.cancel",
    "authoring.save",
    "authoring.publish",
    "authoring.validate",
    "authoring.run",
    "authoring.delete_preview",
    "authoring.delete",
    "authoring.import",
    "authoring.crop_template",
    "authoring.template",
    "authoring.match_template",
];

/// A host-supplied admission check; no caller identity is accepted from JSON.
pub(super) type Admission = Arc<dyn Fn() -> Result<()> + Send + Sync>;
fn admit(admission: &Option<Admission>) -> Result<()> {
    if let Some(check) = admission {
        check()?;
    }
    Ok(())
}

#[derive(Clone)]
pub(crate) struct AuthoringService {
    pub(super) inner: Arc<AuthoringState>,
}
pub(super) struct AuthoringState {
    pub(super) packages: Arc<PackageStore>,
    _runs: Arc<RunManager>,
    authoring_gates: Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pub(super) jobs: Arc<Mutex<BTreeMap<String, Job>>>,
    pub(super) gate: Arc<Mutex<()>>,
    pub(super) diagnostics: Arc<Mutex<Option<crate::store::Db>>>,
    pub(super) trials: Arc<PrivateTrialRuntime>,
}
impl AuthoringService {
    pub(crate) fn new(packages: Arc<PackageStore>, runs: Arc<RunManager>) -> Self {
        Self {
            inner: Arc::new(AuthoringState {
                packages,
                trials: Arc::new(PrivateTrialRuntime::new(runs.clone())),
                _runs: runs,
                authoring_gates: Mutex::new(BTreeMap::new()),
                jobs: Arc::new(Mutex::new(BTreeMap::new())),
                gate: Arc::new(Mutex::new(())),
                diagnostics: Arc::new(Mutex::new(None)),
            }),
        }
    }
    pub(crate) fn with_diagnostics(self, db: crate::store::Db) -> Self {
        *self.inner.diagnostics.lock() = Some(db);
        self
    }
    pub(crate) fn with_devices(self, devices: Arc<crate::device::DeviceManager>) -> Self {
        *self.inner.trials.devices.lock() = Some(devices);
        self
    }
    pub(crate) fn with_event_sink(self, sink: Arc<dyn crate::core::events::EventSink>) -> Self {
        *self.inner.trials.event_sink.lock() = Some(sink);
        self
    }
    pub(crate) fn attach(&self, extensions: &Arc<crate::extensions::ExtensionService>) {
        *self.inner.trials.extensions.lock() = Arc::downgrade(extensions);
    }
    pub(crate) async fn call(&self, action: &str, values: Value) -> Result<Value> {
        ensure!(
            ACTIONS.contains(&action),
            "unknown authoring action: {action}"
        );
        self.inner.authoring_call(action, values).await
    }
    pub(crate) async fn call_admitted(
        &self,
        action: &str,
        values: Value,
        admission: Admission,
    ) -> Result<Value> {
        ensure!(
            ACTIONS.contains(&action),
            "unknown authoring action: {action}"
        );
        self.inner
            .authoring_call_with_policy(action, values, None, Some(admission))
            .await
    }
    // Keep the frozen source, templates, materials and admission as explicit inputs.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn import_candidate(
        &self,
        package: &str,
        script: &str,
        candidate: &Value,
        templates: BTreeMap<String, Vec<u8>>,
        materials: Vec<Value>,
        functions: BTreeMap<String, String>,
        admission: Option<Admission>,
    ) -> Result<Value> {
        admit(&admission)?;
        let _activity = self.inner.packages.acquire_activity(package)?;
        let script = script_name(script)?;
        let gate = self
            .inner
            .authoring_gates
            .lock()
            .entry(format!("{package}/{script}"))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _draft = gate.lock().await;
        admit(&admission)?;
        let _gate = self.inner.gate.lock();
        ensure!(
            !self.inner.authoring_path(package, &script)?.exists(),
            "authoring.draft_exists: choose a new script_id"
        );
        ensure!(
            self.inner
                .packages
                .read_text(package, "gamer-yaml", &format!("automations/{script}"))?
                .is_none(),
            "authoring.formal_exists: import the formal script or choose a new script_id"
        );
        let yaml = required(candidate, "yaml")?.to_owned();
        let base = functions
            .iter()
            .map(|(path, source)| (path.clone(), source.as_bytes().to_vec()))
            .collect();
        let diagnostics = self.inner.authoring_diagnostics(&yaml, &base, &templates);
        let mut doc = self.inner.authoring_read(package, &script)?;
        doc.extra
            .insert("origin_candidate_id".into(), candidate["id"].clone());
        doc.extra.insert(
            "origin_candidate_revision".into(),
            candidate["revision"].clone(),
        );
        doc.extra
            .insert("origin_candidate_metadata".into(), candidate.clone());
        doc.versions.push(Version {
            extra: Map::new(),
            version: "0.1".into(),
            yaml,
            created_at: chrono::Utc::now().to_rfc3339(),
            instruction: "Explicit import of frozen legacy candidate".into(),
            source: "candidate_import".into(),
            published: false,
            diagnostics,
            usage: Value::Null,
            material_refs: materials,
            template_refs: vec![],
            frozen_functions: Some(functions),
            generated_templates: templates,
            crop_checks: BTreeMap::new(),
        });
        admit(&admission)?;
        self.inner.authoring_write(package, &doc)?;
        Ok(json!({"script":self.inner.authoring_view(&doc)}))
    }
    pub(crate) async fn run_external(
        &self,
        values: Value,
        policy: crate::core::ExecutionPolicy,
        admission: Admission,
    ) -> Result<Value> {
        self.inner
            .authoring_call_with_policy("authoring.run", values, Some(policy), Some(admission))
            .await
    }
    pub(crate) fn runs(&self) -> Arc<RunManager> {
        self.inner._runs.clone()
    }
    pub(crate) async fn stop(&self) {
        self.inner.trials.stop().await;
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Version {
    #[serde(flatten)]
    extra: Map<String, Value>,
    version: String,
    yaml: String,
    created_at: String,
    instruction: String,
    source: String,
    published: bool,
    diagnostics: Value,
    usage: Value,
    #[serde(default)]
    material_refs: Vec<Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    template_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frozen_functions: Option<BTreeMap<String, String>>,
    #[serde(default, with = "base64_map")]
    generated_templates: BTreeMap<String, Vec<u8>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    crop_checks: BTreeMap<String, Value>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Document {
    #[serde(default)]
    id: String,
    #[serde(default = "legacy_run_binding")]
    legacy_run_binding: bool,
    #[serde(default)]
    private_run_ids: Vec<String>,
    script_id: String,
    versions: Vec<Version>,
    #[serde(flatten)]
    extra: Map<String, Value>,
    published_version: Option<String>,
    published_resource_hash: Option<String>,
    job: Option<Value>,
}
fn legacy_run_binding() -> bool {
    true
}
fn revision(major: u32, minor: u32) -> String {
    format!("{major}.{minor}")
}
fn number(version: &str) -> (u32, u32) {
    let (major, minor) = version.split_once('.').expect("internal script revision");
    (major.parse().expect("major"), minor.parse().expect("minor"))
}
impl Document {
    fn head(&self) -> &str {
        self.versions
            .last()
            .map(|v| v.version.as_str())
            .unwrap_or("")
    }
    fn minor(&self) -> String {
        let major = self
            .published_version
            .as_deref()
            .map(number)
            .map(|v| v.0)
            .unwrap_or(0);
        let minor = self
            .versions
            .iter()
            .map(|v| number(&v.version))
            .filter(|v| v.0 == major)
            .map(|v| v.1)
            .max()
            .unwrap_or(0)
            + 1;
        revision(major, minor)
    }
}
impl AuthoringState {
    fn authoring_path(&self, package: &str, script: &str) -> Result<PathBuf> {
        self.packages.manifest(package)?;
        let root = self
            .packages
            .data_root()
            .join("extension-data/gamer-yaml/authoring")
            .join(package);
        Ok(root.join(format!("{:x}.json", Sha256::digest(script.as_bytes()))))
    }
    fn authoring_read(&self, package: &str, script: &str) -> Result<Document> {
        let path = self.authoring_path(package, script)?;
        if path.exists() {
            let doc: Document = serde_json::from_slice(&std::fs::read(path)?)?;
            ensure!(doc.script_id == script, "authoring.draft_identity_invalid");
            return Ok(doc);
        }
        let doc = Document {
            id: uuid::Uuid::new_v4().to_string(),
            legacy_run_binding: false,
            private_run_ids: vec![],
            script_id: script.into(),
            versions: vec![],
            extra: Map::new(),
            published_version: None,
            published_resource_hash: None,
            job: None,
        };
        Ok(doc)
    }
    fn authoring_write(&self, package: &str, doc: &Document) -> Result<()> {
        ensure!(
            serde_json::to_vec(doc)?.len() <= 64 * 1024 * 1024,
            "script history exceeds64MiB"
        );
        crate::core::fs::atomic_write(
            &self.authoring_path(package, &doc.script_id)?,
            &serde_json::to_vec(doc)?,
        )
    }
    fn authoring_view(&self, doc: &Document) -> Value {
        let mut value = serde_json::to_value(doc).unwrap();
        for version in value["versions"].as_array_mut().unwrap() {
            version
                .as_object_mut()
                .unwrap()
                .remove("generated_templates");
            // Derived from this immutable source, never the current formal entrypoint.
            version["parameter_schema"] = version["yaml"]
                .as_str()
                .and_then(|yaml| syntax::parse_script(yaml).ok())
                .map(|script| {
                    crate::extensions::gamer_yaml::task_params::decls_schema_json(&script.params)
                })
                .unwrap_or(Value::Null);
        }
        if let Some(job) = value
            .get_mut("job")
            .filter(|job| job["state"] == "generating")
        {
            if !self
                .jobs
                .lock()
                .contains_key(job["id"].as_str().unwrap_or(""))
            {
                job["state"] = json!("interrupted");
            }
        }
        value["head_version"] = json!(doc.head());
        value
    }
    fn authoring_diagnostics(
        &self,
        yaml: &str,
        base: &BTreeMap<String, Vec<u8>>,
        templates: &BTreeMap<String, Vec<u8>>,
    ) -> Value {
        let script = match syntax::parse_script(yaml) {
            Ok(script) => script,
            Err(errors) => return json!(errors),
        };
        let mut names = super::native_funcs::native_names();
        let mut errors = vec![];
        for (path, bytes) in base
            .iter()
            .filter(|(path, _)| path.starts_with("automations/") && path.ends_with(".yaml"))
        {
            match std::str::from_utf8(bytes)
                .ok()
                .and_then(|source| syntax::callable_source(path, source).ok())
            {
                Some(library) => {
                    for (name, _) in library {
                        if !names.insert(name.clone()) {
                            errors.push(json!({"code":"yaml.function.conflict","path":path,"message":format!("函数重名：{name}")}));
                        }
                    }
                }
                None => errors.push(
                    json!({"code":"yaml.function.invalid","path":path,"message":"函数库无法解析"}),
                ),
            }
        }
        for called in script.called_functions() {
            if !names.contains(&called) {
                errors.push(json!({"code":"yaml.function.missing","path":"run","message":format!("不存在的函数：{called}")}));
            }
        }
        fn literal(expr: &Value) -> Option<Value> {
            match expr["expr"].as_str()? {
                "lit" => Some(expr["value"].clone()),
                "map" => Some(Value::Object(
                    expr["value"]
                        .as_object()?
                        .iter()
                        .map(|(key, value)| Some((key.clone(), literal(value)?)))
                        .collect::<Option<Map<String, Value>>>()?,
                )),
                "list" => Some(Value::Array(
                    expr["value"]
                        .as_array()?
                        .iter()
                        .map(literal)
                        .collect::<Option<Vec<_>>>()?,
                )),
                _ => None,
            }
        }
        fn check_calls(value: &Value, errors: &mut Vec<Value>) {
            if value["op"] == "fn" {
                if let Some(function) = value["fn"]
                    .as_str()
                    .and_then(super::native_funcs::native_function)
                {
                    if let Some(args) = literal(&value["args"]) {
                        let values = match args {
                            Value::Null => Map::new(),
                            Value::Object(map) => map,
                            value => function
                                .params
                                .first()
                                .map(|p| Map::from_iter([(p.name.to_string(), value)]))
                                .unwrap_or_default(),
                        };
                        for (key, value) in &values {
                            match function.params.iter().find(|p|p.name==key){Some(p)=>if let Err(message)=syntax::check_type(p.ty,value){errors.push(json!({"code":"yaml.args.invalid","path":"run","message":format!("{}.{key}: {message}",function.name)}));},None=>errors.push(json!({"code":"yaml.args.unknown","path":"run","message":format!("{} 不支持参数 {key}",function.name)}))}
                        }
                        for param in &function.params {
                            if param.required
                                && param.default.is_none()
                                && !values.contains_key(param.name)
                            {
                                errors.push(json!({"code":"yaml.args.required","path":"run","message":format!("{} 缺少参数 {}",function.name,param.name)}));
                            }
                        }
                    }
                }
            }
            if let Some(map) = value.as_object() {
                for child in map.values() {
                    check_calls(child, errors)
                }
            } else if let Some(list) = value.as_array() {
                for child in list {
                    check_calls(child, errors)
                }
            }
        }
        let program = syntax::build_program(&script, &vec![], Map::new(), 0);
        check_calls(&program, &mut errors);
        fn check_templates(
            value: &Value,
            templates: &BTreeMap<String, Vec<u8>>,
            errors: &mut Vec<Value>,
        ) {
            if let Some(map) = value.as_object() {
                for (key, value) in map {
                    if key == "template" {
                        if let Some(name) = value
                            .as_str()
                            .or_else(|| {
                                if value["expr"] == "lit" {
                                    value["value"].as_str()
                                } else {
                                    None
                                }
                            })
                            .filter(|name| !name.starts_with('$'))
                        {
                            if crate::resources::resolve_short_relative_path(
                                name,
                                templates.keys().map(String::as_str),
                            )
                            .is_err()
                            {
                                errors.push(json!({"code":"yaml.template.missing","path":"targets","message":format!("素材未找到或不唯一：{name}")}));
                            }
                        }
                    }
                    check_templates(value, templates, errors);
                }
            } else if let Some(list) = value.as_array() {
                for value in list {
                    check_templates(value, templates, errors);
                }
            }
        }
        if let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(yaml) {
            check_templates(
                &serde_json::to_value(value).unwrap(),
                templates,
                &mut errors,
            );
        }
        check_templates(&program, templates, &mut errors);
        json!(errors)
    }
    pub(super) async fn authoring_call(
        self: &Arc<Self>,
        action: &str,
        args: Value,
    ) -> Result<Value> {
        self.authoring_call_with_policy(action, args, None, None)
            .await
    }
    async fn authoring_call_with_policy(
        self: &Arc<Self>,
        action: &str,
        args: Value,
        execution_policy: Option<crate::core::ExecutionPolicy>,
        admission: Option<Admission>,
    ) -> Result<Value> {
        admit(&admission)?;
        let package = required(&args, "package_id")?;
        let _activity = self.packages.acquire_activity(package)?;
        if action == "authoring.list" {
            let mut names = BTreeSet::new();
            let root = self
                .packages
                .data_root()
                .join("extension-data/gamer-yaml/authoring")
                .join(package);
            if root.exists() {
                for entry in std::fs::read_dir(root)? {
                    let entry = entry?;
                    if entry.path().extension().is_some_and(|ext| ext == "json") {
                        let bytes = match std::fs::read(entry.path()) {
                            Ok(bytes) => bytes,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                            Err(error) => return Err(error.into()),
                        };
                        let doc: Document = serde_json::from_slice(&bytes)?;
                        names.insert(doc.script_id);
                    }
                }
            }
            let scripts = names.iter().map(|name| { let doc = self.authoring_read(package, name)?; Ok(json!({"script_id":name,"head_version":doc.head(),"published_version":doc.published_version})) }).collect::<Result<Vec<Value>>>()?;
            return Ok(json!({"scripts":scripts}));
        }
        let script = script_name(required(&args, "script_id")?)?;
        let gate = self
            .authoring_gates
            .lock()
            .entry(format!("{package}/{script}"))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let authoring_guard = gate.lock().await;
        admit(&admission)?;
        if matches!(
            action,
            "authoring.delete_preview" | "authoring.delete" | "authoring.import"
        ) {
            return self
                .authoring_lifecycle(action, package, &script, &args, &admission)
                .await;
        }
        let mut doc = self.authoring_read(package, &script)?;
        // Reading an old document must preserve its exact bytes. Assign its
        // durable identity only as part of an already requested mutation.
        if doc.id.is_empty()
            && matches!(
                action,
                "authoring.save" | "authoring.publish" | "authoring.crop_template"
            )
        {
            doc.id = uuid::Uuid::new_v4().to_string();
        }
        if action == "authoring.get" {
            ensure!(
                self.authoring_path(package, &script)?.exists(),
                "authoring.draft_not_found"
            );
            return Ok(json!({"script":self.authoring_view(&doc)}));
        }
        if action == "authoring.cancel" {
            if let Some(id) = doc.job.as_ref().and_then(|j| j["id"].as_str()) {
                if let Some(job) = self.jobs.lock().get(id) {
                    job.cancel.store(true, Ordering::Release);
                }
            }
            return Ok(json!({"cancelled":true}));
        }
        if action == "authoring.create" {
            let _gate = self.gate.lock();
            ensure!(
                self.packages
                    .read_text(
                        package,
                        super::YAML_EXTENSION_ID,
                        &format!("automations/{script}")
                    )?
                    .is_none(),
                "authoring.import_required: 正式脚本请通过导入建立工作稿"
            );
            ensure!(
                !self.authoring_path(package, &script)?.exists() && doc.versions.is_empty(),
                "script already exists"
            );
            admit(&admission)?;
            self.authoring_write(package, &doc)?;
            return Ok(json!({"script":self.authoring_view(&doc)}));
        }
        ensure!(
            self.authoring_path(package, &script)?.exists(),
            "authoring.draft_not_found"
        );
        let mut base = revisions::snapshot(&self.packages, package)?;
        base.retain(|path, _| path.starts_with("automations/") || path.starts_with("templates/"));
        let expected = args["expected_version"]
            .as_str()
            .context("expected_version required")?;
        ensure!(
            expected == doc.head(),
            "script_version_conflict: 请刷新版本后重试"
        );
        ensure!(
            !doc.job.as_ref().is_some_and(|job| self
                .jobs
                .lock()
                .contains_key(job["id"].as_str().unwrap_or(""))),
            "script_busy"
        );
        let chosen = args["version"].as_str().unwrap_or(doc.head());
        let prior = doc.versions.iter().find(|v| v.version == chosen).cloned();
        if let Some(frozen) = prior
            .as_ref()
            .and_then(|version| version.frozen_functions.as_ref())
        {
            base.retain(|path, _| !path.starts_with("automations/"));
            base.extend(
                frozen
                    .iter()
                    .map(|(path, source)| (path.clone(), source.as_bytes().to_vec())),
            );
        }
        if let Some(prior) = &prior {
            for (name, bytes) in &prior.generated_templates {
                base.insert(format!("templates/{name}"), bytes.clone());
            }
        }
        let templates: BTreeMap<_, _> = base
            .iter()
            .filter_map(|(path, bytes)| {
                path.strip_prefix("templates/")
                    .map(|name| (name.to_string(), bytes.clone()))
            })
            .collect();
        if action == "authoring.validate" {
            let source = prior.context("script version not found")?;
            let diagnostics = self.authoring_diagnostics(&source.yaml, &base, &templates);
            if !diagnostics.as_array().is_some_and(Vec::is_empty) || source.material_refs.is_empty()
            {
                return Ok(
                    json!({"version":source.version,"diagnostics":diagnostics,"scope":"syntax_and_references"}),
                );
            }
            let mut samples = vec![];
            for material in &source.material_refs {
                let sample =
                    super::authoring_materials::read_reference(&self.packages, package, material)?;
                samples.push(offline_validation::SampleInput {
                    manifest: sample.manifest.clone(),
                    files: decode_files(&sample)?,
                });
            }
            let functions_sources = base
                .iter()
                .filter(|(path, _)| path.starts_with("automations/") && path.ends_with(".yaml"))
                .map(|(path, bytes)| Ok((path.clone(), String::from_utf8(bytes.clone())?)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            // Validation uses this frozen snapshot and need not block lifecycle
            // operations on the draft while matching thousands of sample frames.
            drop(authoring_guard);
            let report = offline_validation::validate_candidate(
                ValidationRequest {
                    yaml: source.yaml.clone(),
                    templates,
                    samples,
                    args: Map::new(),
                    settings: super::settings::load(self.packages.data_root())?,
                    functions_sources,
                },
                Arc::new(AtomicBool::new(false)),
            )
            .await?;
            let findings: Vec<_> = report
                .samples
                .iter()
                .flat_map(|sample| sample.diagnostics.iter().cloned())
                .chain(report.diagnostics.iter().cloned())
                .collect();
            return Ok(
                json!({"version":source.version,"diagnostics":findings,"report":report,"scope":"screenshot_templates"}),
            );
        }
        if matches!(
            action,
            "authoring.template" | "authoring.match_template" | "authoring.crop_template"
        ) {
            let source = prior.context("script version not found")?;
            let name = required(&args, "name")?;
            crate::resources::sanitize_rel_path(name)?;
            ensure!(
                !name.contains('/')
                    && !name.contains('\\')
                    && name.ends_with(".png")
                    && name.len() <= 128,
                "invalid private template name"
            );
            if action == "authoring.template" {
                let resolved = crate::resources::resolve_short_relative_path(
                    name,
                    templates.keys().map(String::as_str),
                )?;
                let bytes = &templates[&resolved];
                let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)?;
                return Ok(
                    json!({"name":resolved,"base64":base64::Engine::encode(&base64::engine::general_purpose::STANDARD,bytes),
                    "mime_type":"image/png","width":image.width(),"height":image.height(),"sha256":format!("{:x}",Sha256::digest(bytes))}),
                );
            }
            let reference = args.get("sample_ref").context("sample_ref required")?;
            let sample =
                super::authoring_materials::read_reference(&self.packages, package, reference)?;
            let png = super::authoring_materials::frame_png(&sample, required(&args, "frame_id")?)?;
            if action == "authoring.match_template" {
                let resolved = crate::resources::resolve_short_relative_path(
                    name,
                    templates.keys().map(String::as_str),
                )?;
                let threshold = args["threshold"].as_f64().unwrap_or(0.9);
                ensure!(
                    threshold.is_finite() && (0.0..=1.0).contains(&threshold),
                    "invalid threshold"
                );
                let pixels = crate::matcher::DecodedFrame::from_png(&png)?;
                let region: Option<[u32; 4]> = args
                    .get("region")
                    .map(|value| serde_json::from_value(value.clone()))
                    .transpose()?;
                let req = crate::matcher::DecodedMatchRequest {
                    template_png: templates[&resolved].clone(),
                    threshold: Some(threshold as f32),
                    region,
                    color: crate::matcher::template_color_from_name(&resolved),
                };
                drop(authoring_guard);
                let matched = crate::matcher::compute::run(move || {
                    crate::matcher::match_decoded_frame(&pixels, &req)
                })
                .await??;
                return Ok(
                    json!({"scope":"single_screenshot_template","real_execution":false,"match":matched,"found":matched.is_some(),"template":resolved}),
                );
            }
            let [x, y, width, height]: [u32; 4] = serde_json::from_value(args["rect"].clone())?;
            let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)?;
            ensure!(
                width > 0
                    && height > 0
                    && u64::from(width) * u64::from(height) <= 16 * 1024 * 1024
                    && x.checked_add(width).is_some_and(|v| v <= image.width())
                    && y.checked_add(height).is_some_and(|v| v <= image.height()),
                "crop outside original PNG"
            );
            let mut cropped = std::io::Cursor::new(Vec::new());
            image
                .crop_imm(x, y, width, height)
                .to_luma8()
                .write_to(&mut cropped, image::ImageFormat::Png)?;
            let bytes = cropped.into_inner();
            ensure!(bytes.len() <= 8 * 1024 * 1024, "private template too large");
            if let Some(old) = templates.get(name) {
                ensure!(old == &bytes, "template_name_conflict: choose a new name");
            }
            let mut next = source.clone();
            next.version = doc.minor();
            next.published = false;
            next.source = "external_crop".into();
            next.created_at = chrono::Utc::now().to_rfc3339();
            next.usage = Value::Null;
            if !next.material_refs.contains(reference) {
                next.material_refs.push(reference.clone());
            }
            ensure!(
                next.material_refs.len() <= 12,
                "too many material references"
            );
            next.generated_templates.insert(name.into(), bytes);
            next.crop_checks.insert(name.into(),json!({"source":reference,"frame_id":args["frame_id"],"rect":[x,y,width,height],"frame_sha256":format!("{:x}",Sha256::digest(&png)),"status":"private_crop_created"}));
            let mut next_templates = templates.clone();
            next_templates.extend(next.generated_templates.clone());
            next.diagnostics = self.authoring_diagnostics(&next.yaml, &base, &next_templates);
            let _gate = self.gate.lock();
            let latest = self.authoring_read(package, &script)?;
            ensure!(latest.head() == expected, "script_version_conflict");
            doc.versions.push(next);
            admit(&admission)?;
            self.authoring_write(package, &doc)?;
            return Ok(json!({"script":self.authoring_view(&doc)}));
        }
        if action == "authoring.run" {
            let device = required(&args, "device_id")?.to_string();
            let source = prior.context("script version not found")?;
            let diagnostics = self.authoring_diagnostics(&source.yaml, &base, &templates);
            ensure!(
                diagnostics.as_array().is_some_and(Vec::is_empty),
                "script_invalid: {diagnostics}"
            );
            let run_args: Map<String, Value> =
                serde_json::from_value(args.get("args").cloned().unwrap_or_else(|| json!({})))?;
            let functions_sources = base
                .iter()
                .filter(|(path, _)| path.starts_with("automations/") && path.ends_with(".yaml"))
                .map(|(path, bytes)| Ok((path.clone(), String::from_utf8(bytes.clone())?)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            let request = ValidationRequest {
                yaml: source.yaml.clone(),
                templates: templates.clone(),
                samples: vec![],
                args: run_args,
                settings: super::settings::load(self.packages.data_root())?,
                functions_sources: functions_sources.clone(),
            };
            let mut program = offline_validation::prepare_program_value(&request)?;
            program["_source_files"] = json!(functions_sources);
            program["_source_files"][format!("automations/{script}")] = json!(source.yaml);
            program["_native_settings"] = json!(request.settings);
            program["_script_revision"] = json!(source.version);
            if let Some(policy) = &execution_policy {
                super::private_trial::validate_external_policy(&program, policy)?;
            }
            if doc.id.is_empty() {
                doc.id = uuid::Uuid::new_v4().to_string();
                admit(&admission)?;
                self.authoring_write(package, &doc)?;
            }
            program["_authoring_document"] = json!(doc.id);
            let devices = self
                .trials
                .devices
                .lock()
                .clone()
                .context("device service unavailable")?;
            let app = crate::targets::app_context(
                &devices,
                &device,
                Some(crate::core::AppPackageId::new(package)?),
            )?;
            let token = uuid::Uuid::new_v4().to_string();
            self.trials
                .pending_trials
                .lock()
                .insert(token.clone(), TrialInput { program, templates });
            self._runs.register_executor(
                "gamer-yaml-version",
                Arc::new(PrivateTrialExecutor {
                    inner: Arc::downgrade(&self.trials),
                }),
            );
            let mut run_request = crate::core::RunRequest::for_app(
                app,
                "gamer-yaml-version",
                format!("{package}/{script}"),
                crate::core::RunPayload::new(json!({"trial_token":token})),
            )?;
            run_request.execution_policy = execution_policy;
            if let Err(error) = admit(&admission) {
                self.trials.pending_trials.lock().remove(&token);
                return Err(error);
            }
            let run = match self._runs.submit(
                crate::run_manager::StartRequest {
                    request: run_request,
                    source: crate::run_manager::RunSource::Manual,
                    task_id: None,
                    scheduled_at: None,
                    realtime_logs: true,
                },
                None,
            ) {
                Ok(run) => run,
                Err(error) => {
                    self.trials.pending_trials.lock().remove(&token);
                    anyhow::bail!("{error:?}")
                }
            };
            let run_id = run.run_id.clone();
            self.trials.active_trials.lock().insert(run_id.clone());
            let inner = self.clone();
            let cleanup_id = run_id.clone();
            tokio::spawn(async move {
                inner._runs.wait_terminal(&cleanup_id).await;
                inner.trials.active_trials.lock().remove(&cleanup_id);
                inner.trials.pending_trials.lock().remove(&token);
            });
            if !source.published {
                doc.private_run_ids.push(run_id.clone());
                if let Err(error) =
                    admit(&admission).and_then(|_| self.authoring_write(package, &doc))
                {
                    // A submitted run must stay registered for cancellation and
                    // lease cleanup even when its durable owner receipt fails.
                    self._runs.cancel(&run_id);
                    self._runs.wait_terminal(&run_id).await;
                    anyhow::bail!("authoring.run_tracking_failed: 私有试跑登记失败，已请求取消 {run_id}: {error}");
                }
            }
            return Ok(json!({"run_id":run_id,"version":source.version}));
        }
        if action == "authoring.save" {
            let source = required(&args, "yaml")?;
            ensure!(source.len() <= 512 * 1024, "script too large");
            let version = doc.minor();
            let material_refs: Vec<Value> = match args.get("material_refs") {
                Some(value) => serde_json::from_value(value.clone())?,
                None => prior
                    .as_ref()
                    .map(|version| version.material_refs.clone())
                    .unwrap_or_default(),
            };
            ensure!(
                material_refs.len() <= 12,
                "material reference budget exceeded"
            );
            if args.get("material_refs").is_some() {
                for reference in &material_refs {
                    super::authoring_materials::read_reference(&self.packages, package, reference)?;
                }
            }
            let template_refs: Vec<String> = match args.get("template_refs") {
                Some(value) => serde_json::from_value(value.clone())?,
                None => prior
                    .as_ref()
                    .map(|version| version.template_refs.clone())
                    .unwrap_or_default(),
            };
            ensure!(
                template_refs.len() <= 64,
                "template reference budget exceeded"
            );
            let mut generated_templates = prior
                .as_ref()
                .map(|version| version.generated_templates.clone())
                .unwrap_or_default();
            if args.get("template_refs").is_some() {
                let formal = revisions::snapshot(&self.packages, package)?;
                let formal_templates: BTreeMap<_, _> = formal
                    .into_iter()
                    .filter_map(|(path, bytes)| {
                        path.strip_prefix("templates/")
                            .map(|name| (name.to_owned(), bytes))
                    })
                    .collect();
                for name in &template_refs {
                    let resolved = crate::resources::resolve_short_relative_path(
                        name,
                        formal_templates.keys().map(String::as_str),
                    )?;
                    generated_templates
                        .insert(resolved.clone(), formal_templates[&resolved].clone());
                }
            }
            let mut checked_templates = templates.clone();
            checked_templates.extend(generated_templates.clone());
            let diagnostics = self.authoring_diagnostics(source, &base, &checked_templates);
            let _gate = self.gate.lock();
            let latest = self.authoring_read(package, &script)?;
            ensure!(latest.head() == expected, "script_version_conflict");
            ensure!(
                !latest.job.as_ref().is_some_and(|job| self
                    .jobs
                    .lock()
                    .contains_key(job["id"].as_str().unwrap_or(""))),
                "script_busy"
            );
            doc.versions.push(Version {
                extra: Map::new(),
                version,
                yaml: source.into(),
                created_at: chrono::Utc::now().to_rfc3339(),
                instruction: args["instruction"].as_str().unwrap_or("").into(),
                source: "manual".into(),
                published: false,
                diagnostics,
                usage: Value::Null,
                material_refs,
                template_refs,
                frozen_functions: prior
                    .as_ref()
                    .and_then(|version| version.frozen_functions.clone()),
                crop_checks: prior
                    .as_ref()
                    .map(|v| v.crop_checks.clone())
                    .unwrap_or_default(),
                generated_templates,
            });
            admit(&admission)?;
            self.authoring_write(package, &doc)?;
            return Ok(json!({"script":self.authoring_view(&doc)}));
        }
        if action == "authoring.publish" {
            let source = prior.context("script version not found")?;
            let diagnostics = self.authoring_diagnostics(&source.yaml, &base, &templates);
            ensure!(
                diagnostics.as_array().is_some_and(Vec::is_empty),
                "publish_invalid: {diagnostics}"
            );
            let _gate = self.gate.lock();
            return self.packages.with_package_write(package, || {
                let latest = self.authoring_read(package, &script)?;
                ensure!(latest.head() == expected, "script_version_conflict");
                ensure!(
                    !latest.job.as_ref().is_some_and(|job| self
                        .jobs
                        .lock()
                        .contains_key(job["id"].as_str().unwrap_or(""))),
                    "script_busy"
                );
                let current = self
                    .packages
                    .read_text(
                        package,
                        super::YAML_EXTENSION_ID,
                        &format!("automations/{script}"),
                    )?
                    .map(|v| format!("{:x}", Sha256::digest(v.content.as_bytes())));
                ensure!(
                    current == doc.published_resource_hash,
                    "published_script_changed: 脚本库内容已被其他编辑修改"
                );
                if let Some(expected) = args.get("expected_source_sha256") {
                    ensure!(
                        expected.as_str()
                            == Some(
                                format!("{:x}", Sha256::digest(source.yaml.as_bytes())).as_str()
                            ),
                        "publish_source_changed: selected source SHA256 changed"
                    );
                    let expected_resource = args
                        .get("expected_resource_sha256")
                        .context("publish_resource_pin_required")?;
                    ensure!(
                        expected_resource.is_null() || expected_resource.is_string(),
                        "invalid publication resource pin"
                    );
                    ensure!(
                        expected_resource.as_str() == current.as_deref(),
                        "published_script_changed: formal resource SHA256 changed"
                    );
                }
                let version = revision(
                    doc.published_version
                        .as_deref()
                        .map(number)
                        .map(|v| v.0 + 1)
                        .unwrap_or(1),
                    0,
                );
                let mut changes = BTreeMap::from([(
                    format!("automations/{script}"),
                    source.yaml.as_bytes().to_vec(),
                )]);
                for (name, bytes) in &source.generated_templates {
                    let path = format!("templates/{name}");
                    if let Some(current) =
                        self.packages
                            .read_binary(package, super::YAML_EXTENSION_ID, &path)?
                    {
                        ensure!(
                            current == *bytes,
                            "template_conflict: 素材 {name} 已存在且内容不同，请使用新名称"
                        );
                    }
                    changes.insert(path, bytes.clone());
                }
                let mut published_doc = doc.clone();
                published_doc.published_resource_hash =
                    Some(format!("{:x}", Sha256::digest(source.yaml.as_bytes())));
                published_doc.published_version = Some(version.clone());
                published_doc.versions.push(Version {
                    version: version.clone(),
                    published: true,
                    source: "publish".into(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    ..source.clone()
                });
                ensure!(
                    serde_json::to_vec(&published_doc)?.len() <= 64 * 1024 * 1024,
                    "script history exceeds64MiB"
                );
                // Once admitted, complete the shared publication transaction and
                // history write even if revocation races its durable commit.
                admit(&admission)?;
                revisions::commit(
                    &self.packages,
                    package,
                    &revisions::version(&self.packages, package)?,
                    &changes,
                    &format!("{script}@{version}"),
                    "published",
                    false,
                )?;
                doc.published_resource_hash =
                    Some(format!("{:x}", Sha256::digest(source.yaml.as_bytes())));
                doc.published_version = Some(version.clone());
                doc.versions.push(Version {
                    version,
                    published: true,
                    source: "publish".into(),
                    created_at: chrono::Utc::now().to_rfc3339(),
                    ..source
                });
                self.authoring_write(package, &doc)?;
                Ok(json!({"script":self.authoring_view(&doc)}))
            });
        }
        anyhow::bail!("unknown authoring action: {action}")
    }
}
