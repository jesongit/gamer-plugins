//! Immutable private-version execution, shared by authoring and candidates.
//! Pending tokens are process-local and never restored or replayed after restart.
use super::authoring_support::required;
use crate::{
    extensions::ExtensionService,
    run_manager::{RunExecutor, RunManager},
};
use anyhow::{Context, Result};
use futures_util::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{atomic::AtomicBool, Arc, Weak},
};

pub(super) struct PrivateTrialRuntime {
    pub(super) devices: Mutex<Option<Arc<crate::device::DeviceManager>>>,
    pub(super) extensions: Mutex<Weak<ExtensionService>>,
    pub(super) event_sink: Mutex<Option<Arc<dyn crate::core::events::EventSink>>>,
    pub(super) pending_trials: Mutex<BTreeMap<String, TrialInput>>,
    pub(super) active_trials: Mutex<BTreeSet<String>>,
    runs: Arc<RunManager>,
}
impl PrivateTrialRuntime {
    pub(super) fn new(runs: Arc<RunManager>) -> Self {
        Self {
            devices: Mutex::new(None),
            extensions: Mutex::new(Weak::new()),
            event_sink: Mutex::new(None),
            pending_trials: Mutex::new(BTreeMap::new()),
            active_trials: Mutex::new(BTreeSet::new()),
            runs,
        }
    }
    pub(super) async fn stop(&self) {
        let trials: Vec<_> = self.active_trials.lock().iter().cloned().collect();
        for id in &trials {
            self.runs.cancel(id);
        }
        for id in &trials {
            self.runs.wait_terminal(id).await;
        }
    }
}

#[derive(Clone)]
pub(super) struct TrialInput {
    pub(super) program: Value,
    pub(super) templates: BTreeMap<String, Vec<u8>>,
}

pub(super) struct PrivateTrialExecutor {
    pub(super) inner: Weak<PrivateTrialRuntime>,
}
impl RunExecutor for PrivateTrialExecutor {
    fn prepare<'a>(
        &'a self,
        context: &'a crate::core::RunContext,
        request: &'a crate::core::RunRequest,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let inner = self
                .inner
                .upgrade()
                .context("private trial service stopped")?;
            let token = required(request.payload.as_value(), "trial_token")?;
            let input = inner
                .pending_trials
                .lock()
                .get(token)
                .cloned()
                .context("private trial snapshot unavailable; do not replay")?;
            if let Some(policy) = &request.execution_policy {
                validate_external_policy(&input.program, policy)?;
            }
            let devices = inner
                .devices
                .lock()
                .clone()
                .context("device service unavailable")?;
            crate::targets::prepare(&devices, context.device_id().as_str()).await
        })
    }
    fn acquire(
        &self,
        context: &crate::core::RunContext,
    ) -> Result<Box<dyn crate::core::ActivityLease>> {
        let inner = self
            .inner
            .upgrade()
            .context("private trial service stopped")?;
        let devices = inner
            .devices
            .lock()
            .clone()
            .context("device service unavailable")?;
        crate::targets::acquire(&devices, context.device_id().as_str())
    }
    fn execute<'a>(
        &'a self,
        context: &'a crate::core::RunContext,
        request: &'a crate::core::RunRequest,
        _: bool,
        stop: Arc<AtomicBool>,
    ) -> BoxFuture<'a, Result<Vec<(String, String)>>> {
        Box::pin(async move {
            let inner = self
                .inner
                .upgrade()
                .context("private trial service stopped")?;
            let token = required(request.payload.as_value(), "trial_token")?;
            let input = inner
                .pending_trials
                .lock()
                .get(token)
                .cloned()
                .context("private trial snapshot unavailable; do not replay")?;
            let mut program = input.program;
            if let Some(policy) = &request.execution_policy {
                validate_external_policy(&program, policy)?;
            }
            program["trace"] = json!({"run_id":context.run_id.as_str(),"entry":{"package_id":request.app.content_package,"plugin_id":super::YAML_EXTENSION_ID,"path":request.entrypoint,"private_trial":true}});
            let extensions = inner
                .extensions
                .lock()
                .upgrade()
                .context("extension service unavailable")?;
            let sink = inner.event_sink.lock().clone();
            crate::capabilities::adapters::with_caller_input_source("runner", async {
                super::run_yaml_program_with_templates(
                    &extensions,
                    program,
                    context.app.clone(),
                    stop,
                    sink,
                    Some(input.templates),
                    request.execution_policy.clone(),
                )
                .await
            })
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            Ok(vec![])
        })
    }
}

/// Check only instructions reachable from this frozen program. Data strings,
/// comments and unused library entries cannot grant or impersonate effects.
#[cfg(test)]
fn validate_external_program(program: &Value) -> Result<()> {
    validate_external_policy(
        program,
        &crate::core::ExecutionPolicy::external(
            String::new(),
            ["*".into()].into(),
            ["plugin.gamer-ocr.recognize".into()].into(),
        ),
    )
}
/// Static closure admission and actual native dispatch share one restriction.
/// A Component cannot gain authority by ignoring the frozen wire program.
pub(crate) fn authorize_external_native(
    policy: &crate::core::ExecutionPolicy,
    name: &str,
) -> Result<()> {
    let native = super::native_funcs::native_function(name)
        .context("external native function is unavailable")?;
    if matches!(name, "ocr" | "find_text") {
        anyhow::ensure!(
            policy.allows_plugin("gamer-ocr") && policy.has_scope("plugin.gamer-ocr.recognize"),
            "external_execution_denied: OCR requires the OCR plugin and recognize scope"
        );
    }
    anyhow::ensure!(
        !matches!(
            name,
            "notify" | "switch_live_device" | "wait_live_confirmation"
        ) && !native.permissions.iter().any(|permission| matches!(
            permission,
            crate::extensions::Permission::NotifySend | crate::extensions::Permission::MediaStream
        )),
        "external_execution_denied: external YAML cannot invoke {name}"
    );
    Ok(())
}
pub(crate) fn validate_external_policy(
    program: &Value,
    policy: &crate::core::ExecutionPolicy,
) -> Result<()> {
    fn visit(
        steps: &Value,
        functions: &Value,
        seen: &mut BTreeSet<String>,
        policy: &crate::core::ExecutionPolicy,
    ) -> Result<()> {
        for step in steps.as_array().into_iter().flatten() {
            let operation = step["op"]
                .as_str()
                .context("external instruction must have a fixed operation")?;
            if operation == "fn" {
                let name = step["fn"]
                    .as_str()
                    .context("external function name must be fixed")?;
                if super::native_funcs::native_function(name).is_some() {
                    authorize_external_native(policy, name)?;
                } else if seen.insert(name.into()) {
                    let function = functions
                        .get(name)
                        .context("external function body is unavailable")?;
                    visit(&function["run"], functions, seen, policy)?;
                }
            }
            match operation {
                "fn" | "break" | "return" => {}
                "if" => {
                    visit(&step["then"], functions, seen, policy)?;
                    visit(&step["else"], functions, seen, policy)?;
                }
                "repeat" => visit(&step["do"], functions, seen, policy)?,
                "match_templates" | "match_templates_loop" => {
                    for case in step["cases"].as_array().into_iter().flatten() {
                        visit(&case["do"], functions, seen, policy)?;
                    }
                    visit(&step["else"], functions, seen, policy)?;
                }
                _ => {
                    anyhow::bail!("external_execution_denied: unsupported instruction {operation}")
                }
            }
        }
        Ok(())
    }
    visit(
        &program["run"],
        &program["functions"],
        &mut BTreeSet::new(),
        policy,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    struct UnreachableExecutor;
    impl RunExecutor for UnreachableExecutor {
        fn prepare<'a>(
            &'a self,
            _: &'a crate::core::RunContext,
            _: &'a crate::core::RunRequest,
        ) -> BoxFuture<'a, Result<()>> {
            Box::pin(async { anyhow::bail!("fixture must not prepare a target") })
        }
        fn acquire(
            &self,
            _: &crate::core::RunContext,
        ) -> Result<Box<dyn crate::core::ActivityLease>> {
            anyhow::bail!("fixture must not acquire a target")
        }
        fn execute<'a>(
            &'a self,
            _: &'a crate::core::RunContext,
            _: &'a crate::core::RunRequest,
            _: bool,
            _: Arc<AtomicBool>,
        ) -> BoxFuture<'a, Result<Vec<(String, String)>>> {
            Box::pin(async { anyhow::bail!("fixture must not execute a target") })
        }
    }
    #[test]
    fn external_policy_checks_the_frozen_transitive_closure_without_scanning_data() {
        let mut program = json!({"run":[{"op":"fn","fn":"helper"}],"functions":{
            "helper":{"run":[{"op":"fn","fn":"deeper"}]},
            "deeper":{"run":[{"op":"fn","fn":"notify"}]},
            "unused":{"run":[{"op":"fn","fn":"switch_live_device"}]}
        }});
        assert!(validate_external_program(&program)
            .unwrap_err()
            .to_string()
            .contains("notify"));
        program["functions"]["deeper"]["run"] =
            json!([{"op":"fn","fn":"log","args":{"op":"fn","fn":"notify"}}]);
        validate_external_program(&program).unwrap();
        program["run"] = json!([{"op":"repeat","do":[{"op":"fn","fn":"wait_live_confirmation"}]}]);
        assert!(validate_external_program(&program).is_err());
        assert!(validate_external_program(&json!({"run":[{"op":"future_effect"}]})).is_err());
    }
    #[test]
    fn external_policy_checks_all_lowered_template_cases_including_polling_and_function_calls() {
        let policy = crate::core::ExecutionPolicy::external(
            "fixture".into(),
            ["gamer-yaml".into()].into(),
            ["run.submit".into()].into(),
        );
        for times in [1, 3] {
            for call in [
                "notify: {content: forbidden}",
                "switch_live_device: {}",
                "wait_live_confirmation: {}",
                "ocr: {}",
            ] {
                let source = format!("functions:\n  helper:\n    run:\n      - {call}\nrun:\n  - match_templates:\n      times: {times}\n      cases:\n        - template: probe.png\n          do:\n            - helper: {{}}\n");
                let script = super::super::syntax::parse_script(&source).unwrap();
                let program = super::super::syntax::build_program(
                    &script,
                    &vec![],
                    serde_json::Map::new(),
                    0,
                );
                assert_eq!(
                    program["run"][0]["op"],
                    if times == 1 {
                        "match_templates"
                    } else {
                        "match_templates_loop"
                    }
                );
                let error = validate_external_policy(&program, &policy)
                    .unwrap_err()
                    .to_string();
                assert!(
                    error.contains("external_execution_denied"),
                    "{call}: {error}"
                );
            }
        }
    }
    #[tokio::test]
    async fn forbidden_private_program_is_rejected_before_device_preparation() {
        let runs = Arc::new(RunManager::new(Arc::new(UnreachableExecutor)));
        let runtime = Arc::new(PrivateTrialRuntime::new(runs));
        runtime.pending_trials.lock().insert(
            "fixture".into(),
            TrialInput {
                program: json!({"run":[{"op":"fn","fn":"notify"}],"functions":{}}),
                templates: BTreeMap::new(),
            },
        );
        let executor = PrivateTrialExecutor {
            inner: Arc::downgrade(&runtime),
        };
        let app = crate::core::AppContext::for_test("unconfigured-target", "default").unwrap();
        let context = crate::core::RunContext::new(crate::core::RunId::generate(), app.clone());
        let request = crate::core::RunRequest::for_app(
            app,
            "gamer-yaml-version",
            "default/test.yaml",
            crate::core::RunPayload::new(json!({"trial_token":"fixture"})),
        )
        .unwrap()
        .with_execution_policy(crate::core::ExecutionPolicy::external(
            "fixture".into(),
            ["gamer-yaml".into()].into(),
            ["run.submit".into()].into(),
        ));
        let error = executor
            .prepare(&context, &request)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("external_execution_denied") && error.contains("notify"),
            "{error}"
        );
    }
    #[tokio::test]
    async fn private_trial_never_replays_a_token_from_a_previous_runtime() {
        let runs = Arc::new(RunManager::new(Arc::new(UnreachableExecutor)));
        let previous = Arc::new(PrivateTrialRuntime::new(runs.clone()));
        previous.pending_trials.lock().insert(
            "old-token".into(),
            TrialInput {
                program: json!({"run":[]}),
                templates: BTreeMap::new(),
            },
        );
        let fresh = Arc::new(PrivateTrialRuntime::new(runs));
        let executor = PrivateTrialExecutor {
            inner: Arc::downgrade(&fresh),
        };
        let app = crate::core::AppContext::for_test("synthetic-target", "default").unwrap();
        let context = crate::core::RunContext::new(crate::core::RunId::generate(), app.clone());
        let request = crate::core::RunRequest::for_app(
            app,
            "gamer-yaml-version",
            "default/script.yaml",
            crate::core::RunPayload::new(json!({"trial_token":"old-token"})),
        )
        .unwrap();
        let error = executor
            .execute(&context, &request, false, Arc::new(AtomicBool::new(false)))
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("private trial snapshot unavailable; do not replay"));
        assert_eq!(previous.pending_trials.lock().len(), 1);
        assert!(fresh.pending_trials.lock().is_empty());
    }
}
