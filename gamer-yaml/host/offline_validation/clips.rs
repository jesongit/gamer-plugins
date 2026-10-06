//! Immutable original clips back sparse evidence without inventing intermediate
//! frames. Core MediaService supplies the sole probe/index/exact decode path.
use super::*;
use crate::media::{FrameRequest, MediaId, MediaService};
use std::collections::VecDeque;

struct Segment {
    original_id: String,
    id: MediaId,
    start: u64,
    end: u64,
    pts: Vec<u64>,
}
pub(super) struct ClipEvidence {
    // Service must drop before TempDir removes its private files.
    service: MediaService,
    segments: Vec<Segment>,
    anchors: BTreeMap<(String, u32), (String, FrameHandle)>,
    pub(super) cached: Mutex<VecDeque<Arc<ReplayFrame>>>,
    size: FrameSize,
    stop: Arc<AtomicBool>,
    _temp: tempfile::TempDir,
}
impl ClipEvidence {
    pub(super) fn new(
        sample: &SampleInput,
        frames: &[FrameSource],
        ffmpeg: &str,
        stop: Arc<AtomicBool>,
    ) -> Result<Self> {
        let temp = tempfile::Builder::new()
            .prefix("gamer-sample-replay-")
            .tempdir()?;
        let service = MediaService::open(temp.path().join("media"), ffmpeg.to_string())?
            .with_cancel(stop.clone())
            .restrict_to_local_mp4();
        let m = &sample.manifest;
        let size = FrameSize::new(
            m["coordinates"]["width"].as_u64().unwrap() as u32,
            m["coordinates"]["height"].as_u64().unwrap() as u32,
        );
        let mut segments = vec![];
        for segment in m["segments"].as_array().unwrap() {
            if stop.load(Ordering::Relaxed) {
                bail!("CANCELLED: 素材探测已取消")
            }
            let path = segment["clip_path"]
                .as_str()
                .ok_or_else(|| anyhow!("EVIDENCE_CLIP_MISSING"))?;
            let bytes = sample
                .files
                .get(path)
                .ok_or_else(|| anyhow!("EVIDENCE_CLIP_MISSING: {path}"))?;
            ensure!(
                bytes.len() >= 12 && &bytes[4..8] == b"ftyp",
                "UNSUPPORTED: clip must be a self-contained MP4, not an auto-detected playlist"
            );
            let meta = service
                .import_bytes("sample.mp4", bytes)
                .map_err(|e| anyhow!("EVIDENCE_CLIP_DECODE: {e}"))?;
            ensure!(
                meta.sha256 == segment["clip_sha256"].as_str().unwrap_or(""),
                "EVIDENCE_CLIP_HASH"
            );
            ensure!(
                meta.width == size.width && meta.height == size.height && meta.rotation == 0,
                "EVIDENCE_CLIP_GEOMETRY: 原视频和坐标空间不一致"
            );
            let positions = service.frame_positions(&meta.id)?;
            ensure!(
                !positions.is_empty() && positions.len() <= 200_000,
                "EVIDENCE_CLIP_INDEX_BUDGET"
            );
            let pts: Vec<_> = positions.iter().map(|p| p.pts_us).collect();
            ensure!(
                pts.windows(2).all(|pair| pair[0] < pair[1]),
                "UNSUPPORTED_DUPLICATE_PTS: 视频展示时间必须严格递增；同时间多帧需序列感知回放"
            );
            let start = segment["start_us"]
                .as_u64()
                .ok_or_else(|| anyhow!("EVIDENCE_CLIP_START"))?;
            let duration = meta
                .duration_us
                .ok_or_else(|| anyhow!("EVIDENCE_CLIP_DURATION"))?;
            let declared = segment["duration_us"].as_u64().unwrap();
            let end = start
                .checked_add(duration.min(declared))
                .ok_or_else(|| anyhow!("EVIDENCE_CLIP_TIME_OVERFLOW"))?;
            segments.push(Segment {
                original_id: segment["media_id"].as_str().unwrap().into(),
                id: meta.id,
                start,
                end,
                pts,
            });
        }
        segments.sort_by_key(|s| s.start);
        let start = m["start"]["timeline_us"].as_u64().unwrap();
        let end = m["end"]["timeline_us"].as_u64().unwrap();
        let mut covered = start;
        for s in &segments {
            let first = s
                .start
                .checked_add(s.pts[0])
                .ok_or_else(|| anyhow!("EVIDENCE_CLIP_TIME_OVERFLOW"))?;
            if s.end < start || first > end {
                continue;
            }
            ensure!(first <= covered, "EVIDENCE_CLIP_GAP: 视频分段间缺少证据");
            covered = covered.max(s.end);
        }
        ensure!(covered >= end, "EVIDENCE_CLIP_GAP: 视频未覆盖 END");
        let mut anchors = BTreeMap::new();
        // Anchors cannot override contradictory video pixels. Verify each once,
        // sequentially, retaining only source bytes and a bounded decode cache.
        for (entry, frame) in m["frames"].as_array().unwrap().iter().zip(frames) {
            if stop.load(Ordering::Relaxed) {
                bail!("CANCELLED: 素材画面校验已取消")
            }
            let original = entry["media_id"].as_str().unwrap();
            let index = entry["frame_index"].as_u64().unwrap() as u32;
            let s = segments
                .iter()
                .find(|s| s.original_id == original)
                .ok_or_else(|| anyhow!("EVIDENCE_ANCHOR_SEGMENT"))?;
            let pts = s
                .pts
                .get(index as usize)
                .ok_or_else(|| anyhow!("EVIDENCE_ANCHOR_INDEX"))?;
            ensure!(
                s.start + *pts == frame.at && *pts == entry["pts_us"].as_u64().unwrap(),
                "EVIDENCE_ANCHOR_TIME: PNG 来源索引与视频不一致"
            );
            let extracted = Self::extract(&service, s, index)?;
            ensure!(
                extracted.descriptor.media_sha256 == entry["media_sha256"].as_str().unwrap_or(""),
                "EVIDENCE_ANCHOR_MEDIA_HASH"
            );
            let image = crate::matcher::DecodedFrame::from_png(&extracted.png)?;
            let anchor = crate::matcher::DecodedFrame::from_png(&frame.png)?;
            ensure!(
                image.dimensions() == anchor.dimensions()
                    && image.pixel_sha256() == anchor.pixel_sha256(),
                "EVIDENCE_ANCHOR_PIXELS: PNG 与原视频矛盾"
            );
            anchors.insert(
                (original.to_string(), index),
                (frame.id.clone(), frame.handle),
            );
        }
        Ok(Self {
            service,
            segments,
            anchors,
            cached: Mutex::new(VecDeque::new()),
            size,
            stop,
            _temp: temp,
        })
    }
    fn extract(
        service: &MediaService,
        segment: &Segment,
        index: u32,
    ) -> Result<crate::media::FrameExtraction> {
        let pts = segment.pts[index as usize];
        let unique = (index == 0 || segment.pts[index as usize - 1] != pts)
            && segment
                .pts
                .get(index as usize + 1)
                .is_none_or(|next| *next != pts);
        // Accurate seek to the already-selected real FLOOR PTS avoids decoding
        // the entire battle from frame zero for every observation. Duplicate
        // PTS keep explicit presentation-index order.
        let request = if unique {
            FrameRequest {
                pts_us: Some(pts),
                ..Default::default()
            }
        } else {
            FrameRequest {
                index: Some(index),
                ..Default::default()
            }
        };
        let frame = service.extract_frame(&segment.id, &request)?;
        ensure!(
            frame.descriptor.index == index && frame.descriptor.pts_us == pts,
            "EVIDENCE_CLIP_FRAME_ID"
        );
        Ok(frame)
    }

    #[cfg(test)]
    pub(super) fn temp_path(&self) -> std::path::PathBuf {
        self._temp.path().to_path_buf()
    }

    pub(super) fn frame_at(&self, at: u64) -> Result<Arc<ReplayFrame>> {
        if self.stop.load(Ordering::Relaxed) {
            bail!("CANCELLED: 视频回放已取消")
        }
        let s = self
            .segments
            .iter()
            .rev()
            .find(|s| at >= s.start + s.pts[0] && at <= s.end)
            .ok_or_else(|| anyhow!("EVIDENCE_CLIP_GAP: {at}us 没有对应视频"))?;
        let relative = at - s.start;
        let index = s
            .pts
            .partition_point(|pts| *pts <= relative)
            .checked_sub(1)
            .ok_or_else(|| anyhow!("EVIDENCE_CLIP_NO_PRIOR_FRAME"))? as u32;
        let actual = s.start + s.pts[index as usize];
        let (id, handle) = self
            .anchors
            .get(&(s.original_id.clone(), index))
            .cloned()
            .unwrap_or_else(|| {
                (
                    format!("clip:{}:{index}", s.original_id),
                    FrameHandle::new(),
                )
            });
        let mut cache = self.cached.lock().unwrap();
        if let Some(frame) = cache.iter().find(|f| f.id == id) {
            return Ok(frame.clone());
        }
        let extracted = Self::extract(&self.service, s, index)?;
        if self.stop.load(Ordering::Relaxed) {
            bail!("CANCELLED: 视频回放已取消")
        }
        ensure!(
            extracted.descriptor.pts_us == s.pts[index as usize],
            "EVIDENCE_CLIP_INDEX_CHANGED"
        );
        let image = crate::matcher::DecodedFrame::from_png(&extracted.png)?;
        ensure!(
            image.dimensions() == (self.size.width, self.size.height),
            "EVIDENCE_CLIP_GEOMETRY_CHANGED"
        );
        let frame = Arc::new(ReplayFrame {
            id,
            at: actual,
            content_sha256: image.pixel_sha256(),
            handle,
            image,
        });
        cache_frame(&mut cache, frame.clone());
        Ok(frame)
    }
    pub(super) fn next_time_after(&self, at: u64, end: u64) -> Option<u64> {
        self.segments
            .iter()
            .filter_map(|s| {
                let index = s.pts.partition_point(|p| s.start + *p <= at);
                s.pts.get(index).map(|p| s.start + *p)
            })
            .filter(|next| *next <= end)
            .min()
    }
}
