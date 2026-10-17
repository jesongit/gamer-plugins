//! Read-only access to exact legacy candidate files, plus explicit new-draft import.
use super::*;
use std::io::{Read, Seek, SeekFrom};
impl GenerationService {
    pub(crate) fn authoring(&self) -> AuthoringService {
        self.inner.authoring.clone()
    }
    pub(crate) fn packages(&self) -> Arc<PackageStore> {
        self.inner.packages.clone()
    }
    pub(crate) async fn legacy_call_admitted(
        &self,
        action: &str,
        args: Value,
        admission: authoring::Admission,
    ) -> Result<Value> {
        if action == "generation.import_draft" {
            self.inner
                .legacy_extra_admitted(action, &args, Some(admission))
                .await
        } else {
            admission()?;
            self.legacy_call(action, args).await
        }
    }
    pub(crate) async fn legacy_call(&self, action: &str, args: Value) -> Result<Value> {
        ensure!(
            matches!(
                action,
                "generation.list"
                    | "generation.get"
                    | "generation.template"
                    | "generation.history"
                    | "generation.export"
                    | "generation.import_draft"
            ),
            "unsupported legacy read/import action"
        );
        if action == "generation.history" {
            return revisions::history_read_only(
                &self.inner.packages,
                required(&args, "package_id")?,
            );
        }
        self.inner.dispatch(action, args).await
    }
}
#[derive(Deserialize)]
struct FrozenSnapshot {
    samples: Vec<Sample>,
    #[serde(with = "base64_map")]
    base: BTreeMap<String, Vec<u8>>,
}
struct Stored {
    candidate: Value,
    samples: Vec<Sample>,
    base: BTreeMap<String, Vec<u8>>,
}
#[derive(Deserialize)]
struct Crop {
    name: String,
    sample_id: String,
    frame_id: String,
    rect: [u32; 4],
}
fn projected_candidate(mut candidate: Value) -> Value {
    candidate["legacy_read_only"] = json!(true);
    if matches!(
        candidate["state"].as_str(),
        Some("generating" | "validating")
    ) {
        candidate["legacy_original_state"] = candidate["state"].clone();
        candidate["state"] = json!("retired");
        candidate["reason"] = json!("ai_workflow_removed");
    }
    candidate
}
impl Inner {
    pub(super) fn root(&self, package: &str) -> Result<PathBuf> {
        self.packages.manifest(package)?;
        Ok(self
            .packages
            .data_root()
            .join("extension-data/gamer-yaml/candidates")
            .join(package))
    }
    pub(super) fn file(&self, package: &str, id: &str) -> Result<PathBuf> {
        ensure!(uuid::Uuid::parse_str(id).is_ok(), "invalid candidate id");
        Ok(self.root(package)?.join(format!("{id}.json")))
    }
    fn read_envelope(&self, package: &str, id: &str) -> Result<Value> {
        let value = super::super::authoring_materials::read_bounded_json(
            &self.file(package, id)?,
            8 * 1024 * 1024,
        )?;
        ensure!(
            value["candidate"].is_object()
                && value["candidate"]["id"] == id
                && value["candidate"]["package_id"] == package,
            "candidate scope mismatch"
        );
        Ok(value)
    }
    fn read(&self, package: &str, id: &str) -> Result<Stored> {
        let envelope = self.read_envelope(package, id)?;
        let path = self.root(package)?.join(format!("{id}.snapshot"));
        let raw = super::super::authoring_materials::read_bounded_json(&path, 280 * 1024 * 1024)?;
        let frozen: FrozenSnapshot = serde_json::from_value(raw)?;
        Ok(Stored {
            candidate: envelope["candidate"].clone(),
            samples: frozen.samples,
            base: frozen.base,
        })
    }
    pub(super) async fn dispatch(self: &Arc<Self>, action: &str, args: Value) -> Result<Value> {
        if authoring::ACTIONS.contains(&action) {
            return self.authoring.call(action, args).await;
        }
        ensure!(ACTIONS.contains(&action), "unknown or retired YAML action");
        let package = required(&args, "package_id")?;
        let _activity = self.packages.acquire_activity(package)?;
        match action {
            "generation.export" | "generation.import_draft" => {
                self.legacy_extra(action, &args).await
            }
            "generation.history" => revisions::history_read_only(&self.packages, package),
            "automation.validate_source" => {
                let source = required(&args, "yaml")?;
                ensure!(source.len() <= 512 * 1024, "script too large");
                let diagnostics = if args["kind"] == "function_library" {
                    super::super::syntax::parse_function_library(source).err()
                } else {
                    super::super::syntax::parse_script(source).err()
                };
                Ok(
                    json!({"valid":diagnostics.is_none(),"diagnostics":diagnostics.unwrap_or_default()}),
                )
            }
            "generation.list" => {
                let _gate = self.gate.lock();
                let root = self.root(package)?;
                let mut candidates = vec![];
                if root.exists() {
                    for entry in std::fs::read_dir(root)? {
                        let entry = entry?;
                        if entry
                            .path()
                            .extension()
                            .is_some_and(|extension| extension == "json")
                        {
                            if let Some(id) =
                                entry.path().file_stem().and_then(|name| name.to_str())
                            {
                                if let Ok(envelope) = self.read_envelope(package, id) {
                                    candidates
                                        .push(projected_candidate(envelope["candidate"].clone()));
                                }
                            }
                        }
                    }
                }
                candidates.sort_by(|a, b| b["updated_at"].as_str().cmp(&a["updated_at"].as_str()));
                Ok(json!({"candidates":candidates,"legacy_read_only":true}))
            }
            "generation.get" => {
                let _gate = self.gate.lock();
                let mut envelope = self.read_envelope(package, required(&args, "candidate_id")?)?;
                envelope["candidate"] = projected_candidate(envelope["candidate"].clone());
                envelope["legacy_read_only"] = json!(true);
                Ok(envelope)
            }
            "generation.template" => {
                let _gate = self.gate.lock();
                let mut stored = self.read(package, required(&args, "candidate_id")?)?;
                if args["pending"] == true {
                    ensure!(
                        stored.candidate["pending_proposal"]["proposal"].is_object(),
                        "no pending proposal"
                    );
                    stored.candidate["templates"] =
                        stored.candidate["pending_proposal"]["proposal"]["templates"].clone();
                }
                let name = crop_name(required(&args, "name")?)?;
                let templates = all_templates(&stored)?;
                let bytes = templates.get(&name).context("template missing")?;
                let image = decode_image(bytes)?;
                Ok(
                    json!({"name":name,"mime_type":"image/png","base64":base64::engine::general_purpose::STANDARD.encode(bytes),"width":image.width(),"height":image.height(),"legacy_read_only":true}),
                )
            }
            _ => anyhow::bail!("unknown or retired YAML action"),
        }
    }
}
fn crop_name(name: &str) -> Result<String> {
    let name = if name.ends_with(".png") {
        name.to_owned()
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
fn decode_image(bytes: &[u8]) -> Result<image::DynamicImage> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    Ok(reader.decode()?)
}
fn all_templates(stored: &Stored) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut templates = BTreeMap::new();
    let frozen_versions = stored.candidate["repair"]["template_versions"].as_object();
    for (path, bytes) in &stored.base {
        if let Some(name) = path.strip_prefix("templates/") {
            if name.ends_with(".png")
                && frozen_versions.is_none_or(|versions| versions.contains_key(path))
            {
                templates.insert(name.to_owned(), bytes.clone());
            }
        }
    }
    let crops: Vec<Crop> = serde_json::from_value(
        stored
            .candidate
            .get("templates")
            .cloned()
            .unwrap_or_else(|| json!([])),
    )?;
    ensure!(crops.len() <= 64, "template budget exceeded");
    let mut crop_pixels = 0u64;
    let mut crop_bytes = 0usize;
    let mut names = std::collections::BTreeSet::new();
    for crop in crops {
        let name = crop_name(&crop.name)?;
        ensure!(names.insert(name.clone()), "duplicate template name");
        let sample = stored
            .samples
            .iter()
            .find(|sample| sample.manifest["id"] == crop.sample_id)
            .context("template sample missing")?;
        let png = super::super::authoring_materials::frame_png(sample, &crop.frame_id)?;
        let image = decode_image(&png)?;
        let [x, y, width, height] = crop.rect;
        ensure!(
            width > 0
                && height > 0
                && x.checked_add(width).is_some_and(|end| end <= image.width())
                && y.checked_add(height)
                    .is_some_and(|end| end <= image.height()),
            "template crop outside original PNG"
        );
        crop_pixels = crop_pixels
            .checked_add(u64::from(width) * u64::from(height))
            .context("crop pixel count overflow")?;
        ensure!(
            crop_pixels <= 64 * 1024 * 1024,
            "template pixel budget exceeded"
        );
        let mut out = Cursor::new(vec![]);
        image
            .crop_imm(x, y, width, height)
            .write_to(&mut out, image::ImageFormat::Png)?;
        let bytes = out.into_inner();
        crop_bytes += bytes.len();
        ensure!(
            crop_bytes <= 32 * 1024 * 1024,
            "template byte budget exceeded"
        );
        templates.insert(name, bytes);
    }
    for (name, encoded) in stored.candidate["trace_templates"]
        .as_object()
        .into_iter()
        .flatten()
    {
        let name = crop_name(name)?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(
            encoded
                .as_str()
                .context("invalid historical template bytes")?,
        )?;
        ensure!(
            bytes.len() <= 8 * 1024 * 1024,
            "historical template too large"
        );
        decode_image(&bytes)?;
        templates.insert(name, bytes);
    }
    Ok(templates)
}
fn functions(stored: &Stored) -> Result<BTreeMap<String, String>> {
    stored
        .base
        .iter()
        .filter(|(path, _)| path.starts_with("automations/") && path.ends_with(".yaml"))
        .map(|(path, bytes)| Ok((path.clone(), String::from_utf8(bytes.clone())?)))
        .collect()
}

impl Inner {
    pub(super) async fn legacy_extra(
        self: &Arc<Self>,
        action: &str,
        args: &Value,
    ) -> Result<Value> {
        self.legacy_extra_admitted(action, args, None).await
    }
    async fn legacy_extra_admitted(
        self: &Arc<Self>,
        action: &str,
        args: &Value,
        admission: Option<authoring::Admission>,
    ) -> Result<Value> {
        if let Some(check) = &admission {
            check()?;
        }
        let package = required(args, "package_id")?;
        let id = required(args, "candidate_id")?;
        let _activity = self.packages.acquire_activity(package)?;
        let metadata_path = self.file(package, id)?;
        let metadata =
            super::super::authoring_materials::read_bounded_json(&metadata_path, 8 * 1024 * 1024)?;
        ensure!(
            metadata["candidate"]["package_id"] == package && metadata["candidate"]["id"] == id,
            "candidate scope mismatch"
        );
        if action == "generation.export" {
            let part = args["part"].as_str().unwrap_or("metadata");
            let path = match part {
                "metadata" => metadata_path,
                "snapshot" => self.root(package)?.join(format!("{id}.snapshot")),
                _ => anyhow::bail!("invalid export part"),
            };
            let maximum = if part == "metadata" {
                8 * 1024 * 1024
            } else {
                280 * 1024 * 1024
            };
            let stat = std::fs::symlink_metadata(&path)?;
            ensure!(
                stat.is_file() && !stat.file_type().is_symlink() && stat.len() <= maximum,
                "invalid frozen file"
            );
            let offset = args["offset"].as_u64().unwrap_or(0);
            ensure!(offset <= stat.len(), "offset outside file");
            let count = args["max_bytes"].as_u64().unwrap_or(256 * 1024);
            ensure!(count > 0 && count <= 256 * 1024, "invalid chunk size");
            let mut file = std::fs::File::open(&path)?;
            file.seek(SeekFrom::Start(offset))?;
            let mut bytes = vec![];
            file.take(count).read_to_end(&mut bytes)?;
            let next = offset + bytes.len() as u64;
            return Ok(
                json!({"candidate_id":id,"part":part,"offset":offset,"total_bytes":stat.len(),
                "chunk_sha256":format!("{:x}",Sha256::digest(&bytes)),"base64":base64::engine::general_purpose::STANDARD.encode(bytes),
                "next_offset":if next<stat.len(){Some(next)}else{None},"complete":next==stat.len(),"original_bytes":true}),
            );
        }
        ensure!(
            action == "generation.import_draft",
            "unsupported legacy action"
        );
        let (metadata, templates, materials, functions) = {
            let _gate = self.gate.lock();
            let metadata = super::super::authoring_materials::read_bounded_json(
                &metadata_path,
                8 * 1024 * 1024,
            )?;
            ensure!(
                metadata["candidate"]["package_id"] == package && metadata["candidate"]["id"] == id,
                "candidate scope mismatch"
            );
            let stored = self.read(package, id)?;
            let templates = all_templates(&stored)?;
            // Capture the same frozen snapshot under the shared mutation gate.
            ensure!(
                templates.values().map(Vec::len).sum::<usize>() <= 32 * 1024 * 1024,
                "legacy template budget exceeded"
            );
            let materials=stored.samples.iter().map(|sample|json!({"plugin_id":"gamer-yaml","candidate_id":id,
                "sample_id":sample.manifest["id"],"content_sha256":sample.manifest["content_sha256"]})).collect();
            (metadata, templates, materials, functions(&stored)?)
        };
        self.authoring
            .import_candidate(
                package,
                required(args, "new_script_id")?,
                &metadata["candidate"],
                templates,
                materials,
                functions,
                admission,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn legacy_export_is_byte_exact_and_import_creates_a_separate_private_history() {
        let (_root, service, created) = super::super::tests::fixture();
        let id = created["candidate"]["id"].as_str().unwrap();
        let path = service.inner.file("default", id).unwrap();
        let mut envelope: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        envelope["candidate"]["future_metadata"] = json!({"keep":[1,2,"exact"]});
        let original = serde_json::to_vec_pretty(&envelope).unwrap();
        std::fs::write(&path, &original).unwrap();
        let snapshot_path = service
            .inner
            .root("default")
            .unwrap()
            .join(format!("{id}.snapshot"));
        let snapshot = std::fs::read(&snapshot_path).unwrap();
        let view = service
            .legacy_call(
                "generation.get",
                json!({"package_id":"default","candidate_id":id}),
            )
            .await
            .unwrap();
        assert_eq!(view["candidate"]["state"], "retired");
        assert_eq!(view["candidate"]["legacy_original_state"], "generating");
        assert_eq!(
            view["candidate"]["future_metadata"],
            envelope["candidate"]["future_metadata"]
        );
        assert_eq!(view["unknown_envelope"], envelope["unknown_envelope"]);
        service.recover_all().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        for (part, expected) in [("metadata", &original), ("snapshot", &snapshot)] {
            let mut exported = vec![];
            let mut offset = 0;
            loop {
                let value=service.legacy_call("generation.export",json!({"package_id":"default","candidate_id":id,"part":part,"offset":offset,"max_bytes":4096})).await.unwrap();
                exported.extend(
                    base64::engine::general_purpose::STANDARD
                        .decode(value["base64"].as_str().unwrap())
                        .unwrap(),
                );
                if value["complete"] == true {
                    break;
                }
                offset = value["next_offset"].as_u64().unwrap();
            }
            assert_eq!(&exported, expected);
        }
        let imported = service
            .legacy_call(
                "generation.import_draft",
                json!({"package_id":"default","candidate_id":id,"new_script_id":"imported.yaml"}),
            )
            .await
            .unwrap();
        assert_eq!(imported["script"]["head_version"], "0.1");
        assert!(imported["script"]["published_version"].is_null());
        assert_eq!(
            imported["script"]["versions"][0]["yaml"],
            envelope["candidate"]["yaml"]
        );
        assert_eq!(
            imported["script"]["origin_candidate_metadata"]["future_metadata"],
            envelope["candidate"]["future_metadata"]
        );
        assert!(service
            .packages()
            .read_text("default", "gamer-yaml", "automations/imported.yaml")
            .unwrap()
            .is_none());
        assert!(service
            .legacy_call(
                "generation.import_draft",
                json!({"package_id":"default","candidate_id":id,"new_script_id":"imported.yaml"})
            )
            .await
            .is_err());
        assert!(service
            .legacy_call(
                "generation.start",
                json!({"package_id":"default","candidate_id":id})
            )
            .await
            .is_err());
        let reference = imported["script"]["versions"][0]["material_refs"][0].clone();
        let sample = super::super::super::authoring_materials::read_reference(
            &service.packages(),
            "default",
            &reference,
        )
        .unwrap();
        let frame_id = sample.manifest["frames"][0]["id"].as_str().unwrap();
        let original_png =
            super::super::super::authoring_materials::frame_png(&sample, frame_id).unwrap();
        let mut invalid = reference.clone();
        invalid["content_sha256"] = json!("changed");
        assert!(service.authoring().call("authoring.crop_template",json!({"package_id":"default","script_id":"imported.yaml","expected_version":"0.1","name":"private.png","sample_ref":invalid,"frame_id":frame_id,"rect":[1,2,4,5]})).await.is_err());
        let cropped=service.authoring().call("authoring.crop_template",json!({"package_id":"default","script_id":"imported.yaml","expected_version":"0.1","name":"private.png","sample_ref":reference,"frame_id":frame_id,"rect":[1,2,4,5]})).await.unwrap();
        assert_eq!(cropped["script"]["head_version"], "0.2");
        let private=service.authoring().call("authoring.template",json!({"package_id":"default","script_id":"imported.yaml","expected_version":"0.2","name":"private.png"})).await.unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(private["base64"].as_str().unwrap())
            .unwrap();
        let expected = image::load_from_memory(&original_png)
            .unwrap()
            .crop_imm(1, 2, 4, 5)
            .to_luma8();
        assert_eq!(
            image::load_from_memory(&bytes).unwrap().to_luma8(),
            expected
        );
        assert!(service
            .packages()
            .read_binary("default", "gamer-yaml", "templates/private.png")
            .unwrap()
            .is_none());
        let saved=service.authoring().call("authoring.save",json!({"package_id":"default","script_id":"imported.yaml","expected_version":"0.2","yaml":"run: []\n","material_refs":[],"template_refs":[]})).await.unwrap();
        assert_eq!(saved["script"]["versions"][2]["material_refs"], json!([]));
        assert_eq!(
            saved["script"]["versions"][0]["material_refs"],
            imported["script"]["versions"][0]["material_refs"]
        );
        // Clearing associations does not erase the immutable private PNG.
        assert!(service.authoring().call("authoring.template",json!({"package_id":"default","script_id":"imported.yaml","expected_version":"0.3","name":"private.png"})).await.is_ok());
        service
            .legacy_call("generation.list", json!({"package_id":"default"}))
            .await
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert_eq!(std::fs::read(snapshot_path).unwrap(), snapshot);
    }
}
