//! Deliberate external surface. No generic dispatch: dispatch also activates
//! sender machinery, and credential/confirmation actions stay management-only.
use crate::{
    extensions::ExternalToolHandler,
    mcp::{ErrorKind, InvocationContext, McpError, ToolResult},
    media::output::{self, OutputRequest},
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) struct ExternalTools {
    service: Arc<super::LiveService>,
    output_binding: tokio::sync::Mutex<Option<OutputBinding>>,
}
struct OutputBinding {
    stream_id: String,
    device_id: String,
    package_id: String,
}
impl OutputBinding {
    fn matches(&self, stream: &str, device: &str, package: &str) -> bool {
        self.stream_id == stream && self.device_id == device && self.package_id == package
    }
}
impl ExternalTools {
    pub(crate) fn new(service: Arc<super::LiveService>) -> Self {
        Self {
            service,
            output_binding: tokio::sync::Mutex::new(None),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    device_id: String,
    package_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueueStatus {
    device_id: String,
    package_id: String,
    #[serde(default)]
    offset: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Package {
    package_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    package_id: String,
    kind: String,
    payload: Value,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveRules {
    device_id: String,
    package_id: String,
    ruleset: super::rules::RuleSet,
    expected_version: Option<String>,
    operation_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartStream {
    device_id: String,
    package_id: String,
    operation_id: String,
    #[serde(default = "yes")]
    audio: bool,
    #[serde(default = "fps")]
    fps: u32,
    #[serde(default = "bitrate")]
    bitrate_kbps: u32,
}
fn yes() -> bool {
    true
}
fn fps() -> u32 {
    30
}
fn bitrate() -> u32 {
    4000
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StopStream {
    device_id: String,
    package_id: String,
    stream_id: String,
    operation_id: String,
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, McpError> {
    serde_json::from_value(value).map_err(|_| McpError::invalid("Invalid live tool arguments"))
}
fn business(error: impl std::fmt::Display) -> McpError {
    let message = error.to_string();
    if message.contains("version_") || message.contains("已被其他页面修改") {
        McpError::new(ErrorKind::Conflict, "version_conflict", message)
    } else if message.starts_with("scope_denied:") {
        McpError::forbidden(message)
    } else {
        McpError::invalid(message)
    }
}
fn value_text<'a>(value: &'a Value, key: &str) -> Result<&'a str, McpError> {
    value[key]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| McpError::invalid(format!("{key} required")))
}
fn check_operation(context: &InvocationContext, operation: &str) -> Result<(), McpError> {
    if context.operation_id.as_deref() == Some(operation) {
        Ok(())
    } else {
        Err(McpError::invalid("operation_id mismatch"))
    }
}
fn matches_target(value: &Value, device: &str, package: &str) -> bool {
    value["device_id"] == device && value["package_id"] == package
}

#[async_trait]
impl ExternalToolHandler for ExternalTools {
    fn trusted_manifest(&self) -> Option<&'static str> {
        Some(include_str!("../manifest.toml"))
    }
    async fn authorize(
        &self,
        context: &InvocationContext,
        action: &str,
        arguments: &Value,
    ) -> Result<(), McpError> {
        context.check_active()?;
        context.authorize_plugin(super::ID)?;
        let scopes: &[&str] = match action {
            "external.status" => &["plugin.gamer-live.status.read"],
            "external.queue_status" => &["plugin.gamer-live.queue.read"],
            "external.rules_read" | "external.rules_preview" => {
                &["plugin.gamer-live.rules.read", "resource.read"]
            }
            "external.rules_save" => &[
                "plugin.gamer-live.rules.write",
                "resource.write",
                "task.write",
                "run.submit",
            ],
            "external.stream_start" | "external.stream_stop" => {
                &["plugin.gamer-live.stream.control", "media.record"]
            }
            _ => return Err(McpError::forbidden("Live action is not exposed externally")),
        };
        for scope in scopes {
            context.require_scope(scope)?;
        }
        context.authorize_package(value_text(arguments, "package_id")?)?;
        if let Some(device) = arguments["device_id"].as_str() {
            context.authorize_device(device)?;
            crate::targets::capabilities(&self.service.devices, device).map_err(business)?;
        }
        if action == "external.rules_save" {
            context.authorize_runner(super::rules::RUNNER)?;
            context.authorize_plugin(super::rules::RUNNER)?;
            if arguments["ruleset"].get("external_origin").is_some() {
                return Err(McpError::forbidden(
                    "Rule execution authority is assigned by the host",
                ));
            }
        }
        Ok(())
    }

    async fn call(
        &self,
        context: InvocationContext,
        action: &str,
        arguments: Value,
    ) -> Result<ToolResult, McpError> {
        self.authorize(&context, action, &arguments).await?;
        let result = match action {
            "external.status" => {
                let request: Target = decode(arguments)?;
                let queue = self.service.queue().map_err(business)?.status(0, "").await;
                let target_matches =
                    matches_target(&queue["target"], &request.device_id, &request.package_id);
                let stream = self
                    .service
                    .output
                    .lock()
                    .await
                    .as_ref()
                    .map(|output| output.status.lock().clone());
                let binding = self.output_binding.lock().await;
                let stream=stream.filter(|stream|stream.device_id==request.device_id && binding.as_ref().is_some_and(|binding|binding.matches(&stream.id,&request.device_id,&request.package_id))).map(|stream|json!({"id":stream.id,"device_id":stream.device_id,"package_id":request.package_id,"state":stream.state,"mode":stream.mode,"video_frames":stream.video_frames,"audio_packets":stream.audio_packets,"started_at":stream.started_at,"has_error":stream.error.is_some(),"delegated":true,"continues_after_token_expiry":true}));
                json!({"device_id":request.device_id,"package_id":request.package_id,"stream":stream,"queue_bound_to_target":target_matches,"queue_paused":target_matches.then(||queue["paused"].clone()),"has_queue_error":target_matches.then(||!queue["blocked"].is_null())})
            }
            "external.queue_status" => {
                let request: QueueStatus = decode(arguments)?;
                if request.offset > 100_000 {
                    return Err(McpError::invalid("offset too large"));
                }
                self.service
                    .queue()
                    .map_err(business)?
                    .external_status(&request.device_id, &request.package_id, request.offset)
                    .await
            }
            "external.rules_read" => {
                let request: Package = decode(arguments)?;
                self.service
                    .queue()
                    .map_err(business)?
                    .read_rules(&request.package_id)
                    .map_err(business)?
            }
            "external.rules_preview" => {
                let request: Preview = decode(arguments)?;
                if !["message", "gift"].contains(&request.kind.as_str())
                    || request.payload.to_string().len() > 8192
                {
                    return Err(McpError::invalid("Invalid simulated message/gift"));
                }
                let source = self
                    .service
                    .queue()
                    .map_err(business)?
                    .read_rules(&request.package_id)
                    .map_err(business)?;
                let rules: Vec<super::rules::Rule> =
                    serde_json::from_value(source["rules"].clone()).map_err(business)?;
                let event = super::events::LiveEvent {
                    schema_version: 1,
                    seq: 0,
                    platform_id: "preview".into(),
                    connection_id: String::new(),
                    identity_namespace: "local-simulation".into(),
                    room_id: String::new(),
                    event_id: None,
                    kind: request.kind,
                    occurred_at: None,
                    received_at: chrono::Utc::now().to_rfc3339(),
                    actor: Some(json!({"id":"preview","name":"preview"})),
                    payload: request.payload,
                    platform_data: Value::Null,
                };
                let matched=rules.into_iter().filter(|rule|rule.matches(&event)).map(|rule|{
                    let args=rule.bind(&event).map_err(business)?;
                    Ok(json!({"id":rule.id,"name":rule.name,"entrypoint":rule.entrypoint,"args":args}))
                }).collect::<Result<Vec<_>,McpError>>()?;
                json!({"matched_rules":matched,"version":source["version"],"enqueued":false,"scope":"rule_match_and_parameter_binding_only"})
            }
            "external.rules_save" => {
                let mut request: SaveRules = decode(arguments)?;
                check_operation(&context, &request.operation_id)?;
                if request.ruleset.external_origin.is_some() {
                    return Err(McpError::forbidden(
                        "Rule execution authority is assigned by the host",
                    ));
                }
                request.ruleset.external_origin = Some(super::rules::ExternalRuleOrigin {
                    policy: crate::core::ExecutionPolicy::external(
                        context.principal.token_id.clone(),
                        context.principal.allowed.plugins.clone(),
                        context.principal.scopes.clone(),
                    ),
                    device_id: request.device_id.clone(),
                    package_id: request.package_id.clone(),
                    runner_id: super::rules::RUNNER.into(),
                });
                request
                    .ruleset
                    .validate(&request.package_id)
                    .map_err(business)?;
                let queue = self.service.queue().map_err(business)?;
                let mut saved = queue
                    .external_save_rules(
                        &request.device_id,
                        &request.package_id,
                        request.ruleset,
                        request.expected_version.as_deref(),
                        || context.check_active().map_err(anyhow::Error::new),
                    )
                    .await
                    .map_err(business)?;
                saved["device_id"] = json!(request.device_id);
                saved["package_id"] = json!(request.package_id);
                saved["runner_id"] = json!(super::rules::RUNNER);
                saved["delegated"] = json!(true);
                saved["continues_after_token_expiry"] = json!(true);
                saved
            }
            "external.stream_start" => {
                let request: StartStream = decode(arguments)?;
                check_operation(&context, &request.operation_id)?;
                let _manual = self
                    .service
                    .automatic
                    .manual_guard()
                    .await
                    .map_err(business)?;
                let output = OutputRequest {
                    device_id: request.device_id.clone(),
                    video_folder: None,
                    mode: "local".into(),
                    push_url: String::new(),
                    audio: request.audio,
                    fps: request.fps,
                    bitrate_kbps: request.bitrate_kbps,
                };
                output.validate().map_err(business)?;
                let mut slot = self.service.output.lock().await;
                if slot.as_ref().is_some_and(|handle| !handle.finished()) {
                    return Err(McpError::new(
                        ErrorKind::Conflict,
                        "stream_busy",
                        "Existing output must be stopped in its authorized target first",
                    ));
                }
                if let Some(old) = slot.take() {
                    old.stop().await;
                }
                context.check_active()?;
                let handle = output::start(
                    self.service.devices.clone(),
                    output,
                    Some(self.service.links.stream_url().to_owned()),
                )
                .await
                .map_err(business)?;
                let status = handle.status.lock().clone();
                *slot = Some(handle);
                *self.output_binding.lock().await = Some(OutputBinding {
                    stream_id: status.id.clone(),
                    device_id: request.device_id.clone(),
                    package_id: request.package_id.clone(),
                });
                // No sender, connection or automatic execution is activated.
                json!({"device_id":request.device_id,"package_id":request.package_id,"state":status.state,"id":status.id,"mode":"local","delegated":true,"continues_after_token_expiry":true})
            }
            "external.stream_stop" => {
                let request: StopStream = decode(arguments)?;
                check_operation(&context, &request.operation_id)?;
                let _manual = self
                    .service
                    .automatic
                    .manual_guard()
                    .await
                    .map_err(business)?;
                let mut slot = self.service.output.lock().await;
                let binding = self.output_binding.lock().await;
                if !binding.as_ref().is_some_and(|binding| {
                    binding.matches(&request.stream_id, &request.device_id, &request.package_id)
                }) {
                    return Err(McpError::forbidden("Output has no matching frozen target binding; use Gamer to stop management-started output"));
                }
                if let Some(handle) = slot.as_ref() {
                    let status = handle.status.lock();
                    if status.device_id != request.device_id || status.id != request.stream_id {
                        return Err(McpError::forbidden(
                            "Output belongs to another target or generation",
                        ));
                    }
                }
                context.check_active()?;
                if let Some(handle) = slot.take() {
                    handle.stop().await;
                }
                json!({"device_id":request.device_id,"package_id":request.package_id,"state":"stopped"})
            }
            _ => return Err(McpError::forbidden("Live action is not exposed externally")),
        };
        if matches!(
            action,
            "external.status"
                | "external.queue_status"
                | "external.rules_read"
                | "external.rules_preview"
        ) {
            context.check_active()?;
        }
        Ok(ToolResult::json(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_output_identity_cannot_stop_a_later_output_on_the_same_device() {
        let binding = OutputBinding {
            stream_id: "new-output".into(),
            device_id: "phone".into(),
            package_id: "package".into(),
        };
        assert!(binding.matches("new-output", "phone", "package"));
        assert!(!binding.matches("old-output", "phone", "package"));
        assert!(!binding.matches("new-output", "another-phone", "package"));
        assert!(!binding.matches("new-output", "phone", "another-package"));
    }
    #[test]
    fn a_read_classified_context_cannot_supply_its_own_write_receipt() {
        let root = tempfile::tempdir().unwrap();
        let mcp = crate::mcp::McpService::open(
            root.path(),
            crate::mcp::ServerInfo {
                name: "test".into(),
                version: "1".into(),
            },
        )
        .unwrap();
        let credentials = mcp.credentials();
        let created = credentials
            .create(crate::mcp::CreateTokenRequest {
                label: "test".into(),
                scopes: ["plugin.gamer-live.rules.write".into()].into(),
                allowed: Default::default(),
                expires_in_seconds: 300,
            })
            .unwrap();
        let mut context = InvocationContext::new(
            credentials
                .principal_for_id(&created.metadata.token_id)
                .unwrap(),
            "forged_read_tool".into(),
            None,
            credentials,
            mcp.receipts(),
        );
        assert!(check_operation(&context, "client-only").is_err());
        context.operation_id = Some("trusted-receipt".into());
        assert!(check_operation(&context, "client-only").is_err());
        assert!(check_operation(&context, "trusted-receipt").is_ok());
    }
    #[test]
    fn stream_input_rejects_credential_urls_and_filesystem_sources() {
        for field in ["push_url", "video_folder", "mode", "credentials"] {
            let mut input = json!({"device_id":"d","package_id":"p","operation_id":"op"});
            input[field] = json!("forbidden");
            assert!(serde_json::from_value::<StartStream>(input).is_err());
        }
    }
    #[test]
    fn queue_scope_uses_recorded_target_not_caller_claim() {
        assert!(matches_target(
            &json!({"device_id":"a","package_id":"p"}),
            "a",
            "p"
        ));
        assert!(!matches_target(
            &json!({"device_id":"a","package_id":"p"}),
            "b",
            "p"
        ));
        assert!(!matches_target(
            &json!({"device_id":"a","package_id":"p"}),
            "a",
            "q"
        ));
    }
}
