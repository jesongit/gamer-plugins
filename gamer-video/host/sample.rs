//! Portable evidence samples. This is a data contract, never a script generator.
//! ZIP files contain a deterministic manifest, full-resolution PNGs and optional
//! original segments. Every read validates all entries, including unused clips.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Cursor, Read, Write};
use std::path::Path;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::extensions::{ExtensionError, ExtensionResult, Permission};
use crate::media::{FrameRequest, MediaService};
use crate::recording::{
    InputEventRecord, RecordingId, RecordingService, RecordingState, SegmentMeta,
};
use crate::resources::PackageStore;

pub(crate) const ACTIONS: &[&str] = &["sample.create", "sample.read", "sample.list"];
pub const MAX_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;
const MAX_FILES: usize = 260;
const MAX_MANIFEST: u64 = 2 * 1024 * 1024;
const MAX_FRAMES: usize = 240;
const MAX_ACTIONS: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleManifest {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub content_sha256: String,
    pub recording_id: String,
    pub start: Boundary,
    pub end: Boundary,
    pub goal: Goal,
    pub coordinates: Coordinates,
    pub status: String,
    pub max_frame_gap_us: u64,
    pub diagnostics: Vec<Diagnostic>,
    pub actions: Vec<InputEventRecord>,
    pub frames: Vec<SampleFrame>,
    pub windows: Vec<Window>,
    pub segments: Vec<SampleSegment>,
    pub files: Vec<SampleFile>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Boundary {
    pub timeline_us: u64,
    pub frame_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Goal {
    pub description: String,
    pub confirmed: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Coordinates {
    pub space: String,
    pub width: u32,
    pub height: u32,
    pub rotation: u16,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleFrame {
    pub id: String,
    pub path: String,
    pub sha256: String,
    pub media_id: String,
    pub media_sha256: String,
    pub frame_index: u32,
    pub pts_us: u64,
    pub timeline_us: u64,
    pub width: u32,
    pub height: u32,
    pub rotation: u16,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub event_id: String,
    pub before_frame_ids: Vec<String>,
    pub after_frame_ids: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleSegment {
    pub media_id: String,
    pub start_us: u64,
    pub duration_us: u64,
    /// Original capture PTS, diagnostic only. File PTS starts at zero.
    pub base_pts_us: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip_sha256: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SampleFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    package_id: String,
    recording_id: String,
    name: String,
    start_us: u64,
    end_us: u64,
    goal: Goal,
    #[serde(default = "default_include_clips")]
    include_clips: bool,
}

fn default_include_clips() -> bool {
    true
}

pub(crate) fn accepts(id: &str, action: &str) -> bool {
    id == super::VIDEO_EXTENSION_ID && ACTIONS.contains(&action)
}
pub(crate) fn permissions(id: &str, action: &str) -> Option<&'static [Permission]> {
    if !accepts(id, action) {
        return None;
    }
    Some(if action == "sample.create" {
        &[
            Permission::MediaRead,
            Permission::MediaEventsRead,
            Permission::MediaWrite,
        ]
    } else {
        &[Permission::MediaRead]
    })
}
pub(crate) fn catalog() -> Vec<Value> {
    ACTIONS.iter().map(|a|json!({"action":a,"version":1,"surface":"native","permissions":permissions(super::VIDEO_EXTENSION_ID,a).unwrap().iter().map(|p|p.as_str()).collect::<Vec<_>>()})).collect()
}
pub(crate) fn call(
    id: &str,
    action: &str,
    values: &Value,
    data_dir: &Path,
) -> Option<ExtensionResult<Value>> {
    if !accepts(id, action) {
        return None;
    }
    Some(
        dispatch(action, values, data_dir)
            .map_err(|e| ExtensionError::CallRejected(format!("sample: {e:#}"))),
    )
}
fn dispatch(action: &str, values: &Value, data_dir: &Path) -> anyhow::Result<Value> {
    let cfg = crate::config::Config {
        data_dir: data_dir.to_path_buf(),
        ..Default::default()
    };
    let store = PackageStore::open(&cfg)?;
    let package = values["package_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("package_id required"))?;
    store.manifest(package)?;
    match action {
        "sample.create" => {
            let req: CreateRequest = serde_json::from_value(values.clone())?;
            let recording = crate::recording::service(&cfg);
            let media = crate::media::service(&cfg);
            let (manifest, bytes) = create(&req, &recording, &media)?;
            let path = sample_path(&manifest.id)?;
            let entry = store.write_binary(
                &req.package_id,
                super::VIDEO_EXTENSION_ID,
                &path,
                &bytes,
                None,
                false,
            )?;
            Ok(json!({"manifest":manifest,"path":path,"version":entry.version}))
        }
        "sample.read" => {
            let id = values["sample_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("sample_id required"))?;
            let bytes = store
                .read_binary(package, super::VIDEO_EXTENSION_ID, &sample_path(id)?)?
                .ok_or_else(|| anyhow::anyhow!("sample_not_found"))?;
            let (manifest, files) = validate_archive(&bytes)?;
            anyhow::ensure!(manifest.id == id, "sample id mismatch");
            let files:Vec<Value>=files.into_iter().filter(|(path,_)|path.starts_with("frames/")).map(|(path,bytes)|json!({"path":path,"base64":base64::engine::general_purpose::STANDARD.encode(bytes)})).collect();
            Ok(json!({"manifest":manifest,"files":files}))
        }
        "sample.list" => {
            let mut samples = Vec::new();
            for entry in store.list(package, super::VIDEO_EXTENSION_ID, "samples/")? {
                if !entry.path.ends_with(".gamersample") {
                    continue;
                }
                let bytes = store
                    .read_binary(package, super::VIDEO_EXTENSION_ID, &entry.path)?
                    .unwrap_or_default();
                match validate_archive(&bytes).and_then(|(manifest,files)| {
                    anyhow::ensure!(entry.path == sample_path(&manifest.id)?, "sample ID/path mismatch");
                    Ok((manifest,files))
                }) {
                    Ok((manifest,_))=>samples.push(json!({"id":manifest.id,"name":manifest.name,"status":manifest.status,"content_sha256":manifest.content_sha256,"path":entry.path,"diagnostics":manifest.diagnostics})),
                    Err(e)=>samples.push(json!({"path":entry.path,"status":"corrupt","diagnostics":[{"code":"archive.invalid","message":e.to_string()}]})),
                }
            }
            Ok(json!({"samples":samples}))
        }
        _ => anyhow::bail!("unsupported action"),
    }
}
fn sample_path(id: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !id.is_empty()
            && id.len() <= 80
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid sample id"
    );
    Ok(format!("samples/{id}.gamersample"))
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn fingerprint(manifest: &SampleManifest) -> anyhow::Result<String> {
    let mut clean = manifest.clone();
    clean.content_sha256.clear();
    // serde_json::Value sorts all object keys with the default BTreeMap backend.
    Ok(hash(&serde_json::to_vec(&serde_json::to_value(clean)?)?))
}
fn issue(
    manifest: &mut SampleManifest,
    code: &str,
    message: impl Into<String>,
    event_id: Option<&str>,
) {
    manifest.status = "unable_to_validate".into();
    manifest.diagnostics.push(Diagnostic {
        code: code.into(),
        message: message.into(),
        event_id: event_id.map(str::to_string),
    });
}
fn action_end(event: &InputEventRecord) -> u64 {
    event
        .timeline_us
        .saturating_add(event.payload["duration_us"].as_u64().unwrap_or(0))
}
fn segment_at(segments: &[SegmentMeta], t: u64) -> Option<&SegmentMeta> {
    segments
        .iter()
        .find(|s| t >= s.start_us && t <= s.start_us.saturating_add(s.duration_us))
}

fn create(
    req: &CreateRequest,
    recording: &RecordingService,
    media: &MediaService,
) -> anyhow::Result<(SampleManifest, Vec<u8>)> {
    anyhow::ensure!(
        !req.name.trim().is_empty() && req.name.chars().count() <= 120,
        "sample name required (max 120)"
    );
    anyhow::ensure!(
        req.goal.confirmed
            && !req.goal.description.trim().is_empty()
            && req.goal.description.chars().count() <= 2000,
        "END requires explicit user-confirmed goal"
    );
    anyhow::ensure!(
        req.start_us < req.end_us && req.end_us - req.start_us <= 300_000_000,
        "select START < END, at most 5 minutes"
    );
    let session = recording.status(&RecordingId(req.recording_id.clone()))?;
    anyhow::ensure!(
        !matches!(
            session.state,
            RecordingState::Recording | RecordingState::Finalizing
        ),
        "finish recording before making a sample"
    );
    let mut load_issue = None;
    let events = match recording.events(&session.id) {
        Ok(e) => e,
        Err(e) => {
            load_issue = Some(e.to_string());
            Vec::new()
        }
    };
    let cuts_action = events.iter().any(|e| {
        (e.timeline_us < req.start_us && action_end(e) > req.start_us)
            || (e.timeline_us <= req.end_us && action_end(e) > req.end_us)
    });
    let actions: Vec<_> = events
        .into_iter()
        .filter(|e| e.timeline_us >= req.start_us && e.timeline_us <= req.end_us)
        .collect();
    anyhow::ensure!(
        actions.len() <= MAX_ACTIONS,
        "sample has too many actions (max 100)"
    );
    let first = segment_at(&session.segments, req.start_us)
        .ok_or_else(|| anyhow::anyhow!("START has no media segment"))?;
    let meta = media.get(&first.media_id)?;
    let mut manifest = SampleManifest {
        schema_version: 1,
        id: uuid::Uuid::new_v4().simple().to_string(),
        name: req.name.trim().into(),
        content_sha256: String::new(),
        recording_id: req.recording_id.clone(),
        start: Boundary {
            timeline_us: req.start_us,
            frame_id: String::new(),
        },
        end: Boundary {
            timeline_us: req.end_us,
            frame_id: String::new(),
        },
        goal: req.goal.clone(),
        coordinates: Coordinates {
            space: "device-display".into(),
            width: meta.width,
            height: meta.height,
            rotation: meta.rotation,
        },
        status: "complete".into(),
        max_frame_gap_us: 500_000,
        diagnostics: Vec::new(),
        actions,
        frames: Vec::new(),
        windows: Vec::new(),
        segments: Vec::new(),
        files: Vec::new(),
    };
    if cuts_action {
        issue(
            &mut manifest,
            "boundary.cuts_action",
            "START/END cuts through a captured action",
            None,
        );
    }
    if let Some(e) = load_issue {
        issue(&mut manifest, "events.missing", e, None);
    }
    if session.state != RecordingState::Completed {
        issue(
            &mut manifest,
            "recording.incomplete",
            format!("recording state {:?}", session.state),
            None,
        );
    }
    for e in &session.evidence_issues {
        issue(&mut manifest, "recording.evidence_loss", e, None);
    }
    let mut files = BTreeMap::new();
    let selected: Vec<_> = session
        .segments
        .iter()
        .filter(|s| {
            s.start_us <= req.end_us && s.start_us.saturating_add(s.duration_us) >= req.start_us
        })
        .cloned()
        .collect();
    for seg in &selected {
        let mut segment = SampleSegment {
            media_id: seg.media_id.0.clone(),
            start_us: seg.start_us,
            duration_us: seg.duration_us,
            base_pts_us: seg.base_pts_us,
            clip_path: None,
            clip_sha256: None,
        };
        if req.include_clips {
            let remaining =
                MAX_ARCHIVE_BYTES.saturating_sub(files.values().map(Vec::len).sum::<usize>());
            match media.file_path(&seg.media_id).and_then(|path| {
                anyhow::ensure!(
                    std::fs::metadata(&path)?.len() <= remaining as u64,
                    "clip exceeds sample size limit"
                );
                Ok(std::fs::read(path)?)
            }) {
                Ok(bytes) => {
                    let path = format!("clips/{}.mp4", seg.media_id.0);
                    segment.clip_sha256 = Some(hash(&bytes));
                    segment.clip_path = Some(path.clone());
                    files.insert(path, bytes);
                }
                Err(e) => issue(&mut manifest, "clip.missing", e.to_string(), None),
            }
        }
        manifest.segments.push(segment);
    }
    manifest.start.frame_id = add_frame(
        media,
        &selected,
        &mut manifest,
        &mut files,
        req.start_us,
        false,
        "start",
        None,
    )
    .unwrap_or_else(|e| {
        issue(&mut manifest, "start.missing", e.to_string(), None);
        String::new()
    });
    manifest.end.frame_id = add_frame(
        media,
        &selected,
        &mut manifest,
        &mut files,
        req.end_us,
        true,
        "end",
        None,
    )
    .unwrap_or_else(|e| {
        issue(&mut manifest, "end.missing", e.to_string(), None);
        String::new()
    });
    for (label, boundary) in [
        ("start", manifest.start.clone()),
        ("end", manifest.end.clone()),
    ] {
        if manifest
            .frames
            .iter()
            .find(|f| f.id == boundary.frame_id)
            .is_none_or(|f| f.timeline_us != boundary.timeline_us)
        {
            issue(
                &mut manifest,
                "boundary.not_exact",
                format!("{label} must be pinned to an exact confirmed video frame"),
                None,
            );
        }
    }
    let actions = manifest.actions.clone();
    for (i, event) in actions.iter().enumerate() {
        if let Some(reason) = unsupported_action(event, &manifest.coordinates) {
            issue(
                &mut manifest,
                "action.unsupported",
                reason,
                Some(&event.event_id),
            );
        }
        let mut window = Window {
            event_id: event.event_id.clone(),
            before_frame_ids: Vec::new(),
            after_frame_ids: Vec::new(),
        };
        let before_floor = i
            .checked_sub(1)
            .map(|p| action_end(&actions[p]))
            .unwrap_or(req.start_us);
        let after_limit = actions
            .get(i + 1)
            .map(|e| e.timeline_us)
            .unwrap_or(req.end_us);
        let before_offsets: &[u64] = if req.include_clips {
            &[1]
        } else {
            &[250_000, 1]
        };
        for &delta in before_offsets {
            let t = event.timeline_us.saturating_sub(delta).max(before_floor);
            if t >= event.timeline_us {
                continue;
            }
            if let Ok(id) = add_frame(
                media,
                &selected,
                &mut manifest,
                &mut files,
                t,
                true,
                "before",
                Some(&event.event_id),
            ) {
                let frame = manifest.frames.iter().find(|f| f.id == id).unwrap();
                if frame.timeline_us < event.timeline_us
                    && frame.timeline_us >= before_floor
                    && !window.before_frame_ids.contains(&id)
                {
                    window.before_frame_ids.push(id);
                }
            }
        }
        let after_offsets: &[u64] = if req.include_clips {
            &[0, 250_000]
        } else {
            &[0, 250_000, 750_000]
        };
        for &delta in after_offsets {
            let t = action_end(event).saturating_add(delta);
            if t > after_limit {
                continue;
            }
            if let Ok(id) = add_frame(
                media,
                &selected,
                &mut manifest,
                &mut files,
                t,
                false,
                "after",
                Some(&event.event_id),
            ) {
                let frame = manifest.frames.iter().find(|f| f.id == id).unwrap();
                if frame.timeline_us >= action_end(event)
                    && frame.timeline_us <= after_limit
                    && !window.after_frame_ids.contains(&id)
                {
                    window.after_frame_ids.push(id);
                }
            }
        }
        if window.before_frame_ids.is_empty() || window.after_frame_ids.len() < 2 {
            issue(
                &mut manifest,
                "window.insufficient",
                "Need a genuine before frame and two distinct after frames",
                Some(&event.event_id),
            );
        }
        manifest.windows.push(window);
    }
    // Preserve additional observations throughout longer waits, without inventing
    // timestamps for static duplicate images. Sampling uses actual display PTS.
    let stride = 250_000;
    let mut t = req.start_us.saturating_add(stride);
    while !req.include_clips && t < req.end_us && manifest.frames.len() < MAX_FRAMES {
        let _ = add_frame(
            media,
            &selected,
            &mut manifest,
            &mut files,
            t,
            false,
            "observation",
            None,
        );
        t = t.saturating_add(stride);
    }
    manifest.frames.sort_by(|a, b| {
        a.timeline_us
            .cmp(&b.timeline_us)
            .then_with(|| a.id.cmp(&b.id))
    });
    let observed_gap = manifest
        .frames
        .windows(2)
        .map(|f| f[1].timeline_us - f[0].timeline_us)
        .max()
        .unwrap_or(req.end_us - req.start_us);
    if !req.include_clips && observed_gap > manifest.max_frame_gap_us {
        issue(
            &mut manifest,
            "frames.sparse",
            "Observation gap exceeds 500 ms; shorten or supplement the recording",
            None,
        );
    }
    manifest.files = files
        .iter()
        .map(|(path, bytes)| SampleFile {
            path: path.clone(),
            sha256: hash(bytes),
            size: bytes.len() as u64,
        })
        .collect();
    manifest.content_sha256 = fingerprint(&manifest)?;
    let bytes = encode_archive(&manifest, &files)?;
    validate_archive(&bytes)?;
    Ok((manifest, bytes))
}

fn unsupported_action(e: &InputEventRecord, c: &Coordinates) -> Option<String> {
    if e.schema_version != 1
        || e.operation_id.is_empty()
        || e.status != "accepted"
        || e.time_domain != "recording"
        || e.coordinate_space != "device-display"
    {
        return Some("untrusted action metadata".into());
    }
    if e.display_size.width != c.width || e.display_size.height != c.height || c.rotation != 0 {
        return Some("coordinate dimensions or rotation differ".into());
    }
    if !matches!(e.kind.as_str(), "tap" | "swipe" | "key" | "wait") {
        return Some(format!("{} cannot be reproduced", e.kind));
    }
    if matches!(e.kind.as_str(), "tap" | "swipe" | "key")
        && e.payload["duration_us"].as_u64().is_none()
    {
        return Some("captured duration missing".into());
    }
    if e.kind == "key"
        && !(e.payload["code"].is_u64()
            || e.payload["code"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty() && s.len() <= 128))
    {
        return Some("key identity missing".into());
    }
    if e.kind == "wait" && e.payload["duration_us"].as_u64().is_none() {
        return Some("wait duration missing".into());
    }
    if matches!(e.kind.as_str(), "tap" | "swipe") {
        for (key, limit) in [("x", c.width), ("y", c.height)] {
            if e.payload[key].as_u64().is_none_or(|n| n >= limit as u64) {
                return Some("action coordinate out of range".into());
            }
        }
    }
    if e.kind == "swipe" {
        let (Some(x), Some(y), Some(x2), Some(y2)) = (
            e.payload["x"].as_f64(),
            e.payload["y"].as_f64(),
            e.payload["x2"].as_f64(),
            e.payload["y2"].as_f64(),
        ) else {
            return Some("swipe endpoint missing".into());
        };
        if x2 < 0. || y2 < 0. || x2 >= c.width as f64 || y2 >= c.height as f64 {
            return Some("swipe endpoint out of range".into());
        }
        let Some(points) = e.payload["points"].as_array() else {
            return Some("captured swipe trajectory missing".into());
        };
        if points.len() < 2 || points.len() > 257 {
            return Some("swipe trajectory incomplete".into());
        }
        let duration = e.payload["duration_us"].as_u64().unwrap();
        let length = (x2 - x).hypot(y2 - y);
        if length < 1. {
            return Some("closed swipe needs unsupported gesture replay".into());
        }
        let mut maximum_projection = 0f64;
        let mut previous_time = 0u64;
        for p in points {
            let (Some(time), Some(px), Some(py)) = (
                p.get(0).and_then(Value::as_u64),
                p.get(1).and_then(Value::as_u64),
                p.get(2).and_then(Value::as_u64),
            ) else {
                return Some("invalid swipe trajectory point".into());
            };
            let (px, py) = (px as f64, py as f64);
            let projection = ((px - x) * (x2 - x) + (py - y) * (y2 - y)) / length;
            let perpendicular = ((x2 - x) * (y - py) - (x - px) * (y2 - y)).abs() / length;
            if time < previous_time
                || time > duration
                || px >= c.width as f64
                || py >= c.height as f64
                || perpendicular > 8.
                || projection < -8.
                || projection > length + 8.
                || projection + 8. < maximum_projection
            {
                return Some("non-linear, overshooting or reversing swipe cannot be replayed as a straight swipe".into());
            }
            previous_time = time;
            maximum_projection = maximum_projection.max(projection);
        }
        let first = &points[0];
        let last = points.last().unwrap();
        if first[0].as_u64() != Some(0)
            || first[1].as_f64() != Some(x)
            || first[2].as_f64() != Some(y)
            || last[0].as_u64() != Some(duration)
            || last[1].as_f64() != Some(x2)
            || last[2].as_f64() != Some(y2)
        {
            return Some("swipe trajectory endpoints or duration mismatch".into());
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn add_frame(
    media: &MediaService,
    segments: &[SegmentMeta],
    m: &mut SampleManifest,
    files: &mut BTreeMap<String, Vec<u8>>,
    t: u64,
    before: bool,
    role: &str,
    event_id: Option<&str>,
) -> anyhow::Result<String> {
    let seg =
        segment_at(segments, t).ok_or_else(|| anyhow::anyhow!("timestamp falls in segment gap"))?;
    let pts = seg.media_pts_for_timeline(t);
    let info = media.frames_info(&seg.media_id, Some(pts))?;
    let pos = if before {
        if let Some(p) = info.current {
            if p.pts_us > pts {
                media
                    .frame_neighbors(&seg.media_id, p.index)?
                    .prev
                    .ok_or_else(|| anyhow::anyhow!("no frame before timestamp"))?
            } else {
                p
            }
        } else {
            media
                .frame_neighbors(
                    &seg.media_id,
                    info.frame_count
                        .checked_sub(1)
                        .ok_or_else(|| anyhow::anyhow!("empty media"))? as u32,
                )?
                .position
        }
    } else {
        info.current
            .ok_or_else(|| anyhow::anyhow!("no frame after timestamp"))?
    };
    let id = format!("{}-{}", seg.media_id.0, pos.index);
    if m.frames.iter().any(|f| f.id == id) {
        return Ok(id);
    }
    anyhow::ensure!(m.frames.len() < MAX_FRAMES, "sample frame limit exceeded");
    let source = media.get(&seg.media_id)?;
    anyhow::ensure!(
        source.width <= 8192
            && source.height <= 8192
            && (source.width as u64) * (source.height as u64) <= 16_777_216,
        "source image dimensions exceed sample budget"
    );
    let extracted = media.extract_frame(
        &seg.media_id,
        &FrameRequest {
            index: Some(pos.index),
            pts_us: None,
            max_width: None,
        },
    )?;
    let d = extracted.descriptor;
    anyhow::ensure!(
        d.index == pos.index && d.pts_us == pos.pts_us,
        "frame identity changed"
    );
    let path = format!("frames/{id}.png");
    let timeline = seg.start_us.saturating_add(d.pts_us);
    anyhow::ensure!(
        timeline >= m.start.timeline_us && timeline <= m.end.timeline_us,
        "frame outside START/END"
    );
    if d.work_width != m.coordinates.width
        || d.work_height != m.coordinates.height
        || d.rotation != m.coordinates.rotation
    {
        issue(
            m,
            "frame.geometry_changed",
            "Frame dimensions or rotation changed",
            event_id,
        );
    }
    let sha = hash(&extracted.png);
    files.insert(path.clone(), extracted.png);
    anyhow::ensure!(
        files.values().map(Vec::len).sum::<usize>() <= MAX_ARCHIVE_BYTES,
        "sample exceeds 128 MiB; shorten range or omit original clips"
    );
    m.frames.push(SampleFrame {
        id: id.clone(),
        path,
        sha256: sha,
        media_id: seg.media_id.0.clone(),
        media_sha256: d.media_sha256,
        frame_index: d.index,
        pts_us: d.pts_us,
        timeline_us: timeline,
        width: d.work_width,
        height: d.work_height,
        rotation: d.rotation,
        role: role.into(),
        event_id: event_id.map(str::to_string),
    });
    Ok(id)
}

fn encode_archive(
    m: &SampleManifest,
    files: &BTreeMap<String, Vec<u8>>,
) -> anyhow::Result<Vec<u8>> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    zip.start_file("manifest.json", options)?;
    zip.write_all(&serde_json::to_vec(m)?)?;
    for (path, bytes) in files {
        zip.start_file(path, options)?;
        zip.write_all(bytes)?;
    }
    let bytes = zip.finish()?.into_inner();
    anyhow::ensure!(
        bytes.len() <= MAX_ARCHIVE_BYTES,
        "sample archive exceeds 128 MiB"
    );
    Ok(bytes)
}
fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() < 200
        && !path.contains('\\')
        && !path.contains(':')
        && !path.starts_with('/')
        && path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        })
}

/// ZipArchive indexes by filename; duplicate central entries may otherwise be
/// collapsed before callers can inspect them. Validate raw directory first.
fn validate_zip_directory(bytes: &[u8]) -> anyhow::Result<()> {
    fn u16_at(b: &[u8], p: usize) -> anyhow::Result<u16> {
        Ok(u16::from_le_bytes(
            b.get(p..p + 2)
                .ok_or_else(|| anyhow::anyhow!("truncated ZIP"))?
                .try_into()?,
        ))
    }
    fn u32_at(b: &[u8], p: usize) -> anyhow::Result<u32> {
        Ok(u32::from_le_bytes(
            b.get(p..p + 4)
                .ok_or_else(|| anyhow::anyhow!("truncated ZIP"))?
                .try_into()?,
        ))
    }
    anyhow::ensure!(
        bytes.len() >= 22 && bytes.len() <= MAX_ARCHIVE_BYTES,
        "archive size limit"
    );
    let end = (bytes.len().saturating_sub(65557)..=bytes.len() - 22)
        .rev()
        .find(|&p| {
            u32_at(bytes, p).ok() == Some(0x06054b50)
                && u16_at(bytes, p + 20)
                    .is_ok_and(|comment| p + 22 + comment as usize == bytes.len())
        })
        .ok_or_else(|| anyhow::anyhow!("ZIP end missing"))?;
    anyhow::ensure!(
        u16_at(bytes, end + 4)? == 0 && u16_at(bytes, end + 6)? == 0,
        "split ZIP forbidden"
    );
    anyhow::ensure!(
        end < 20 || u32_at(bytes, end - 20)? != 0x07064b50,
        "ZIP64 unsupported"
    );
    let count = u16_at(bytes, end + 10)? as usize;
    anyhow::ensure!(
        count > 0 && count <= MAX_FILES && u16_at(bytes, end + 8)? as usize == count,
        "ZIP entry count mismatch"
    );
    let start = u32_at(bytes, end + 16)? as usize;
    let size = u32_at(bytes, end + 12)? as usize;
    anyhow::ensure!(
        start.checked_add(size) == Some(end),
        "ZIP central directory range mismatch"
    );
    let mut pos = start;
    let mut names = BTreeSet::new();
    let mut total = 0u64;
    for _ in 0..count {
        anyhow::ensure!(
            pos + 46 <= end && u32_at(bytes, pos)? == 0x02014b50,
            "invalid ZIP directory"
        );
        let len = u16_at(bytes, pos + 28)? as usize;
        let extra = u16_at(bytes, pos + 30)? as usize;
        let comment = u16_at(bytes, pos + 32)? as usize;
        let next = pos + 46 + len + extra + comment;
        anyhow::ensure!(next <= end, "truncated ZIP directory");
        let name = std::str::from_utf8(&bytes[pos + 46..pos + 46 + len])?;
        anyhow::ensure!(
            safe_path(name) && names.insert(name),
            "unsafe or duplicate ZIP path"
        );
        anyhow::ensure!(
            u16_at(bytes, pos + 8)? & 1 == 0
                && [0, 8].contains(&u16_at(bytes, pos + 10)?)
                && u16_at(bytes, pos + 34)? == 0,
            "unsupported ZIP entry"
        );
        anyhow::ensure!(
            (u32_at(bytes, pos + 38)? >> 16) & 0o170000 != 0o120000,
            "symlink entry forbidden"
        );
        let file_size = u32_at(bytes, pos + 24)? as u64;
        total = total
            .checked_add(file_size)
            .ok_or_else(|| anyhow::anyhow!("ZIP size overflow"))?;
        anyhow::ensure!(
            total <= MAX_ARCHIVE_BYTES as u64
                && (name != "manifest.json" || file_size <= MAX_MANIFEST),
            "ZIP expanded size limit"
        );
        let local = u32_at(bytes, pos + 42)? as usize;
        anyhow::ensure!(
            local.checked_add(30).is_some_and(|n| n <= start)
                && u32_at(bytes, local)? == 0x04034b50,
            "invalid local ZIP entry"
        );
        let local_len = u16_at(bytes, local + 26)? as usize;
        let local_extra = u16_at(bytes, local + 28)? as usize;
        anyhow::ensure!(
            bytes.get(local + 30..local + 30 + local_len) == Some(name.as_bytes()),
            "ZIP local path mismatch"
        );
        anyhow::ensure!(
            (local + 30 + local_len + local_extra)
                .checked_add(u32_at(bytes, pos + 20)? as usize)
                .is_some_and(|n| n <= start),
            "ZIP entry overlaps directory"
        );
        let mut at = pos + 46 + len;
        let limit = at + extra;
        while at < limit {
            anyhow::ensure!(
                at + 4 <= limit && u16_at(bytes, at)? != 1,
                "ZIP64 or broken extra field"
            );
            at += 4 + u16_at(bytes, at + 2)? as usize;
            anyhow::ensure!(at <= limit, "broken ZIP extra field");
        }
        pos = next;
    }
    anyhow::ensure!(
        pos == end && names.contains("manifest.json"),
        "ZIP directory or manifest mismatch"
    );
    Ok(())
}

pub fn validate_archive(
    bytes: &[u8],
) -> anyhow::Result<(SampleManifest, BTreeMap<String, Vec<u8>>)> {
    anyhow::ensure!(bytes.len() <= MAX_ARCHIVE_BYTES, "archive too large");
    validate_zip_directory(bytes)?;
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))?;
    anyhow::ensure!(
        !zip.is_empty() && zip.len() <= MAX_FILES,
        "archive entry limit"
    );
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        let path = file.name().to_owned();
        anyhow::ensure!(
            safe_path(&path) && !file.is_dir(),
            "unsafe archive path: {path}"
        );
        anyhow::ensure!(
            file.unix_mode()
                .is_none_or(|mode| mode & 0o170000 != 0o120000),
            "symlink entry forbidden"
        );
        anyhow::ensure!(!files.contains_key(&path), "duplicate archive path");
        let limit = if path == "manifest.json" {
            MAX_MANIFEST
        } else {
            MAX_ARCHIVE_BYTES as u64
        };
        anyhow::ensure!(file.size() <= limit, "entry too large");
        total = total
            .checked_add(file.size())
            .ok_or_else(|| anyhow::anyhow!("size overflow"))?;
        anyhow::ensure!(
            total <= MAX_ARCHIVE_BYTES as u64,
            "expanded archive too large"
        );
        let mut body = Vec::new();
        let declared = file.size();
        file.by_ref().take(declared + 1).read_to_end(&mut body)?;
        anyhow::ensure!(
            body.len() as u64 == file.size() && body.len() as u64 <= limit,
            "entry size mismatch"
        );
        files.insert(path, body);
    }
    let manifest_bytes = files
        .remove("manifest.json")
        .ok_or_else(|| anyhow::anyhow!("manifest missing"))?;
    let manifest: SampleManifest = serde_json::from_slice(&manifest_bytes)?;
    validate_manifest(&manifest, &files, true)?;
    Ok((manifest, files))
}
/// Server-side portable-format contract, including optional clips. Keep this
/// internal bundle server-side; public sample.read returns only PNG previews.
pub fn read_bundle(bytes: &[u8]) -> anyhow::Result<Value> {
    let (manifest, files) = validate_archive(bytes)?;
    let files: Vec<Value> = files.into_iter().map(|(path,bytes)|json!({"path":path,"base64":base64::engine::general_purpose::STANDARD.encode(bytes)})).collect();
    Ok(json!({"manifest":manifest,"files":files}))
}

pub fn validate_bundle(manifest: &Value, files: &BTreeMap<String, Vec<u8>>) -> anyhow::Result<()> {
    let manifest: SampleManifest = serde_json::from_value(manifest.clone())?;
    validate_manifest(&manifest, files, false)
}

fn validate_manifest(
    m: &SampleManifest,
    files: &BTreeMap<String, Vec<u8>>,
    require_clips: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(m.schema_version == 1, "sample schema unsupported");
    sample_path(&m.id)?;
    anyhow::ensure!(
        m.name.chars().count() <= 120 && !m.name.trim().is_empty(),
        "invalid sample name"
    );
    anyhow::ensure!(
        m.content_sha256 == fingerprint(m)?,
        "sample fingerprint mismatch"
    );
    anyhow::ensure!(
        m.start.timeline_us < m.end.timeline_us
            && m.end.timeline_us - m.start.timeline_us <= 300_000_000,
        "invalid START/END range"
    );
    anyhow::ensure!(
        m.goal.confirmed
            && !m.goal.description.trim().is_empty()
            && m.goal.description.chars().count() <= 2000,
        "goal not explicitly confirmed"
    );
    anyhow::ensure!(
        matches!(m.status.as_str(), "complete" | "unable_to_validate"),
        "invalid status"
    );
    anyhow::ensure!(
        m.max_frame_gap_us == 500_000,
        "unsupported evidence gap bound"
    );
    anyhow::ensure!(
        m.status != "complete" || m.diagnostics.is_empty(),
        "complete sample has diagnostics"
    );
    anyhow::ensure!(
        m.frames.len() <= MAX_FRAMES
            && m.actions.len() <= MAX_ACTIONS
            && m.files.len() >= files.len()
            && (!require_clips || m.files.len() == files.len()),
        "manifest limits or file set mismatch"
    );
    anyhow::ensure!(
        m.coordinates.space == "device-display"
            && m.coordinates.width > 0
            && m.coordinates.height > 0
            && m.coordinates.width <= 16384
            && m.coordinates.height <= 16384
            && [0, 90, 180, 270].contains(&m.coordinates.rotation),
        "invalid coordinates"
    );
    anyhow::ensure!(
        m.files
            .iter()
            .try_fold(0u64, |sum, f| sum.checked_add(f.size))
            .is_some_and(|n| n <= MAX_ARCHIVE_BYTES as u64),
        "expanded bundle exceeds size limit"
    );
    let mut indexed = BTreeSet::new();
    for f in &m.files {
        anyhow::ensure!(
            safe_path(&f.path) && crate::media::is_sha256_hex(&f.sha256),
            "invalid file metadata"
        );
        anyhow::ensure!(indexed.insert(&f.path), "duplicate file index");
        if !require_clips
            && !files.contains_key(&f.path)
            && m.segments.iter().any(|seg| {
                seg.clip_path.as_deref() == Some(f.path.as_str())
                    && seg.clip_sha256.as_deref() == Some(f.sha256.as_str())
            })
        {
            continue;
        }
        let bytes = files
            .get(&f.path)
            .ok_or_else(|| anyhow::anyhow!("file missing: {}", f.path))?;
        anyhow::ensure!(
            bytes.len() as u64 == f.size && hash(bytes) == f.sha256,
            "file hash/size mismatch: {}",
            f.path
        );
    }
    anyhow::ensure!(
        files.keys().all(|path| indexed.contains(path)),
        "payload is missing from canonical file index"
    );
    anyhow::ensure!(
        files
            .values()
            .try_fold(0usize, |sum, b| sum.checked_add(b.len()))
            .is_some_and(|n| n <= MAX_ARCHIVE_BYTES),
        "actual bundle bytes exceed size limit"
    );
    let clip_backed = !m.segments.is_empty()
        && m.segments.iter().all(|seg| {
            seg.clip_path
                .as_ref()
                .is_some_and(|path| files.contains_key(path))
        });
    let mut frame_ids = BTreeSet::new();
    let mut source_frames = BTreeSet::new();
    let mut media_hashes = BTreeMap::new();
    let mut frame_paths = BTreeSet::new();
    let mut prev = 0;
    for f in &m.frames {
        anyhow::ensure!(
            frame_ids.insert(&f.id)
                && frame_paths.insert(&f.path)
                && source_frames.insert((&f.media_id, f.frame_index)),
            "duplicate frame"
        );
        if let Some(previous) = media_hashes.insert(&f.media_id, &f.media_sha256) {
            anyhow::ensure!(previous == &f.media_sha256, "source media identity changed");
        }
        anyhow::ensure!(
            f.timeline_us >= m.start.timeline_us
                && f.timeline_us <= m.end.timeline_us
                && f.timeline_us >= prev,
            "frame outside range or order"
        );
        prev = f.timeline_us;
        anyhow::ensure!(
            f.path.starts_with("frames/") && f.path.ends_with(".png"),
            "invalid frame path"
        );
        let bytes = files
            .get(&f.path)
            .ok_or_else(|| anyhow::anyhow!("frame missing"))?;
        anyhow::ensure!(hash(bytes) == f.sha256, "frame hash mismatch");
        let mut reader =
            image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        let decoded = reader.decode()?;
        let image = (decoded.width(), decoded.height());
        anyhow::ensure!(
            image == (f.width, f.height)
                && f.width > 0
                && f.height > 0
                && f.width <= 8192
                && f.height <= 8192
                && (f.width as u64) * (f.height as u64) <= 16_777_216,
            "frame geometry mismatch"
        );
        let seg = m
            .segments
            .iter()
            .find(|s| s.media_id == f.media_id)
            .ok_or_else(|| anyhow::anyhow!("frame segment missing"))?;
        anyhow::ensure!(
            f.timeline_us
                == seg
                    .start_us
                    .checked_add(f.pts_us)
                    .ok_or_else(|| anyhow::anyhow!("frame time overflow"))?,
            "frame timing mismatch"
        );
        anyhow::ensure!(f.pts_us <= seg.duration_us, "frame extends beyond segment");
        anyhow::ensure!(
            crate::media::is_sha256_hex(&f.media_sha256),
            "invalid source SHA"
        );
        if m.status == "complete" {
            anyhow::ensure!(
                f.width == m.coordinates.width
                    && f.height == m.coordinates.height
                    && f.rotation == m.coordinates.rotation,
                "coordinate mapping unavailable"
            );
        }
    }
    let mut segment_ids = BTreeSet::new();
    for seg in &m.segments {
        anyhow::ensure!(
            segment_ids.insert(&seg.media_id)
                && crate::media::is_valid_media_id(&seg.media_id)
                && seg.duration_us > 0,
            "invalid/duplicate sample segment"
        );
        match (&seg.clip_path, &seg.clip_sha256) {
            (Some(path), Some(sha)) => {
                anyhow::ensure!(
                    path.starts_with("clips/") && path.ends_with(".mp4"),
                    "invalid clip path"
                );
                anyhow::ensure!(
                    m.files.iter().any(|f| &f.path == path && &f.sha256 == sha),
                    "clip file metadata missing"
                );
                anyhow::ensure!(
                    files
                        .get(path)
                        .map(|b| hash(b) == *sha)
                        .unwrap_or(!require_clips),
                    "clip hash mismatch"
                );
            }
            (None, None) => {}
            _ => anyhow::bail!("partial clip reference"),
        }
    }
    let referenced: BTreeSet<&str> = m
        .frames
        .iter()
        .map(|f| f.path.as_str())
        .chain(m.segments.iter().filter_map(|s| s.clip_path.as_deref()))
        .collect();
    anyhow::ensure!(
        files.keys().all(|p| referenced.contains(p.as_str())),
        "unreferenced archive payload"
    );
    let mut event_ids = BTreeSet::new();
    let mut previous = 0;
    for e in &m.actions {
        anyhow::ensure!(
            event_ids.insert(&e.event_id) && !e.event_id.is_empty(),
            "duplicate action id"
        );
        anyhow::ensure!(
            e.timeline_us >= m.start.timeline_us
                && e.timeline_us <= m.end.timeline_us
                && e.timeline_us >= previous,
            "action ordering/range invalid"
        );
        previous = e.timeline_us;
        anyhow::ensure!(
            e.session_id == m.recording_id,
            "action recording identity mismatch"
        );
        if m.status == "complete" {
            anyhow::ensure!(
                action_end(e) <= m.end.timeline_us,
                "action extends beyond END"
            );
            anyhow::ensure!(
                unsupported_action(e, &m.coordinates).is_none(),
                "unsupported action cannot be complete"
            );
        }
    }
    let mut window_ids = BTreeSet::new();
    for w in &m.windows {
        anyhow::ensure!(window_ids.insert(&w.event_id), "duplicate window");
        let e = m
            .actions
            .iter()
            .find(|e| e.event_id == w.event_id)
            .ok_or_else(|| anyhow::anyhow!("window action missing"))?;
        let event_index = m
            .actions
            .iter()
            .position(|a| a.event_id == w.event_id)
            .unwrap();
        let before_floor = event_index
            .checked_sub(1)
            .map(|i| action_end(&m.actions[i]))
            .unwrap_or(m.start.timeline_us);
        let after_limit = m
            .actions
            .get(event_index + 1)
            .map(|a| a.timeline_us)
            .unwrap_or(m.end.timeline_us);
        let mut unique = BTreeSet::new();
        for (before, ids) in [(true, &w.before_frame_ids), (false, &w.after_frame_ids)] {
            let mut timestamps = BTreeSet::new();
            for id in ids {
                anyhow::ensure!(unique.insert(id), "duplicate window frame");
                let f = m
                    .frames
                    .iter()
                    .find(|f| &f.id == id)
                    .ok_or_else(|| anyhow::anyhow!("window frame missing"))?;
                anyhow::ensure!(
                    timestamps.insert(f.timeline_us),
                    "window frames must have distinct source timestamps"
                );
                anyhow::ensure!(
                    if before {
                        f.timeline_us < e.timeline_us && f.timeline_us >= before_floor
                    } else {
                        f.timeline_us >= action_end(e) && f.timeline_us <= after_limit
                    },
                    "window is not genuine before/after evidence"
                );
            }
        }
        if m.status == "complete" {
            anyhow::ensure!(
                !w.before_frame_ids.is_empty() && w.after_frame_ids.len() >= 2,
                "window evidence insufficient"
            );
        }
    }
    if m.status == "complete" {
        anyhow::ensure!(
            clip_backed
                || m.frames
                    .windows(2)
                    .all(|pair| pair[1].timeline_us - pair[0].timeline_us <= m.max_frame_gap_us),
            "sparse frames need original clip evidence; import the complete archive"
        );
        anyhow::ensure!(
            m.windows.len() == m.actions.len()
                && frame_ids.contains(&m.start.frame_id)
                && frame_ids.contains(&m.end.frame_id),
            "missing complete sample evidence"
        );
        let start = m.frames.iter().find(|f| f.id == m.start.frame_id).unwrap();
        let end = m.frames.iter().find(|f| f.id == m.end.frame_id).unwrap();
        anyhow::ensure!(
            start.timeline_us == m.start.timeline_us
                && end.timeline_us == m.end.timeline_us
                && start.timeline_us < end.timeline_us,
            "START/END must identify exact confirmed boundary frames"
        );
        anyhow::ensure!(
            m.actions
                .first()
                .is_none_or(|e| start.timeline_us <= e.timeline_us),
            "START follows first action"
        );
        anyhow::ensure!(
            m.actions
                .last()
                .is_none_or(|e| end.timeline_us >= action_end(e)),
            "END predates final action completion"
        );
    }
    Ok(())
}

/// Raw PackageStore uploads use the same validator as public reads. The ID is
/// immutable and independent of destination package, so import is portable.
pub fn validate_resource(path: &str, bytes: &[u8]) -> Result<(), Value> {
    if !path.starts_with("samples/") {
        return Ok(());
    }
    let result = (|| {
        let (m, _) = validate_archive(bytes)?;
        anyhow::ensure!(path == sample_path(&m.id)?, "sample ID/path mismatch");
        Ok::<_, anyhow::Error>(())
    })();
    result.map_err(|e| json!([{"code":"sample.invalid","message":e.to_string(),"path":path}]))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn png() -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(8, 8)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }
    fn fixture() -> (SampleManifest, BTreeMap<String, Vec<u8>>) {
        let bytes = png();
        let sha = hash(&bytes);
        let media = "a".repeat(32);
        let frame = |id: &str, t| SampleFrame {
            id: id.into(),
            path: format!("frames/{id}.png"),
            sha256: sha.clone(),
            media_id: media.clone(),
            media_sha256: "b".repeat(64),
            frame_index: t as u32,
            pts_us: t,
            timeline_us: t,
            width: 8,
            height: 8,
            rotation: 0,
            role: "observation".into(),
            event_id: None,
        };
        let frames = vec![frame("start", 0), frame("end", 1000)];
        let files = frames
            .iter()
            .map(|f| (f.path.clone(), bytes.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut m = SampleManifest {
            schema_version: 1,
            id: "sample-1".into(),
            name: "example".into(),
            content_sha256: String::new(),
            recording_id: "r1".into(),
            start: Boundary {
                timeline_us: 0,
                frame_id: "start".into(),
            },
            end: Boundary {
                timeline_us: 1000,
                frame_id: "end".into(),
            },
            goal: Goal {
                description: "goal visible".into(),
                confirmed: true,
            },
            coordinates: Coordinates {
                space: "device-display".into(),
                width: 8,
                height: 8,
                rotation: 0,
            },
            status: "complete".into(),
            max_frame_gap_us: 500_000,
            diagnostics: vec![],
            actions: vec![],
            frames,
            windows: vec![],
            segments: vec![SampleSegment {
                media_id: media,
                start_us: 0,
                duration_us: 1000,
                base_pts_us: 5_000_000,
                clip_path: None,
                clip_sha256: None,
            }],
            files: files
                .iter()
                .map(|(p, b)| SampleFile {
                    path: p.clone(),
                    sha256: hash(b),
                    size: b.len() as u64,
                })
                .collect(),
        };
        m.content_sha256 = fingerprint(&m).unwrap();
        (m, files)
    }
    #[test]
    fn portable_archive_is_deterministic_and_self_contained() {
        let (m, f) = fixture();
        let a = encode_archive(&m, &f).unwrap();
        assert_eq!(a, encode_archive(&m, &f).unwrap());
        let (r, files) = validate_archive(&a).unwrap();
        assert_eq!(r.content_sha256, m.content_sha256);
        assert_eq!(files, f);
        assert!(validate_resource("samples/sample-1.gamersample", &a).is_ok());
        assert!(validate_resource("samples/other.gamersample", &a).is_err());
    }
    #[test]
    fn tamper_and_false_completeness_are_rejected() {
        let (mut m, mut f) = fixture();
        f.get_mut("frames/end.png").unwrap().push(1);
        assert!(validate_archive(&encode_archive(&m, &f).unwrap()).is_err());
        let (_, f) = fixture();
        m.goal.confirmed = false;
        m.content_sha256 = fingerprint(&m).unwrap();
        assert!(validate_archive(&encode_archive(&m, &f).unwrap()).is_err());
    }
    #[test]
    fn unsafe_and_duplicate_zip_entries_are_rejected() {
        for names in [
            vec!["../outside"],
            vec!["/outside"],
            vec!["a\\b"],
            vec!["manifest.json", "manifest.json"],
        ] {
            let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for n in names {
                if z.start_file(n, zip::write::SimpleFileOptions::default())
                    .is_ok()
                {
                    z.write_all(b"{}").unwrap();
                }
            }
            let b = z.finish().unwrap().into_inner();
            assert!(validate_archive(&b).is_err());
        }
    }
    #[test]
    fn exact_boundary_and_neighbor_windows_cannot_be_reassigned() {
        let (mut m, mut files) = fixture();
        m.start.frame_id = "end".into();
        m.content_sha256 = fingerprint(&m).unwrap();
        assert!(validate_manifest(&m, &files, true).is_err());
        let (mut m, _) = fixture();
        let template = m.frames[0].clone();
        for t in [400, 600, 800] {
            let mut f = template.clone();
            f.id = format!("f{t}");
            f.path = format!("frames/f{t}.png");
            f.pts_us = t;
            f.timeline_us = t;
            f.frame_index = t as u32;
            files.insert(f.path.clone(), png());
            m.frames.push(f);
        }
        m.frames.sort_by_key(|f| f.timeline_us);
        m.files = files
            .iter()
            .map(|(p, b)| SampleFile {
                path: p.clone(),
                sha256: hash(b),
                size: b.len() as u64,
            })
            .collect();
        let event = |id: &str, t| InputEventRecord {
            schema_version: 1,
            event_id: id.into(),
            operation_id: format!("op-{id}"),
            session_id: "r1".into(),
            source: "manual".into(),
            kind: "tap".into(),
            timeline_us: t,
            time_domain: "recording".into(),
            coordinate_space: "device-display".into(),
            display_size: crate::recording::DisplaySize {
                width: 8,
                height: 8,
            },
            payload: json!({"x":1,"y":1,"duration_us":10}),
            status: "accepted".into(),
        };
        m.actions = vec![event("a1", 300), event("a2", 700)];
        m.windows = vec![
            Window {
                event_id: "a1".into(),
                before_frame_ids: vec!["start".into()],
                after_frame_ids: vec!["f400".into(), "f600".into()],
            },
            Window {
                event_id: "a2".into(),
                before_frame_ids: vec!["f600".into()],
                after_frame_ids: vec!["f800".into(), "end".into()],
            },
        ];
        m.content_sha256 = fingerprint(&m).unwrap();
        validate_manifest(&m, &files, true).unwrap();
        m.windows[0].after_frame_ids = vec!["f800".into(), "end".into()];
        m.content_sha256 = fingerprint(&m).unwrap();
        assert!(validate_manifest(&m, &files, true).is_err());
    }
    #[test]
    fn inline_bundle_allows_only_omitted_optional_clips() {
        let (mut m, files) = fixture();
        let path = "clips/original.mp4".to_string();
        let sha = hash(b"clip");
        m.segments[0].clip_path = Some(path.clone());
        m.segments[0].clip_sha256 = Some(sha.clone());
        m.files.push(SampleFile {
            path,
            sha256: sha,
            size: 4,
        });
        m.content_sha256 = fingerprint(&m).unwrap();
        validate_bundle(&serde_json::to_value(&m).unwrap(), &files).unwrap();
        assert!(validate_manifest(&m, &files, true).is_err());
        let mut missing = files;
        missing.remove("frames/end.png");
        assert!(validate_bundle(&serde_json::to_value(&m).unwrap(), &missing).is_err());
    }

    #[test]
    fn collinear_overshoot_and_reverse_gestures_remain_unsupported() {
        let coordinates = Coordinates {
            space: "device-display".into(),
            width: 200,
            height: 200,
            rotation: 0,
        };
        let mut event = InputEventRecord {
            schema_version: 1,
            event_id: "e".into(),
            operation_id: "op".into(),
            session_id: "r".into(),
            source: "manual".into(),
            kind: "swipe".into(),
            timeline_us: 0,
            time_domain: "recording".into(),
            coordinate_space: "device-display".into(),
            display_size: crate::recording::DisplaySize {
                width: 200,
                height: 200,
            },
            payload: json!({"x":10,"y":10,"x2":100,"y2":10,"duration_us":1000,"points":[[0,10,10],[500,50,10],[1000,100,10]]}),
            status: "accepted".into(),
        };
        assert!(unsupported_action(&event, &coordinates).is_none());
        event.payload["x2"] = json!(20);
        event.payload["points"] = json!([[0, 10, 10], [500, 100, 10], [1000, 20, 10]]);
        assert!(unsupported_action(&event, &coordinates).is_some());
        event.payload["x2"] = json!(100);
        event.payload["points"] =
            json!([[0, 10, 10], [300, 80, 10], [600, 30, 10], [1000, 100, 10]]);
        assert!(unsupported_action(&event, &coordinates).is_some());
    }
    #[test]
    fn inline_frame_bytes_must_be_in_the_canonical_file_index() {
        let (mut m, files) = fixture();
        for (i, file) in m.files.iter_mut().enumerate() {
            file.path = format!("clips/{i}.mp4");
            let mut seg = m.segments[0].clone();
            seg.media_id = if i == 0 {
                "a".repeat(32)
            } else {
                "b".repeat(32)
            };
            seg.clip_path = Some(file.path.clone());
            seg.clip_sha256 = Some(file.sha256.clone());
            if i == 0 {
                m.segments[0] = seg;
            } else {
                m.segments.push(seg);
            }
        }
        m.content_sha256 = fingerprint(&m).unwrap();
        assert!(validate_bundle(&serde_json::to_value(&m).unwrap(), &files)
            .unwrap_err()
            .to_string()
            .contains("file index"));
    }

    #[test]
    fn resource_handler_rejects_text_uploads_and_identity_renames() {
        use crate::resources::ResourceHandler;
        let root = tempfile::tempdir().unwrap();
        let store = PackageStore::open(&crate::config::Config {
            data_dir: root.path().into(),
            ..Default::default()
        })
        .unwrap();
        let handler = super::super::project::VideoProjectResourceHandler;
        assert_eq!(
            handler.max_upload_bytes("samples/ok.gamersample"),
            MAX_ARCHIVE_BYTES
        );
        assert_eq!(
            handler.max_upload_bytes("projects/ok.json"),
            16 * 1024 * 1024
        );
        assert!(handler
            .validate_save(crate::resources::SaveValidation {
                package: "p",
                plugin: super::super::VIDEO_EXTENSION_ID,
                path: "samples/ok.gamersample",
                content: "{}",
                store: &store
            })
            .is_err());
        assert!(handler
            .before_rename(
                &store,
                "p",
                super::super::VIDEO_EXTENSION_ID,
                "samples/ok.gamersample",
                "samples/changed.gamersample"
            )
            .is_err());
    }

    #[test]
    fn sparse_pngs_require_supplied_original_clip_evidence() {
        let (mut m, mut files) = fixture();
        m.end.timeline_us = 1_000_000;
        m.frames[1].timeline_us = 1_000_000;
        m.frames[1].pts_us = 1_000_000;
        m.segments[0].duration_us = 1_000_000;
        let path = "clips/original.mp4".to_string();
        let bytes = b"opaque clip bytes; decoded coverage is validated by replay".to_vec();
        let sha = hash(&bytes);
        m.segments[0].clip_path = Some(path.clone());
        m.segments[0].clip_sha256 = Some(sha.clone());
        m.files.push(SampleFile {
            path: path.clone(),
            sha256: sha,
            size: bytes.len() as u64,
        });
        m.content_sha256 = fingerprint(&m).unwrap();
        assert!(validate_bundle(&serde_json::to_value(&m).unwrap(), &files).is_err());
        files.insert(path.clone(), bytes);
        validate_bundle(&serde_json::to_value(&m).unwrap(), &files).unwrap();
        let archive = encode_archive(&m, &files).unwrap();
        let bundle = read_bundle(&archive).unwrap();
        assert!(bundle["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|file| file["path"] == path));
    }

    #[test]
    fn raw_zip_duplicates_and_symlinks_are_rejected_before_indexing() {
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for name in ["manifest.json", "same-a", "same-b"] {
            z.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"{}").unwrap();
        }
        let mut bytes = z.finish().unwrap().into_inner();
        for i in 0..bytes.len() - 6 {
            if &bytes[i..i + 6] == b"same-b" {
                bytes[i + 5] = b'a';
            }
        }
        assert!(validate_archive(&bytes)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
        let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
        z.start_file("manifest.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(b"{}").unwrap();
        z.add_symlink(
            "frames/link",
            "../../outside",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        assert!(validate_archive(&z.finish().unwrap().into_inner())
            .unwrap_err()
            .to_string()
            .contains("symlink"));
    }
    #[test]
    fn mismatched_disk_entry_counts_are_rejected() {
        let (m, f) = fixture();
        let mut bytes = encode_archive(&m, &f).unwrap();
        let end = bytes.len() - 22;
        bytes[end + 10..end + 12].copy_from_slice(&1u16.to_le_bytes());
        assert!(validate_archive(&bytes)
            .unwrap_err()
            .to_string()
            .contains("count"));
    }

    #[test]
    fn fingerprint_covers_action_goal_and_file_evidence() {
        let (m, _) = fixture();
        let mut changed = m.clone();
        changed.goal.description = "different".into();
        assert_ne!(fingerprint(&m).unwrap(), fingerprint(&changed).unwrap());
        changed = m.clone();
        changed.frames[1].timeline_us = 999;
        assert_ne!(fingerprint(&m).unwrap(), fingerprint(&changed).unwrap());
    }
}
