//! Uses only the existing Core runner/describer boundaries; no YAML interpreter here.
use super::{
    queue::{Backend, SubmitError, Target},
    rules::{self, CompletionSpec, ExternalRuleOrigin, RuleSet, RULE_PATH, RUNNER},
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

fn save_rules_resource(
    packages: &PackageStore,
    package: &str,
    rules: &RuleSet,
    version: Option<&str>,
    authorize: &dyn Fn() -> Result<()>,
) -> Result<String> {
    rules.validate(package)?;
    let text = serde_json::to_string_pretty(rules)?;
    ensure!(text.len() <= 256 * 1024, "规则文件过大");
    packages.with_package_write(package, || {
        // Check after the resource barrier, including waits for replacement or
        // export. The queue's earlier target-lock check cannot cover this wait.
        authorize()?;
        Ok(packages
            .write_text(package, super::ID, RULE_PATH, &text, version, false)?
            .version())
    })
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
        save_rules_resource(&self.packages, package, rules, version, &|| Ok(()))
    }
    fn save_rules_admitted(
        &self,
        package: &str,
        rules: &RuleSet,
        version: Option<&str>,
        authorize: &dyn Fn() -> Result<()>,
    ) -> Result<String> {
        save_rules_resource(&self.packages, package, rules, version, authorize)
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
        self.submit_request(t, entry, args, None).await
    }
    fn validate_external_origin(&self, origin: &ExternalRuleOrigin) -> Result<()> {
        origin.authorize_target(&origin.device_id, &origin.package_id)?;
        let registry = self.scheduler.runner_registry();
        let runner = registry
            .get_runner(RUNNER)
            .context("external_execution_denied: Runner unavailable")?;
        ensure!(
            runner.owner_extension_id == RUNNER
                && runner.runner.supports_external_execution(RUNNER),
            "external_execution_denied: Runner does not support restricted external execution"
        );
        Ok(())
    }
    async fn submit_external(
        &self,
        t: &Target,
        entry: &str,
        args: &Map<String, Value>,
        origin: &ExternalRuleOrigin,
    ) -> std::result::Result<String, SubmitError> {
        self.submit_request(t, entry, args, Some(origin)).await
    }
    async fn run(&self, id: &str) -> Result<Option<Value>> {
        if let Some(run) = self.runs.get_run(id) {
            return Ok(Some(serde_json::to_value(run)?));
        }
        self.db.stored_run(id.to_owned()).await
    }
    async fn completed_child(&self, run_id: &str, full_entrypoint: &str) -> Result<bool> {
        completed_child_in_journal(&self.db, run_id, full_entrypoint).await
    }
    async fn completed_checkpoint(&self, run_id: &str, spec: &CompletionSpec) -> Result<bool> {
        completed_checkpoint_in_journal(&self.db, run_id, spec).await
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
impl Runtime {
    async fn submit_request(
        &self,
        t: &Target,
        entry: &str,
        args: &Map<String, Value>,
        origin: Option<&ExternalRuleOrigin>,
    ) -> std::result::Result<String, SubmitError> {
        if let Some(origin) = origin {
            origin
                .authorize_target(&t.device_id, &t.package_id)
                .and_then(|_| self.validate_external_origin(origin))
                .map_err(|error| SubmitError::Blocked(error.to_string()))?;
        }
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
        // Frozen authority survives token expiry, and the YAML Runner enforces
        // its native-function closure restrictions before device acquisition.
        let request = match origin {
            Some(origin) => request.with_execution_policy(origin.policy.clone()),
            None => request,
        };
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
}

async fn completed_checkpoint_in_journal(
    db: &Db,
    run_id: &str,
    spec: &CompletionSpec,
) -> Result<bool> {
    let (package, relative) = spec
        .entrypoint
        .split_once('/')
        .context("完成入口必须是配置包内脚本")?;
    rules::validate_entrypoint(package, &spec.entrypoint)?;
    ensure!(
        relative.ends_with(".yaml") && !relative.contains('#'),
        "完成入口必须是完整脚本"
    );
    let path = format!("automations/{relative}");
    let mut after = 0;
    loop {
        let page = db.run_event_page(run_id.to_owned(), after).await?;
        let events = page["events"].as_array().context("运行日志分页无效")?;
        if events.iter().any(|event| {
            let trace = &event["trace"];
            let source = &trace["source"];
            event["ev"] == "detail"
                && event["name"] == "checkpoint"
                && event["data"]["verified"] == true
                && event["data"]["id"].as_str() == Some(spec.checkpoint_id.as_str())
                && trace["run_id"].as_str() == Some(run_id)
                && source["plugin_id"] == "gamer-yaml"
                && source["package_id"].as_str() == Some(package)
                && source["path"].as_str() == Some(path.as_str())
        }) {
            return Ok(true);
        }
        if page["has_more"] != true {
            return Ok(false);
        }
        after = page["next"]
            .as_i64()
            .filter(|next| *next > after)
            .context("运行日志分页游标无效")?;
    }
}

// A successful nested function is insufficient: only the direct, whole-script
// child of this exact run can establish the business completion boundary.
async fn completed_child_in_journal(db: &Db, run_id: &str, full_entrypoint: &str) -> Result<bool> {
    let (package, relative) = full_entrypoint
        .split_once('/')
        .context("完成入口必须是配置包内脚本")?;
    rules::validate_entrypoint(package, full_entrypoint)?;
    ensure!(
        relative.ends_with(".yaml") && !relative.contains('#'),
        "完成入口必须是完整脚本"
    );
    let path = format!("automations/{relative}");
    let mut after = 0;
    loop {
        let page = db.run_event_page(run_id.to_owned(), after).await?;
        let events = page["events"].as_array().context("运行日志分页无效")?;
        if events.iter().any(|event| {
            let trace = &event["trace"];
            let source = &trace["source"];
            event["ev"] == "call_end"
                && event["ok"] == true
                && event["target"].as_str() == Some(relative)
                && trace["run_id"].as_str() == Some(run_id)
                && trace["parent_frame_id"].as_u64() == Some(0)
                && trace["frame_id"].as_u64().is_some_and(|frame| frame > 0)
                && source["plugin_id"] == "gamer-yaml"
                && source["package_id"].as_str() == Some(package)
                && source["path"].as_str() == Some(path.as_str())
        }) {
            return Ok(true);
        }
        if page["has_more"] != true {
            return Ok(false);
        }
        let next = page["next"]
            .as_i64()
            .filter(|next| *next > after)
            .context("运行日志分页游标无效")?;
        after = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn revoked_rules_write_waiting_for_the_resource_barrier_preserves_the_file() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let root = tempfile::tempdir().unwrap();
        let packages = Arc::new(
            PackageStore::open(&crate::config::Config {
                data_dir: root.path().into(),
                ..Default::default()
            })
            .unwrap(),
        );
        packages.ensure_default_package().unwrap();
        let version =
            save_rules_resource(&packages, "default", &RuleSet::default(), None, &|| Ok(()))
                .unwrap();
        let before = packages
            .read_text("default", super::super::ID, RULE_PATH)
            .unwrap()
            .unwrap()
            .content;
        let (locked, acquired) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let holder = std::thread::spawn({
            let packages = packages.clone();
            move || {
                packages
                    .with_package_write("default", || {
                        locked.send(()).unwrap();
                        released
                            .recv_timeout(std::time::Duration::from_secs(5))
                            .unwrap();
                        Ok(())
                    })
                    .unwrap()
            }
        });
        acquired
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let active = Arc::new(AtomicBool::new(true));
        let started = Arc::new(AtomicBool::new(false));
        let pending = tokio::task::spawn_blocking({
            let packages = packages.clone();
            let active = active.clone();
            let started = started.clone();
            move || {
                started.store(true, Ordering::Release);
                save_rules_resource(
                    &packages,
                    "default",
                    &RuleSet::default(),
                    Some(&version),
                    &|| {
                        ensure!(active.load(Ordering::Acquire), "credential_invalid");
                        Ok(())
                    },
                )
            }
        });
        while !started.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!pending.is_finished());
        active.store(false, Ordering::Release);
        release.send(()).unwrap();
        holder.join().unwrap();
        assert!(pending.await.unwrap().is_err());
        assert_eq!(
            packages
                .read_text("default", super::super::ID, RULE_PATH)
                .unwrap()
                .unwrap()
                .content,
            before
        );
    }

    fn save_parent_run(db: &Db, run_id: &str) {
        db.save_run_record(&crate::run_manager::RunRecord {
            run_id: run_id.into(),
            device_id: "fixture-browser".into(),
            runner_id: "gamer-yaml".into(),
            entrypoint: "hsr/live.yaml".into(),
            content_package: Some("hsr".into()),
            script_id: "hsr/live.yaml".into(),
            source: crate::run_manager::RunSource::Manual,
            task_id: None,
            scheduled_at: None,
            state: crate::run_manager::RunState::Failed,
            started_at: chrono::Utc::now(),
            finished_at: Some(chrono::Utc::now()),
            error: Some("fixture cleanup failure".into()),
        })
        .unwrap();
    }

    fn completed(run_id: &str) -> Value {
        json!({"ev":"call_end","ok":true,"target":"daily.yaml","trace":{"run_id":run_id,
            "frame_id":1,"parent_frame_id":0,"source":{"plugin_id":"gamer-yaml","package_id":"hsr","path":"automations/daily.yaml"}}})
    }

    fn reward_checkpoint(run_id: &str) -> Value {
        json!({"ev":"detail","name":"checkpoint",
            "data":{"id":"hsr_daily_final_reward","verified":true,"template":"daily_500_claimed_check.png"},
            "trace":{"run_id":run_id,"frame_id":9,"parent_frame_id":7,
                "source":{"plugin_id":"gamer-yaml","package_id":"hsr","path":"automations/daily.yaml"}}})
    }

    #[tokio::test]
    async fn completed_checkpoint_requires_exact_verified_goal_and_source_in_same_run() {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(
            crate::store::Store::open(&crate::config::Config {
                data_dir: directory.path().into(),
                ..Default::default()
            })
            .unwrap(),
        );
        let spec = CompletionSpec {
            entrypoint: "hsr/daily.yaml".into(),
            checkpoint_id: "hsr_daily_final_reward".into(),
        };
        for (label, mutate) in [
            (
                "not-detail",
                (|e: &mut Value| e["ev"] = json!("call_end")) as fn(&mut Value),
            ),
            ("wrong-detail", |e: &mut Value| {
                e["name"] = json!("effective_args")
            }),
            ("unverified", |e: &mut Value| {
                e["data"]["verified"] = json!(false)
            }),
            ("missing-verification", |e: &mut Value| {
                e["data"].as_object_mut().unwrap().remove("verified");
            }),
            ("string-verification", |e: &mut Value| {
                e["data"]["verified"] = json!("true")
            }),
            ("wrong-id", |e: &mut Value| {
                e["data"]["id"] = json!("another_reward")
            }),
            ("missing-id", |e: &mut Value| {
                e["data"].as_object_mut().unwrap().remove("id");
            }),
            ("other-run", |e: &mut Value| {
                e["trace"]["run_id"] = json!("other")
            }),
            ("missing-trace", |e: &mut Value| {
                e.as_object_mut().unwrap().remove("trace");
            }),
            ("other-plugin", |e: &mut Value| {
                e["trace"]["source"]["plugin_id"] = json!("other")
            }),
            ("other-package", |e: &mut Value| {
                e["trace"]["source"]["package_id"] = json!("other")
            }),
            ("other-path", |e: &mut Value| {
                e["trace"]["source"]["path"] = json!("automations/other.yaml")
            }),
            ("function-path", |e: &mut Value| {
                e["trace"]["source"]["path"] = json!("automations/daily.yaml#daily_tasks")
            }),
        ] {
            save_parent_run(&db, label);
            let mut event = reward_checkpoint(label);
            mutate(&mut event);
            db.append_run_event(label.into(), event).await.unwrap();
            assert!(
                !completed_checkpoint_in_journal(&db, label, &spec)
                    .await
                    .unwrap(),
                "{label}"
            );
        }
        save_parent_run(&db, "checkpoint-nested");
        save_parent_run(&db, "checkpoint-absent");
        db.append_run_event(
            "checkpoint-nested".into(),
            reward_checkpoint("checkpoint-nested"),
        )
        .await
        .unwrap();
        assert!(
            completed_checkpoint_in_journal(&db, "checkpoint-nested", &spec)
                .await
                .unwrap(),
            "the reward is inside daily_tasks; a nested frame is valid evidence"
        );
        assert!(
            !completed_checkpoint_in_journal(&db, "checkpoint-absent", &spec)
                .await
                .unwrap()
        );
        let function_spec = CompletionSpec {
            entrypoint: "hsr/daily.yaml#daily_tasks".into(),
            ..spec
        };
        assert!(
            completed_checkpoint_in_journal(&db, "checkpoint-nested", &function_spec)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn completed_checkpoint_follows_all_journal_pages_without_accepting_other_goals() {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(
            crate::store::Store::open(&crate::config::Config {
                data_dir: directory.path().into(),
                ..Default::default()
            })
            .unwrap(),
        );
        let spec = CompletionSpec {
            entrypoint: "hsr/daily.yaml".into(),
            checkpoint_id: "hsr_daily_final_reward".into(),
        };
        save_parent_run(&db, "checkpoint-paged");
        for index in 0..501 {
            let mut event = reward_checkpoint("checkpoint-paged");
            event["data"]["id"] = json!(format!("other-goal-{index}"));
            db.append_run_event("checkpoint-paged".into(), event)
                .await
                .unwrap();
        }
        assert!(
            !completed_checkpoint_in_journal(&db, "checkpoint-paged", &spec)
                .await
                .unwrap()
        );
        db.append_run_event(
            "checkpoint-paged".into(),
            reward_checkpoint("checkpoint-paged"),
        )
        .await
        .unwrap();
        assert!(
            completed_checkpoint_in_journal(&db, "checkpoint-paged", &spec)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn cleanup_reconciliation_requires_exact_direct_script_success_in_same_run() {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(
            crate::store::Store::open(&crate::config::Config {
                data_dir: directory.path().into(),
                ..Default::default()
            })
            .unwrap(),
        );
        for (label, mutate) in [
            (
                "failed",
                (|event: &mut Value| event["ok"] = json!(false)) as fn(&mut Value),
            ),
            ("nested", |event: &mut Value| {
                event["trace"]["parent_frame_id"] = json!(1)
            }),
            ("other-run", |event: &mut Value| {
                event["trace"]["run_id"] = json!("another")
            }),
            ("other-package", |event: &mut Value| {
                event["trace"]["source"]["package_id"] = json!("other")
            }),
            ("other-script", |event: &mut Value| {
                event["trace"]["source"]["path"] = json!("automations/other.yaml")
            }),
            ("function", |event: &mut Value| {
                event["target"] = json!("daily.yaml#daily_tasks")
            }),
            ("root", |event: &mut Value| {
                event["trace"]["frame_id"] = json!(0)
            }),
            ("not-a-call", |event: &mut Value| {
                event["ev"] = json!("run_end")
            }),
        ] {
            save_parent_run(&db, label);
            let mut event = completed(label);
            mutate(&mut event);
            db.append_run_event(label.into(), event).await.unwrap();
            assert!(
                !completed_child_in_journal(&db, label, "hsr/daily.yaml")
                    .await
                    .unwrap(),
                "{label}"
            );
        }
        save_parent_run(&db, "accepted");
        save_parent_run(&db, "absent");
        db.append_run_event("accepted".into(), completed("accepted"))
            .await
            .unwrap();
        assert!(
            completed_child_in_journal(&db, "accepted", "hsr/daily.yaml")
                .await
                .unwrap()
        );
        assert!(!completed_child_in_journal(&db, "absent", "hsr/daily.yaml")
            .await
            .unwrap());
        assert!(
            completed_child_in_journal(&db, "accepted", "hsr/daily.yaml#daily_tasks")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn cleanup_reconciliation_follows_all_journal_pages() {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(
            crate::store::Store::open(&crate::config::Config {
                data_dir: directory.path().into(),
                ..Default::default()
            })
            .unwrap(),
        );
        save_parent_run(&db, "paged");
        for index in 0..501 {
            db.append_run_event(
                "paged".into(),
                json!({"ev":"detail","data":{"index":index}}),
            )
            .await
            .unwrap();
        }
        db.append_run_event("paged".into(), completed("paged"))
            .await
            .unwrap();
        assert!(completed_child_in_journal(&db, "paged", "hsr/daily.yaml")
            .await
            .unwrap());
    }
}
