//! Per-task policy interpretation belongs to the notification plugin.
use super::{ID, SEND};
use crate::{
    extensions::{ExtensionId, ExtensionService},
    store::Db,
    timer_core::{Task, TaskResult, TaskResultHook},
};
use anyhow::{ensure, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    results: BTreeMap<String, Rule>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    channels: Vec<String>,
    #[serde(default)]
    title: String,
    #[serde(default)]
    content: String,
}

pub fn result_hook(extensions: Weak<ExtensionService>, db: Db) -> TaskResultHook {
    Arc::new(move |task, result| {
        // Core retains opaque policy even when this plugin is absent.
        if task.extensions.get(ID).is_none() {
            return;
        }
        let extensions = extensions.clone();
        let db = db.clone();
        tokio::spawn(async move {
            let send = async {
                let policy: Policy = serde_json::from_value(task.extensions[ID].clone())?;
                if !policy.enabled {
                    return Ok::<_, anyhow::Error>(());
                }
                let Some(rule) = policy.results.get(&result.state).filter(|r| r.enabled) else {
                    return Ok(());
                };
                ensure!(
                    !rule.channels.is_empty() && rule.channels.len() <= 100,
                    "任务通知未选择通道或通道过多"
                );
                let device_name = db
                    .get_device_async(task.app.device_id.as_str())
                    .await?
                    .map(|d| d.name)
                    .unwrap_or_else(|| task.app.device_id.to_string());
                let messages = messages(&task, &result, &device_name, rule)?;
                let extensions = extensions
                    .upgrade()
                    .ok_or_else(|| anyhow::anyhow!("通知插件不可用"))?;
                let id = ExtensionId::parse(ID)?;
                let mut errors = Vec::new();
                for values in messages {
                    match extensions.call_extension(&id, SEND, values).await {
                        Ok(response) if response["accepted"] == true => {}
                        Ok(response) => errors.push(
                            response["record"]["message"]
                                .as_str()
                                .unwrap_or("通知未发送")
                                .to_string(),
                        ),
                        Err(_) => errors.push("通知插件未安装、未启用或发送能力不可用".into()),
                    }
                }
                ensure!(errors.is_empty(), "{}", errors.join("；"));
                Ok(())
            }
            .await;
            if let Err(error) = send {
                let _ = db
                    .add_log_async(
                        task.app.device_id.as_str(),
                        &task.entrypoint,
                        "warn",
                        &format!("任务结果通知未发送：{error}"),
                    )
                    .await;
            }
        });
    })
}

/// The sender is selected for this specific terminal outcome, never by the presence of any task policy.
pub(crate) fn owns_result(task: &Task, state: &str) -> bool {
    owns_policy(task.extensions.get(ID), state)
}
fn owns_policy(policy: Option<&Value>, state: &str) -> bool {
    policy
        .and_then(|v| serde_json::from_value::<Policy>(v.clone()).ok())
        .is_some_and(|p| {
            p.enabled
                && p.results
                    .get(state)
                    .is_some_and(|r| r.enabled && !r.channels.is_empty() && r.channels.len() <= 100)
        })
}

fn messages(
    task: &Task,
    result: &TaskResult,
    device_name: &str,
    rule: &Rule,
) -> Result<Vec<Value>> {
    let label = match result.state.as_str() {
        "success" => "成功",
        "failed" => "失败",
        "cancelled" => "取消",
        _ => "跳过",
    };
    let values = BTreeMap::from([
        ("task.name", task.name.clone()),
        ("task.id", task.id.clone()),
        ("device.name", device_name.into()),
        ("device.id", task.app.device_id.to_string()),
        ("result", label.into()),
        ("elapsed", format!("{} 秒", result.elapsed_secs)),
        (
            "time",
            result
                .finished_at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
        ),
        (
            "error",
            super::truncate(result.error.as_deref().unwrap_or("无"), 1024),
        ),
        ("entrypoint", task.entrypoint.clone()),
        ("run.id", result.run_id.clone().unwrap_or_default()),
    ]);
    let title = render(
        if rule.title.trim().is_empty() {
            "{{task.name}}：{{result}}"
        } else {
            &rule.title
        },
        &values,
    )?;
    let content = render(
        if rule.content.trim().is_empty() {
            "任务：{{task.name}}\n设备：{{device.name}}\n结果：{{result}}\n时间：{{time}}\n耗时：{{elapsed}}\n错误：{{error}}"
        } else {
            &rule.content
        },
        &values,
    )?;
    let mut channels = rule.channels.clone();
    channels.sort();
    channels.dedup();
    Ok(channels.into_iter().map(|channel| json!({"channel":channel,"title":title,"content":content,"source":"task","source_id":result.event_id})).collect())
}

fn render(template: &str, values: &BTreeMap<&str, String>) -> Result<String> {
    let mut rest = template;
    let mut out = String::new();
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        let end = rest
            .find("}}")
            .ok_or_else(|| anyhow::anyhow!("通知文案变量缺少结束括号"))?;
        let name = rest[..end].trim();
        let value = values
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("未知通知文案变量：{name}"))?;
        out.push_str(value);
        rest = &rest[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sender_ownership_is_per_actual_terminal_rule_and_requires_channels() {
        let mut policy = BTreeMap::new();
        policy.insert(ID.to_string(),json!({"enabled":true,"results":{"success":{"enabled":true,"channels":["wechat"]},"failed":{"enabled":false,"channels":["wechat"]},"cancelled":{"enabled":true,"channels":[]}}}));
        assert!(owns_policy(policy.get(ID), "success"));
        assert!(!owns_policy(policy.get(ID), "failed"));
        assert!(!owns_policy(policy.get(ID), "cancelled"));
        policy.insert(
            ID.to_string(),
            json!({"enabled":false,"results":{"success":{"enabled":true,"channels":["wechat"]}}}),
        );
        assert!(!owns_policy(policy.get(ID), "success"));
    }
    #[test]
    fn template_values_are_literal_and_unknown_names_are_rejected() {
        let vars = BTreeMap::from([("task.name", "{{error}}".into()), ("error", "秘密".into())]);
        assert_eq!(
            render("任务 {{task.name}}", &vars).unwrap(),
            "任务 {{error}}"
        );
        assert!(render("{{unknown}}", &vars).is_err());
        assert!(render("{{task.name", &vars).is_err());
    }
}
