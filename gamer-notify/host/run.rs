//! Script result subscriptions cover manual, private-version and scheduled runs.
//! They observe archived Core results, so interpreter errors cannot bypass delivery.
use super::{settings::Settings, ID, SEND};
use crate::{
    extensions::{ExtensionId, ExtensionService},
    run_manager::{FinishHook, RunRecord, RunState},
    store::Db,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Weak},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    package_id: String,
    entrypoint: String,
    runner_ids: Vec<String>,
    channels: Vec<String>,
    enabled: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rules {
    version: Option<String>,
    rules: Vec<Rule>,
}
impl Rules {
    fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                ensure!(bytes.len() <= 256 * 1024, "运行通知配置过大");
                serde_json::from_slice(&bytes).context("运行通知配置损坏")
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
}

pub(super) fn dispatch(
    path: &Path,
    channels_path: &Path,
    action: &str,
    values: Value,
) -> Result<Value> {
    let mut saved = Rules::load(path)?;
    if action == "runs.read" {
        return Ok(serde_json::to_value(saved)?);
    }
    ensure!(
        saved.version.as_deref() == values["expected_version"].as_str(),
        "运行通知配置已更新，请刷新后重试"
    );
    let package = values["rule"]["package_id"]
        .as_str()
        .context("缺少配置包")?;
    let entrypoint = values["rule"]["entrypoint"]
        .as_str()
        .context("缺少脚本入口")?;
    crate::resources::validate_scope_id("package", package)?;
    ensure!(
        entrypoint.len() <= 512 && entrypoint.starts_with(&format!("{package}/")),
        "脚本入口与配置包不匹配"
    );
    if action == "runs.save" {
        let mut rule: Rule = serde_json::from_value(values["rule"].clone())?;
        ensure!(
            !rule.runner_ids.is_empty() && rule.runner_ids.len() <= 16,
            "未选择运行器或运行器过多"
        );
        for runner in &rule.runner_ids {
            crate::resources::validate_scope_id("runner", runner)?;
        }
        ensure!(
            !rule.channels.is_empty() && rule.channels.len() <= 100,
            "未选择通知通道或通道过多"
        );
        let channels = Settings::load(channels_path)?;
        for channel in &rule.channels {
            channels.resolve(Some(channel))?;
        }
        rule.channels.sort();
        rule.channels.dedup();
        rule.runner_ids.sort();
        rule.runner_ids.dedup();
        if let Some(old) = saved
            .rules
            .iter_mut()
            .find(|old| old.package_id == rule.package_id && old.entrypoint == rule.entrypoint)
        {
            *old = rule;
        } else {
            ensure!(saved.rules.len() < 100, "运行通知规则过多");
            saved.rules.push(rule);
        }
    } else {
        saved
            .rules
            .retain(|rule| rule.package_id != package || rule.entrypoint != entrypoint);
    }
    saved.version = Some(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(path.parent().context("运行通知路径无效")?)?;
    crate::core::fs::atomic_write(path, &serde_json::to_vec(&saved)?)?;
    Ok(serde_json::to_value(saved)?)
}

fn matching<'a>(saved: &'a Rules, record: &RunRecord) -> Option<&'a Rule> {
    saved.rules.iter().find(|rule| {
        rule.enabled
            && record.content_package.as_deref() == Some(rule.package_id.as_str())
            && record.entrypoint == rule.entrypoint
            && rule.runner_ids.contains(&record.runner_id)
    })
}

pub(super) fn channels_for_scope(path: &Path, scope: &Value) -> Result<Option<Vec<String>>> {
    let package = scope["package_id"]
        .as_str()
        .context("通知脚本作用域缺少配置包")?;
    let entrypoint = scope["entrypoint"]
        .as_str()
        .context("通知脚本作用域缺少入口")?;
    crate::resources::validate_scope_id("package", package)?;
    ensure!(
        entrypoint.starts_with(&format!("{package}/")) && entrypoint.len() <= 512,
        "通知脚本入口与配置包不匹配"
    );
    let rules = Rules::load(path)?;
    Ok(rules
        .rules
        .into_iter()
        .find(|rule| {
            rule.enabled
                && rule.package_id == package
                && rule.entrypoint == entrypoint
                && rule
                    .runner_ids
                    .iter()
                    .any(|id| matches!(id.as_str(), "gamer-yaml" | "gamer-yaml-version"))
        })
        .map(|rule| {
            let mut channels = rule.channels;
            channels.sort();
            channels.dedup();
            channels
        }))
}

pub(super) async fn script_owns_result_policy(db: &Db, run_id: &str) -> Result<bool> {
    let mut after = 0;
    loop {
        let page = db.run_event_page(run_id.to_string(), after).await?;
        if page["events"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|event| {
                event["ev"] == "detail"
                    && event["name"] == "notification_policy"
                    && event["data"]["frame_id"] == 0
            })
        {
            return Ok(true);
        }
        if page["has_more"] != true {
            return Ok(false);
        }
        let next = page["next"]
            .as_i64()
            .filter(|next| *next > after)
            .context("运行事件游标未前进")?;
        after = next;
    }
}

pub fn result_hook(extensions: Weak<ExtensionService>, db: Db, data_root: PathBuf) -> FinishHook {
    Arc::new(move |record, _| {
        let record = record.clone();
        let extensions = extensions.clone();
        let db = db.clone();
        let path = data_root
            .join("extension-data")
            .join(ID)
            .join("run-notifications.json");
        tokio::spawn(async move {
            let send = async {
                let saved = Rules::load(&path)?;
                let Some(rule) = matching(&saved, &record) else {
                    return Ok::<_, anyhow::Error>(());
                };
                // A script's explicit result policy also owns disabled outcomes.
                // Preparation errors have no marker and retain the saved fallback.
                if script_owns_result_policy(&db, &record.run_id).await? {
                    return Ok(());
                }
                let target = super::target_name(&db, &record.device_id).await?;
                let state = serde_json::to_value(record.state)?
                    .as_str()
                    .unwrap()
                    .to_string();
                // An explicit task policy owns its channels; do not send twice there.
                let task = match record.task_id.as_deref() {
                    Some(id) => db.get_timer_task_async(id).await?,
                    None => None,
                };
                let policy = task.as_ref().map(|task| &task.extensions[ID]);
                let task_channels = policy
                    .filter(|policy| policy["enabled"] == true)
                    .map(|policy| &policy["results"][&state])
                    .filter(|result| result["enabled"] == true)
                    .and_then(|result| result["channels"].as_array());
                let label = match record.state {
                    RunState::Success => "成功",
                    RunState::Cancelled => "取消",
                    _ => "失败",
                };
                let finished = record.finished_at.unwrap_or_else(chrono::Utc::now);
                let title = format!("{target} · {}：{label}", record.entrypoint);
                let content = format!(
                    "脚本：{}\n目标：{}\n结果：{}\n时间：{}\n耗时：{} 秒\n运行：{}\n错误：{}",
                    record.entrypoint,
                    super::truncate(&target, 256),
                    label,
                    finished
                        .with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M:%S"),
                    (finished - record.started_at).num_seconds().max(0),
                    record.run_id,
                    super::truncate(record.error.as_deref().unwrap_or("无"), 768)
                );
                let extensions = extensions.upgrade().context("通知插件不可用")?;
                let id = ExtensionId::parse(ID)?;
                for channel in &rule.channels {
                    if task_channels.is_some_and(|channels| {
                        channels.iter().any(|id| id.as_str() == Some(channel))
                    }) {
                        continue;
                    }
                    let result = extensions
                        .call_extension(
                            &id,
                            SEND,
                            json!({"channel":channel,
                        "title":super::truncate(&title,256),"content":content,"source":"run",
                        "source_id":format!("run:{}",record.run_id)}),
                        )
                        .await?;
                    ensure!(
                        result["accepted"] == true,
                        "{}",
                        result["record"]["message"].as_str().unwrap_or("通知未提交")
                    );
                }
                Ok(())
            }
            .await;
            if let Err(error) = send {
                let _ = db
                    .add_log_async(
                        &record.device_id,
                        &record.entrypoint,
                        "warn",
                        &format!("运行结果通知未发送：{error}"),
                    )
                    .await;
            }
        });
    })
}
