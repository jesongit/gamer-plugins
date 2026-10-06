//! Deterministic sample validation using the production parser, interpreter,
//! NativeYamlHost and NCC matcher. This module has no live device/network
//! services. The recording is evidence, never a counterfactual game simulator.
mod clips;
use clips::ClipEvidence;

use super::{
    native_funcs, syntax,
    yaml_extension::{NativeYamlHost, YamlRunState},
};
use crate::capabilities::*;
use anyhow::{anyhow, bail, ensure, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

const MAX_SAMPLE_FRAMES: usize = 240;
const MAX_INPUT_BYTES: usize = 128 * 1024 * 1024;
const MAX_GAP_US: u64 = 500_000;
const MAX_DURATION_US: u64 = 300_000_000;
const MAX_EVENTS: usize = 10_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationRequest {
    pub yaml: String,
    pub templates: BTreeMap<String, Vec<u8>>,
    pub samples: Vec<SampleInput>,
    #[serde(default)]
    pub settings: super::settings::Settings,
    #[serde(default)]
    pub args: Map<String, Value>,
    #[serde(default)]
    pub functions_sources: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleInput {
    pub manifest: Value,
    pub files: BTreeMap<String, Vec<u8>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    Passed,
    Failed,
    InsufficientEvidence,
    Unsupported,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationDiagnostic {
    pub code: String,
    pub message: String,
    pub path: Option<String>,
    pub frame_id: Option<String>,
    pub event_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SampleReport {
    pub sample_id: String,
    pub content_sha256: String,
    pub status: ValidationStatus,
    pub diagnostics: Vec<ValidationDiagnostic>,
    pub events: Vec<Value>,
    pub consumed_actions: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationReport {
    pub status: ValidationStatus,
    pub candidate_sha256: String,
    pub samples: Vec<SampleReport>,
    pub diagnostics: Vec<ValidationDiagnostic>,
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn candidate_sha256(request: &ValidationRequest) -> Result<String> {
    // Bind exact payload bytes without expanding video/PNG bytes into enormous
    // JSON integer arrays merely to hash them.
    let templates: BTreeMap<_, _> = request
        .templates
        .iter()
        .map(|(name, bytes)| (name, json!({"sha256":sha(bytes),"size":bytes.len()})))
        .collect();
    let samples:Vec<_> = request.samples.iter().map(|sample|json!({"manifest":sample.manifest,"files":sample.files.iter().map(|(path,bytes)|(path,json!({"sha256":sha(bytes),"size":bytes.len()}))).collect::<BTreeMap<_,_>>()})).collect();
    Ok(sha(&serde_json::to_vec(
        &json!({"runtime_contract":yaml_interp::RUNTIME_CONTRACT,"host_version":env!("CARGO_PKG_VERSION"),"yaml":request.yaml,"args":request.args,"settings":request.settings,"functions_sources":request.functions_sources,"templates":templates,"samples":samples}),
    )?))
}
fn diagnostic(code: &str, message: impl Into<String>) -> ValidationDiagnostic {
    ValidationDiagnostic {
        code: code.into(),
        message: message.into(),
        path: None,
        frame_id: None,
        event_id: None,
    }
}
fn status_for(error: &str) -> ValidationStatus {
    if error.contains("UNSUPPORTED") {
        ValidationStatus::Unsupported
    } else if error.contains("EVIDENCE_") {
        ValidationStatus::InsufficientEvidence
    } else {
        ValidationStatus::Failed
    }
}
fn failed(error: impl Into<String>) -> CapabilityError {
    CapabilityError::Failed(error.into())
}
fn unsupported() -> CapabilityError {
    failed("UNSUPPORTED: 回放禁止外部副作用或未记录输入")
}

/// Runs each selected sample independently from START. A negative/incomplete
/// sample is retained in the report and can never be silently excluded.
pub async fn validate_candidate(
    request: ValidationRequest,
    stop: Arc<AtomicBool>,
) -> Result<ValidationReport> {
    validate_candidate_with_media(request, stop, "ffmpeg".to_string()).await
}

pub async fn validate_candidate_with_media(
    request: ValidationRequest,
    stop: Arc<AtomicBool>,
    ffmpeg_path: String,
) -> Result<ValidationReport> {
    ensure!(
        !request.samples.is_empty() && request.samples.len() <= 32,
        "需选择 1..32 份素材"
    );
    ensure!(request.yaml.len() <= 1024 * 1024, "脚本超过 1MiB");
    request.settings.validate()?;
    let size: usize = request.templates.values().map(Vec::len).sum::<usize>()
        + request
            .samples
            .iter()
            .flat_map(|s| s.files.values())
            .map(Vec::len)
            .sum::<usize>();
    ensure!(size <= MAX_INPUT_BYTES, "验证输入超过 128MiB");
    let fingerprint = candidate_sha256(&request)?;
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        validate_sync(request, stop, handle, fingerprint, ffmpeg_path)
    })
    .await?
}
pub async fn preflight_samples(
    samples: Vec<SampleInput>,
    stop: Arc<AtomicBool>,
    ffmpeg_path: String,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        ensure!(
            !samples.is_empty() && samples.len() <= 32,
            "需选择1..32份素材"
        );
        ensure!(
            samples
                .iter()
                .flat_map(|sample| sample.files.values())
                .map(Vec::len)
                .sum::<usize>()
                <= MAX_INPUT_BYTES,
            "素材总输入超过128MiB"
        );
        for sample in &samples {
            let _ = Replay::new(sample, &BTreeMap::new(), stop.clone(), &ffmpeg_path)?;
        }
        Ok(())
    })
    .await?
}

fn validate_sync(
    request: ValidationRequest,
    stop: Arc<AtomicBool>,
    runtime: tokio::runtime::Handle,
    fingerprint: String,
    ffmpeg_path: String,
) -> Result<ValidationReport> {
    let program = prepare_program(&request);
    let mut reports = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for sample in &request.samples {
        let id = sample.manifest["id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        ensure!(seen.insert(id.clone()), "样本 id 重复: {id}");
        let mut report = SampleReport {
            sample_id: id,
            content_sha256: sample.manifest["content_sha256"]
                .as_str()
                .unwrap_or("")
                .into(),
            status: ValidationStatus::Failed,
            diagnostics: Vec::new(),
            events: Vec::new(),
            consumed_actions: 0,
        };
        let result = (|| -> Result<()> {
            if stop.load(Ordering::Relaxed) {
                bail!("CANCELLED: 验证已取消");
            }
            let program = program.as_ref().map_err(|e| anyhow!(e.to_string()))?;
            let replay = Arc::new(Replay::new(
                sample,
                &request.templates,
                stop.clone(),
                &ffmpeg_path,
            )?);
            let registry = CapabilityRegistry::builder()
                .with_device_service(replay.clone())
                .with_frame_service(replay.clone())
                .with_input_service(replay.clone())
                .with_vision_service(replay.clone())
                .with_resource_service(replay.clone())
                .with_runtime_service(replay.clone())
                .with_log_service(replay.clone())
                .build();
            let manifest = crate::extensions::manifest::parse_manifest(
                super::yaml_extension::YAML_EXTENSION_MANIFEST_TOML.as_bytes(),
            )?;
            let host =
                crate::extensions::HostApi::for_manifest(registry, Default::default(), &manifest)?;
            let context = crate::core::AppContext {
                device_id: crate::core::DeviceId::new("offline-replay")?,
                android_package: None,
                content_package: Some(crate::core::AppPackageId::new("offline-replay")?),
            };
            let native = runtime
                .block_on(NativeYamlHost::new(host, context, stop.clone(), None))?
                .with_runtime(replay.clone())
                .with_settings(request.settings.clone());
            let state = native.run_state.clone();
            let events = ReplayEvents {
                replay: replay.clone(),
                state,
            };
            let invoker = ReplayFunctions {
                native,
                replay: replay.clone(),
                runtime: runtime.clone(),
            };
            let outcome = yaml_interp::run(program, &invoker, Some(&events));
            let progress = replay.progress.lock().unwrap();
            report.events = progress.events.clone();
            report.consumed_actions = progress.action;
            if let Err(error) = outcome {
                let mut d = diagnostic("replay.execution", &error);
                d.path = progress.path.clone();
                d.frame_id = progress.last_frame.clone();
                d.event_id = replay
                    .actions
                    .get(progress.action)
                    .and_then(|a| a["event_id"].as_str())
                    .map(str::to_string);
                report.diagnostics.push(d);
                bail!("{error}");
            }
            ensure!(
                progress.action == replay.actions.len(),
                "ACTION_MISSING: {} 个记录动作未执行",
                replay.actions.len() - progress.action
            );
            ensure!(progress.goal_checked, "GOAL_NOT_REACHED: 未验证完成目标");
            ensure!(
                progress.last_frame.as_deref() == Some(replay.frames[replay.end_index].id.as_str()),
                "EVIDENCE_END_UNOBSERVED: finish 未观察素材 END 证据"
            );
            Ok(())
        })();
        match result {
            Ok(()) => report.status = ValidationStatus::Passed,
            Err(e) => {
                report.status = status_for(&e.to_string());
                if report.diagnostics.is_empty() {
                    report
                        .diagnostics
                        .push(diagnostic("replay.validation", e.to_string()));
                }
            }
        }
        reports.push(report);
    }
    let status = if reports.iter().all(|r| r.status == ValidationStatus::Passed) {
        ValidationStatus::Passed
    } else if reports.iter().any(|r| r.status == ValidationStatus::Failed) {
        ValidationStatus::Failed
    } else if reports
        .iter()
        .any(|r| r.status == ValidationStatus::Unsupported)
    {
        ValidationStatus::Unsupported
    } else {
        ValidationStatus::InsufficientEvidence
    };
    Ok(ValidationReport {
        status,
        candidate_sha256: fingerprint,
        samples: reports,
        diagnostics: vec![],
    })
}
fn prepare_program(request: &ValidationRequest) -> Result<yaml_interp::Program> {
    let script = syntax::parse_script(&request.yaml).map_err(|d| {
        anyhow!(d
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "))
    })?;
    let mut library = vec![];
    let mut names = native_funcs::native_names();
    for source in request.functions_sources.values() {
        for (name, def) in
            syntax::parse_function_library(source).map_err(|d| anyhow!(format!("{d:?}")))?
        {
            ensure!(names.insert(name.clone()), "FUNCTION_CONFLICT: {name}");
            library.push((name, def));
        }
    }
    for called in script
        .called_functions()
        .into_iter()
        .chain(library.iter().flat_map(|(_, d)| d.called_functions()))
    {
        ensure!(names.contains(&called), "FUNCTION_NOT_FOUND: {called}");
    }
    let bound = super::task_params::bind_entry_args("offline", &script.params, &request.args, true)
        .map_err(|d| anyhow!(format!("{d:?}")))?;
    let mut vars: Map<String, Value> = script.vars.iter().cloned().collect();
    vars.extend(bound.resolved);
    Ok(serde_json::from_value(syntax::build_program(
        &script, &library, vars, 0,
    ))?)
}
struct FrameSource {
    id: String,
    at: u64,
    handle: FrameHandle,
    png: Arc<Vec<u8>>,
}
struct ReplayFrame {
    id: String,
    at: u64,
    content_sha256: String,
    handle: FrameHandle,
    image: crate::matcher::DecodedFrame,
}
#[derive(Clone)]
struct TrustedObservation {
    query: TemplateQuery,
    bounds: MatchBox,
    frame_id: String,
    action: usize,
    clock_ms: u64,
}
#[derive(Default)]
struct Progress {
    observations: Vec<TrustedObservation>,
    clock_us: u64,
    evidence_exhausted: bool,
    timeline_us: u64,
    action: usize,
    last_frame: Option<String>,
    goal_checked: bool,
    events: Vec<Value>,
    path: Option<String>,
}
struct Replay {
    frames: Vec<FrameSource>,
    png_cache: Mutex<std::collections::VecDeque<Arc<ReplayFrame>>>,
    clips: Option<ClipEvidence>,
    actions: Vec<Value>,
    templates: HashMap<ResourceHandle, (String, Vec<u8>)>,
    by_name: BTreeMap<String, ResourceHandle>,
    size: FrameSize,
    stamp: FrameStamp,
    start_us: u64,
    end_us: u64,
    end_index: usize,
    max_gap: u64,
    progress: Mutex<Progress>,
    stop: Arc<AtomicBool>,
}
fn cache_frame(cache: &mut std::collections::VecDeque<Arc<ReplayFrame>>, frame: Arc<ReplayFrame>) {
    let bytes = |f: &ReplayFrame| {
        let (w, h) = f.image.dimensions();
        (w as u64) * (h as u64) * 3
    };
    while !cache.is_empty()
        && (cache.len() >= 8
            || cache.iter().map(|f| bytes(f)).sum::<u64>() + bytes(&frame) > 64 * 1024 * 1024)
    {
        cache.pop_front();
    }
    cache.push_back(frame);
}

impl Replay {
    fn new(
        sample: &SampleInput,
        templates: &BTreeMap<String, Vec<u8>>,
        stop: Arc<AtomicBool>,
        ffmpeg_path: &str,
    ) -> Result<Self> {
        crate::extensions::video::sample::validate_bundle(&sample.manifest, &sample.files)
            .map_err(|error| anyhow!("EVIDENCE_INVALID_BUNDLE: {error}"))?;
        let m = &sample.manifest;
        ensure!(m["schema_version"] == 1, "UNSUPPORTED: 素材格式版本");
        ensure!(
            m["status"] == "complete",
            "EVIDENCE_INCOMPLETE: 素材未完成质量验收"
        );
        ensure!(
            m["goal"]["confirmed"] == true,
            "EVIDENCE_GOAL_UNCONFIRMED: END 不代表已确认完成目标"
        );
        ensure!(
            !matches!(
                m["goal"]["outcome"].as_str(),
                Some("failed" | "negative" | "unknown")
            ),
            "EVIDENCE_NEGATIVE: 素材不是成功目标证据"
        );
        let width = m["coordinates"]["width"]
            .as_u64()
            .ok_or_else(|| anyhow!("EVIDENCE_COORDINATES"))? as u32;
        let height = m["coordinates"]["height"]
            .as_u64()
            .ok_or_else(|| anyhow!("EVIDENCE_COORDINATES"))? as u32;
        ensure!(
            width > 0 && height > 0 && width <= 8192 && height <= 8192,
            "EVIDENCE_COORDINATES: 无效尺寸"
        );
        ensure!(
            m["coordinates"]["rotation"].as_u64().unwrap_or(0) == 0,
            "UNSUPPORTED: 旋转坐标系"
        );
        let start = m["start"]["timeline_us"]
            .as_u64()
            .ok_or_else(|| anyhow!("EVIDENCE_START"))?;
        let end = m["end"]["timeline_us"]
            .as_u64()
            .ok_or_else(|| anyhow!("EVIDENCE_END"))?;
        ensure!(
            end >= start && end - start <= MAX_DURATION_US,
            "EVIDENCE_TIMELINE: 时间范围无效或过长"
        );
        let max_gap = m["max_frame_gap_us"]
            .as_u64()
            .ok_or_else(|| anyhow!("EVIDENCE_WINDOWS: 缺少帧覆盖精度声明"))?;
        ensure!(
            max_gap > 0 && max_gap <= MAX_GAP_US,
            "EVIDENCE_WINDOWS: 帧窗口过于稀疏"
        );
        let clip_backed = m["segments"].as_array().is_some_and(|segments| {
            !segments.is_empty()
                && segments.iter().all(|segment| {
                    segment["clip_path"]
                        .as_str()
                        .is_some_and(|path| sample.files.contains_key(path))
                })
        });
        let entries = m["frames"]
            .as_array()
            .ok_or_else(|| anyhow!("EVIDENCE_FRAMES"))?;
        ensure!(
            !entries.is_empty() && entries.len() <= MAX_SAMPLE_FRAMES,
            "EVIDENCE_FRAMES: 数量无效"
        );
        ensure!(
            (width as u64) * (height as u64) * 3 <= 64 * 1024 * 1024,
            "EVIDENCE_IMAGE_BUDGET: 单帧解码超过 64MiB"
        );
        let mut frames = vec![];
        let mut frame_ids = std::collections::BTreeSet::new();
        for f in entries {
            let id = f["id"]
                .as_str()
                .ok_or_else(|| anyhow!("EVIDENCE_FRAME_ID"))?
                .to_string();
            ensure!(frame_ids.insert(id.clone()), "EVIDENCE_FRAME_ID: 重复帧 id");
            let path = f["path"]
                .as_str()
                .ok_or_else(|| anyhow!("EVIDENCE_FRAME_PATH"))?;
            ensure!(
                !path.contains("..") && !path.starts_with('/') && !path.contains('\\'),
                "EVIDENCE_FRAME_PATH"
            );
            let bytes = sample
                .files
                .get(path)
                .ok_or_else(|| anyhow!("EVIDENCE_MISSING_FRAME: {path}"))?;
            ensure!(
                f["sha256"].as_str() == Some(sha(bytes).as_str()),
                "EVIDENCE_FRAME_HASH: {path}"
            );
            let at = f["timeline_us"]
                .as_u64()
                .ok_or_else(|| anyhow!("EVIDENCE_FRAME_TIME"))?;
            ensure!(
                at >= start && at <= end,
                "EVIDENCE_FRAME_TIME: 帧超出 START/END"
            );
            if let Some(last) = frames.last() {
                let last: &FrameSource = last;
                ensure!(at >= last.at, "EVIDENCE_FRAME_ORDER");
                ensure!(
                    clip_backed || at - last.at <= max_gap,
                    "EVIDENCE_WINDOW_GAP: 间隔 {}us 超出精度",
                    at - last.at
                );
            }
            frames.push(FrameSource {
                id,
                at,
                handle: FrameHandle::new(),
                png: Arc::new(bytes.clone()),
            });
        }
        let start_id = m["start"]["frame_id"]
            .as_str()
            .ok_or_else(|| anyhow!("EVIDENCE_START"))?;
        let end_id = m["end"]["frame_id"]
            .as_str()
            .ok_or_else(|| anyhow!("EVIDENCE_END"))?;
        ensure!(
            frames
                .first()
                .is_some_and(|f| f.id == start_id && f.at == start),
            "EVIDENCE_START: 必须从真实 START 帧开始"
        );
        let end_index = frames
            .iter()
            .position(|f| f.id == end_id && f.at == end)
            .ok_or_else(|| anyhow!("EVIDENCE_END: 缺少精确 END 帧"))?;
        let actions = m["actions"]
            .as_array()
            .ok_or_else(|| anyhow!("EVIDENCE_ACTIONS"))?
            .clone();
        ensure!(actions.len() <= 10000, "EVIDENCE_ACTIONS: 过多动作");
        let mut last = start;
        let mut ids = std::collections::BTreeSet::new();
        for a in &actions {
            ensure!(a["status"] == "accepted", "EVIDENCE_ACTION_REJECTED");
            ensure!(
                matches!(a["kind"].as_str(), Some("tap" | "swipe" | "key" | "wait")),
                "UNSUPPORTED: 输入类型 {}",
                a["kind"]
            );
            ensure!(
                a["event_id"]
                    .as_str()
                    .is_some_and(|s| ids.insert(s.to_string())),
                "EVIDENCE_ACTION_ID"
            );
            let at = a["timeline_us"]
                .as_u64()
                .ok_or_else(|| anyhow!("EVIDENCE_ACTION_TIME"))?;
            let duration = a["payload"]["duration_us"]
                .as_u64()
                .ok_or_else(|| anyhow!("EVIDENCE_ACTION_DURATION: 动作缺少真实时长"))?;
            if matches!(a["kind"].as_str(), Some("tap" | "key")) {
                ensure!(duration<=200_000,"UNSUPPORTED: recorded_press_duration {}us cannot be validated as a short native tap/key press",duration);
            }
            ensure!(
                at >= last && at.checked_add(duration).is_some_and(|v| v <= end),
                "EVIDENCE_ACTION_ORDER"
            );
            last = at + duration;
            ensure!(
                a["display_size"]["width"] == width && a["display_size"]["height"] == height,
                "EVIDENCE_ACTION_COORDINATES"
            );
            ensure!(
                frames.iter().any(|f| f.at < at) && frames.iter().any(|f| f.at >= last),
                "EVIDENCE_ACTION_WINDOW: 缺少动作前后画面"
            );
        }
        let mut resources = HashMap::new();
        let mut by_name = BTreeMap::new();
        for (name, bytes) in templates {
            ensure!(
                !name.contains("..")
                    && !name.contains('/')
                    && !name.contains('\\')
                    && name.ends_with(".png"),
                "模板名必须为安全 PNG 文件名"
            );
            crate::matcher::DecodedFrame::from_png(bytes)?;
            let h = ResourceHandle::new();
            resources.insert(h, (name.clone(), bytes.clone()));
            by_name.insert(name.clone(), h);
        }
        let clips = if clip_backed {
            Some(ClipEvidence::new(
                sample,
                &frames,
                ffmpeg_path,
                stop.clone(),
            )?)
        } else {
            None
        };
        Ok(Self {
            frames,
            png_cache: Mutex::new(std::collections::VecDeque::new()),
            clips,
            actions,
            templates: resources,
            by_name,
            size: FrameSize::new(width, height),
            stamp: FrameStamp {
                target: "offline-replay".into(),
                epoch: m["id"].as_str().unwrap_or("sample").into(),
                revision: 0,
            },
            start_us: start,
            end_us: end,
            end_index,
            max_gap,
            progress: Mutex::new(Progress {
                timeline_us: start,
                ..Default::default()
            }),
            stop,
        })
    }
    fn check_cancel(&self) -> CapabilityResult<()> {
        if self.stop.load(Ordering::Relaxed) {
            Err(CapabilityError::Cancelled)
        } else {
            Ok(())
        }
    }
    fn event(&self, value: Value) {
        let mut p = self.progress.lock().unwrap();
        if p.events.len() < MAX_EVENTS {
            p.events.push(value)
        }
    }
    fn frame(&self, h: FrameHandle) -> CapabilityResult<Arc<ReplayFrame>> {
        if let Some(clips) = &self.clips {
            if let Some(frame) = clips.cached.lock().unwrap().iter().find(|f| f.handle == h) {
                return Ok(frame.clone());
            }
        }
        if let Some(frame) = self
            .png_cache
            .lock()
            .unwrap()
            .iter()
            .find(|f| f.handle == h)
        {
            return Ok(frame.clone());
        }
        Err(failed("EVIDENCE_UNKNOWN_FRAME"))
    }
    fn frame_at(&self, at: u64) -> CapabilityResult<Arc<ReplayFrame>> {
        if let Some(clips) = &self.clips {
            return clips
                .frame_at(at)
                .map_err(|e| failed(format!("EVIDENCE_CLIP: {e}")));
        }
        let source = self
            .frames
            .iter()
            .rfind(|f| f.at <= at)
            .ok_or_else(|| failed("EVIDENCE_NO_CURRENT_FRAME"))?;
        let mut cache = self.png_cache.lock().unwrap();
        if let Some(frame) = cache.iter().find(|f| f.handle == source.handle) {
            return Ok(frame.clone());
        }
        let image = crate::matcher::DecodedFrame::from_png(&source.png)
            .map_err(|e| failed(e.to_string()))?;
        let frame = Arc::new(ReplayFrame {
            id: source.id.clone(),
            at: source.at,
            content_sha256: image.pixel_sha256(),
            handle: source.handle,
            image,
        });
        cache_frame(&mut cache, frame.clone());
        Ok(frame)
    }
    fn check_stamp(&self, stamp: Option<&FrameStamp>) -> CapabilityResult<()> {
        if stamp.is_some_and(|s| s != &self.stamp) {
            Err(failed("STALE_FRAME: 回放坐标代次不匹配"))
        } else {
            Ok(())
        }
    }
    fn action(&self, kind: &str, payload: Value) -> CapabilityResult<()> {
        self.check_cancel()?;
        let mut p = self.progress.lock().unwrap();
        let a = self
            .actions
            .get(p.action)
            .ok_or_else(|| failed("ACTION_EXTRA: 脚本执行了未记录的动作"))?;
        if a["kind"] != kind {
            return Err(failed(format!(
                "ACTION_ORDER: 期望 {}，实际 {kind}",
                a["kind"]
            )));
        }
        let action_at = a["timeline_us"].as_u64().unwrap();
        let observed_at = p.timeline_us.min(action_at.saturating_sub(1));
        let current = self.frame_at(observed_at)?;
        let before = self.frame_at(action_at.saturating_sub(1))?;
        let mut proof = None;
        let mut visual_candidate = false;
        if kind == "tap" {
            let contains = |bounds: MatchBox, point: &Value| -> bool {
                point["x"]
                    .as_f64()
                    .zip(point["y"].as_f64())
                    .is_some_and(|(x, y)| {
                        x >= bounds.x as f64
                            && y >= bounds.y as f64
                            && x < (bounds.x + bounds.width) as f64
                            && y < (bounds.y + bounds.height) as f64
                    })
            };
            for observation in p.observations.iter().rev().filter(|o| {
                o.action == p.action
                    && (p.clock_us / 1000).saturating_sub(o.clock_ms) <= 5_000
                    && contains(o.bounds, &payload)
            }) {
                let (name, bytes) = self
                    .templates
                    .get(&observation.query.template())
                    .ok_or_else(unsupported)?;
                visual_candidate = true;
                let options = observation.query.options();
                let current_match = crate::matcher::match_decoded_frame(
                    &current.image,
                    &crate::matcher::DecodedMatchRequest {
                        template_png: bytes.clone(),
                        threshold: options.threshold,
                        region: options.region.map(|r| [r.x, r.y, r.width, r.height]),
                        color: options.color_check
                            || crate::matcher::template_color_from_name(name),
                    },
                )
                .map_err(|error| failed(error.to_string()))?;
                if !current_match.is_some_and(|m| {
                    contains(
                        MatchBox {
                            x: m.x,
                            y: m.y,
                            width: m.width,
                            height: m.height,
                            score: m.score,
                        },
                        &payload,
                    )
                }) {
                    continue;
                }
                let found = crate::matcher::match_decoded_frame(
                    &before.image,
                    &crate::matcher::DecodedMatchRequest {
                        template_png: bytes.clone(),
                        threshold: options.threshold,
                        region: options.region.map(|r| [r.x, r.y, r.width, r.height]),
                        color: options.color_check
                            || crate::matcher::template_color_from_name(name),
                    },
                )
                .map_err(|e| failed(e.to_string()))?;
                if found.is_some_and(|m| {
                    contains(
                        MatchBox {
                            x: m.x,
                            y: m.y,
                            width: m.width,
                            height: m.height,
                            score: m.score,
                        },
                        &a["payload"],
                    )
                }) {
                    proof = Some(
                        json!({"kind":"matched_template","template":name,"observed_frame_id":observation.frame_id,"recorded_before_frame_id":before.id}),
                    );
                    break;
                }
            }
        }
        if visual_candidate && proof.is_none() {
            return Err(failed(
                "ACTION_STALE_OBSERVATION: 当前画面不再支持点击观察目标，或与记录动作前目标不一致",
            ));
        }
        if proof.is_none() {
            if current.content_sha256 != before.content_sha256 {
                return Err(failed("UNSUPPORTED: literal_action_alignment 当前画面不是记录输入前状态，须用真实视觉目标证明等价操作"));
            }
            proof = Some(json!({"kind":"literal_frame","recorded_before_frame_id":before.id}));
        }
        let expected = &a["payload"];
        let close = |key: &str| -> bool {
            expected[key]
                .as_f64()
                .zip(payload[key].as_f64())
                .is_some_and(|(a, b)| (a - b).abs() <= 8.0)
        };
        let okay = match kind {
            "tap" => close("x") && close("y"),
            "swipe" => {
                close("x")
                    && close("y")
                    && close("x2")
                    && close("y2")
                    && expected["duration_us"]
                        .as_u64()
                        .zip(payload["duration_us"].as_u64())
                        .is_some_and(|(a, b)| a.abs_diff(b) <= 100_000.max(a / 5))
            }
            "key" => canonical_key(&expected["code"]) == canonical_key(&payload["code"]),
            _ => false,
        };
        if !okay {
            return Err(failed(format!(
                "ACTION_MISMATCH: {} 目标或手势属性不一致",
                a["event_id"]
            )));
        }
        let evidence_at = p.timeline_us;
        let script_at = self.start_us.saturating_add(p.clock_us);
        p.events.push(json!({"ev":"replay_action_alignment","event_id":a["event_id"],"script_timeline_us":script_at,"evidence_timeline_us":evidence_at,"recorded_timeline_us":action_at,"time_shift_us":(action_at as i128-script_at as i128) as i64,"proof":proof}));
        p.timeline_us =
            a["timeline_us"].as_u64().unwrap() + expected["duration_us"].as_u64().unwrap();
        p.action += 1;
        p.goal_checked = false;
        Ok(())
    }
}
fn canonical_key(value: &Value) -> String {
    let s = value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string());
    match s.to_ascii_uppercase().as_str() {
        "HOME" => "3".into(),
        "BACK" => "4".into(),
        "ENTER" | "RETURN" => "66".into(),
        _ => s,
    }
}
struct ReplayEvents {
    replay: Arc<Replay>,
    state: Arc<YamlRunState>,
}
impl yaml_interp::EventSink for ReplayEvents {
    fn emit(&self, event: Value) {
        if let Some(path) = event["path"].as_str() {
            self.replay.progress.lock().unwrap().path = Some(path.into());
            self.state
                .trace_context
                .write()
                .unwrap()
                .clone_from(&json!({"path":path}));
        }
        self.replay.event(event)
    }
}
struct ReplayFunctions {
    native: NativeYamlHost,
    replay: Arc<Replay>,
    runtime: tokio::runtime::Handle,
}
impl yaml_interp::HostFunctions for ReplayFunctions {
    fn cancelled(&self) -> bool {
        self.replay.stop.load(Ordering::Relaxed)
    }
    fn invoke(
        &self,
        name: &str,
        args: Value,
    ) -> std::result::Result<Value, yaml_interp::HostError> {
        let result = (|| -> Result<Value> {
            ensure!(
                !matches!(name, "launch" | "stop_app" | "notify" | "input_text"),
                "UNSUPPORTED: 回放禁止 {name} 外部副作用"
            );
            if name == "sleep" {
                let duration = args.get("duration").unwrap_or(&args);
                let milliseconds = duration
                    .as_f64()
                    .or_else(|| duration.as_str().and_then(syntax::parse_duration_ms))
                    .ok_or_else(|| anyhow!("sleep 参数无效"))?;
                let remaining = self
                    .replay
                    .end_us
                    .saturating_sub(self.replay.progress.lock().unwrap().timeline_us);
                ensure!(
                    milliseconds * 1000.0 <= remaining as f64,
                    "EVIDENCE_END_EXHAUSTED: 显式等待超过记录的 END 边界"
                );
            }
            if name == "finish" {
                ensure!(
                    self.replay.progress.lock().unwrap().action == self.replay.actions.len(),
                    "ACTION_MISSING: finish 前还有未执行的演示动作"
                );
            }
            let value = self
                .runtime
                .block_on(self.native.call_function(name, args.clone()))?;
            if name == "finish" && !value.is_null() {
                // The script already reached its goal on the forward path.
                // Separately check the declared END postcondition. Walk all
                // remaining evidence in order; never expose future END to an
                // earlier wait and never cross an unconsumed action.
                loop {
                    let current = self.replay.progress.lock().unwrap().timeline_us;
                    let next = if let Some(clips) = &self.replay.clips {
                        clips.next_time_after(current, self.replay.end_us)
                    } else {
                        self.replay
                            .frames
                            .iter()
                            .find(|f| f.at > current)
                            .map(|f| f.at)
                    };
                    let Some(next) = next else { break };
                    self.runtime
                        .block_on(self.replay.sleep(Duration::from_micros(next - current)))?;
                    if self.replay.clips.is_none() || next == self.replay.end_us {
                        self.runtime
                            .block_on(self.replay.capture(&DeviceHandle::new(
                                crate::capabilities::DeviceId::new("offline-replay"),
                            )))?;
                    }
                }
                let mut end_args = args;
                if let Some(map) = end_args.as_object_mut() {
                    map.insert("timeout".into(), json!("0ms"));
                }
                self.runtime
                    .block_on(self.native.call_function("finish", end_args))
                    .map_err(|e| anyhow!("GOAL_END_MISMATCH: END 画面不满足已达到目标: {e}"))?;
                self.replay.event(json!({"ev":"sample_end_postcondition","ok":true,"image_frame_id":self.replay.frames[self.replay.end_index].id}));
                self.replay.progress.lock().unwrap().goal_checked = true;
            }
            Ok(value)
        })();
        result.map_err(|e| {
            yaml_interp::HostError::new(
                if self.cancelled() {
                    yaml_interp::HostErrorKind::Cancelled
                } else {
                    yaml_interp::HostErrorKind::Failed
                },
                e.to_string(),
            )
        })
    }
}
#[async_trait]
impl DeviceService for Replay {
    async fn resolve(&self, id: &crate::capabilities::DeviceId) -> CapabilityResult<DeviceHandle> {
        if id.as_str() != "offline-replay" {
            return Err(unsupported());
        }
        Ok(DeviceHandle::new(id.clone()))
    }
    async fn start_app(&self, _: &DeviceHandle, _: &AppId) -> CapabilityResult<()> {
        Err(unsupported())
    }
    async fn stop_app(&self, _: &DeviceHandle, _: &AppId) -> CapabilityResult<()> {
        Err(unsupported())
    }
}
#[async_trait]
impl RuntimeService for Replay {
    fn now_ms(&self) -> u64 {
        self.progress.lock().unwrap().clock_us / 1000
    }
    fn cancelled(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
    async fn sleep(&self, d: Duration) -> CapabilityResult<()> {
        self.check_cancel()?;
        let mut p = self.progress.lock().unwrap();
        if d.as_micros() > 0 && p.timeline_us >= self.end_us {
            return Err(failed(
                "EVIDENCE_END_EXHAUSTED: 等待需要 END 之后的未知画面",
            ));
        }
        p.clock_us = p.clock_us.saturating_add(d.as_micros() as u64);
        if p.clock_us > 3_600_000_000 {
            return Err(failed("STEP_BUDGET_EXCEEDED: 总虚拟等待预算"));
        }
        let next = self
            .actions
            .iter()
            .skip(p.action)
            .find(|a| a["kind"] != "wait")
            .and_then(|a| a["timeline_us"].as_u64())
            .unwrap_or(self.end_us);
        let requested_timeline = p.timeline_us.saturating_add(d.as_micros() as u64);
        if next >= self.end_us && requested_timeline > self.end_us {
            p.evidence_exhausted = true;
        }
        p.timeline_us = requested_timeline.min(next).min(self.end_us);
        while let Some(a) = self.actions.get(p.action) {
            if a["kind"] != "wait" {
                break;
            }
            let end =
                a["timeline_us"].as_u64().unwrap() + a["payload"]["duration_us"].as_u64().unwrap();
            if p.timeline_us < end {
                break;
            }
            p.action += 1;
        }
        Ok(())
    }
}
#[async_trait]
impl FrameService for Replay {
    async fn coordinate_space(
        &self,
        _: &DeviceHandle,
    ) -> CapabilityResult<(FrameSize, Option<FrameStamp>)> {
        Ok((self.size, Some(self.stamp.clone())))
    }
    async fn stamp(&self, _: FrameHandle) -> CapabilityResult<Option<FrameStamp>> {
        Ok(Some(self.stamp.clone()))
    }
    async fn device_size(&self, _: &DeviceHandle) -> CapabilityResult<FrameSize> {
        Ok(self.size)
    }
    async fn latest(&self, d: &DeviceHandle) -> CapabilityResult<Option<FrameHandle>> {
        self.capture(d).await.map(Some)
    }
    async fn capture(&self, _: &DeviceHandle) -> CapabilityResult<FrameHandle> {
        self.check_cancel()?;
        let p = self.progress.lock().unwrap();
        if p.evidence_exhausted {
            return Err(failed(
                "EVIDENCE_END_EXHAUSTED: 所请求观察时刻超过记录 END，不能提前查看 END 冒充当前画面",
            ));
        }
        let visible_until = self
            .actions
            .iter()
            .skip(p.action)
            .find(|a| a["kind"] != "wait")
            .and_then(|a| a["timeline_us"].as_u64())
            .map(|at| p.timeline_us.min(at.saturating_sub(1)))
            .unwrap_or(p.timeline_us);
        drop(p);
        let frame = self.frame_at(visible_until)?;
        if self.clips.is_none() && visible_until.saturating_sub(frame.at) > self.max_gap {
            return Err(failed("EVIDENCE_STALE_FRAME"));
        }
        self.progress.lock().unwrap().last_frame = Some(frame.id.clone());
        Ok(frame.handle)
    }
    async fn size(&self, h: FrameHandle) -> CapabilityResult<FrameSize> {
        self.frame(h)?;
        Ok(self.size)
    }
}
#[async_trait]
impl ResourceService for Replay {
    async fn resolve(&self, id: &ResourceId) -> CapabilityResult<ResourceHandle> {
        let name = id
            .path()
            .strip_prefix("templates/")
            .ok_or_else(unsupported)?;
        self.by_name
            .get(name)
            .copied()
            .ok_or_else(|| CapabilityError::NotFound(format!("模板 {name}")))
    }
    async fn open(&self, h: ResourceHandle) -> CapabilityResult<ResourceLease> {
        let (_, bytes) = self.templates.get(&h).ok_or_else(unsupported)?;
        Ok(ResourceLease::new(h, Some(bytes.len() as u64)))
    }
    async fn resolved_file_name(&self, h: ResourceHandle) -> CapabilityResult<String> {
        Ok(self.templates.get(&h).ok_or_else(unsupported)?.0.clone())
    }
    async fn freeze(&self, h: ResourceHandle) -> CapabilityResult<ResourceHandle> {
        self.templates.get(&h).ok_or_else(unsupported)?;
        Ok(h)
    }
    async fn fingerprint(&self, h: ResourceHandle) -> CapabilityResult<String> {
        Ok(sha(&self.templates.get(&h).ok_or_else(unsupported)?.1))
    }
}
#[async_trait]
impl VisionService for Replay {
    async fn match_template(
        &self,
        frame: FrameHandle,
        query: TemplateQuery,
    ) -> CapabilityResult<MatchOutcome> {
        self.check_cancel()?;
        let f = self.frame(frame)?;
        let (name, bytes) = self
            .templates
            .get(&query.template())
            .ok_or_else(unsupported)?;
        let o = query.options();
        let r = o.region.map(|r| [r.x, r.y, r.width, r.height]);
        let matched = crate::matcher::match_decoded_frame(
            &f.image,
            &crate::matcher::DecodedMatchRequest {
                template_png: bytes.clone(),
                threshold: o.threshold,
                region: r,
                color: o.color_check || crate::matcher::template_color_from_name(name),
            },
        )
        .map_err(|e| failed(e.to_string()))?;
        if let Some(m) = &matched {
            let mut progress = self.progress.lock().unwrap();
            let observation = TrustedObservation {
                query,
                bounds: MatchBox {
                    x: m.x,
                    y: m.y,
                    width: m.width,
                    height: m.height,
                    score: m.score,
                },
                frame_id: f.id.clone(),
                action: progress.action,
                clock_ms: progress.clock_us / 1000,
            };
            if progress.observations.len() >= 64 {
                progress.observations.remove(0);
            }
            progress.observations.push(observation);
        }
        self.event(json!({"ev":"replay_vision","image_frame_id":f.id,"template":name,"template_sha256":sha(bytes),"found":matched.is_some(),"timeline_us":f.at,"threshold":o.threshold,"region":r}));
        Ok(matched
            .map(|m| {
                MatchOutcome::Found(MatchBox {
                    x: m.x,
                    y: m.y,
                    width: m.width,
                    height: m.height,
                    score: m.score,
                })
            })
            .unwrap_or(MatchOutcome::NotFound))
    }
    async fn match_many(&self, r: &MatchManyRequest) -> CapabilityResult<Vec<MatchManyResult>> {
        let mut out = vec![];
        for q in r.templates() {
            out.push(MatchManyResult {
                template: q.template(),
                outcome: self.match_template(r.frame(), *q).await?,
            })
        }
        Ok(out)
    }
    async fn sample_color(&self, h: FrameHandle, p: FramePoint) -> CapabilityResult<ColorSample> {
        let [red, green, blue] = self
            .frame(h)?
            .image
            .pixel(p.x, p.y)
            .ok_or_else(|| failed("颜色坐标超出帧"))?;
        Ok(ColorSample { red, green, blue })
    }
}
#[async_trait]
impl InputService for Replay {
    async fn tap_from_frame(
        &self,
        d: &DeviceHandle,
        p: TouchPoint,
        stamp: Option<&FrameStamp>,
    ) -> CapabilityResult<()> {
        self.check_stamp(stamp)?;
        self.tap(d, p).await
    }
    async fn tap(&self, _: &DeviceHandle, p: TouchPoint) -> CapabilityResult<()> {
        self.action("tap", json!({"x":p.x(),"y":p.y()}))
    }
    async fn swipe_from_frame(
        &self,
        d: &DeviceHandle,
        g: SwipeGesture,
        s: Option<&FrameStamp>,
    ) -> CapabilityResult<()> {
        self.check_stamp(s)?;
        self.swipe(d, g).await
    }
    async fn swipe(&self, _: &DeviceHandle, g: SwipeGesture) -> CapabilityResult<()> {
        self.action("swipe",json!({"x":g.start().x(),"y":g.start().y(),"x2":g.end().x(),"y2":g.end().y(),"duration_us":g.duration().as_micros() as u64}))
    }
    async fn key_named(&self, _: &DeviceHandle, name: &str, a: KeyAction) -> CapabilityResult<()> {
        if a != KeyAction::Press {
            return Err(unsupported());
        }
        self.action("key", json!({"code":name}))
    }
    async fn key(&self, _: &DeviceHandle, i: KeyInput) -> CapabilityResult<()> {
        if i.action() != KeyAction::Press {
            return Err(unsupported());
        }
        self.action("key", json!({"code":i.code().value()}))
    }
    async fn text(&self, _: &DeviceHandle, _: TextInput) -> CapabilityResult<()> {
        Err(unsupported())
    }
}
impl LogService for Replay {
    fn write(&self, r: LogRecord) -> CapabilityResult<()> {
        self.event(json!({"ev":"log","message":r.message()}));
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn validation_fixture_samples() -> (String, BTreeMap<String, Vec<u8>>, Vec<SampleInput>)
{
    let request = tests::request();
    (request.yaml, request.templates, request.samples)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn png(image: &image::RgbImage) -> Vec<u8> {
        let mut data = std::io::Cursor::new(vec![]);
        image.write_to(&mut data, image::ImageFormat::Png).unwrap();
        data.into_inner()
    }
    fn pattern(seed: u32) -> image::RgbImage {
        image::RgbImage::from_fn(9, 9, |x, y| {
            let v = ((x * 73856093u32)
                .wrapping_add(y * 19349663)
                .wrapping_add(seed * 83492791)
                ^ (x * y * seed * 29)) as u8;
            image::Rgb([v, v.wrapping_mul(3), v.wrapping_add((seed * 41) as u8)])
        })
    }
    fn screen(template: &image::RgbImage) -> Vec<u8> {
        let mut image = image::RgbImage::from_fn(64, 48, |x, y| {
            image::Rgb([((x * 17 + y * 3) % 67) as u8; 3])
        });
        image::imageops::replace(&mut image, template, 20, 15);
        png(&image)
    }
    fn seal(sample: &mut SampleInput) {
        sample.manifest["content_sha256"] = json!("");
        let typed: crate::extensions::video::sample::SampleManifest =
            serde_json::from_value(sample.manifest.clone()).unwrap();
        sample.manifest = serde_json::to_value(typed).unwrap();
        sample.manifest["content_sha256"] =
            json!(sha(&serde_json::to_vec(&sample.manifest).unwrap()));
    }
    fn fixture(popup: bool) -> SampleInput {
        let states: Vec<(u64, u32)> = if popup {
            vec![
                (0, 1),
                (100_000, 1),
                (250_000, 2),
                (300_000, 2),
                (450_000, 3),
                (500_000, 3),
                (750_000, 3),
                (1_000_000, 3),
            ]
        } else {
            vec![
                (0, 1),
                (100_000, 1),
                (250_000, 3),
                (500_000, 3),
                (750_000, 3),
                (1_000_000, 3),
            ]
        };
        let mut files = BTreeMap::new();
        let mut frames = vec![];
        for (i, (at, seed)) in states.iter().enumerate() {
            let path = format!("frames/f{i}.png");
            let bytes = screen(&pattern(*seed));
            frames.push(json!({"id":format!("f{i}"),"path":path,"sha256":sha(&bytes),"media_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","media_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","frame_index":i,"pts_us":at,"timeline_us":at,"width":64,"height":48,"rotation":0,"role":"observation"}));
            files.insert(path, bytes);
        }
        let action = |id: &str, at: u64| json!({"schema_version":1,"event_id":id,"operation_id":id,"session_id":"recording","source":"manual","kind":"tap","timeline_us":at,"time_domain":"recording","coordinate_space":"device-display","display_size":{"width":64,"height":48},"payload":{"x":25,"y":20,"duration_us":50_000},"status":"accepted"});
        let actions = if popup {
            vec![action("claim", 150_000), action("confirm", 350_000)]
        } else {
            vec![action("claim", 150_000)]
        };
        let windows = if popup {
            json!([{"event_id":"claim","before_frame_ids":["f0","f1"],"after_frame_ids":["f2","f3"]},{"event_id":"confirm","before_frame_ids":["f2","f3"],"after_frame_ids":["f4","f5"]}])
        } else {
            json!([{"event_id":"claim","before_frame_ids":["f0","f1"],"after_frame_ids":["f2","f3"]}])
        };
        let mut result = SampleInput {
            manifest: json!({"schema_version":1,"id":if popup{"with-confirm"}else{"without-confirm"},"name":"synthetic NCC fixture","content_sha256":"","recording_id":"recording","start":{"timeline_us":0,"frame_id":"f0"},"end":{"timeline_us":1_000_000,"frame_id":format!("f{}",frames.len()-1)},"goal":{"description":"done pattern visible","confirmed":true},"coordinates":{"space":"device-display","width":64,"height":48,"rotation":0},"status":"complete","max_frame_gap_us":500_000,"diagnostics":[],"actions":actions,"frames":frames,"windows":windows,"segments":[{"media_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","start_us":0,"duration_us":1_000_000,"base_pts_us":0}],"files":files.iter().map(|(path,b)|json!({"path":path,"size":b.len(),"sha256":sha(b)})).collect::<Vec<_>>()}),
            files,
        };
        seal(&mut result);
        result
    }
    pub(super) fn request() -> ValidationRequest {
        ValidationRequest{yaml:"version: 2\ntargets:\n  claim: {template: claim.png, threshold: 0.99}\n  confirm: {template: confirm.png, threshold: 0.99}\n  done: {template: done.png, threshold: 0.99}\nrun:\n  - id: claim_button\n    wait: claim\n    then: [{tap: claim}]\n  - optional: {find: confirm, timeout: 250ms, then: [{tap: confirm}]}\n  - finish: done\n".into(),templates:BTreeMap::from([("claim.png".into(),png(&pattern(1))),("confirm.png".into(),png(&pattern(2))),("done.png".into(),png(&pattern(3)))]),samples:vec![fixture(true),fixture(false)],args:Map::new(),settings:super::super::settings::Settings::default(),functions_sources:BTreeMap::new()}
    }
    fn set_frame_bytes(sample: &mut SampleInput, id: &str, bytes: Vec<u8>) {
        let frame = sample.manifest["frames"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|f| f["id"] == id)
            .unwrap();
        let path = frame["path"].as_str().unwrap().to_string();
        frame["sha256"] = json!(sha(&bytes));
        for file in sample.manifest["files"].as_array_mut().unwrap() {
            if file["path"] == path {
                file["sha256"] = json!(sha(&bytes));
                file["size"] = json!(bytes.len());
            }
        }
        sample.files.insert(path, bytes);
        seal(sample);
    }
    fn clip_fixture() -> SampleInput {
        let temp = tempfile::tempdir().unwrap();
        for i in 0..21 {
            std::fs::write(
                temp.path().join(format!("frame-{i:03}.png")),
                screen(&pattern(if i < 4 {
                    1
                } else if i < 8 {
                    2
                } else {
                    3
                })),
            )
            .unwrap();
        }
        let output = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-framerate", "20", "-i"])
            .arg(temp.path().join("frame-%03d.png"))
            .args([
                "-c:v",
                "libx264rgb",
                "-crf",
                "0",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "rgb24",
                "-y",
            ])
            .arg(temp.path().join("clip.mp4"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let bytes = std::fs::read(temp.path().join("clip.mp4")).unwrap();
        let mut sample = fixture(true);
        let digest = sha(&bytes);
        for frame in sample.manifest["frames"].as_array_mut().unwrap() {
            frame["media_sha256"] = json!(digest);
            frame["frame_index"] = json!(frame["timeline_us"].as_u64().unwrap() / 50_000);
        }
        sample.manifest["segments"][0]["clip_path"] = json!("clips/original.mp4");
        sample.manifest["segments"][0]["clip_sha256"] = json!(digest);
        sample.manifest["files"]
            .as_array_mut()
            .unwrap()
            .push(json!({"path":"clips/original.mp4","sha256":digest,"size":bytes.len()}));
        sample.files.insert("clips/original.mp4".into(), bytes);
        seal(&mut sample);
        sample
    }
    #[tokio::test]
    async fn animated_hud_does_not_invalidate_trusted_visual_target_action() {
        let mut r = request();
        for sample in &mut r.samples {
            let path = sample.manifest["frames"][0]["path"].as_str().unwrap();
            let mut image = image::load_from_memory(&sample.files[path])
                .unwrap()
                .to_rgb8();
            for x in 0..10 {
                image.put_pixel(x, 0, image::Rgb([255, 17, 220]));
            }
            set_frame_bytes(sample, "f0", png(&image));
        }
        let report = validate_candidate(r, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(report.status, ValidationStatus::Passed, "{report:#?}");
        assert!(report.samples.iter().all(|s| s
            .events
            .iter()
            .any(|e| e["ev"] == "replay_action_alignment"
                && e["proof"]["kind"] == "matched_template")));
    }
    #[tokio::test]
    async fn visual_target_moved_or_vanished_during_input_delay_is_rejected() {
        for vanished in [false, true] {
            let mut r = request();
            let mut sample = fixture(false);
            if vanished {
                sample.manifest["actions"][0]["timeline_us"] = json!(350_000);
                sample.manifest["frames"][2]["timeline_us"] = json!(300_000);
                sample.manifest["frames"][2]["pts_us"] = json!(300_000);
                sample.manifest["windows"][0] = json!({"event_id":"claim","before_frame_ids":["f0","f2"],"after_frame_ids":["f3","f4"]});
                set_frame_bytes(&mut sample, "f1", screen(&pattern(7)));
                set_frame_bytes(&mut sample, "f2", screen(&pattern(1)));
                r.settings.before_click_ms = 100;
                r.settings.after_click_ms = 0;
            } else {
                let mut shifted = image::RgbImage::from_fn(64, 48, |x, y| {
                    image::Rgb([((x * 17 + y * 3) % 67) as u8; 3])
                });
                image::imageops::replace(&mut shifted, &pattern(1), 25, 15);
                sample.manifest["actions"][0]["payload"]["x"] = json!(29);
                set_frame_bytes(&mut sample, "f1", png(&shifted));
            }
            seal(&mut sample);
            r.samples = vec![sample];
            let report = validate_candidate(r, Arc::new(AtomicBool::new(false)))
                .await
                .unwrap();
            assert_eq!(report.status, ValidationStatus::Failed, "{report:#?}");
            assert!(
                report.samples[0]
                    .diagnostics
                    .iter()
                    .any(|d| d.message.contains("ACTION_STALE_OBSERVATION")),
                "{report:#?}"
            );
            assert_eq!(report.samples[0].consumed_actions, 0);
        }
    }

    #[tokio::test]
    async fn clip_backed_replay_uses_unlisted_actual_frames_and_cleans_private_media() {
        let sample = clip_fixture();
        let mut r = request();
        r.samples = vec![sample.clone()];
        let report = validate_candidate(r, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(report.status, ValidationStatus::Passed, "{report:#?}");
        assert!(report.samples[0].events.iter().any(|e| e["image_frame_id"]
            .as_str()
            .is_some_and(|id| id.starts_with("clip:"))));
        let replay = Replay::new(
            &sample,
            &BTreeMap::new(),
            Arc::new(AtomicBool::new(false)),
            "ffmpeg",
        )
        .unwrap();
        let path = replay.clips.as_ref().unwrap().temp_path();
        assert!(path.exists());
        drop(replay);
        assert!(!path.exists());
    }
    #[tokio::test]
    async fn clip_anchor_contradiction_and_playlist_masquerade_are_rejected() {
        let sample = clip_fixture();
        let mut forged = sample.clone();
        set_frame_bytes(&mut forged, "f0", screen(&pattern(7)));
        assert!(preflight_samples(
            vec![forged],
            Arc::new(AtomicBool::new(false)),
            "ffmpeg".into()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("EVIDENCE_ANCHOR_PIXELS"));
        let mut playlist = sample;
        let bytes = b"#EXTM3U\nhttps://example.invalid/track.ts\n".to_vec();
        let digest = sha(&bytes);
        playlist
            .files
            .insert("clips/original.mp4".into(), bytes.clone());
        playlist.manifest["segments"][0]["clip_sha256"] = json!(digest);
        for frame in playlist.manifest["frames"].as_array_mut().unwrap() {
            frame["media_sha256"] = json!(digest);
        }
        for file in playlist.manifest["files"].as_array_mut().unwrap() {
            if file["path"] == "clips/original.mp4" {
                file["sha256"] = json!(digest);
                file["size"] = json!(bytes.len());
            }
        }
        seal(&mut playlist);
        assert!(preflight_samples(
            vec![playlist],
            Arc::new(AtomicBool::new(false)),
            "ffmpeg".into()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("self-contained MP4"));
    }
    #[test]
    fn decoded_cache_is_bounded_and_overlarge_geometry_is_rejected_without_allocation() {
        let mut cache = std::collections::VecDeque::new();
        for i in 0..20 {
            let image = crate::matcher::DecodedFrame::from_rgb(image::RgbImage::new(2, 2));
            cache_frame(
                &mut cache,
                Arc::new(ReplayFrame {
                    id: i.to_string(),
                    at: i,
                    content_sha256: image.pixel_sha256(),
                    handle: FrameHandle::new(),
                    image,
                }),
            );
        }
        assert_eq!(cache.len(), 8);
        let mut sample = fixture(false);
        sample.manifest["coordinates"]["width"] = json!(8192);
        sample.manifest["coordinates"]["height"] = json!(8192);
        seal(&mut sample);
        assert!(Replay::new(
            &sample,
            &BTreeMap::new(),
            Arc::new(AtomicBool::new(false)),
            "ffmpeg"
        )
        .is_err());
    }

    #[tokio::test]
    async fn no_post_finish_steps_can_observe_end_and_change_execution() {
        let mut r = request();
        r.yaml
            .push_str("  - tap: [0.9, 0.9]\n  - fail: unreachable\n");
        let report = validate_candidate(r, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(report.status, ValidationStatus::Passed, "{report:#?}");
        assert!(report.samples.iter().all(|sample| !sample
            .events
            .iter()
            .any(|e| e["data"]["function"] == "fail")));
    }

    #[tokio::test]
    async fn native_settings_are_used_and_bound_to_validation_fingerprint() {
        let defaults = request();
        let default_hash = candidate_sha256(&defaults).unwrap();
        let mut immediate = defaults.clone();
        immediate.settings.before_click_ms = 0;
        immediate.settings.after_click_ms = 0;
        assert_ne!(candidate_sha256(&immediate).unwrap(), default_hash);
        let slow = validate_candidate(defaults, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        let fast = validate_candidate(immediate, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(slow.status, ValidationStatus::Passed, "{slow:#?}");
        assert_eq!(fast.status, ValidationStatus::Passed, "{fast:#?}");
        let first = |report: &ValidationReport| {
            report.samples[0]
                .events
                .iter()
                .find(|e| e["ev"] == "replay_action_alignment")
                .unwrap()["script_timeline_us"]
                .as_u64()
                .unwrap()
        };
        assert_eq!(first(&slow), 300_000);
        assert_eq!(first(&fast), 0);
    }
    #[tokio::test]
    async fn full_virtual_sleep_never_peeks_at_end_before_requested_observation() {
        let mut r = request();
        let mut sample = fixture(false);
        sample.manifest["actions"] = json!([]);
        sample.manifest["windows"] = json!([]);
        sample.manifest["frames"]
            .as_array_mut()
            .unwrap()
            .truncate(2);
        sample.manifest["end"] = json!({"timeline_us":100_000,"frame_id":"f1"});
        sample.manifest["segments"][0]["duration_us"] = json!(100_000);
        sample
            .files
            .retain(|path, _| matches!(path.as_str(), "frames/f0.png" | "frames/f1.png"));
        sample.manifest["files"]
            .as_array_mut()
            .unwrap()
            .retain(|f| matches!(f["path"].as_str(), Some("frames/f0.png" | "frames/f1.png")));
        set_frame_bytes(&mut sample, "f1", screen(&pattern(3)));
        let replay = Replay::new(
            &sample,
            &BTreeMap::new(),
            Arc::new(AtomicBool::new(false)),
            "ffmpeg",
        )
        .unwrap();
        replay.sleep(Duration::from_millis(250)).await.unwrap();
        assert_eq!(replay.now_ms(), 250);
        assert!(replay
            .capture(&DeviceHandle::new(crate::capabilities::DeviceId::new(
                "offline-replay"
            )))
            .await
            .unwrap_err()
            .to_string()
            .contains("EVIDENCE_END_EXHAUSTED"));
        r.yaml =
            "version: 2\nrun: [{finish: {template: done.png}, timeout: 250ms, interval: 250ms}]"
                .into();
        r.samples = vec![sample];
        let report = validate_candidate(r, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(
            report.status,
            ValidationStatus::InsufficientEvidence,
            "{report:#?}"
        );
    }

    #[tokio::test]
    async fn same_engine_real_ncc_optional_paths_all_samples_and_repeatable() {
        let a = validate_candidate(request(), Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(a.status, ValidationStatus::Passed, "{a:#?}");
        assert_eq!(
            a.samples
                .iter()
                .map(|s| s.consumed_actions)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        let b = validate_candidate(request(), Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
    }
    #[tokio::test]
    async fn required_missing_template_and_wrong_action_never_pass() {
        for mode in 0..3 {
            let mut r = request();
            match mode {
                0 => {
                    r.templates.remove("claim.png");
                }
                1 => {
                    r.yaml = r.yaml.replace("tap: claim", "tap: [0.9, 0.9]");
                }
                _ => {
                    r.templates.insert("done.png".into(), png(&pattern(5)));
                }
            }
            let result = validate_candidate(r, Arc::new(AtomicBool::new(false)))
                .await
                .unwrap();
            assert_ne!(result.status, ValidationStatus::Passed, "mode={mode}");
            assert_eq!(result.samples.len(), 2);
        }
    }
    #[tokio::test]
    async fn unconfirmed_corrupt_sparse_and_unfinished_cannot_pass() {
        for mode in 0..4 {
            let mut r = request();
            match mode {
                0 => r.samples[0].manifest["goal"]["confirmed"] = json!(false),
                1 => r.samples[0].manifest["status"] = json!("unable_to_validate"),
                2 => {
                    r.samples[0].files.values_mut().next().unwrap()[0] ^= 1;
                }
                _ => r.yaml = r.yaml.replace("  - finish: done", "  - return: true"),
            };
            let result = validate_candidate(r, Arc::new(AtomicBool::new(false)))
                .await
                .unwrap();
            assert_ne!(result.status, ValidationStatus::Passed);
        }
    }
    #[tokio::test]
    async fn offline_side_effects_and_waits_without_future_evidence_fail() {
        for step in ["launch: {}", "notify: {content: hi}", "sleep: 10s"] {
            let mut r = request();
            r.yaml = r
                .yaml
                .replace("  - finish: done", &format!("  - {step}\n  - finish: done"));
            let result = validate_candidate(r, Arc::new(AtomicBool::new(false)))
                .await
                .unwrap();
            assert_ne!(result.status, ValidationStatus::Passed, "{step}");
        }
    }
    #[tokio::test]
    async fn no_reading_success_past_unconsumed_action() {
        let mut r = request();
        r.yaml = r.yaml.replace("    then: [{tap: claim}]", "");
        let result = validate_candidate(r, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_ne!(result.status, ValidationStatus::Passed);
        assert!(result.samples.iter().all(|s| s.consumed_actions == 0));
    }
}
