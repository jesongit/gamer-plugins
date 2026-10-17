//! Deterministic workspace facade and read-only legacy candidate access.
//! Model requests and autonomous candidate mutation are intentionally absent.
#[path = "generation/legacy.rs"]
mod legacy;
use super::{
    authoring::{self, AuthoringService},
    authoring_support::{base64_map, required, Sample},
    revisions,
};
use crate::{
    extensions::{service::BuiltinService, ExtensionError, ExtensionResult, ExtensionService},
    resources::PackageStore,
    run_manager::RunManager,
};
use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use base64::Engine;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Cursor, path::PathBuf, sync::Arc};

pub(crate) const AUTHORING_ACTIONS: &[&str] = authoring::ACTIONS;
pub(crate) const ACTIONS: &[&str] = &[
    "generation.list",
    "generation.get",
    "generation.template",
    "generation.history",
    "generation.export",
    "generation.import_draft",
    "automation.validate_source",
];
pub(crate) struct GenerationService {
    inner: Arc<Inner>,
}
struct Inner {
    authoring: AuthoringService,
    packages: Arc<PackageStore>,
    gate: Arc<Mutex<()>>,
}
impl GenerationService {
    pub(crate) fn new(packages: Arc<PackageStore>, runs: Arc<RunManager>) -> Self {
        let authoring = AuthoringService::new(packages.clone(), runs);
        Self {
            inner: Arc::new(Inner {
                gate: authoring.inner.gate.clone(),
                authoring,
                packages,
            }),
        }
    }
    pub(crate) fn with_diagnostics(self, db: crate::store::Db) -> Self {
        *self.inner.authoring.inner.diagnostics.lock() = Some(db);
        self
    }
    pub(crate) fn with_event_sink(self, sink: Arc<dyn crate::core::events::EventSink>) -> Self {
        *self.inner.authoring.inner.trials.event_sink.lock() = Some(sink);
        self
    }
    pub(crate) fn with_devices(self, devices: Arc<crate::device::DeviceManager>) -> Self {
        *self.inner.authoring.inner.trials.devices.lock() = Some(devices);
        self
    }
    /// Only the existing publication transaction journal is recovered. Legacy
    /// candidates and old model jobs are never restarted or rewritten.
    pub(crate) fn recover_all(&self) -> Result<()> {
        for package in self.inner.packages.list_packages()? {
            revisions::recover(&self.inner.packages, &package.id)?;
        }
        Ok(())
    }
    pub(crate) fn attach(&self, extensions: &Arc<ExtensionService>) {
        self.inner.authoring.attach(extensions);
    }
}
#[async_trait]
impl BuiltinService for GenerationService {
    fn extension_id(&self) -> &str {
        super::YAML_EXTENSION_ID
    }
    async fn call(&self, action: &str, values: Value) -> ExtensionResult<Value> {
        if ACTIONS.contains(&action) || authoring::ACTIONS.contains(&action) {
            self.inner
                .dispatch(action, values)
                .await
                .map_err(|error| ExtensionError::CallRejected(error.to_string()))
        } else {
            super::actions::native_call_action(
                super::YAML_EXTENSION_ID,
                action,
                &values,
                self.inner.packages.data_root(),
            )
            .unwrap_or_else(|| Err(ExtensionError::CallRejected("unknown YAML action".into())))
        }
    }
    async fn stop(&self) {
        self.inner.authoring.stop().await;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    struct NoExecutor;
    impl crate::run_manager::RunExecutor for NoExecutor {
        fn prepare<'a>(
            &'a self,
            _: &'a crate::core::RunContext,
            _: &'a crate::core::RunRequest,
        ) -> futures_util::future::BoxFuture<'a, Result<()>> {
            Box::pin(async { anyhow::bail!("history fixture must not prepare a target") })
        }
        fn acquire(
            &self,
            _: &crate::core::RunContext,
        ) -> Result<Box<dyn crate::core::ActivityLease>> {
            anyhow::bail!("history fixture must not acquire a target")
        }
        fn execute<'a>(
            &'a self,
            _: &'a crate::core::RunContext,
            _: &'a crate::core::RunRequest,
            _: bool,
            _: Arc<AtomicBool>,
        ) -> futures_util::future::BoxFuture<'a, Result<Vec<(String, String)>>> {
            Box::pin(async { anyhow::bail!("history fixture must not execute a target") })
        }
    }
    pub(super) fn fixture() -> (tempfile::TempDir, GenerationService, Value) {
        let root = tempfile::tempdir().unwrap();
        let packages = Arc::new(
            PackageStore::open(&crate::config::Config {
                data_dir: root.path().into(),
                ..Default::default()
            })
            .unwrap(),
        );
        packages.ensure_default_package().unwrap();
        super::super::resources::register_resource_handlers(&packages);
        let service =
            GenerationService::new(packages, Arc::new(RunManager::new(Arc::new(NoExecutor))));
        let (yaml, _, samples) = super::super::offline_validation::validation_fixture_samples();
        let samples=samples.into_iter().map(|sample|json!({"manifest":sample.manifest,"files":sample.files.into_iter().map(|(path,bytes)|json!({"path":path,"base64":base64::engine::general_purpose::STANDARD.encode(bytes)})).collect::<Vec<_>>()})).collect::<Vec<_>>();
        let id = uuid::Uuid::new_v4().to_string();
        let envelope = json!({"candidate":{"id":id,"package_id":"default","name":"old.yaml","state":"generating","revision":7,"yaml":yaml,"templates":[{"name":"claim.png","sample_id":"with-confirm","frame_id":"f0","rect":[20,15,9,9]},{"name":"confirm.png","sample_id":"with-confirm","frame_id":"f2","rect":[20,15,9,9]},{"name":"done.png","sample_id":"with-confirm","frame_id":"f4","rect":[20,15,9,9]}],"known_tokens":42,"unknown_usage":true,"updated_at":"2026-01-01T00:00:00Z"},"base_yaml":"","base_exists":false,"verification_scope":{"scope":"historical"},"unknown_envelope":{"keep":true}});
        let path = service.inner.file("default", &id).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();
        let snapshot = json!({"samples":samples,"base":{"automations/_function.yaml":base64::engine::general_purpose::STANDARD.encode(b"functions: {helper: {run: [{log: historical}]}}\n")},"unknown_snapshot":{"keep":true}});
        std::fs::write(
            path.with_extension("snapshot"),
            serde_json::to_vec_pretty(&snapshot).unwrap(),
        )
        .unwrap();
        (root, service, envelope)
    }
    #[tokio::test]
    async fn deterministic_facade_rejects_retired_model_actions_and_shares_authoring_gate() {
        let (_root, service, envelope) = fixture();
        assert!(Arc::ptr_eq(
            &service.inner.gate,
            &service.inner.authoring.inner.gate
        ));
        for action in [
            "generation.create",
            "generation.start",
            "generation.repair",
            "generation.edit",
            "generation.apply_proposal",
            "generation.validate",
            "generation.retry",
            "generation.cancel",
            "generation.save",
            "generation.rollback",
            "generation.readiness",
            "authoring.generate",
        ] {
            assert!(
                BuiltinService::call(
                    &service,
                    action,
                    json!({"package_id":"default","candidate_id":envelope["candidate"]["id"]})
                )
                .await
                .is_err(),
                "{action}"
            );
        }
        BuiltinService::call(
            &service,
            "authoring.create",
            json!({"package_id":"default","script_id":"manual.yaml"}),
        )
        .await
        .unwrap();
        assert!(BuiltinService::call(
            &service,
            "authoring.get",
            json!({"package_id":"default","script_id":"manual.yaml"})
        )
        .await
        .is_ok());
    }
}
