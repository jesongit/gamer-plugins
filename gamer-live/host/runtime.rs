//! Uses only the existing Core runner/describer boundaries; no YAML interpreter here.
use super::{
    queue::{Backend, SubmitError, Target},
    rules::{self, RuleSet, RULE_PATH, RUNNER},
};
use crate::{
    core::{AppPackageId, RunPayload, RunRequest},
    device::DeviceManager,
    resources::PackageStore,
    run_manager::RunManager,
    scheduler::Scheduler,
    store::Db,
    timer_core::{TimerRunner, TimerRunnerError},
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub struct Runtime {
    pub devices: Arc<DeviceManager>,
    pub packages: Arc<PackageStore>,
    pub scheduler: Arc<Scheduler>,
    pub runs: Arc<RunManager>,
    pub db: Db,
}
#[async_trait::async_trait]
impl Backend for Runtime {
    fn target(&self, device: &str, package: &str) -> Result<Target> {
        self.packages.manifest(package)?;
        let context =
            crate::targets::app_context(&self.devices, device, Some(AppPackageId::new(package)?))?;
        // Package directory birth identity detects atomic import/replacement without coupling to plugin internals.
        let path = self.packages.resource_path(package, super::ID, RULE_PATH)?;
        let root = path.ancestors().nth(4).context("配置包路径无效")?;
        let meta = std::fs::metadata(root)?;
        let stamp = format!("{:?}", meta.created().or_else(|_| meta.modified())?);
        Ok(Target {
            device_id: device.into(),
            package_id: package.into(),
            android_package: context.android_package.map(|p| p.as_str().to_owned()),
            package_stamp: stamp,
        })
    }
    fn check(&self, target: &Target) -> Result<()> {
        ensure!(
            self.target(&target.device_id, &target.package_id)? == *target,
            "配置包或设备目标已改变，请停止并重新绑定"
        );
        ensure!(
            self.scheduler.runner_registry().supports(RUNNER),
            "自动化插件未启用，请启用后继续队列"
        );
        crate::targets::check_available(&self.devices, &target.device_id)?;
        Ok(())
    }
    fn rules(&self, package: &str) -> Result<(RuleSet, Option<String>)> {
        self.packages.manifest(package)?;
        match self.packages.read_text(package, super::ID, RULE_PATH)? {
            Some(entry) => {
                ensure!(entry.content.len() <= 256 * 1024, "规则文件过大");
                let rules: RuleSet = serde_json::from_str(&entry.content)?;
                rules.validate(package)?;
                Ok((rules, Some(entry.version())))
            }
            None => Ok((RuleSet::default(), None)),
        }
    }
    fn save_rules(&self, package: &str, rules: &RuleSet, version: Option<&str>) -> Result<String> {
        self.packages.manifest(package)?;
        rules.validate(package)?;
        let text = serde_json::to_string_pretty(rules)?;
        ensure!(text.len() <= 256 * 1024, "规则文件过大");
        Ok(self
            .packages
            .write_text(package, super::ID, RULE_PATH, &text, version, false)?
            .version())
    }
    fn describe(&self, entry: &str) -> Result<Value> {
        self.scheduler
            .describe_entrypoint(RUNNER, entry)
            .map_err(|e| anyhow::anyhow!("入口不可用：{e:?}"))
    }
    fn bind(&self, entry: &str, args: Map<String, Value>) -> Result<Map<String, Value>> {
        self.scheduler
            .bind_entrypoint(RUNNER, entry, &args)
            .map_err(|e| anyhow::anyhow!("参数无效：{e:?}"))
    }
    async fn submit(
        &self,
        t: &Target,
        entry: &str,
        args: &Map<String, Value>,
    ) -> std::result::Result<String, SubmitError> {
        self.check(t)
            .map_err(|e| SubmitError::Blocked(e.to_string()))?;
        let request = (|| -> Result<RunRequest> {
            rules::validate_entrypoint(&t.package_id, entry)?;
            let bound = self.bind(entry, args.clone())?;
            let app = crate::targets::app_context(
                &self.devices,
                &t.device_id,
                Some(AppPackageId::new(&t.package_id)?),
            )?;
            Ok(RunRequest::for_app(
                app,
                RUNNER,
                entry,
                RunPayload::new(json!({"args":bound})),
            )?)
        })()
        .map_err(|e| SubmitError::Failed(e.to_string()))?;
        self.scheduler
            .runner_registry()
            .submit(request, "", None, Arc::new(|_| {}))
            .await
            .map(|r| r.run_id)
            .map_err(|e| match e {
                TimerRunnerError::Conflict(_) => SubmitError::Busy,
                TimerRunnerError::DependencyMissing(e) => SubmitError::Blocked(e),
                TimerRunnerError::ShuttingDown => SubmitError::Blocked("服务正在退出".into()),
                other => SubmitError::Failed(other.to_string()),
            })
    }
    async fn run(&self, id: &str) -> Result<Option<Value>> {
        if let Some(run) = self.runs.get_run(id) {
            return Ok(Some(serde_json::to_value(run)?));
        }
        self.db.stored_run(id.to_owned()).await
    }
    async fn cancel(&self, id: &str) -> Result<()> {
        match self.runs.cancel(id) {
            crate::run_manager::CancelOutcome::NotFound => anyhow::bail!("运行状态未知，请核对"),
            _ => Ok(()),
        }
    }
    fn active(&self, device: &str) -> Option<Value> {
        self.runs
            .active_for_device(device)
            .and_then(|r| serde_json::to_value(r).ok())
    }
}
