//! OCR is a native function over the same captured frame used by other vision
//! functions. Preview size, active tabs, and host filesystem paths are absent.
use super::*;
use base64::Engine;

/// This guard owns only the handle created by this OCR capture. Output stamps
/// and persisted trace evidence do not need the transient frame-store entry.
struct CapturedFrame {
    registry: CapabilityRegistry,
    handle: crate::capabilities::FrameHandle,
}
impl Drop for CapturedFrame {
    fn drop(&mut self) {
        if let Some(frames) = self.registry.frame() {
            frames.release(self.handle);
        }
    }
}

impl NativeYamlHost {
    pub(super) async fn ocr(&self, args: &BoundArgs, find: bool) -> Result<Value> {
        let recognizer = self
            .ocr
            .as_ref()
            .ok_or_else(|| anyhow!("OCR_UNAVAILABLE: 请在设置的运行依赖中准备文字识别引擎"))?;
        let mode = args.string("mode")?;
        if !matches!(mode.as_str(), "line" | "block" | "sparse") {
            bail!("OCR_INVALID_ARGUMENT: mode 必须是 line / block / sparse");
        }
        let scale = args.number("scale")?.unwrap_or(2.0);
        if !(1.0..=3.0).contains(&scale) || scale.fract() != 0.0 {
            bail!("OCR_INVALID_ARGUMENT: scale 必须是 1..3 的整数");
        }
        let timeout_ms = args.duration_ms("timeout")?;
        if !(1..=15_000).contains(&timeout_ms) {
            bail!("OCR_INVALID_ARGUMENT: timeout 必须在 1ms..15s");
        }
        let match_mode = if find {
            args.string("match")?
        } else {
            "contains".into()
        };
        if !matches!(match_mode.as_str(), "contains" | "exact") {
            bail!("OCR_INVALID_ARGUMENT: match 必须是 contains / exact");
        }
        let min_confidence = if find {
            args.number("min_confidence")?.unwrap_or(0.5)
        } else {
            0.0
        };
        if !(0.0..=1.0).contains(&min_confidence) {
            bail!("OCR_INVALID_ARGUMENT: min_confidence 必须在 0..1");
        }
        if self.runtime.cancelled() {
            bail!("CANCELLED: 运行已取消");
        }
        // Capture at original resolution, and bind the observation time before
        // preprocessing/recognition. Slow OCR never rejuvenates stale pixels.
        let frames = self
            .registry
            .frame()
            .ok_or_else(|| anyhow!("frame capability 未注册"))?;
        let frame = frames
            .capture(&self.device)
            .await
            .map_err(anyhow::Error::new)?;
        let captured = CapturedFrame {
            registry: self.registry.clone(),
            handle: frame,
        };
        self.refresh_screen(&frame).await?;
        *self.frame_stamp.write().unwrap() =
            frames.stamp(frame).await.map_err(anyhow::Error::new)?;
        let observed_at_ms = self.runtime.now_ms();
        let size = self.screen();
        let stamp = self
            .frame_stamp
            .read()
            .unwrap()
            .clone()
            .ok_or_else(|| anyhow!("OCR_FRAME_UNAVAILABLE: 原始截图缺少可信 FrameStamp"))?;
        let roi = region_pixels(args.region("region")?, size)?;
        let png = self
            .registry
            .frame()
            .unwrap()
            .image_png(frame, roi)
            .await
            .map_err(anyhow::Error::new)?;
        // Trace the exact observation even when recognition fails or times out.
        let mut metadata = self.run_state.metadata("ocr");
        metadata["region"] = json!(roi.map(|r| [r.x, r.y, r.width, r.height]));
        let _ = self
            .registry
            .frame()
            .unwrap()
            .trace_frame(frame, metadata, false)
            .await;
        // PNG bytes and the trusted stamp are now sufficient. Do not retain a
        // full decoded RGB frame while waiting in the OCR work queue.
        drop(captured);
        let mut request = json!({
            "png_base64": base64::engine::general_purpose::STANDARD.encode(png),
            "language": args.string("language")?, "mode": mode,
            "scale": scale as u64, "timeout_ms": timeout_ms,
        });
        if let Some(whitelist) = args.opt_string("whitelist")? {
            request["whitelist"] = json!(whitelist);
        }
        let operation = recognizer(request);
        tokio::pin!(operation);
        let deadline = tokio::time::sleep(Duration::from_millis(timeout_ms));
        tokio::pin!(deadline);
        let raw = loop {
            if self.runtime.cancelled() {
                bail!("CANCELLED: 运行已取消");
            }
            tokio::select! {
                result = &mut operation => break result?,
                _ = &mut deadline => bail!("OCR_TIMEOUT: 本地识别超过 {}ms", timeout_ms),
                // Use wall time, never RuntimeService::sleep: replay's virtual
                // clock must stay on the evidence frame during local compute.
                _ = tokio::time::sleep(Duration::from_millis(25)) => {}
            }
        };
        if self.runtime.cancelled() {
            bail!("CANCELLED: 运行已取消");
        }
        let result = mapped_result(raw, roi, size, &stamp, observed_at_ms)?;
        if !find {
            return Ok(result);
        }
        let needle = args.string("text")?;
        for candidate in result["lines"]
            .as_array()
            .unwrap()
            .iter()
            .chain(result["words"].as_array().unwrap().iter())
        {
            let text = candidate["text"].as_str().unwrap();
            let matches = if match_mode == "exact" {
                text == needle
            } else {
                text.contains(&needle)
            };
            if matches && candidate["confidence"].as_f64().unwrap() >= min_confidence {
                return Ok(candidate.clone());
            }
        }
        Ok(Value::Null)
    }
}

fn region_pixels(region: Option<[f64; 4]>, size: FrameSize) -> Result<Option<SearchRegion>> {
    let Some([x, y, w, h]) = region else {
        return Ok(None);
    };
    if w <= 0.0 || h <= 0.0 || x + w > 1.0 + f64::EPSILON || y + h > 1.0 + f64::EPSILON {
        bail!("OCR_INVALID_ARGUMENT: region 必须有正宽高且完全位于原始截图内");
    }
    let x = (x * f64::from(size.width)).floor() as u32;
    let y = (y * f64::from(size.height)).floor() as u32;
    let width = (w * f64::from(size.width)).floor() as u32;
    let height = (h * f64::from(size.height)).floor() as u32;
    if width == 0 || height == 0 || x + width > size.width || y + height > size.height {
        bail!("OCR_INVALID_ARGUMENT: region 像素范围为空或越界");
    }
    Ok(Some(SearchRegion::new(x, y, width, height)))
}

fn confidence(value: &Value) -> Result<f64> {
    value
        .as_f64()
        .filter(|v| (0.0..=1.0).contains(v))
        .ok_or_else(|| anyhow!("OCR_INVALID_RESULT: confidence 必须在 0..1"))
}

fn mapped_result(
    mut raw: Value,
    roi: Option<SearchRegion>,
    size: FrameSize,
    stamp: &crate::capabilities::FrameStamp,
    observed_at_ms: u64,
) -> Result<Value> {
    let roi = roi.unwrap_or(SearchRegion::new(0, 0, size.width, size.height));
    if raw["width"].as_u64() != Some(u64::from(roi.width))
        || raw["height"].as_u64() != Some(u64::from(roi.height))
    {
        bail!("OCR_INVALID_RESULT: OCR 结果尺寸必须对应原始截图区域");
    }
    if !raw["text"].is_string() {
        bail!("OCR_INVALID_RESULT: text 必须是字符串");
    }
    confidence(&raw["confidence"])?;
    for name in ["lines", "words"] {
        let boxes = raw[name]
            .as_array_mut()
            .ok_or_else(|| anyhow!("OCR_INVALID_RESULT: 缺少 {name}"))?;
        for bounds in boxes {
            if !bounds["text"].is_string() {
                bail!("OCR_INVALID_RESULT: text 必须是字符串");
            }
            confidence(&bounds["confidence"])?;
            let coordinate = |key: &str| -> Result<u32> {
                bounds[key]
                    .as_u64()
                    .and_then(|v| u32::try_from(v).ok())
                    .ok_or_else(|| anyhow!("OCR_INVALID_RESULT: 无效 {key}"))
            };
            let (x, y, width, height) = (
                coordinate("x")?,
                coordinate("y")?,
                coordinate("width")?,
                coordinate("height")?,
            );
            if width == 0
                || height == 0
                || x.checked_add(width).is_none_or(|end| end > roi.width)
                || y.checked_add(height).is_none_or(|end| end > roi.height)
            {
                bail!("OCR_INVALID_RESULT: 文字边界超出原始截图区域");
            }
            let (x, y) = (roi.x + x, roi.y + y);
            bounds["x"] = json!(x);
            bounds["y"] = json!(y);
            bounds["center"] = json!({
                "x": (f64::from(x) + f64::from(width) / 2.0) / f64::from(size.width),
                "y": (f64::from(y) + f64::from(height) / 2.0) / f64::from(size.height),
                "_frame": stamp, "_observed_at_ms": observed_at_ms,
            });
            bounds["_frame"] = json!(stamp);
            bounds["_observed_at_ms"] = json!(observed_at_ms);
        }
    }
    raw["width"] = json!(size.width);
    raw["height"] = json!(size.height);
    raw["region"] = json!([roi.x, roi.y, roi.width, roi.height]);
    raw["_frame"] = json!(stamp);
    raw["_observed_at_ms"] = json!(observed_at_ms);
    Ok(raw)
}
