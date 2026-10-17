//! Explicit semantic tools for external Agents. This is not an admin-call tunnel.
use super::{
    authoring::AuthoringService, authoring_support::required, generation::GenerationService,
};
use crate::{
    extensions::ExternalToolHandler,
    mcp::{
        ErrorKind, InvocationContext, McpError, ObjectKind, ObjectScope, TemporaryWork, ToolResult,
    },
    run_manager::RunManager,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) struct YamlTools {
    service: Arc<GenerationService>,
}
impl YamlTools {
    pub(crate) fn new(service: Arc<GenerationService>) -> Self {
        Self { service }
    }
    fn authoring(&self) -> AuthoringService {
        self.service.authoring()
    }
    fn package<'a>(&self, ctx: &InvocationContext, args: &'a Value) -> Result<&'a str, McpError> {
        ctx.authorize_plugin("gamer-yaml")?;
        let package = required(args, "package_id").map_err(error)?;
        ctx.authorize_package(package)?;
        Ok(package)
    }
    fn reference(&self, ctx: &InvocationContext, reference: &Value) -> Result<(), McpError> {
        match reference["plugin_id"].as_str() {
            Some("gamer-video") => {
                ctx.require_scope("media.read")?;
                ctx.authorize_plugin("gamer-video")?;
                ctx.authorize_media(required(reference, "sample_id").map_err(error)?)
            }
            Some("gamer-yaml") => Ok(()),
            _ => Err(McpError::invalid("Unsupported material owner")),
        }
    }
    async fn selected(&self, args: &Value) -> Result<Value, McpError> {
        let result = self
            .authoring()
            .call(
                "authoring.get",
                json!({"package_id":args["package_id"],"script_id":args["script_id"]}),
            )
            .await
            .map_err(error)?;
        let script = &result["script"];
        let selected = args["version"]
            .as_str()
            .or_else(|| script["head_version"].as_str())
            .unwrap_or("");
        script["versions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|version| version["version"] == selected)
            .cloned()
            .ok_or_else(|| McpError::invalid("Select an existing script version"))
    }
}
fn error(error: impl std::fmt::Display) -> McpError {
    let message = error.to_string();
    if message.starts_with("credential_invalid:") {
        return McpError::unauthorized();
    }
    let conflict = ["conflict", "exists", "busy", "changed"]
        .iter()
        .any(|part| message.contains(part));
    McpError::new(
        if conflict {
            ErrorKind::Conflict
        } else {
            ErrorKind::InvalidParams
        },
        if conflict {
            "authoring_conflict"
        } else {
            "authoring_invalid"
        },
        message,
    )
}
fn image_result(mut value: Value) -> Result<ToolResult, McpError> {
    let encoded = value
        .as_object_mut()
        .and_then(|value| value.remove("base64"))
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| McpError::internal("Missing image bytes"))?;
    Ok(ToolResult {
        content: vec![
            json!({"type":"text","text":value.to_string()}),
            json!({"type":"image","mimeType":"image/png","data":encoded}),
        ],
        structured_content: Some(value),
        is_error: false,
    })
}
struct PrivateRun {
    runs: Arc<RunManager>,
    id: String,
}
#[async_trait]
impl TemporaryWork for PrivateRun {
    async fn cancel_and_wait(&self) {
        self.runs.cancel(&self.id);
        self.runs.wait_terminal(&self.id).await;
    }
}
pub(super) async fn bind_private_run(
    ctx: &InvocationContext,
    runs: Arc<RunManager>,
    id: &str,
    scope: &ObjectScope,
) -> Result<(), McpError> {
    let bind = (|| {
        ctx.record_owned_object(ObjectKind::Run, id, scope)?;
        ctx.register_temporary_work(
            id,
            Arc::new(PrivateRun {
                runs: runs.clone(),
                id: id.into(),
            }),
        )
    })();
    let lease = match bind {
        Ok(lease) => lease,
        Err(error) => {
            runs.cancel(id);
            runs.wait_terminal(id).await;
            return Err(error);
        }
    };
    let id = id.to_owned();
    tokio::spawn(async move {
        runs.wait_terminal(&id).await;
        drop(lease);
    });
    Ok(())
}
#[async_trait]
impl ExternalToolHandler for YamlTools {
    fn trusted_manifest(&self) -> Option<&'static str> {
        Some(include_str!("../manifest.toml"))
    }
    async fn authorize(
        &self,
        ctx: &InvocationContext,
        action: &str,
        args: &Value,
    ) -> Result<(), McpError> {
        let package = self.package(ctx, args)?;
        if matches!(
            action,
            "external.draft_create"
                | "external.draft_import"
                | "external.draft_save"
                | "external.draft_crop"
                | "external.legacy_import"
                | "external.draft_run"
        ) && ctx.operation_id.is_none()
        {
            return Err(McpError::forbidden(
                "A side-effecting YAML action requires host receipt admission",
            ));
        }
        let scope = match action {
            "external.syntax"
            | "external.functions"
            | "external.draft_list"
            | "external.draft_get"
            | "external.draft_diff"
            | "external.template_read" => "plugin.gamer-yaml.read",
            "external.draft_create"
            | "external.draft_import"
            | "external.draft_save"
            | "external.draft_crop"
            | "external.legacy_import" => "plugin.gamer-yaml.draft.write",
            "external.draft_validate" | "external.draft_match" => "plugin.gamer-yaml.validate",
            "external.draft_run" => "plugin.gamer-yaml.draft.run",
            "external.legacy_list"
            | "external.legacy_get"
            | "external.legacy_export"
            | "external.history" => "plugin.gamer-yaml.history.read",
            _ => {
                return Err(McpError::forbidden(
                    "This YAML action is not an external tool",
                ))
            }
        };
        ctx.require_scope(scope)?;
        if !matches!(
            action,
            "external.legacy_list"
                | "external.legacy_get"
                | "external.legacy_export"
                | "external.history"
        ) {
            ctx.require_scope("plugin.gamer-yaml.read")?;
        }
        if action == "external.legacy_import" {
            ctx.require_scope("plugin.gamer-yaml.history.read")?;
        }
        if action == "external.template_read" {
            if args["script_id"].is_string() {
                if !args["expected_version"].is_string() {
                    return Err(McpError::invalid(
                        "Private template reads require expected_version",
                    ));
                }
            } else if args.get("expected_version").is_some() || args.get("version").is_some() {
                return Err(McpError::invalid(
                    "Version selectors require a private script_id",
                ));
            }
        }
        for reference in args["material_refs"].as_array().into_iter().flatten() {
            self.reference(ctx, reference)?;
        }
        if let Some(reference) = args.get("sample_ref") {
            self.reference(ctx, reference)?;
        }
        if matches!(
            action,
            "external.draft_validate"
                | "external.draft_run"
                | "external.draft_crop"
                | "external.draft_match"
        ) || (action == "external.template_read" && args["script_id"].is_string())
        {
            let selected = self.selected(args).await?;
            for reference in selected["material_refs"].as_array().into_iter().flatten() {
                self.reference(ctx, reference)?;
            }
        }
        if action == "external.draft_run" {
            ctx.require_scope("run.submit")?;
            ctx.authorize_runner("gamer-yaml-version")?;
            ctx.authorize_device(required(args, "device_id").map_err(error)?)?;
        }
        // No standalone sample is assumed to belong to this Package.
        ctx.authorize_package(package)?;
        ctx.check_active()
    }
    async fn call(
        &self,
        ctx: InvocationContext,
        action: &str,
        args: Value,
    ) -> Result<ToolResult, McpError> {
        self.authorize(&ctx, action, &args).await?;
        let admitted = ctx.clone();
        let admission: super::authoring::Admission =
            Arc::new(move || admitted.check_active().map_err(anyhow::Error::new));
        let result = match action {
            "external.syntax" => {
                let text = include_str!("../../../docs/reference/YAML.md");
                let offset = args["offset"].as_u64().unwrap_or(0) as usize;
                let count = args["max_chars"].as_u64().unwrap_or(20000).min(30000) as usize;
                let chars: Vec<_> = text.chars().collect();
                if offset > chars.len() {
                    return Err(McpError::invalid("Guide offset outside text"));
                }
                let end = (offset + count).min(chars.len());
                json!({"format":"current_unversioned_yaml","text":chars[offset..end].iter().collect::<String>(),"next_offset":if end<chars.len(){Some(end)}else{None},"complete":end==chars.len()})
            }
            "external.functions" => {
                let package = required(&args, "package_id").map_err(error)?;
                let base =
                    super::revisions::snapshot(&self.service.packages(), package).map_err(error)?;
                let libraries:Vec<_>=base.iter().filter(|(path,_)|path.starts_with("automations/")&&path.rsplit('/').next().is_some_and(|name|name.starts_with("_function")&&name.ends_with(".yaml")))
                    .map(|(path,bytes)|json!({"path":path,"source":String::from_utf8_lossy(bytes)})).collect();
                json!({"native":super::native_funcs::native_functions().iter().map(super::native_funcs::native_schema_json).collect::<Vec<_>>(),"function_libraries":libraries})
            }
            "external.draft_get" => {
                let mut result = self
                    .authoring()
                    .call_admitted("authoring.get", args.clone(), admission.clone())
                    .await
                    .map_err(error)?;
                let versions = result["script"]
                    .as_object_mut()
                    .and_then(|script| script.remove("versions"))
                    .and_then(|versions| versions.as_array().cloned())
                    .ok_or_else(|| McpError::internal("Missing version history"))?;
                let total = versions.len();
                let (page, offset, next, complete, selection) =
                    if let Some(version) = args["version"].as_str() {
                        if args.get("offset").is_some() || args.get("limit").is_some() {
                            return Err(McpError::invalid(
                                "Select a version or a history page, not both",
                            ));
                        }
                        let selected = versions
                            .iter()
                            .find(|entry| entry["version"] == version)
                            .cloned()
                            .ok_or_else(|| {
                                McpError::new(
                                    ErrorKind::NotFound,
                                    "version_missing",
                                    "The selected version does not exist",
                                )
                            })?;
                        (vec![selected], None, None, true, Some(version.to_owned()))
                    } else {
                        let offset = args["offset"].as_u64().unwrap_or(0) as usize;
                        let limit = args["limit"].as_u64().unwrap_or(5) as usize;
                        if offset > total || limit == 0 || limit > 20 {
                            return Err(McpError::invalid("Invalid version history page"));
                        }
                        let end = offset.saturating_add(limit).min(total);
                        (
                            versions[offset..end].to_vec(),
                            Some(offset),
                            if end < total { Some(end) } else { None },
                            end == total,
                            None,
                        )
                    };
                result["script"]["versions"] = json!(page);
                result["total"] = json!(total);
                result["offset"] = json!(offset);
                result["next_offset"] = json!(next);
                result["complete"] = json!(complete);
                result["selected_version"] = json!(selection);
                result
            }
            "external.draft_diff" => {
                let data = self
                    .authoring()
                    .call("authoring.get", args.clone())
                    .await
                    .map_err(error)?;
                let versions = data["script"]["versions"]
                    .as_array()
                    .ok_or_else(|| McpError::internal("Missing version history"))?;
                let find = |key: &str| -> Result<&Value, McpError> {
                    versions
                        .iter()
                        .find(|version| version["version"] == args[key])
                        .ok_or_else(|| McpError::invalid("Unknown diff version"))
                };
                let from = find("from_version")?;
                let to = find("to_version")?;
                json!({"from_version":from["version"],"to_version":to["version"],"from_yaml":from["yaml"],"to_yaml":to["yaml"],"changed":from["yaml"]!=to["yaml"]})
            }
            "external.legacy_list"
            | "external.legacy_get"
            | "external.legacy_export"
            | "external.legacy_import"
            | "external.history" => {
                let mapped = match action {
                    "external.legacy_list" => "generation.list",
                    "external.legacy_get" => "generation.get",
                    "external.legacy_export" => "generation.export",
                    "external.legacy_import" => "generation.import_draft",
                    _ => "generation.history",
                };
                self.service
                    .legacy_call_admitted(mapped, args.clone(), admission.clone())
                    .await
                    .map_err(error)?
            }
            "external.template_read" if !args["script_id"].is_string() => {
                let package = required(&args, "package_id").map_err(error)?;
                let name = required(&args, "name").map_err(error)?;
                crate::resources::sanitize_rel_path(name).map_err(error)?;
                let bytes = self
                    .service
                    .packages()
                    .read_binary(package, "gamer-yaml", &format!("templates/{name}"))
                    .map_err(error)?
                    .ok_or_else(|| {
                        McpError::new(
                            ErrorKind::NotFound,
                            "template_missing",
                            "Template is unavailable",
                        )
                    })?;
                let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
                    .map_err(error)?;
                ctx.check_active()?;
                return image_result(
                    json!({"name":name,"width":image.width(),"height":image.height(),"sha256":format!("{:x}",sha2::Sha256::digest(&bytes)),"base64":base64::Engine::encode(&base64::engine::general_purpose::STANDARD,&bytes)}),
                );
            }
            _ => {
                let mapped = match action {
                    "external.draft_list" => "authoring.list",
                    "external.draft_get" => "authoring.get",
                    "external.draft_create" => "authoring.create",
                    "external.draft_import" => "authoring.import",
                    "external.draft_save" => "authoring.save",
                    "external.draft_validate" => "authoring.validate",
                    "external.draft_run" => "authoring.run",
                    "external.draft_crop" => "authoring.crop_template",
                    "external.draft_match" => "authoring.match_template",
                    "external.template_read" => "authoring.template",
                    _ => return Err(McpError::forbidden("Unsupported external YAML action")),
                };
                let value = if action == "external.draft_run" {
                    let policy = crate::core::ExecutionPolicy::external(
                        ctx.principal.token_id.clone(),
                        ctx.principal.allowed.plugins.clone(),
                        ctx.principal.scopes.clone(),
                    );
                    self.authoring()
                        .run_external(args.clone(), policy, admission.clone())
                        .await
                        .map_err(error)?
                } else {
                    self.authoring()
                        .call_admitted(mapped, args.clone(), admission.clone())
                        .await
                        .map_err(error)?
                };
                if action == "external.template_read" {
                    ctx.check_active()?;
                    return image_result(value);
                }
                if action == "external.draft_run" {
                    let id = required(&value, "run_id").map_err(error)?;
                    let scope = ObjectScope {
                        device_id: Some(required(&args, "device_id").map_err(error)?.into()),
                        package_id: Some(required(&args, "package_id").map_err(error)?.into()),
                        plugin_id: Some("gamer-yaml".into()),
                        runner_id: Some("gamer-yaml-version".into()),
                    };
                    bind_private_run(&ctx, self.authoring().runs(), id, &scope).await?;
                }
                value
            }
        };
        ctx.check_active()?;
        Ok(ToolResult::json(result))
    }
}
use sha2::Digest;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn template_read_schema_requires_the_private_version_and_rejects_ignored_selectors() {
        let schema: Value =
            serde_json::from_str(include_str!("../schemas/external_template_read.json")).unwrap();
        let validator = jsonschema::draft202012::options().build(&schema).unwrap();
        for args in [
            json!({"package_id":"default","name":"test.png"}),
            json!({"package_id":"default","script_id":"test.yaml","expected_version":"0.1","name":"test.png"}),
        ] {
            assert!(validator.is_valid(&args), "{args}");
        }
        for args in [
            json!({"package_id":"default","script_id":"test.yaml","name":"test.png"}),
            json!({"package_id":"default","expected_version":"0.1","name":"test.png"}),
            json!({"package_id":"default","version":"0.1","name":"test.png"}),
        ] {
            assert!(!validator.is_valid(&args), "{args}");
        }
    }
    use crate::{
        core::{ActivityLease, RunContext, RunRequest},
        mcp::{AllowedObjects, CreateTokenRequest, CredentialStore, ReceiptStore},
        run_manager::{RunExecutor, RunSource, RunState, StartRequest},
    };
    use futures_util::future::BoxFuture;
    use std::sync::atomic::{AtomicBool, Ordering};
    struct SyntheticExecutor;
    impl RunExecutor for SyntheticExecutor {
        fn prepare<'a>(
            &'a self,
            _: &'a RunContext,
            _: &'a RunRequest,
        ) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn acquire(&self, _: &RunContext) -> anyhow::Result<Box<dyn ActivityLease>> {
            Ok(Box::new(crate::core::NoopLease))
        }
        fn execute<'a>(
            &'a self,
            _: &'a RunContext,
            _: &'a RunRequest,
            _: bool,
            stop: Arc<AtomicBool>,
        ) -> BoxFuture<'a, anyhow::Result<Vec<(String, String)>>> {
            Box::pin(async move {
                while !stop.load(Ordering::Acquire) {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                Ok(vec![])
            })
        }
    }
    fn context(root: &std::path::Path, media: &str) -> (InvocationContext, Arc<CredentialStore>) {
        let credentials = Arc::new(CredentialStore::open(root).unwrap());
        let created = credentials
            .create(CreateTokenRequest {
                label: "synthetic".into(),
                scopes: [
                    "plugin.gamer-yaml.read",
                    "plugin.gamer-yaml.draft.write",
                    "media.read",
                ]
                .map(str::to_owned)
                .into(),
                allowed: AllowedObjects {
                    devices: ["synthetic-target".into()].into(),
                    packages: ["default".into()].into(),
                    plugins: ["gamer-yaml".into(), "gamer-video".into()].into(),
                    runners: ["gamer-yaml-version".into()].into(),
                    media: [media.into()].into(),
                    ..Default::default()
                },
                expires_in_seconds: 60,
            })
            .unwrap();
        let principal = credentials.authenticate(&created.token).unwrap();
        (
            InvocationContext::new(
                principal,
                "synthetic".into(),
                Some("synthetic-operation".into()),
                credentials.clone(),
                Arc::new(ReceiptStore::open(root).unwrap()),
            ),
            credentials,
        )
    }
    fn submit(runs: &Arc<RunManager>) -> String {
        let request = RunRequest::for_app(
            crate::core::AppContext::for_test("synthetic-target", "default").unwrap(),
            "gamer-yaml-version",
            "default/test.yaml",
            crate::core::RunPayload::empty(),
        )
        .unwrap();
        runs.submit(
            StartRequest {
                request,
                source: RunSource::Manual,
                task_id: None,
                scheduled_at: None,
                realtime_logs: false,
            },
            None,
        )
        .unwrap()
        .run_id
    }
    fn scope() -> ObjectScope {
        ObjectScope {
            device_id: Some("synthetic-target".into()),
            package_id: Some("default".into()),
            plugin_id: Some("gamer-yaml".into()),
            runner_id: Some("gamer-yaml-version".into()),
        }
    }
    #[tokio::test]
    async fn private_trial_revoke_and_failed_registration_both_drain_the_real_run_slot() {
        let root = tempfile::tempdir().unwrap();
        let (ctx, credentials) = context(root.path(), "sample-a");
        let runs = Arc::new(RunManager::new(Arc::new(SyntheticExecutor)));
        let id = submit(&runs);
        bind_private_run(&ctx, runs.clone(), &id, &scope())
            .await
            .unwrap();
        assert!(ctx.authorize_run(&id).is_ok());
        credentials.revoke(&ctx.principal.token_id).unwrap();
        let record =
            tokio::time::timeout(std::time::Duration::from_secs(2), runs.wait_terminal(&id))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(record.state, RunState::Cancelled);
        assert!(runs.active_for_device("synthetic-target").is_none());
        let second = submit(&runs);
        assert!(bind_private_run(&ctx, runs.clone(), &second, &scope())
            .await
            .is_err());
        assert!(runs.active_for_device("synthetic-target").is_none());
        assert_eq!(runs.get_run(&second).unwrap().state, RunState::Cancelled);
    }
    #[tokio::test]
    async fn yaml_tools_reject_admin_actions_cross_package_and_ungranted_standalone_material() {
        let root = tempfile::tempdir().unwrap();
        let (ctx, _) = context(&root.path().join("mcp"), "sample-a");
        let packages = Arc::new(
            crate::resources::PackageStore::open(&crate::config::Config {
                data_dir: root.path().into(),
                ..Default::default()
            })
            .unwrap(),
        );
        packages.ensure_default_package().unwrap();
        let tools = YamlTools::new(Arc::new(GenerationService::new(
            packages,
            Arc::new(RunManager::new(Arc::new(SyntheticExecutor))),
        )));
        assert!(tools
            .authorize(&ctx, "authoring.publish", &json!({"package_id":"default"}))
            .await
            .is_err());
        assert!(tools
            .authorize(&ctx, "external.draft_get", &json!({"package_id":"other"}))
            .await
            .is_err());
        assert!(tools.authorize(&ctx,"external.draft_save",&json!({"package_id":"default","material_refs":[{"plugin_id":"gamer-video","sample_id":"sample-b"}]})).await.is_err());
        let material = json!({"plugin_id":"gamer-video","sample_id":"sample-a"});
        tools.reference(&ctx, &material).unwrap();
        let mut without_owner = ctx.clone();
        without_owner
            .principal
            .allowed
            .plugins
            .remove("gamer-video");
        assert!(
            tools.reference(&without_owner, &material).is_err(),
            "An allowed media ID cannot authorize its ungranted owner plugin"
        );
        let mut forged = ctx.clone();
        forged.operation_id = None;
        assert!(tools.call(forged,"external.draft_create",json!({"package_id":"default","script_id":"forged.yaml","operation_id":"client-cannot-admit"})).await.is_err());
        let result = tools
            .call(
                ctx,
                "external.draft_create",
                json!({"package_id":"default","script_id":"agent.yaml"}),
            )
            .await
            .unwrap();
        assert!(result.structured_content.unwrap()["script"]["versions"]
            .as_array()
            .unwrap()
            .is_empty());
    }
    #[tokio::test]
    async fn external_draft_history_pages_and_selected_versions_preserve_all_metadata_and_source() {
        let root = tempfile::tempdir().unwrap();
        let (ctx, _) = context(&root.path().join("mcp"), "sample-a");
        let packages = Arc::new(
            crate::resources::PackageStore::open(&crate::config::Config {
                data_dir: root.path().into(),
                ..Default::default()
            })
            .unwrap(),
        );
        packages.ensure_default_package().unwrap();
        let service = Arc::new(GenerationService::new(
            packages,
            Arc::new(RunManager::new(Arc::new(SyntheticExecutor))),
        ));
        let authoring = service.authoring();
        authoring
            .call(
                "authoring.create",
                json!({"package_id":"default","script_id":"history.yaml"}),
            )
            .await
            .unwrap();
        for index in 1..=7 {
            authoring.call("authoring.save",json!({"package_id":"default","script_id":"history.yaml","expected_version":if index==1{String::new()}else{format!("0.{}",index-1)},"yaml":format!("# exact {index}\nrun: []\n")})).await.unwrap();
        }
        let path = root
            .path()
            .join("extension-data/gamer-yaml/authoring/default")
            .join(format!("{:x}.json", sha2::Sha256::digest(b"history.yaml")));
        let mut stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        stored["future_document_metadata"] = json!({"keep":[1,"original"]});
        stored["versions"][1]["future_version_metadata"] = json!({"keep":true});
        let original = serde_json::to_vec_pretty(&stored).unwrap();
        std::fs::write(&path, &original).unwrap();
        let tools = YamlTools::new(service);
        let first = tools
            .call(
                ctx.clone(),
                "external.draft_get",
                json!({"package_id":"default","script_id":"history.yaml"}),
            )
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(first["total"], 7);
        assert_eq!(first["next_offset"], 5);
        assert_eq!(first["complete"], false);
        assert_eq!(first["script"]["versions"].as_array().unwrap().len(), 5);
        assert_eq!(
            first["script"]["future_document_metadata"],
            stored["future_document_metadata"]
        );
        let last = tools
            .call(
                ctx.clone(),
                "external.draft_get",
                json!({"package_id":"default","script_id":"history.yaml","offset":5,"limit":20}),
            )
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(last["complete"], true);
        assert!(last["next_offset"].is_null());
        assert_eq!(last["script"]["versions"].as_array().unwrap().len(), 2);
        for (actual, expected) in first["script"]["versions"]
            .as_array()
            .unwrap()
            .iter()
            .chain(last["script"]["versions"].as_array().unwrap())
            .zip(stored["versions"].as_array().unwrap())
        {
            assert_eq!(actual["version"], expected["version"]);
            assert_eq!(actual["yaml"], expected["yaml"]);
        }
        let selected = tools
            .call(
                ctx.clone(),
                "external.draft_get",
                json!({"package_id":"default","script_id":"history.yaml","version":"0.2"}),
            )
            .await
            .unwrap()
            .structured_content
            .unwrap();
        assert_eq!(selected["total"], 7);
        assert_eq!(selected["selected_version"], "0.2");
        assert_eq!(selected["complete"], true);
        assert_eq!(selected["script"]["versions"].as_array().unwrap().len(), 1);
        assert_eq!(
            selected["script"]["versions"][0]["future_version_metadata"],
            stored["versions"][1]["future_version_metadata"]
        );
        assert_eq!(
            selected["script"]["versions"][0]["yaml"],
            stored["versions"][1]["yaml"]
        );
        for args in [
            json!({"offset":8}),
            json!({"limit":21}),
            json!({"version":"0.2","offset":0}),
            json!({"version":"missing"}),
        ] {
            let mut query = json!({"package_id":"default","script_id":"history.yaml"});
            query
                .as_object_mut()
                .unwrap()
                .extend(args.as_object().unwrap().clone());
            assert!(tools
                .call(ctx.clone(), "external.draft_get", query)
                .await
                .is_err());
        }
        assert_eq!(std::fs::read(path).unwrap(), original);
    }
}
