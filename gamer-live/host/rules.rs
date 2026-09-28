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
}
impl Default for RuleSet {
    fn default() -> Self {
        Self {
            schema_version: 1,
            rules: vec![],
        }
    }
}
impl RuleSet {
    pub fn validate(&self, package: &str) -> Result<()> {
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
                    && r.timeout_secs <= 86400,
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
