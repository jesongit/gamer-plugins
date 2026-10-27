use super::events::LiveEvent;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;

pub const RUNNER: &str = "gamer-yaml";
pub const RULE_PATH: &str = "interaction/rules.json";
pub const FIELDS: &[&str] = &[
    "text",
    "gift_id",
    "gift_name",
    "count",
    "actor_name",
    "actor_id",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub source: String,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub field: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CleanupSpec {
    pub entrypoint: String,
    #[serde(default)]
    pub args: Map<String, Value>,
    #[serde(default = "cleanup_timeout")]
    pub timeout_secs: u64,
}
fn cleanup_timeout() -> u64 {
    180
}
impl CleanupSpec {
    pub fn validate(&self, package: &str) -> Result<()> {
        validate_entrypoint(package, &self.entrypoint)?;
        ensure!(
            (1..=3600).contains(&self.timeout_secs) && self.args.len() <= 64,
            "收尾参数超出限制（超时须为 1..3600 秒）"
        );
        ensure!(
            self.args
                .values()
                .all(|value| value.to_string().len() <= 4096),
            "收尾参数值过大"
        );
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConfirmationSpec {
    #[serde(default = "confirmation_timeout")]
    pub timeout_secs: u64,
    pub cancel_cleanup: CleanupSpec,
}
fn confirmation_timeout() -> u64 {
    300
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CompletionSpec {
    pub entrypoint: String,
    pub checkpoint_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub public_name: String,
    #[serde(default)]
    pub enabled: bool,
    pub kind: String,
    #[serde(default = "equals")]
    pub operator: String,
    #[serde(default)]
    pub value: String,
    #[serde(default = "one")]
    pub min_count: u64,
    #[serde(default)]
    pub entrypoint: String,
    #[serde(default)]
    pub args: std::collections::BTreeMap<String, Binding>,
    #[serde(default)]
    pub cooldown_secs: u64,
    #[serde(default)]
    pub timeout_secs: u64,
    #[serde(default = "one")]
    pub cost: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<ConfirmationSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<CompletionSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup: Option<CleanupSpec>,
}
fn equals() -> String {
    "equals".into()
}
fn one() -> u64 {
    1
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleSet {
    pub schema_version: u8,
    pub rules: Vec<Rule>,
    /// Host-minted source of externally edited rules. Older user-authored
    /// files omit it and retain their management execution authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_origin: Option<ExternalRuleOrigin>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExternalRuleOrigin {
    pub policy: crate::core::ExecutionPolicy,
    pub device_id: String,
    pub package_id: String,
    pub runner_id: String,
}
impl ExternalRuleOrigin {
    pub fn authorize_target(&self, device: &str, package: &str) -> Result<()> {
        ensure!(
            self.device_id == device && self.package_id == package && self.runner_id == RUNNER,
            "external_execution_denied: live target differs from frozen rule authority"
        );
        ensure!(
            !device.is_empty()
                && !device.chars().any(char::is_control)
                && self.policy.has_scope("run.submit")
                && self.policy.allows_plugin(RUNNER),
            "external_execution_denied: rule authority does not permit the Runner"
        );
        Ok(())
    }
}
impl Default for RuleSet {
    fn default() -> Self {
        Self {
            schema_version: 1,
            rules: vec![],
            external_origin: None,
        }
    }
}
impl RuleSet {
    pub fn validate(&self, package: &str) -> Result<()> {
        if let Some(origin) = &self.external_origin {
            origin.authorize_target(&origin.device_id, package)?;
        }
        ensure!(
            self.schema_version == 1 && self.rules.len() <= 100,
            "规则格式无效或超过 100 条"
        );
        let mut ids = HashSet::new();
        for r in &self.rules {
            ensure!(
                !r.id.is_empty() && r.id.len() <= 100 && ids.insert(&r.id),
                "规则 ID 无效或重复"
            );
            ensure!(
                !r.name.trim().is_empty() && r.name.len() <= 160,
                "请填写规则名称"
            );
            ensure!(r.public_name.len() <= 160, "公开显示名过长");
            ensure!(
                ["message", "gift"].contains(&r.kind.as_str()),
                "仅支持弹幕和礼物规则"
            );
            ensure!(
                ["equals", "contains"].contains(&r.operator.as_str()),
                "匹配方式无效"
            );
            ensure!(
                r.value.len() <= 500
                    && r.min_count > 0
                    && r.args.len() <= 64
                    && r.cooldown_secs <= 86400
                    && r.timeout_secs <= 86400
                    && r.cost <= 1_000_000,
                "规则超出限制"
            );
            for b in r.args.values() {
                ensure!(
                    b.source == "fixed"
                        || (b.source == "event" && FIELDS.contains(&b.field.as_str())),
                    "参数来源无效"
                );
                ensure!(b.value.to_string().len() <= 4096, "参数值过大");
            }
            if let Some(cleanup) = &r.cleanup {
                cleanup.validate(package)?;
            }
            if let Some(confirmation) = &r.confirmation {
                ensure!(
                    (1..=300).contains(&confirmation.timeout_secs),
                    "本人确认须在 1..300 秒内"
                );
                confirmation.cancel_cleanup.validate(package)?;
            }
            if let Some(completion) = &r.completion {
                validate_entrypoint(package, &completion.entrypoint)?;
                ensure!(
                    completion.entrypoint.ends_with(".yaml")
                        && !completion.entrypoint.contains('#'),
                    "完成入口必须是配置包内完整脚本"
                );
                ensure!(
                    !completion.checkpoint_id.is_empty()
                        && completion.checkpoint_id.len() <= 128
                        && completion
                            .checkpoint_id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b)),
                    "完成检查点 ID 无效"
                );
            }
            if r.enabled {
                ensure!(
                    !r.value.trim().is_empty(),
                    "启用规则前请填写匹配内容或礼物 ID"
                );
                validate_entrypoint(package, &r.entrypoint)?;
            }
        }
        Ok(())
    }
}
pub fn validate_entrypoint(package: &str, entry: &str) -> Result<()> {
    ensure!(
        entry.len() <= 1024
            && (entry.starts_with(&format!("{package}/"))
                || entry.starts_with(&format!("{package}#"))),
        "入口必须属于选定配置包"
    );
    ensure!(
        entry
            .split(['/', '#'])
            .skip(1)
            .all(|s| !s.is_empty() && s != ".." && s != ".")
            && !entry.contains('\\'),
        "执行入口无效"
    );
    Ok(())
}
impl Rule {
    pub fn matches(&self, e: &LiveEvent) -> bool {
        if !self.enabled || self.kind != e.kind {
            return false;
        }
        if self.kind == "gift" {
            let gift = e.payload["gift_id"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| e.payload["gift_id"].to_string());
            return gift == self.value.trim()
                && e.payload["count"]
                    .as_u64()
                    .is_some_and(|n| n >= self.min_count);
        }
        let text = e.payload["text"].as_str().unwrap_or("").trim();
        if self.operator == "contains" {
            text.contains(self.value.trim())
        } else {
            text == self.value.trim()
        }
    }
    pub fn bind(&self, e: &LiveEvent) -> Result<Map<String, Value>> {
        let mut args = Map::new();
        for (name, b) in &self.args {
            let value = if b.source == "fixed" {
                b.value.clone()
            } else {
                match b.field.as_str() {
                    "gift_id" => match &e.payload["gift_id"] {
                        Value::String(id) => Value::String(id.clone()),
                        Value::Number(id) => Value::String(id.to_string()),
                        _ => Value::Null,
                    },
                    "actor_name" => e
                        .actor
                        .as_ref()
                        .map(|a| a["name"].clone())
                        .unwrap_or(Value::Null),
                    "actor_id" => e
                        .actor
                        .as_ref()
                        .map(|a| a["id"].clone())
                        .unwrap_or(Value::Null),
                    field => e.payload[field].clone(),
                }
            };
            ensure!(!value.is_null(), "参数 {name} 的事件字段缺失");
            args.insert(name.clone(), value);
        }
        Ok(args)
    }
}
pub fn bind_schema(mut args: Map<String, Value>, descriptor: &Value) -> Result<Map<String, Value>> {
    let schema = descriptor["schema"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("执行入口没有参数描述"))?;
    ensure!(
        args.keys()
            .all(|k| schema.iter().any(|d| d["name"].as_str() == Some(k))),
        "参数已被入口删除，请重新配置"
    );
    for d in schema {
        let name = d["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("参数描述无效"))?;
        if !args.contains_key(name) && !d["default"].is_null() {
            args.insert(name.into(), d["default"].clone());
        }
        let Some(v) = args.get(name) else {
            ensure!(
                !d["required"].as_bool().unwrap_or(false),
                "缺少必填参数 {name}"
            );
            continue;
        };
        let valid = match d["type"].as_str().unwrap_or("") {
            "string" | "template" => v.is_string(),
            "duration" | "key" => v.is_string() || v.is_number(),
            "any" => true,
            "point" => v.is_array() || v.is_object(),
            "integer" => v.is_i64() || v.is_u64(),
            "number" => v.is_number(),
            "boolean" => v.is_boolean(),
            "object" => v.is_object(),
            "array" | "list" => v.is_array(),
            _ => false,
        };
        ensure!(valid, "参数 {name} 类型不匹配，要求 {}", d["type"]);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rule() -> Value {
        json!({"id":"daily","name":"日常","enabled":true,"kind":"message",
            "value":"日常","entrypoint":"hsr/main.yaml"})
    }

    #[test]
    fn cost_accepts_free_rules_and_bounded_integers_only() {
        assert_eq!(serde_json::from_value::<Rule>(rule()).unwrap().cost, 1);
        for cost in [0, 1, 1_000_000, 1_000_001] {
            let mut value = rule();
            value["cost"] = json!(cost);
            let rules = RuleSet {
                schema_version: 1,
                rules: vec![serde_json::from_value(value).unwrap()],
                external_origin: None,
            };
            assert_eq!(rules.validate("hsr").is_ok(), cost <= 1_000_000);
        }
        for cost in [json!(-1), json!(1.5), json!("0"), Value::Null] {
            let mut value = rule();
            value["cost"] = cost;
            assert!(serde_json::from_value::<Rule>(value).is_err());
        }
    }

    #[test]
    fn cleanup_is_opt_in_and_retains_static_args_and_default_timeout() {
        let ordinary: Rule = serde_json::from_value(rule()).unwrap();
        assert!(ordinary.cleanup.is_none());
        assert!(serde_json::to_value(ordinary)
            .unwrap()
            .get("cleanup")
            .is_none());
        let mut value = rule();
        value["cleanup"] = json!({"entrypoint":"hsr/cleanup.yaml","args":{"notify_failure":false}});
        let configured: Rule = serde_json::from_value(value).unwrap();
        let cleanup = configured.cleanup.as_ref().unwrap();
        assert_eq!(cleanup.timeout_secs, 180);
        assert_eq!(cleanup.args["notify_failure"], false);
        RuleSet {
            schema_version: 1,
            rules: vec![configured],
            external_origin: None,
        }
        .validate("hsr")
        .unwrap();
        let empty: CleanupSpec =
            serde_json::from_value(json!({"entrypoint":"hsr/cleanup.yaml"})).unwrap();
        assert!(empty.args.is_empty());
    }

    #[test]
    fn cleanup_rejects_other_packages_traversal_unknown_fields_and_unbounded_settings() {
        for entry in [
            "other/cleanup.yaml",
            "hsr/../cleanup.yaml",
            "hsr\\cleanup.yaml",
            "hsr//cleanup.yaml",
        ] {
            let cleanup: CleanupSpec = serde_json::from_value(json!({"entrypoint":entry})).unwrap();
            assert!(cleanup.validate("hsr").is_err(), "{entry}");
        }
        for timeout in [0, 3601] {
            let cleanup: CleanupSpec = serde_json::from_value(
                json!({"entrypoint":"hsr/cleanup.yaml","timeout_secs":timeout}),
            )
            .unwrap();
            assert!(cleanup.validate("hsr").is_err());
        }
        let mut cleanup: CleanupSpec =
            serde_json::from_value(json!({"entrypoint":"hsr/cleanup.yaml","timeout_secs":3600}))
                .unwrap();
        cleanup.validate("hsr").unwrap();
        cleanup
            .args
            .insert("message".into(), json!("x".repeat(4097)));
        assert!(cleanup.validate("hsr").is_err());
        assert!(serde_json::from_value::<CleanupSpec>(
            json!({"entrypoint":"hsr/cleanup.yaml","arbitrary":true})
        )
        .is_err());
        let mut disabled = rule();
        disabled["enabled"] = json!(false);
        disabled["cleanup"] = json!({"entrypoint":"other/cleanup.yaml"});
        assert!(RuleSet {
            schema_version: 1,
            rules: vec![serde_json::from_value(disabled).unwrap()],
            external_origin: None,
        }
        .validate("hsr")
        .is_err());
    }
}
