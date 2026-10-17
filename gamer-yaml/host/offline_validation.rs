//! Screenshot/event material checks. No video decoder, virtual game clock,
//! simulated input, action replay, or device lease is involved. A passing report
//! means the referenced controls match saved screenshots, never a real-game pass.
use super::{native_funcs, syntax};
use anyhow::{anyhow, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
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
    pub checked_templates: usize,
    pub matched_templates: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationReport {
    pub scope: String,
    pub status: ValidationStatus,
    pub candidate_sha256: String,
    pub samples: Vec<SampleReport>,
    pub diagnostics: Vec<ValidationDiagnostic>,
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
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
pub fn candidate_sha256(request: &ValidationRequest) -> Result<String> {
    let templates: BTreeMap<_, _> = request
        .templates
        .iter()
        .map(|(name, bytes)| (name, json!({"sha256":sha(bytes),"size":bytes.len()})))
        .collect();
    let samples:Vec<_>=request.samples.iter().map(|sample|json!({"manifest":sample.manifest,"files":sample.files.iter().map(|(path,bytes)|(path,json!({"sha256":sha(bytes),"size":bytes.len()}))).collect::<BTreeMap<_,_>>()})).collect();
    Ok(sha(&serde_json::to_vec(
        &json!({"contract":"screenshot_template_checks_v1","yaml":request.yaml,"args":request.args,"settings":request.settings,"functions_sources":request.functions_sources,"templates":templates,"samples":samples}),
    )?))
}
pub async fn validate_candidate(
    request: ValidationRequest,
    stop: Arc<AtomicBool>,
) -> Result<ValidationReport> {
    ensure!(
        !request.samples.is_empty() && request.samples.len() <= 32,
        "select1..32 screenshot samples"
    );
    ensure!(request.yaml.len() <= 1024 * 1024, "script exceeds1MiB");
    let size: usize = request.templates.values().map(Vec::len).sum::<usize>()
        + request
            .samples
            .iter()
            .flat_map(|s| s.files.values())
            .map(Vec::len)
            .sum::<usize>();
    ensure!(size <= 128 * 1024 * 1024, "material input exceeds128MiB");
    tokio::task::spawn_blocking(move || check_materials(request, stop)).await?
}
pub async fn preflight_samples(samples: Vec<SampleInput>, stop: Arc<AtomicBool>) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        ensure!(
            !samples.is_empty() && samples.len() <= 32,
            "select1..32 samples"
        );
        for sample in samples {
            ensure!(!stop.load(Ordering::Acquire), "CANCELLED");
            crate::extensions::video::sample::validate_bundle(&sample.manifest, &sample.files)?;
            ensure!(
                sample.manifest["status"] == "complete",
                "sample screenshots are incomplete"
            );
        }
        Ok(())
    })
    .await?
}
#[derive(Clone)]
pub(super) struct TemplateCheck {
    pub(super) name: String,
    threshold: f32,
    pub(super) region: Option<[f64; 4]>,
    pub(super) region_explicit: bool,
}
fn template_checks(value: &Value, out: &mut Vec<TemplateCheck>) {
    if let Some(map) = value.as_object() {
        let threshold = map.get("threshold").and_then(Value::as_f64).unwrap_or(0.8) as f32;
        let region = map.get("region").and_then(Value::as_array).and_then(|v| {
            if v.len() != 4 {
                return None;
            }
            Some([
                v[0].as_f64()?,
                v[1].as_f64()?,
                v[2].as_f64()?,
                v[3].as_f64()?,
            ])
        });
        for field in ["template", "templates", "obstacles"] {
            if let Some(value) = map.get(field) {
                let names: Vec<_> = if let Some(name) = value.as_str() {
                    vec![name]
                } else {
                    value
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect()
                };
                for name in names {
                    if !name.starts_with('$') {
                        out.push(TemplateCheck {
                            name: name.into(),
                            threshold,
                            region: if field == "obstacles" { None } else { region },
                            region_explicit: field != "obstacles" && map.contains_key("region"),
                        })
                    }
                }
            }
        }
        for value in map.values() {
            template_checks(value, out)
        }
    } else if let Some(values) = value.as_array() {
        for value in values {
            template_checks(value, out)
        }
    }
}
pub(super) fn template_checks_for_source(yaml: &str) -> Result<Vec<TemplateCheck>> {
    let raw = serde_json::to_value(serde_yaml::from_str::<serde_yaml::Value>(yaml)?)?;
    let mut checks = Vec::new();
    template_checks(&raw, &mut checks);
    Ok(checks)
}
fn check_materials(request: ValidationRequest, stop: Arc<AtomicBool>) -> Result<ValidationReport> {
    let fingerprint = candidate_sha256(&request)?;
    let syntax_error = prepare_program_value(&request).err();
    let checks = template_checks_for_source(&request.yaml).unwrap_or_default();
    let mut reports = vec![];
    let mut sample_ids = BTreeSet::new();
    let mut valid = vec![];
    for sample in &request.samples {
        ensure!(!stop.load(Ordering::Acquire), "CANCELLED");
        let id = sample.manifest["id"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        ensure!(sample_ids.insert(id.clone()), "duplicate sample id");
        let mut report = SampleReport {
            sample_id: id,
            content_sha256: sample.manifest["content_sha256"]
                .as_str()
                .unwrap_or("")
                .into(),
            status: ValidationStatus::Passed,
            diagnostics: vec![],
            events: vec![],
            checked_templates: 0,
            matched_templates: 0,
        };
        match crate::extensions::video::sample::validate_bundle(&sample.manifest, &sample.files) {
            Err(error) => {
                report.status = ValidationStatus::InsufficientEvidence;
                report
                    .diagnostics
                    .push(diagnostic("sample.invalid", error.to_string()));
            }
            Ok(()) if sample.manifest["status"] != "complete" => {
                report.status = ValidationStatus::InsufficientEvidence;
                report
                    .diagnostics
                    .push(diagnostic("sample.incomplete", "素材截图不完整"));
            }
            Ok(()) => {
                if let Some(error) = &syntax_error {
                    report.status = ValidationStatus::Failed;
                    report
                        .diagnostics
                        .push(diagnostic("script.invalid", error.to_string()));
                } else {
                    valid.push(reports.len());
                }
            }
        }
        reports.push(report);
    }
    let mut diagnostics = vec![];
    let mut prepared = vec![];
    for check in checks {
        match crate::resources::resolve_short_relative_path(
            &check.name,
            request.templates.keys().map(String::as_str),
        ) {
            Ok(path) => prepared.push((check, path)),
            Err(error) => diagnostics.push(diagnostic("template.missing", error.to_string())),
        }
    }
    // A control may occur in only one demonstration. Check the union of saved
    // pictures; never require each demonstration to contain every branch.
    let had_valid = !valid.is_empty();
    let mut matched = BTreeSet::new();
    for sample_index in valid {
        let sample = &request.samples[sample_index];
        let report = &mut reports[sample_index];
        report.checked_templates = prepared.len();
        for frame in sample.manifest["frames"].as_array().into_iter().flatten() {
            if matched.len() == prepared.len() {
                break;
            }
            ensure!(!stop.load(Ordering::Acquire), "CANCELLED");
            let frame_path = frame["path"].as_str().context("frame path missing")?;
            let decoded = crate::matcher::DecodedFrame::from_png(&sample.files[frame_path])?;
            let (width, height) = decoded.dimensions();
            for (index, (check, path)) in prepared.iter().enumerate() {
                if matched.contains(&index) {
                    continue;
                }
                ensure!(!stop.load(Ordering::Acquire), "CANCELLED");
                let region = check.region.map(|r| {
                    [
                        (r[0] * width as f64).floor() as u32,
                        (r[1] * height as f64).floor() as u32,
                        (r[2] * width as f64).floor() as u32,
                        (r[3] * height as f64).floor() as u32,
                    ]
                });
                let result = crate::matcher::match_decoded_frame(
                    &decoded,
                    &crate::matcher::DecodedMatchRequest {
                        template_png: request.templates[path].clone(),
                        threshold: Some(check.threshold),
                        region: crate::matcher::effective_search_region(
                            region,
                            Some(path),
                            width,
                            height,
                        ),
                        color: path.ends_with("#1.png"),
                    },
                );
                match result {
                    Ok(Some(hit)) => {
                        matched.insert(index);
                        report.matched_templates += 1;
                        report.events.push(json!({"ev":"template_match","template":check.name,"frame_id":frame["id"],"score":hit.score,"center":{"x":hit.x+hit.width/2,"y":hit.y+hit.height/2}}));
                    }
                    Ok(None) => (),
                    Err(error) => {
                        matched.insert(index);
                        diagnostics.push(diagnostic(
                            "template.invalid",
                            format!("{}: {error}", check.name),
                        ));
                    }
                }
            }
        }
    }
    for (index, (check, _)) in prepared.iter().enumerate() {
        if !matched.contains(&index) && syntax_error.is_none() && had_valid {
            diagnostics.push(diagnostic(
                "template.not_found",
                format!(
                    "模板 {} 未在选定素材截图的指定区域内匹配；这不是游戏运行结果",
                    check.name
                ),
            ));
        }
    }
    let status = if !diagnostics.is_empty()
        || reports.iter().any(|r| r.status == ValidationStatus::Failed)
    {
        ValidationStatus::Failed
    } else if reports
        .iter()
        .any(|r| r.status == ValidationStatus::InsufficientEvidence)
    {
        ValidationStatus::InsufficientEvidence
    } else {
        ValidationStatus::Passed
    };
    Ok(ValidationReport {
        scope: "screenshot_templates".into(),
        status,
        candidate_sha256: fingerprint,
        samples: reports,
        diagnostics,
    })
}
pub(crate) fn prepare_program_value(request: &ValidationRequest) -> Result<Value> {
    let script = syntax::parse_script(&request.yaml).map_err(|d| {
        anyhow!(d
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; "))
    })?;
    let mut library = vec![];
    let mut names = native_funcs::native_names();
    for (path, source) in &request.functions_sources {
        for (name, def) in
            syntax::callable_source(path, source).map_err(|d| anyhow!(format!("{d:?}")))?
        {
            ensure!(names.insert(name.clone()), "FUNCTION_CONFLICT: {name}");
            library.push((name, def));
        }
    }
    let mut roots = script.called_functions();
    // Existing package libraries retain their validation contract. Script
    // callables are checked only when reachable, identically to live runs.
    roots.extend(
        library
            .iter()
            .filter(|(name, _)| !name.contains(".yaml"))
            .flat_map(|(_, def)| def.called_functions()),
    );
    for called in syntax::call_closure(roots, &library) {
        ensure!(names.contains(&called), "FUNCTION_NOT_FOUND: {called}");
    }
    let types = super::reference_types::script(&script, &library);
    ensure!(types.is_empty(), "script argument types invalid: {types:?}");
    let bound = super::task_params::bind_entry_args("offline", &script.params, &request.args, true)
        .map_err(|d| anyhow!(format!("{d:?}")))?;
    let mut vars: Map<String, Value> = script.vars.iter().cloned().collect();
    vars.extend(bound.resolved);
    let mut program = syntax::build_program(&script, &library, vars, 0);
    program["_required_checkpoints"] = json!(checkpoint_ids(request)?);
    Ok(program)
}

pub(crate) fn checkpoint_ids(
    request: &ValidationRequest,
) -> Result<std::collections::BTreeSet<String>> {
    fn visit(
        value: &serde_yaml::Value,
        ids: &mut std::collections::BTreeSet<String>,
    ) -> Result<()> {
        if let Some(map) = value.as_mapping() {
            if let Some(check) = map.get(serde_yaml::Value::String("checkpoint".into())) {
                let id = check["id"]
                    .as_str()
                    .context("checkpoint id 必须为固定字符串")?;
                ensure!(
                    !id.is_empty() && !id.starts_with('$') && id.len() <= 128,
                    "invalid checkpoint id"
                );
                ids.insert(id.to_string());
            }
            for value in map.values() {
                visit(value, ids)?;
            }
        } else if let Some(list) = value.as_sequence() {
            for value in list {
                visit(value, ids)?;
            }
        }
        Ok(())
    }
    let mut ids = std::collections::BTreeSet::new();
    visit(&serde_yaml::from_str(&request.yaml)?, &mut ids)?;
    let script = syntax::parse_script(&request.yaml).map_err(|d| anyhow!("{d:?}"))?;
    let mut needed: Vec<_> = script.called_functions().into_iter().collect();
    for (_, def) in &script.functions {
        needed.extend(def.called_functions());
    }
    let mut visited = std::collections::BTreeSet::new();
    let mut definitions = BTreeMap::new();
    for (path, yaml) in &request.functions_sources {
        let raw: serde_yaml::Value = serde_yaml::from_str(yaml)?;
        for (name, definition) in
            syntax::callable_source(path, yaml).map_err(|d| anyhow!("{d:?}"))?
        {
            let local = name.split_once('#').map(|(_, local)| local);
            let body = if let Some(local) = local {
                raw["functions"][local].clone()
            } else if name.ends_with(".yaml") {
                raw.clone()
            } else {
                raw["functions"][&name].clone()
            };
            definitions.insert(name.clone(), (definition, body));
        }
    }
    while let Some(name) = needed.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        if let Some((definition, raw)) = definitions.get(&name) {
            visit(raw, &mut ids)?;
            needed.extend(definition.called_functions());
        }
    }
    Ok(ids)
}

#[cfg(test)]
pub(crate) fn validation_fixture_samples() -> (String, BTreeMap<String, Vec<u8>>, Vec<SampleInput>)
{
    let r = tests::request();
    (r.yaml, r.templates, r.samples)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
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
            manifest: json!({"schema_version":2,"id":if popup{"with-confirm"}else{"without-confirm"},"name":"synthetic NCC fixture","content_sha256":"","recording_id":"recording","start":{"timeline_us":0,"frame_id":"f0"},"end":{"timeline_us":1_000_000,"frame_id":format!("f{}",frames.len()-1)},"coordinates":{"space":"device-display","width":64,"height":48,"rotation":0},"status":"complete","diagnostics":[],"actions":actions,"frames":frames,"windows":windows,"segments":[{"media_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","start_us":0,"duration_us":1_000_000,"base_pts_us":0}],"files":files.iter().map(|(path,b)|json!({"path":path,"size":b.len(),"sha256":sha(b)})).collect::<Vec<_>>()}),
            files,
        };
        seal(&mut result);
        result
    }
    pub(super) fn request() -> ValidationRequest {
        ValidationRequest{yaml:"targets:\n  claim: {template: claim.png, threshold: 0.99}\n  confirm: {template: confirm.png, threshold: 0.99}\n  done: {template: done.png, threshold: 0.99}\nrun:\n  - id: claim_button\n    wait: claim\n    then: [{tap: claim}]\n  - optional: {find: confirm, timeout: 250ms, then: [{tap: confirm}]}\n  - finish: done\n".into(),templates:BTreeMap::from([("claim.png".into(),png(&pattern(1))),("confirm.png".into(),png(&pattern(2))),("done.png".into(),png(&pattern(3)))]),samples:vec![fixture(true)],args:Map::new(),settings:super::super::settings::Settings::default(),functions_sources:BTreeMap::new()}
    }

    #[tokio::test]
    async fn checks_saved_screenshots_without_original_video_or_input_replay() {
        let request = request();
        let report = validate_candidate(request, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(report.scope, "screenshot_templates");
        assert_eq!(report.status, ValidationStatus::Passed);
        assert!(report
            .samples
            .iter()
            .all(|sample| sample.matched_templates > 0));
    }
    #[tokio::test]
    async fn a_control_missing_in_one_demo_can_match_another_demo() {
        let mut r = request();
        r.samples.insert(0, fixture(false));
        let report = validate_candidate(r, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(report.status, ValidationStatus::Passed);
        assert_eq!(report.samples.len(), 2);
        assert!(report
            .samples
            .iter()
            .all(|s| s.status == ValidationStatus::Passed));
    }
    #[tokio::test]
    async fn rejects_missing_template_and_modified_picture() {
        let mut r = request();
        r.templates.clear();
        let report = validate_candidate(r, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(report.status, ValidationStatus::Failed);
        let mut r = request();
        r.samples[0].files.values_mut().next().unwrap().push(0);
        let report = validate_candidate(r, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        assert_eq!(
            report.samples[0].status,
            ValidationStatus::InsufficientEvidence
        );
    }
    #[test]
    fn material_report_cannot_be_reused_after_script_or_template_changes() {
        let mut r = request();
        let first = candidate_sha256(&r).unwrap();
        r.yaml.push_str("# changed");
        assert_ne!(first, candidate_sha256(&r).unwrap());
    }
}
