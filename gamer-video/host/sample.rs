//! Portable evidence samples. This is a data contract, never a script generator.
//! ZIP files contain a deterministic manifest, full-resolution PNG screenshots
//! and recorded events. The original video is used only during screenshot extraction.
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

#[path = "sample_external.rs"]
pub(crate) mod external_tools;

pub(crate) const ACTIONS: &[&str] = &[
    "sample.create",
    "sample.read",
    "sample.list",
    "sample.import",
    "sample.export",
    "sample.delete",
];
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
    pub coordinates: Coordinates,
    pub status: String,
    pub diagnostics: Vec<Diagnostic>,
    pub actions: Vec<InputEventRecord>,
    pub frames: Vec<SampleFrame>,
    pub windows: Vec<Window>,
    pub segments: Vec<SampleSegment>,
    pub files: Vec<SampleFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_actions: Vec<IgnoredAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recording_issues: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IgnoredAction {
    pub event_id: String,
    pub reason: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Boundary {
    pub timeline_us: u64,
    pub frame_id: String,
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
    recording_id: String,
    name: String,
    start_us: u64,
    end_us: u64,
    #[serde(default)]
    ignored_actions: Vec<IgnoredAction>,
}

pub(crate) fn accepts(id: &str, action: &str) -> bool {
    id == super::VIDEO_EXTENSION_ID && ACTIONS.contains(&action)
}
pub(crate) fn permissions(id: &str, action: &str) -> Option<&'static [Permission]> {
    if !accepts(id, action) {
        return None;
    }
    Some(
        if matches!(action, "sample.create" | "sample.import" | "sample.delete") {
            &[
                Permission::MediaRead,
                Permission::MediaEventsRead,
                Permission::MediaWrite,
            ]
        } else {
            &[Permission::MediaRead]
        },
    )
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
// Serialize writes to the plugin's independent sample library.
static SAMPLE_IO: std::sync::Mutex<()> = std::sync::Mutex::new(());
fn sample_disk(data_dir: &Path, id: &str) -> anyhow::Result<std::path::PathBuf> {
    sample_path(id)?;
    let root = data_dir
        .join("plugin-data")
        .join(super::VIDEO_EXTENSION_ID)
        .join("samples");
    for path in [
        data_dir.join("plugin-data"),
        data_dir.join("plugin-data").join(super::VIDEO_EXTENSION_ID),
        root.clone(),
        root.join(format!("{id}.gamersample")),
    ] {
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            anyhow::ensure!(
                !meta.file_type().is_symlink(),
                "sample path cannot be a symlink"
            );
        }
    }
    Ok(root.join(format!("{id}.gamersample")))
}
pub(crate) fn read_stored_bundle(data_dir: &Path, id: &str) -> anyhow::Result<Value> {
    let _io = SAMPLE_IO
        .lock()
        .map_err(|_| anyhow::anyhow!("sample storage unavailable"))?;
    read_bundle(&std::fs::read(sample_disk(data_dir, id)?)?)
}
fn save_standalone(data_dir: &Path, manifest: &SampleManifest, bytes: &[u8]) -> anyhow::Result<()> {
    let disk = sample_disk(data_dir, &manifest.id)?;
    if disk.exists() {
        anyhow::ensure!(
            std::fs::read(&disk)? == bytes,
            "sample already exists with different content"
        );
        return Ok(());
    }
    std::fs::create_dir_all(disk.parent().unwrap())?;
    crate::core::fs::atomic_write(&disk, bytes)
}
fn dispatch(action: &str, values: &Value, data_dir: &Path) -> anyhow::Result<Value> {
    let _io = SAMPLE_IO
        .lock()
        .map_err(|_| anyhow::anyhow!("sample storage unavailable"))?;
    let cfg = crate::config::Config {
        data_dir: data_dir.to_path_buf(),
        ..Default::default()
    };
    let media = crate::media::service(&cfg);
    match action {
        "sample.create" => {
            let req: CreateRequest = serde_json::from_value(values.clone())?;
            let (manifest, bytes) = create(&req, &crate::recording::service(&cfg), &media)?;
            save_standalone(data_dir, &manifest, &bytes)?;
            Ok(json!({"manifest":manifest}))
        }
        "sample.import" => {
            let encoded = values["base64"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("archive required"))?;
            anyhow::ensure!(
                encoded.len() <= MAX_ARCHIVE_BYTES * 4 / 3 + 4,
                "sample archive too large"
            );
            let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
            let (manifest, _) = validate_archive(&bytes)?;
            save_standalone(data_dir, &manifest, &bytes)?;
            Ok(json!({"manifest":manifest}))
        }
        "sample.read" | "sample.export" | "sample.delete" => {
            let id = values["sample_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("sample_id required"))?;
            let disk = sample_disk(data_dir, id)?;
            let bytes = std::fs::read(&disk)?;
            let (manifest, files) = validate_archive(&bytes)?;
            anyhow::ensure!(manifest.id == id, "sample id mismatch");
            if action == "sample.export" {
                return Ok(
                    json!({"base64":base64::engine::general_purpose::STANDARD.encode(&bytes)}),
                );
            }
            if action == "sample.delete" {
                std::fs::remove_file(disk)?;
                return Ok(json!({"deleted":true}));
            }
            let files:Vec<Value> = files.into_iter().filter(|(path,_)|path.starts_with("frames/")).map(|(path,bytes)|json!({"path":path,"base64":base64::engine::general_purpose::STANDARD.encode(bytes)})).collect();
            Ok(json!({"manifest":manifest,"files":files}))
        }
        "sample.list" => {
            let root = sample_disk(data_dir, "probe")?
                .parent()
                .unwrap()
                .to_path_buf();
            let mut paths = if root.exists() {
                std::fs::read_dir(root)?
                    .map(|item| item.map(|e| e.path()))
                    .collect::<std::io::Result<Vec<_>>>()?
            } else {
                vec![]
            };
            paths.sort();
            let mut samples = vec![];
            for path in paths {
                if path.extension().and_then(|s| s.to_str()) != Some("gamersample") {
                    continue;
                }
                let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                match sample_disk(data_dir, id).and_then(|disk| Ok(validate_archive(&std::fs::read(disk)?)?.0)) {
                    Ok(manifest) => samples.push(json!({"id":manifest.id,"name":manifest.name,"status":manifest.status,"recording_id":manifest.recording_id,"content_sha256":manifest.content_sha256,"diagnostics":manifest.diagnostics})),
                    Err(e) => samples.push(json!({"id":id,"status":"corrupt","diagnostics":[{"code":"archive.invalid","message":e.to_string()}]})),
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
    manifest.status = "incomplete".into();
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
        "invalid sample name"
    );
    anyhow::ensure!(req.start_us < req.end_us, "select START < END");
    let session = recording.status(&RecordingId(req.recording_id.clone()))?;
    anyhow::ensure!(
        !matches!(
            session.state,
            RecordingState::Recording | RecordingState::Finalizing
        ),
        "finish recording before making a sample"
    );
    let events = recording.events(&session.id)?;
    let actions: Vec<_> = events
        .into_iter()
        .filter(|event| event.timeline_us >= req.start_us && event.timeline_us <= req.end_us)
        .collect();
    anyhow::ensure!(
        actions.len() <= MAX_ACTIONS,
        "sample has too many actions (max 100)"
    );
    let first = segment_at(&session.segments, req.start_us)
        .ok_or_else(|| anyhow::anyhow!("START has no media segment"))?;
    let meta = media.get(&first.media_id)?;
    let selected: Vec<_> = session
        .segments
        .iter()
        .filter(|seg| {
            seg.start_us <= req.end_us
                && seg.start_us.saturating_add(seg.duration_us) >= req.start_us
        })
        .cloned()
        .collect();
    let mut manifest = SampleManifest {
        schema_version: 2,
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
        coordinates: Coordinates {
            space: "device-display".into(),
            width: meta.width,
            height: meta.height,
            rotation: meta.rotation,
        },
        status: "complete".into(),
        diagnostics: vec![],
        actions,
        frames: vec![],
        windows: vec![],
        files: vec![],
        segments: selected
            .iter()
            .map(|seg| SampleSegment {
                media_id: seg.media_id.0.clone(),
                start_us: seg.start_us,
                duration_us: seg.duration_us,
                base_pts_us: seg.base_pts_us,
            })
            .collect(),
        ignored_actions: req.ignored_actions.clone(),
        recording_issues: session.evidence_issues.clone(),
    };
    let ignored: BTreeSet<_> = req
        .ignored_actions
        .iter()
        .map(|a| a.event_id.as_str())
        .collect();
    anyhow::ensure!(
        ignored.len() == req.ignored_actions.len(),
        "duplicate ignored action"
    );
    for annotation in &req.ignored_actions {
        anyhow::ensure!(
            manifest
                .actions
                .iter()
                .any(|a| a.event_id == annotation.event_id)
                && !annotation.reason.trim().is_empty()
                && annotation.reason.len() <= 1000,
            "ignored action requires a recorded event and reason"
        );
    }
    let mut files = BTreeMap::new();
    manifest.start.frame_id = add_frame(
        media,
        &selected,
        &mut manifest,
        &mut files,
        req.start_us,
        false,
        "start",
        None,
    )?;
    manifest.end.frame_id = add_frame(
        media,
        &selected,
        &mut manifest,
        &mut files,
        req.end_us,
        true,
        "end",
        None,
    )?;
    // START/END select actual frames. They describe the selected lesson, not a
    // user assertion that every task has already succeeded.
    manifest.start.timeline_us = manifest
        .frames
        .iter()
        .find(|frame| frame.id == manifest.start.frame_id)
        .unwrap()
        .timeline_us;
    manifest.end.timeline_us = manifest
        .frames
        .iter()
        .find(|frame| frame.id == manifest.end.frame_id)
        .unwrap()
        .timeline_us;
    let events = manifest.actions.clone();
    let mut before_frames = vec![];
    for event in events
        .iter()
        .filter(|event| !ignored.contains(event.event_id.as_str()))
    {
        let before_time = event
            .timeline_us
            .saturating_sub(1)
            .max(manifest.start.timeline_us);
        let before = add_frame(
            media,
            &selected,
            &mut manifest,
            &mut files,
            before_time,
            true,
            "before",
            Some(&event.event_id),
        )?;
        before_frames.push((event, before));
    }
    // The next observed operation state is also the previous action's result.
    // Reuse that PNG rather than storing arbitrary fixed-delay duplicates.
    for (index, (event, before)) in before_frames.iter().enumerate() {
        let after = before_frames
            .get(index + 1)
            .map(|(_, frame)| frame.clone())
            .unwrap_or_else(|| manifest.end.frame_id.clone());
        let after_time = manifest
            .frames
            .iter()
            .find(|frame| frame.id == after)
            .unwrap()
            .timeline_us;
        manifest.windows.push(Window {
            event_id: event.event_id.clone(),
            before_frame_ids: vec![before.clone()],
            after_frame_ids: if after_time >= action_end(event) {
                vec![after]
            } else {
                vec![]
            },
        });
    }
    manifest.frames.sort_by_key(|frame| frame.timeline_us);
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
    let neighbors = media.frame_neighbors(&seg.media_id, pos.index)?;
    let unique_pts = neighbors
        .prev
        .as_ref()
        .is_none_or(|p| p.pts_us != pos.pts_us)
        && neighbors
            .next
            .as_ref()
            .is_none_or(|p| p.pts_us != pos.pts_us);
    let extracted = media.extract_frame(
        &seg.media_id,
        &FrameRequest {
            index: (!unique_pts).then_some(pos.index),
            pts_us: unique_pts.then_some(pos.pts_us),
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
        "sample screenshots exceed 128 MiB; select a smaller lesson"
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
    validate_manifest(&manifest, &files)?;
    Ok((manifest, files))
}
/// Portable screenshot/event contract. Reading it needs no original video.
pub fn read_bundle(bytes: &[u8]) -> anyhow::Result<Value> {
    let (manifest, files) = validate_archive(bytes)?;
    let files: Vec<Value> = files.into_iter().map(|(path,bytes)|json!({"path":path,"base64":base64::engine::general_purpose::STANDARD.encode(bytes)})).collect();
    Ok(json!({"manifest":manifest,"files":files}))
}

pub fn validate_bundle(manifest: &Value, files: &BTreeMap<String, Vec<u8>>) -> anyhow::Result<()> {
    let manifest: SampleManifest = serde_json::from_value(manifest.clone())?;
    validate_manifest(&manifest, files)
}

fn validate_manifest(m: &SampleManifest, files: &BTreeMap<String, Vec<u8>>) -> anyhow::Result<()> {
    anyhow::ensure!(
        m.schema_version == 2,
        "only screenshot/event sample schema 2 is supported; remake old video-dependent samples"
    );
    sample_path(&m.id)?;
    anyhow::ensure!(
        !m.name.trim().is_empty() && m.name.chars().count() <= 120,
        "invalid sample name"
    );
    anyhow::ensure!(
        m.content_sha256 == fingerprint(m)?,
        "sample fingerprint mismatch"
    );
    anyhow::ensure!(
        m.start.timeline_us < m.end.timeline_us,
        "invalid START/END range"
    );
    anyhow::ensure!(
        matches!(m.status.as_str(), "complete" | "incomplete"),
        "invalid sample status"
    );
    anyhow::ensure!(
        m.status != "complete" || m.diagnostics.is_empty(),
        "complete sample has diagnostics"
    );
    anyhow::ensure!(
        m.frames.len() >= 2
            && m.frames.len() <= MAX_FRAMES
            && m.actions.len() <= MAX_ACTIONS
            && m.files.len() == files.len(),
        "sample limits or file set mismatch"
    );
    anyhow::ensure!(
        m.coordinates.space == "device-display"
            && m.coordinates.width > 0
            && m.coordinates.height > 0
            && m.coordinates.width <= 8192
            && m.coordinates.height <= 8192
            && [0, 90, 180, 270].contains(&m.coordinates.rotation),
        "invalid coordinates"
    );
    let mut paths = BTreeSet::new();
    let mut total = 0u64;
    for file in &m.files {
        anyhow::ensure!(
            safe_path(&file.path)
                && file.path.starts_with("frames/")
                && file.path.ends_with(".png")
                && paths.insert(file.path.clone()),
            "sample payload must contain unique PNG screenshots only"
        );
        let bytes = files
            .get(&file.path)
            .ok_or_else(|| anyhow::anyhow!("missing screenshot"))?;
        anyhow::ensure!(
            bytes.len() as u64 == file.size && hash(bytes) == file.sha256,
            "screenshot hash/size mismatch"
        );
        total = total
            .checked_add(file.size)
            .ok_or_else(|| anyhow::anyhow!("sample size overflow"))?;
    }
    anyhow::ensure!(
        total <= MAX_ARCHIVE_BYTES as u64,
        "sample screenshots exceed128MiB"
    );
    let mut ids = BTreeSet::new();
    let mut frame_paths = BTreeSet::new();
    for frame in &m.frames {
        anyhow::ensure!(
            !frame.id.is_empty()
                && ids.insert(frame.id.clone())
                && frame_paths.insert(frame.path.clone()),
            "duplicate frame identity"
        );
        anyhow::ensure!(
            frame.timeline_us >= m.start.timeline_us && frame.timeline_us <= m.end.timeline_us,
            "frame outside selected lesson"
        );
        let bytes = files
            .get(&frame.path)
            .ok_or_else(|| anyhow::anyhow!("missing frame PNG"))?;
        anyhow::ensure!(hash(bytes) == frame.sha256, "frame SHA256 mismatch");
        let image = decode_png(bytes)?;
        anyhow::ensure!(
            image.width() == frame.width
                && image.height() == frame.height
                && frame.width == m.coordinates.width
                && frame.height == m.coordinates.height
                && frame.rotation == m.coordinates.rotation,
            "frame geometry mismatch"
        );
    }
    anyhow::ensure!(frame_paths == paths, "unreferenced screenshot payload");
    for boundary in [&m.start, &m.end] {
        anyhow::ensure!(
            m.frames
                .iter()
                .any(|frame| frame.id == boundary.frame_id
                    && frame.timeline_us == boundary.timeline_us),
            "START/END frame missing"
        );
    }
    let mut events = BTreeSet::new();
    let mut previous = m.start.timeline_us;
    for event in &m.actions {
        anyhow::ensure!(
            event.schema_version == 1
                && !event.operation_id.is_empty()
                && event.status == "accepted"
                && event.time_domain == "recording"
                && event.coordinate_space == "device-display",
            "invalid event provenance"
        );
        anyhow::ensure!(
            events.insert(event.event_id.clone())
                && event.timeline_us >= previous
                && event.timeline_us <= m.end.timeline_us,
            "event timeline invalid"
        );
        previous = event.timeline_us;
        if matches!(event.kind.as_str(), "tap" | "long_press" | "swipe") {
            anyhow::ensure!(
                event.payload["x"]
                    .as_u64()
                    .is_some_and(|x| x < m.coordinates.width as u64)
                    && event.payload["y"]
                        .as_u64()
                        .is_some_and(|y| y < m.coordinates.height as u64),
                "event coordinates invalid"
            );
        }
    }
    let ignored: BTreeSet<_> = m
        .ignored_actions
        .iter()
        .map(|a| a.event_id.as_str())
        .collect();
    anyhow::ensure!(
        ignored.len() == m.ignored_actions.len(),
        "duplicate ignored event"
    );
    for ignored in &m.ignored_actions {
        anyhow::ensure!(
            events.contains(&ignored.event_id) && !ignored.reason.trim().is_empty(),
            "ignored event requires source and reason"
        );
    }
    let mut windows = BTreeSet::new();
    for window in &m.windows {
        anyhow::ensure!(
            events.contains(&window.event_id)
                && windows.insert(window.event_id.as_str())
                && !window.before_frame_ids.is_empty(),
            "event screenshot reference invalid"
        );
        for id in window
            .before_frame_ids
            .iter()
            .chain(&window.after_frame_ids)
        {
            anyhow::ensure!(ids.contains(id), "event screenshot missing");
        }
    }
    for event in &m.actions {
        anyhow::ensure!(
            ignored.contains(event.event_id.as_str()) || windows.contains(event.event_id.as_str()),
            "event before screenshot missing"
        );
    }
    Ok(())
}

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

fn decode_png(bytes: &[u8]) -> anyhow::Result<image::DynamicImage> {
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode()?;
    anyhow::ensure!(
        (image.width() as u64) * (image.height() as u64) <= 16_777_216,
        "PNG pixel budget exceeded"
    );
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn fixture() -> (SampleManifest, BTreeMap<String, Vec<u8>>) {
        let mut png = Cursor::new(vec![]);
        image::RgbImage::new(16, 16)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png = png.into_inner();
        let files = BTreeMap::from([
            ("frames/start.png".into(), png.clone()),
            ("frames/end.png".into(), png.clone()),
        ]);
        let frames=vec![("start",0),("end",7_200_000_000u64)].into_iter().map(|(id,at)|json!({"id":id,"path":format!("frames/{id}.png"),"sha256":hash(&png),"media_id":"unavailable-video","media_sha256":"original-source-hash","frame_index":0,"pts_us":at,"timeline_us":at,"width":16,"height":16,"rotation":0,"role":id})).collect::<Vec<_>>();
        let mut manifest:SampleManifest=serde_json::from_value(json!({"schema_version":2,"id":"test-screenshot-only","name":"two-hour sparse lesson","recording_id":"recording","content_sha256":"","start":{"timeline_us":0,"frame_id":"start"},"end":{"timeline_us":7_200_000_000u64,"frame_id":"end"},"coordinates":{"space":"device-display","width":16,"height":16,"rotation":0},"status":"complete","diagnostics":[],"actions":[],"windows":[],"segments":[],"frames":frames,"files":files.iter().map(|(path,bytes)|json!({"path":path,"sha256":hash(bytes),"size":bytes.len()})).collect::<Vec<_>>()})).unwrap();
        manifest.content_sha256 = fingerprint(&manifest).unwrap();
        (manifest, files)
    }
    #[test]
    fn sparse_long_material_import_read_export_needs_no_video_or_package() {
        let (manifest, files) = fixture();
        let archive = encode_archive(&manifest, &files).unwrap();
        let root = tempfile::tempdir().unwrap();
        let imported = dispatch(
            "sample.import",
            &json!({"base64":base64::engine::general_purpose::STANDARD.encode(&archive)}),
            root.path(),
        )
        .unwrap();
        assert_eq!(imported["manifest"]["status"], "complete");
        let read = dispatch(
            "sample.read",
            &json!({"sample_id":manifest.id}),
            root.path(),
        )
        .unwrap();
        assert_eq!(read["files"].as_array().unwrap().len(), 2);
        assert!(!root.path().join("media").exists());
        assert!(!root.path().join("packages").exists());
        let exported = dispatch(
            "sample.export",
            &json!({"sample_id":manifest.id}),
            root.path(),
        )
        .unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(exported["base64"].as_str().unwrap())
                .unwrap(),
            archive
        );
    }
    #[test]
    fn modified_png_and_video_payload_are_rejected() {
        let (mut manifest, mut files) = fixture();
        files.values_mut().next().unwrap().push(0);
        assert!(validate_manifest(&manifest, &files)
            .unwrap_err()
            .to_string()
            .contains("hash/size"));
        let (original, original_files) = fixture();
        manifest = original;
        files = original_files;
        files.insert("clips/video.mp4".into(), vec![1]);
        manifest.files.push(SampleFile {
            path: "clips/video.mp4".into(),
            size: 1,
            sha256: hash(&[1]),
        });
        manifest.content_sha256 = fingerprint(&manifest).unwrap();
        assert!(validate_manifest(&manifest, &files)
            .unwrap_err()
            .to_string()
            .contains("PNG"));
    }
    #[test]
    fn zip_traversal_and_duplicate_entries_rejected_before_read() {
        for names in [
            ["../outside", "manifest.json"],
            ["manifest.json", "manifest.jsom"],
        ] {
            let mut zip = zip::ZipWriter::new(Cursor::new(vec![]));
            for name in names {
                zip.start_file(name, zip::write::SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(b"{}").unwrap();
            }
            let mut archive = zip.finish().unwrap().into_inner();
            for offset in 0..archive.len().saturating_sub(12) {
                if &archive[offset..offset + 13] == b"manifest.jsom" {
                    archive[offset + 12] = b'n';
                }
            }
            assert!(validate_archive(&archive).is_err());
        }
    }
}
