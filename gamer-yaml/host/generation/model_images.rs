//! Model views derive from immutable originals. Server resolves view coordinates;
//! stored crops and replay always address original source pixels.
use super::{decode_image, Sample};
use anyhow::{ensure, Context, Result};
use base64::Engine;
use image::ImageEncoder;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
// Existing AI transport envelope; no additional generation transfer budget.
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_IMAGES: usize = 96;
const DETAIL_EDGE: u32 = 320;
pub(super) struct SourceFrame {
    sample_id: String,
    frame: Value,
    png: String,
    action: Option<Value>,
}
pub(super) struct Sources {
    frames: Vec<SourceFrame>,
    total_frames: usize,
}
#[derive(Debug)]
pub(super) struct Prepared {
    pub images: Vec<Value>,
    pub selected: Vec<Value>,
    pub total_frames: usize,
    pub full_frames: usize,
    pub bytes: usize,
}
pub(super) fn select(samples: &[Sample]) -> Result<Sources> {
    ensure!(
        !samples.is_empty() && samples.len() <= 12,
        "select 1..12 samples"
    );
    let mut result = Sources {
        frames: vec![],
        total_frames: 0,
    };
    for sample in samples {
        let m = &sample.manifest;
        let id = m["id"].as_str().context("sample id missing")?;
        let frames = m["frames"].as_array().context("sample frames missing")?;
        result.total_frames += frames.len();
        let mut ids = BTreeSet::new();
        for boundary in ["start", "end"] {
            ids.insert(
                m[boundary]["frame_id"]
                    .as_str()
                    .context("boundary frame missing")?,
            );
        }
        for window in m["windows"].as_array().into_iter().flatten() {
            ids.extend(
                window["before_frame_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            );
        }
        ids.extend(
            frames
                .iter()
                .filter(|f| f["role"] == "before")
                .filter_map(|f| f["id"].as_str()),
        );
        if m["windows"].as_array().is_none_or(|w| w.is_empty()) {
            ids.extend(frames.iter().filter_map(|f| f["id"].as_str()));
        }
        for frame in frames {
            if !frame["id"].as_str().is_some_and(|id| ids.contains(id)) {
                continue;
            }
            let path = frame["path"].as_str().context("frame path missing")?;
            let png = sample
                .files
                .iter()
                .find(|f| f.path == path)
                .context("frame PNG missing")?;
            let before = frame["role"] == "before"
                || m["windows"].as_array().is_some_and(|windows| {
                    windows.iter().any(|w| {
                        w["before_frame_ids"]
                            .as_array()
                            .is_some_and(|ids| ids.contains(&frame["id"]))
                    })
                });
            let event = frame["event_id"].as_str().or_else(|| {
                m["windows"].as_array()?.iter().find(|w| {
                    w["before_frame_ids"]
                        .as_array()
                        .is_some_and(|ids| ids.contains(&frame["id"]))
                })?["event_id"]
                    .as_str()
            });
            let action = if before {
                m["actions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|a| {
                        a["event_id"].as_str() == event
                            && event.is_some()
                            && a["kind"] == "tap"
                            && a["status"] == "accepted"
                    })
                    .cloned()
            } else {
                None
            };
            result.frames.push(SourceFrame {
                sample_id: id.into(),
                frame: frame.clone(),
                png: png.base64.clone(),
                action,
            });
        }
        ensure!(
            ids.iter().all(|id| frames.iter().any(|f| f["id"] == *id)),
            "selected frame missing"
        );
    }
    Ok(result)
}
fn jpeg(image: &image::RgbImage) -> Result<Vec<u8>> {
    let mut encoder = jpegli::Compress::new(jpegli::ColorSpace::JCS_RGB);
    encoder.set_size(image.width() as usize, image.height() as usize);
    encoder.set_quality(90.0);
    encoder.set_chroma_sampling_pixel_sizes((1, 1), (1, 1));
    encoder.set_optimize_coding(true);
    let mut encoder = encoder.start_compress(Vec::new())?;
    encoder.write_scanlines(image.as_raw())?;
    Ok(encoder.finish()?)
}
pub(super) fn prepare(sources: Sources, stop: &AtomicBool) -> Result<Prepared> {
    let mut result = Prepared {
        images: vec![],
        selected: vec![],
        total_frames: sources.total_frames,
        full_frames: 0,
        bytes: 0,
    };
    for source in sources.frames {
        ensure!(
            !stop.load(Ordering::Acquire),
            "CANCELLED: model image preparation"
        );
        let raw = base64::engine::general_purpose::STANDARD.decode(&source.png)?;
        let original = decode_image(&raw)?;
        let (width, height) = (original.width(), original.height());
        ensure!(
            Some(width as u64) == source.frame["width"].as_u64()
                && Some(height as u64) == source.frame["height"].as_u64(),
            "model source geometry mismatch"
        );
        let rgb = original.to_rgb8();
        let full = jpeg(&rgb)?;
        push(
            &mut result,
            &source,
            [0, 0, width, height],
            full,
            "image/jpeg",
            "full",
        )?;
        result.full_frames += 1;
        if let Some(action) = &source.action {
            let x = action["payload"]["x"].as_u64().context("tap x missing")?;
            let y = action["payload"]["y"].as_u64().context("tap y missing")?;
            ensure!(
                x < width as u64 && y < height as u64,
                "tap outside source image"
            );
            let w = DETAIL_EDGE.min(width);
            let h = DETAIL_EDGE.min(height);
            let left = (x as u32).saturating_sub(w / 2).min(width - w);
            let top = (y as u32).saturating_sub(h / 2).min(height - h);
            let pixels = image::imageops::crop_imm(&rgb, left, top, w, h).to_image();
            let mut png = vec![];
            image::codecs::png::PngEncoder::new_with_quality(
                &mut png,
                image::codecs::png::CompressionType::Best,
                image::codecs::png::FilterType::Adaptive,
            )
            .write_image(pixels.as_raw(), w, h, image::ExtendedColorType::Rgb8)?;
            push(
                &mut result,
                &source,
                [left, top, w, h],
                png,
                "image/png",
                "detail",
            )?;
        }
    }
    ensure!(
        !stop.load(Ordering::Acquire),
        "CANCELLED: model image preparation"
    );
    Ok(result)
}
fn push(
    result: &mut Prepared,
    source: &SourceFrame,
    rect: [u32; 4],
    bytes: Vec<u8>,
    mime: &str,
    kind: &str,
) -> Result<()> {
    result.bytes += bytes.len();
    ensure!(result.bytes <= MAX_BYTES && result.images.len() < MAX_IMAGES, "model_image_transport_limit: critical evidence exceeds AI transport envelope; split workflow into shorter demonstrations");
    let view_id = format!("view_{}", result.images.len());
    let frame_id = source.frame["id"].as_str().context("frame id missing")?;
    let tap = source.action.as_ref().map(|a| {
        json!([
            (a["payload"]["x"].as_f64().unwrap() - rect[0] as f64) / rect[2] as f64,
            (a["payload"]["y"].as_f64().unwrap() - rect[1] as f64) / rect[3] as f64
        ])
    });
    let metadata = json!({"view_id":view_id,"kind":kind,"sample_id":source.sample_id,"frame_id":frame_id,"timeline_us":source.frame["timeline_us"],"role":source.frame["role"],"original_width":source.frame["width"],"original_height":source.frame["height"],"image_width":rect[2],"image_height":rect[3],"source_rect":rect,"tap_in_view":tap});
    result.images.push(json!({"label":metadata.to_string(),"data_url":format!("data:{mime};base64,{}",base64::engine::general_purpose::STANDARD.encode(bytes))}));
    result.selected.push(metadata);
    Ok(())
}
/// AI crops use normalized view coordinates. Provider-side proportional resizing
/// does not change this space. Editors/diagnostics retain original-pixel Crop.
pub(super) fn resolve_proposal(mut proposal: Value, views: &[Value]) -> Result<Value> {
    let templates = proposal["templates"]
        .as_array_mut()
        .context("model templates missing")?;
    for crop in templates {
        let Some(id) = crop.get("view_id").and_then(Value::as_str) else {
            continue;
        };
        let view = views
            .iter()
            .find(|v| v["view_id"] == id)
            .context("unknown model view_id")?;
        let rect = crop["rect"].as_array().context("view crop rect missing")?;
        ensure!(rect.len() == 4, "view crop rect must contain four values");
        let r: Vec<_> = rect
            .iter()
            .map(|v| {
                v.as_f64()
                    .filter(|n| n.is_finite())
                    .context("invalid view crop coordinate")
            })
            .collect::<Result<_>>()?;
        ensure!(
            r[0] >= 0.0
                && r[1] >= 0.0
                && r[2] > 0.0
                && r[3] > 0.0
                && r[0] + r[2] <= 1.000001
                && r[1] + r[3] <= 1.000001,
            "view crop outside normalized image"
        );
        let source = view["source_rect"]
            .as_array()
            .context("view source rect missing")?;
        let source: Vec<_> = source
            .iter()
            .map(|v| v.as_u64().context("invalid view source rect"))
            .collect::<Result<_>>()?;
        let x0 = (r[0] * source[2] as f64).round() as u32;
        let y0 = (r[1] * source[3] as f64).round() as u32;
        let x1 = ((r[0] + r[2]) * source[2] as f64)
            .round()
            .min(source[2] as f64) as u32;
        let y1 = ((r[1] + r[3]) * source[3] as f64)
            .round()
            .min(source[3] as f64) as u32;
        ensure!(
            x1 > x0 && y1 > y0,
            "view crop rounds to empty source pixels"
        );
        let obj = crop.as_object_mut().context("crop must be object")?;
        obj.remove("view_id");
        ensure!(
            !obj.contains_key("sample_id") && !obj.contains_key("frame_id"),
            "crop must choose one coordinate space"
        );
        obj.insert("sample_id".into(), view["sample_id"].clone());
        obj.insert("frame_id".into(), view["frame_id"].clone());
        obj.insert(
            "rect".into(),
            json!([
                source[0] + x0 as u64,
                source[1] + y0 as u64,
                x1 - x0,
                y1 - y0
            ]),
        );
    }
    stabilize_action_regions(&mut proposal, views)?;
    Ok(proposal)
}

/// One demonstration records a concrete layout. Limit each visual target's
/// search to its original crop plus four pixels, so a matching entrance animation
/// cannot produce a click at a different position. Explicit END regions also
/// retain original-resolution matching instead of global coarse downsampling.
/// Multi-sample layouts retain
/// the model's regions and must pass every demonstration independently.
fn stabilize_action_regions(proposal: &mut Value, views: &[Value]) -> Result<()> {
    let samples: BTreeSet<_> = views
        .iter()
        .filter_map(|v| v["sample_id"].as_str())
        .collect();
    if samples.len() != 1 {
        return Ok(());
    }
    let Some(yaml) = proposal["yaml"].as_str() else {
        return Ok(());
    };
    let Ok(mut document) = serde_yaml::from_str::<serde_yaml::Value>(yaml) else {
        return Ok(());
    };
    let Some(targets) = document
        .get_mut("targets")
        .and_then(serde_yaml::Value::as_mapping_mut)
    else {
        return Ok(());
    };
    let mut changed = false;
    for target in targets.values_mut() {
        let Some(name) = target.get("template").and_then(serde_yaml::Value::as_str) else {
            continue;
        };
        let Some(crop) = proposal["templates"]
            .as_array()
            .and_then(|crops| crops.iter().find(|c| c["name"] == name))
        else {
            continue;
        };
        let Some(view) = views
            .iter()
            .find(|v| v["sample_id"] == crop["sample_id"] && v["frame_id"] == crop["frame_id"])
        else {
            continue;
        };
        let Some(rect) = crop["rect"].as_array() else {
            continue;
        };
        let r: Vec<_> = rect
            .iter()
            .map(|v| v.as_u64().context("original crop coordinate missing"))
            .collect::<Result<_>>()?;
        let width = view["original_width"]
            .as_u64()
            .context("view original width missing")?;
        let height = view["original_height"]
            .as_u64()
            .context("view original height missing")?;
        ensure!(
            r.len() == 4 && width > 0 && height > 0,
            "original crop geometry invalid"
        );
        let x_end = r[0].checked_add(r[2]).context("crop x overflow")?;
        let y_end = r[1].checked_add(r[3]).context("crop y overflow")?;
        ensure!(
            x_end <= width && y_end <= height,
            "original crop outside view source"
        );
        let left = r[0].saturating_sub(4);
        let top = r[1].saturating_sub(4);
        let right = x_end.saturating_add(4).min(width);
        let bottom = y_end.saturating_add(4).min(height);
        let region = vec![
            left as f64 / width as f64,
            top as f64 / height as f64,
            (right - left) as f64 / width as f64,
            (bottom - top) as f64 / height as f64,
        ];
        if let Some(mapping) = target.as_mapping_mut() {
            mapping.insert(
                serde_yaml::Value::String("region".into()),
                serde_yaml::to_value(region)?,
            );
            changed = true;
        }
    }
    if changed {
        proposal["yaml"] = json!(serde_yaml::to_string(&document)?);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::super::FileInput;
    use super::*;
    use std::io::Cursor;
    #[test]
    fn stabilizes_single_sample_action_regions_without_constraining_multiple_layouts() {
        let views = vec![
            json!({"view_id":"v","sample_id":"s","frame_id":"f","source_rect":[1600,200,320,320],"original_width":1920,"original_height":1080,"tap_in_view":[0.375,0.5625]}),
        ];
        let proposal = json!({"yaml":"version: 2\ntargets:\n  button: {template: b.png, threshold: 0.9, region: [0,0,1,1]}\nrun: []\n","templates":[{"name":"b.png","view_id":"v","rect":[0.25,0.5,0.25,0.125]}]});
        let resolved = resolve_proposal(proposal.clone(), &views).unwrap();
        let yaml: serde_yaml::Value =
            serde_yaml::from_str(resolved["yaml"].as_str().unwrap()).unwrap();
        assert_eq!(yaml["targets"]["button"]["threshold"].as_f64(), Some(0.9));
        assert!(
            (yaml["targets"]["button"]["region"][2].as_f64().unwrap() - 88.0 / 1920.0).abs()
                < 1e-12
        );
        let mut end_views = views.clone();
        end_views[0]["tap_in_view"] = Value::Null;
        assert_eq!(
            resolve_proposal(proposal.clone(), &end_views).unwrap()["yaml"],
            resolved["yaml"]
        );
        let mut multi = views.clone();
        let mut second = views[0].clone();
        second["sample_id"] = json!("other");
        multi.push(second);
        assert_eq!(
            resolve_proposal(proposal.clone(), &multi).unwrap()["yaml"],
            proposal["yaml"]
        );
    }
    fn sample() -> Sample {
        let rgb = image::RgbImage::from_fn(1920, 1080, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 251) as u8, 87])
        });
        let mut png = Cursor::new(vec![]);
        image::DynamicImage::ImageRgb8(rgb)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
        Sample {
            manifest: json!({"id":"a","start":{"frame_id":"f0"},"end":{"frame_id":"f3"},"frames":(0..4).map(|i|json!({"id":format!("f{i}"),"path":format!("frames/{i}.png"),"width":1920,"height":1080,"role":if i==1 {"before"} else {"observation"},"event_id":"tap"})).collect::<Vec<_>>(),"windows":[{"event_id":"tap","before_frame_ids":["f1"],"after_frame_ids":["f2"]}],"actions":[{"event_id":"tap","kind":"tap","status":"accepted","payload":{"x":1857,"y":341}}]}),
            files: (0..4)
                .map(|i| FileInput {
                    path: format!("frames/{i}.png"),
                    base64: encoded.clone(),
                })
                .collect(),
        }
    }
    #[test]
    fn full_resolution_jpeg_and_exact_detail_keep_source_immutable() {
        let samples = vec![sample()];
        let original = samples[0].files[0].base64.clone();
        let p = prepare(select(&samples).unwrap(), &AtomicBool::new(false)).unwrap();
        assert_eq!(p.full_frames, 3);
        assert_eq!(p.images.len(), 4);
        for (input, view) in p.images.iter().zip(&p.selected) {
            let data = input["data_url"].as_str().unwrap();
            let raw = base64::engine::general_purpose::STANDARD
                .decode(data.split(',').nth(1).unwrap())
                .unwrap();
            let decoded = decode_image(&raw).unwrap();
            if view["kind"] == "full" {
                assert_eq!((decoded.width(), decoded.height()), (1920, 1080));
                assert!(data.starts_with("data:image/jpeg;"));
            } else {
                let r = view["source_rect"].as_array().unwrap();
                let original = decode_image(
                    &base64::engine::general_purpose::STANDARD
                        .decode(&samples[0].files[1].base64)
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(
                    decoded.to_rgb8(),
                    original
                        .crop_imm(
                            r[0].as_u64().unwrap() as u32,
                            r[1].as_u64().unwrap() as u32,
                            320,
                            320
                        )
                        .to_rgb8()
                );
            }
        }
        assert_eq!(samples[0].files[0].base64, original);
    }
    #[test]
    fn server_maps_detail_crop_and_rejects_ambiguous_or_foreign_views() {
        let views = vec![
            json!({"view_id":"v","sample_id":"s","frame_id":"f","source_rect":[1600,200,320,320]}),
        ];
        let input =
            json!({"templates":[{"name":"button.png","view_id":"v","rect":[0.25,0.5,0.25,0.125]}]});
        let mapped = resolve_proposal(input, &views).unwrap();
        assert_eq!(mapped["templates"][0]["rect"], json!([1680, 360, 80, 40]));
        assert_eq!(mapped["templates"][0]["sample_id"], "s");
        assert!(mapped["templates"][0].get("view_id").is_none());
        for invalid in [
            json!({"name":"x","view_id":"other","rect":[0,0,1,1]}),
            json!({"name":"x","view_id":"v","sample_id":"s","rect":[0,0,1,1]}),
            json!({"name":"x","view_id":"v","rect":[0.9,0,0.2,1]}),
        ] {
            assert!(resolve_proposal(json!({"templates":[invalid]}), &views).is_err());
        }
    }
    #[test]
    fn cancellation_and_source_geometry_fail_before_submission() {
        assert!(
            prepare(select(&[sample()]).unwrap(), &AtomicBool::new(true))
                .unwrap_err()
                .to_string()
                .contains("CANCELLED")
        );
        let mut s = sample();
        s.manifest["frames"][0]["width"] = json!(128);
        assert!(prepare(select(&[s]).unwrap(), &AtomicBool::new(false))
            .unwrap_err()
            .to_string()
            .contains("geometry"));
    }
}
