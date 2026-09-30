//! Optional billing snapshots. Organization totals never replace or add to the
//! request ledger; an operator must explicitly associate a bill with a request.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Billing {
    pub protocol: String,
    pub endpoint: String,
    #[serde(default)]
    pub key: String,
    pub scope: String,
}
impl Billing {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            ["openai", "claude", "openrouter"].contains(&self.protocol.as_str()),
            "对账协议不支持"
        );
        let url = reqwest::Url::parse(&self.endpoint)?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "对账服务地址无效"
        );
        ensure!(
            !self.scope.trim().is_empty() && self.scope.len() <= 200,
            "必须指定专用项目、工作区或 key 的统计范围"
        );
        Ok(())
    }
    pub fn public(&self) -> Value {
        json!({"protocol":self.protocol,"endpoint":self.endpoint,"scope":self.scope,"has_key":!self.key.is_empty()})
    }
}
pub fn query(settings: &Billing, start: i64, end: i64) -> Result<reqwest::Url> {
    settings.validate()?;
    ensure!(
        end > start && end - start <= 7 * 86400,
        "对账时间范围最多七天"
    );
    let base = settings.endpoint.trim_end_matches('/');
    let path = match settings.protocol.as_str() {
        "openai" => "organization/costs",
        "claude" => "organizations/cost_report",
        _ => "key",
    };
    let mut url = reqwest::Url::parse(&format!("{base}/{path}"))?;
    if settings.protocol == "openai" {
        url.query_pairs_mut()
            .append_pair("start_time", &start.to_string())
            .append_pair("end_time", &end.to_string())
            .append_pair("project_ids", &settings.scope)
            .append_pair("limit", "7");
    }
    if settings.protocol == "claude" {
        url.query_pairs_mut()
            .append_pair(
                "starting_at",
                &chrono::DateTime::from_timestamp(start, 0)
                    .context("起始时间无效")?
                    .to_rfc3339(),
            )
            .append_pair(
                "ending_at",
                &chrono::DateTime::from_timestamp(end, 0)
                    .context("结束时间无效")?
                    .to_rfc3339(),
            )
            .append_pair("workspace_ids[]", &settings.scope)
            .append_pair("group_by[]", "workspace_id")
            .append_pair("limit", "7");
    }
    Ok(url)
}
pub fn normalize(settings: &Billing, value: &Value) -> Result<Value> {
    if settings.protocol == "openrouter" {
        let d = &value["data"];
        let used = d["usage"]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .context("供应商未返回 key 用量")?;
        return Ok(
            json!({"scope":settings.scope,"confirmed_micros":(used*1e6).ceil()as u64,"balance_micros":d["limit_remaining"].as_f64().map(|n|(n*1e6).floor()as i64),"period":"key_lifetime","not_additive":true}),
        );
    }
    let buckets = value["data"].as_array().context("供应商缺少账单数据")?;
    let mut micros = 0u64;
    for bucket in buckets.iter().take(32) {
        for row in bucket["results"]
            .as_array()
            .context("账单条目缺失")?
            .iter()
            .take(1000)
        {
            let amount = if settings.protocol == "openai" {
                ensure!(
                    row["amount"]["currency"]
                        .as_str()
                        .is_none_or(|c| c.eq_ignore_ascii_case("usd")),
                    "供应商账单币种不是 USD"
                );
                row["amount"]["value"].as_f64().context("费用缺失")?
            } else {
                ensure!(
                    row["currency"]
                        .as_str()
                        .is_none_or(|c| c.eq_ignore_ascii_case("usd")),
                    "供应商账单币种不是 USD"
                );
                row["amount"].as_str().context("费用缺失")?.parse::<f64>()? / 100.0
            };
            ensure!(amount.is_finite() && amount >= 0.0, "费用无效");
            micros = micros.saturating_add((amount * 1e6).ceil() as u64);
        }
    }
    Ok(
        json!({"scope":settings.scope,"confirmed_micros":micros,"balance_micros":null,"not_additive":true,"partial":value["has_more"]==true||value["next_page"].is_string()}),
    )
}
pub async fn sync(settings: &Billing, start: i64, end: i64) -> Result<Value> {
    ensure!(
        !settings.key.is_empty(),
        "未配置独立对账凭据；普通推理密钥不会自动用于管理接口"
    );
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let request = http.get(query(settings, start, end)?);
    let request = if settings.protocol == "claude" {
        request
            .header("x-api-key", &settings.key)
            .header("anthropic-version", "2023-06-01")
    } else {
        request.bearer_auth(&settings.key)
    };
    let mut response = request
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("对账请求失败，未自动重试"))?;
    ensure!(
        response.status().is_success(),
        "对账服务 HTTP {}",
        response.status().as_u16()
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow::anyhow!("对账响应中断"))?
    {
        ensure!(bytes.len() + chunk.len() <= 256 * 1024, "对账响应过大");
        bytes.extend_from_slice(&chunk);
    }
    normalize(settings, &serde_json::from_slice(&bytes)?)
}
