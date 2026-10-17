//! Explicit access to the independent, self-contained sample library.
use super::*;
use crate::{
    extensions::ExternalToolHandler,
    mcp::{ErrorKind, InvocationContext, McpError, ToolResult},
};
use async_trait::async_trait;
use std::path::PathBuf;
pub(crate) struct VideoTools {
    root: PathBuf,
}
impl VideoTools {
    pub(crate) fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.into(),
        }
    }
    fn authorize_sample(&self, ctx: &InvocationContext, args: &Value) -> Result<(), McpError> {
        ctx.require_scope("media.read")?;
        ctx.authorize_media(text(args, "sample_id")?)
    }
    fn bundle(&self, args: &Value) -> Result<Value, McpError> {
        let bundle = read_stored_bundle(&self.root, text(args, "sample_id")?).map_err(error)?;
        if bundle["manifest"]["id"] != args["sample_id"]
            || bundle["manifest"]["content_sha256"] != args["content_sha256"]
        {
            return Err(McpError::new(
                ErrorKind::Conflict,
                "material_changed",
                "Sample identity or content hash changed",
            ));
        }
        Ok(bundle)
    }
    fn authorize_recording(&self, ctx: &InvocationContext, args: &Value) -> Result<(), McpError> {
        ctx.require_scope("media.read")?;
        ctx.require_scope("media.record")?;
        let cfg = crate::config::Config {
            data_dir: self.root.clone(),
            ..Default::default()
        };
        let record = crate::recording::service(&cfg)
            .status(&RecordingId(text(args, "recording_id")?.into()))
            .map_err(error)?;
        ctx.authorize_device(&record.device_id)?;
        for segment in record.segments {
            ctx.authorize_media(&segment.media_id.0)?;
        }
        ctx.authorize_media(text(args, "new_sample_id")?)
    }
    fn export(&self, args: &Value) -> Result<Value, McpError> {
        use std::io::Read;
        let _io = SAMPLE_IO
            .lock()
            .map_err(|_| McpError::internal("Material storage unavailable"))?;
        let path = sample_disk(&self.root, text(args, "sample_id")?).map_err(error)?;
        let file = std::fs::File::open(path).map_err(error)?;
        if file.metadata().map_err(error)?.len() > MAX_ARCHIVE_BYTES as u64 {
            return Err(McpError::invalid("Sample archive is too large"));
        }
        let mut archive = Vec::new();
        file.take(MAX_ARCHIVE_BYTES as u64 + 1)
            .read_to_end(&mut archive)
            .map_err(error)?;
        let (manifest, _) = validate_archive(&archive).map_err(error)?;
        if manifest.id != text(args, "sample_id")?
            || manifest.content_sha256 != text(args, "content_sha256")?
        {
            return Err(McpError::new(
                ErrorKind::Conflict,
                "material_changed",
                "Sample identity or content hash changed",
            ));
        }
        let total = archive.len() as u64;
        let offset = args["offset"].as_u64().unwrap_or(0);
        let maximum = args["max_bytes"].as_u64().unwrap_or(256 * 1024);
        if offset > total || maximum == 0 || maximum > 256 * 1024 {
            return Err(McpError::invalid("Invalid export range"));
        }
        let next = offset.saturating_add(maximum).min(total);
        // Identity, hash and exported bytes all come from this single snapshot.
        let bytes = &archive[offset as usize..next as usize];
        Ok(
            json!({"sample_id":manifest.id,"content_sha256":manifest.content_sha256,"offset":offset,"total_bytes":total,
            "base64":base64::engine::general_purpose::STANDARD.encode(bytes),"chunk_sha256":hash(bytes),"next_offset":if next<total{Some(next)}else{None},"complete":next==total}),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{AllowedObjects, CreateTokenRequest, CredentialStore, ReceiptStore};
    fn context(root: &Path, sample: &str) -> InvocationContext {
        let credentials = std::sync::Arc::new(CredentialStore::open(root).unwrap());
        let token = credentials
            .create(CreateTokenRequest {
                label: "synthetic sample".into(),
                scopes: [
                    "plugin.gamer-video.read",
                    "plugin.gamer-video.import",
                    "media.read",
                ]
                .map(str::to_owned)
                .into(),
                allowed: AllowedObjects {
                    plugins: ["gamer-video".into()].into(),
                    media: [sample.into()].into(),
                    ..Default::default()
                },
                expires_in_seconds: 60,
            })
            .unwrap();
        InvocationContext::new(
            credentials.authenticate(&token.token).unwrap(),
            "synthetic".into(),
            Some("synthetic-operation".into()),
            credentials,
            std::sync::Arc::new(ReceiptStore::open(root).unwrap()),
        )
    }
    #[tokio::test]
    async fn sample_export_checks_the_archive_snapshot_and_refuses_a_replaced_fingerprint() {
        let (mut manifest, files) = super::super::tests::fixture();
        let root = tempfile::tempdir().unwrap();
        let ctx = context(&root.path().join("mcp"), &manifest.id);
        let tools = VideoTools::new(root.path());
        let original = encode_archive(&manifest, &files).unwrap();
        let old = json!({"sample_id":manifest.id,"content_sha256":manifest.content_sha256});
        {
            let _io = SAMPLE_IO.lock().unwrap();
            save_standalone(root.path(), &manifest, &original).unwrap();
        }
        let first = tools
            .call(ctx.clone(), "external.sample_export", old.clone())
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(first["base64"].as_str().unwrap())
                .unwrap(),
            original
        );
        manifest.name = "replacement of the same sample id".into();
        manifest.content_sha256 = fingerprint(&manifest).unwrap();
        let replacement = encode_archive(&manifest, &files).unwrap();
        // Management delete/import can reuse an ID. Every export chunk must
        // validate and use one archive, rather than trusting an earlier read.
        {
            let _io = SAMPLE_IO.lock().unwrap();
            std::fs::remove_file(sample_disk(root.path(), &manifest.id).unwrap()).unwrap();
            save_standalone(root.path(), &manifest, &replacement).unwrap();
        }
        assert!(tools
            .call(ctx.clone(), "external.sample_export", old)
            .await
            .is_err());
        let next = tools
            .call(
                ctx,
                "external.sample_export",
                json!({"sample_id":manifest.id,"content_sha256":manifest.content_sha256}),
            )
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(next["base64"].as_str().unwrap())
                .unwrap(),
            replacement
        );
        assert_eq!(next["chunk_sha256"], hash(&replacement));
    }
    #[tokio::test]
    async fn external_sample_images_events_and_exact_export_need_no_video_or_package() {
        let (manifest, files) = super::super::tests::fixture();
        let archive = encode_archive(&manifest, &files).unwrap();
        let root = tempfile::tempdir().unwrap();
        let ctx = context(&root.path().join("mcp"), &manifest.id);
        let tools = VideoTools::new(root.path());
        let import = json!({"sample_id":manifest.id,"content_sha256":manifest.content_sha256,"base64":base64::engine::general_purpose::STANDARD.encode(&archive),"operation_id":"synthetic-import"});
        let mut forged = ctx.clone();
        forged.operation_id = None;
        assert!(tools
            .call(forged, "external.sample_import", import.clone())
            .await
            .is_err());
        tools
            .call(ctx.clone(), "external.sample_import", import.clone())
            .await
            .unwrap();
        let args = json!({"sample_id":manifest.id,"content_sha256":manifest.content_sha256});
        let read = tools
            .call(ctx.clone(), "external.sample_read", args.clone())
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(read["original_video_required"], false);
        let mut frames = args.clone();
        frames["limit"] = json!(1);
        let page = tools
            .call(ctx.clone(), "external.sample_frames", frames)
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["next_offset"], 1);
        let mut frame = args.clone();
        frame["frame_id"] = json!("start");
        let image = tools
            .call(ctx.clone(), "external.sample_frame", frame)
            .await
            .unwrap();
        assert!(image.content.iter().any(|block| block["type"] == "image"));
        assert_eq!(image.structured_content.unwrap()["original_width"], 16);
        let mut exported = vec![];
        let mut offset = 0;
        loop {
            let mut range = args.clone();
            range["offset"] = json!(offset);
            range["max_bytes"] = json!(97);
            let part = tools
                .call(ctx.clone(), "external.sample_export", range)
                .await
                .unwrap()
                .structured_content
                .unwrap();
            exported.extend(
                base64::engine::general_purpose::STANDARD
                    .decode(part["base64"].as_str().unwrap())
                    .unwrap(),
            );
            if part["complete"] == true {
                break;
            }
            offset = part["next_offset"].as_u64().unwrap();
        }
        assert_eq!(exported, archive);
        let mut wrong = args.clone();
        wrong["sample_id"] = json!("ungranted-sample");
        wrong["package_id"] = json!("default");
        assert!(tools
            .call(ctx.clone(), "external.sample_read", wrong)
            .await
            .is_err());
        let mut wrong_hash = args;
        wrong_hash["content_sha256"] = json!("changed");
        assert!(tools
            .call(ctx.clone(), "external.sample_read", wrong_hash)
            .await
            .is_err());
        let mut forged = import;
        forged["content_sha256"] = json!("changed");
        assert!(tools
            .call(ctx, "external.sample_import", forged)
            .await
            .is_err());
        assert!(!root.path().join("media").exists());
        assert!(!root.path().join("packages").exists());
    }
}
fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str, McpError> {
    args[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| McpError::invalid(format!("Missing {key}")))
}
fn error(error: impl std::fmt::Display) -> McpError {
    McpError::new(
        ErrorKind::InvalidParams,
        "material_invalid",
        error.to_string(),
    )
}
fn page(values: &[Value], args: &Value) -> Result<Value, McpError> {
    let offset = args["offset"].as_u64().unwrap_or(0) as usize;
    let limit = args["limit"].as_u64().unwrap_or(50).min(100) as usize;
    if offset > values.len() {
        return Err(McpError::invalid("Offset outside collection"));
    }
    let end = (offset + limit).min(values.len());
    Ok(
        json!({"items":&values[offset..end],"next_offset":if end<values.len(){Some(end)}else{None},"complete":end==values.len(),"total":values.len()}),
    )
}
#[async_trait]
impl ExternalToolHandler for VideoTools {
    fn trusted_manifest(&self) -> Option<&'static str> {
        Some(include_str!("../manifest.toml"))
    }
    async fn authorize(
        &self,
        ctx: &InvocationContext,
        action: &str,
        args: &Value,
    ) -> Result<(), McpError> {
        ctx.authorize_plugin("gamer-video")?;
        if matches!(action, "external.sample_import" | "external.sample_create")
            && ctx.operation_id.is_none()
        {
            return Err(McpError::forbidden(
                "A sample write requires host receipt admission",
            ));
        }
        ctx.require_scope(match action {
            "external.sample_list"
            | "external.sample_read"
            | "external.sample_frames"
            | "external.sample_frame"
            | "external.sample_events"
            | "external.sample_export" => "plugin.gamer-video.read",
            "external.sample_import" => "plugin.gamer-video.import",
            "external.sample_create" => "plugin.gamer-video.create",
            _ => {
                return Err(McpError::forbidden(
                    "This video action is not an external tool",
                ))
            }
        })?;
        if action == "external.sample_create" {
            self.authorize_recording(ctx, args)?;
        } else if action != "external.sample_list" {
            self.authorize_sample(ctx, args)?;
        } else {
            ctx.require_scope("media.read")?;
        }
        ctx.check_active()
    }
    async fn call(
        &self,
        ctx: InvocationContext,
        action: &str,
        args: Value,
    ) -> Result<ToolResult, McpError> {
        self.authorize(&ctx, action, &args).await?;
        let value = match action {
            "external.sample_list" => {
                let listed = dispatch("sample.list", &json!({}), &self.root).map_err(error)?;
                let allowed: Vec<_> = listed["samples"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|sample| {
                        sample["id"]
                            .as_str()
                            .is_some_and(|id| ctx.authorize_media(id).is_ok())
                    })
                    .cloned()
                    .collect();
                page(&allowed, &args)?
            }
            "external.sample_import" => {
                let encoded = text(&args, "base64")?;
                if encoded.len() > 8 * 1024 * 1024 {
                    return Err(McpError::invalid("MCP inline sample import is limited to 6 MiB; use the existing material import UI for larger archives"));
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(error)?;
                let (manifest, _) = validate_archive(&bytes).map_err(error)?;
                if manifest.id != text(&args, "sample_id")?
                    || manifest.content_sha256 != text(&args, "content_sha256")?
                {
                    return Err(McpError::invalid(
                        "Imported sample identity or hash mismatch",
                    ));
                }
                let _io = SAMPLE_IO
                    .lock()
                    .map_err(|_| McpError::internal("Material storage unavailable"))?;
                ctx.check_active()?;
                save_standalone(&self.root, &manifest, &bytes).map_err(error)?;
                json!({"manifest":manifest})
            }
            "external.sample_create" => {
                let mut request = args.clone();
                request.as_object_mut().unwrap().remove("new_sample_id");
                request.as_object_mut().unwrap().remove("operation_id");
                let req: CreateRequest = serde_json::from_value(request).map_err(error)?;
                let cfg = crate::config::Config {
                    data_dir: self.root.clone(),
                    ..Default::default()
                };
                let _io = SAMPLE_IO
                    .lock()
                    .map_err(|_| McpError::internal("Material storage unavailable"))?;
                let (mut manifest, bytes) = create(
                    &req,
                    &crate::recording::service(&cfg),
                    &crate::media::service(&cfg),
                )
                .map_err(error)?;
                let (_, files) = validate_archive(&bytes).map_err(error)?;
                manifest.id = text(&args, "new_sample_id")?.into();
                sample_path(&manifest.id).map_err(error)?;
                manifest.content_sha256 = fingerprint(&manifest).map_err(error)?;
                let bytes = encode_archive(&manifest, &files).map_err(error)?;
                ctx.check_active()?;
                save_standalone(&self.root, &manifest, &bytes).map_err(error)?;
                json!({"manifest":manifest})
            }
            "external.sample_export" => self.export(&args)?,
            _ => {
                let bundle = self.bundle(&args)?;
                let manifest = &bundle["manifest"];
                match action {
                    "external.sample_read" => {
                        json!({"manifest":manifest,"self_contained":true,"original_video_required":false})
                    }
                    "external.sample_frames" => page(
                        manifest["frames"]
                            .as_array()
                            .ok_or_else(|| McpError::invalid("Missing frames"))?,
                        &args,
                    )?,
                    "external.sample_events" => page(
                        manifest["actions"]
                            .as_array()
                            .ok_or_else(|| McpError::invalid("Missing actions"))?,
                        &args,
                    )?,
                    "external.sample_frame" => {
                        let frame = manifest["frames"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|frame| frame["id"] == args["frame_id"])
                            .ok_or_else(|| McpError::invalid("Unknown frame_id"))?;
                        let encoded = bundle["files"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|file| file["path"] == frame["path"])
                            .and_then(|file| file["base64"].as_str())
                            .ok_or_else(|| McpError::invalid("Original PNG is missing"))?;
                        let png = base64::engine::general_purpose::STANDARD
                            .decode(encoded)
                            .map_err(error)?;
                        if hash(&png) != frame["sha256"] {
                            return Err(McpError::invalid("Frame hash mismatch"));
                        }
                        let image =
                            image::load_from_memory_with_format(&png, image::ImageFormat::Png)
                                .map_err(error)?;
                        let original = [image.width(), image.height()];
                        let maximum =
                            args["max_width"].as_u64().unwrap_or(1920).clamp(320, 1920) as u32;
                        let shown = if image.width() > maximum {
                            image.resize(maximum, u32::MAX, image::imageops::FilterType::Triangle)
                        } else {
                            image
                        };
                        let mut out = Cursor::new(vec![]);
                        shown
                            .write_to(&mut out, image::ImageFormat::Png)
                            .map_err(error)?;
                        let meta = json!({"sample_id":args["sample_id"],"content_sha256":args["content_sha256"],"frame":frame,
                            "original_width":original[0],"original_height":original[1],"image_width":shown.width(),"image_height":shown.height(),
                            "crop_coordinate_space":"original_png_pixels","original_video_required":false});
                        ctx.check_active()?;
                        return Ok(ToolResult {
                            content: vec![
                                json!({"type":"text","text":meta.to_string()}),
                                json!({"type":"image","mimeType":"image/png","data":base64::engine::general_purpose::STANDARD.encode(out.into_inner())}),
                            ],
                            structured_content: Some(meta),
                            is_error: false,
                        });
                    }
                    _ => return Err(McpError::forbidden("Unsupported external sample action")),
                }
            }
        };
        ctx.check_active()?;
        Ok(ToolResult::json(value))
    }
}
