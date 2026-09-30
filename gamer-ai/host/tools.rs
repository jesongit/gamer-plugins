//! One tool directory for Runner and MCP. Management actions never appear here.
use super::{
    runtime::Execution,
    store::{self, Consumption, Verification},
};
use anyhow::{ensure, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
pub const NAMES: &[&str] = &[
    "observe",
    "act",
    "wait",
    "search_guides",
    "read_guide",
    "memory.search",
    "memory.read",
    "memory.propose",
    "list_automations",
    "call_automation",
    "request_approval",
    "finish",
];
pub fn input_schema(name: &str) -> Value {
    fn schema(example: &Value) -> Value {
        match example {
            Value::Object(fields) => {
                json!({"type":"object","properties":fields.iter().map(|(k,v)|(k.clone(),schema(v))).collect::<serde_json::Map<_,_>>()})
            }
            Value::Array(values) => {
                json!({"type":"array","items":values.first().map(schema).unwrap_or(json!({}))})
            }
            Value::Number(n) => json!({"type":if n.is_u64(){"integer"}else{"number"}}),
            Value::Bool(_) => json!({"type":"boolean"}),
            Value::Null => json!({}),
            Value::String(s) => {
                if s.contains('|') {
                    json!({"type":"string","enum":s.split('|').collect::<Vec<_>>()})
                } else {
                    json!({"type":"string"})
                }
            }
        }
    }
    let directory = catalog();
    let example = directory
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .map(|v| &v["args"]);
    let mut result = example
        .map(schema)
        .unwrap_or(json!({"type":"object","properties":{}}));
    result["properties"]["session_id"] =
        json!({"type":"string","description":"Host-issued restricted MCP tool session"});
    let mut required = example
        .and_then(Value::as_object)
        .map(|v| v.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    required.push("session_id".into());
    result["required"] = json!(required);
    result
}
/// External clients cannot classify their own inputs as free.
pub async fn execute_external(e: &Execution, name: &str, mut args: Value) -> Result<Value> {
    if name == "act" {
        let o = e
            .valid_observation(required(&args, "observation_id")?)
            .await?;
        let s = e.runtime.repository.data.lock().sessions[&e.session].clone();
        let reply=e.infer(json!({"mode":"check_external_side_effect","action":args["action"],"goal":s.goal,"account_confirmed":s.account,"cycle_confirmed":s.cycle,"observation_id":o.id,"instruction":"外部客户端消耗声明不可信。仅按图片与准确动作判定，返回 act arguments={safe_coordinates:boolean,account_consistent:boolean,consumption:{category,resource,quantity,purpose,evidence}}。账号无法核实为 false，消耗不确定为 unknown。"}).to_string(),&o,"mcp_gate").await?;
        ensure!(
            reply.decision.tool == "act" && reply.decision.arguments["safe_coordinates"] == true,
            "mcp_side_effect_unverified"
        );
        ensure!(
            s.account.is_none() || reply.decision.arguments["account_consistent"] == true,
            "account_unverified: 当前账号尚未核实"
        );
        args["consumption"] = reply.decision.arguments["consumption"].clone();
    }
    let mut result = execute(e, name, args).await?;
    if name == "observe" {
        use base64::Engine;
        let o = e.observation.lock().clone().context("observe_required")?;
        result["_mcp_image"] = json!({"type":"image","mimeType":"image/png","data":base64::engine::general_purpose::STANDARD.encode(&o.image)});
    }
    Ok(result)
}
pub fn catalog() -> Value {
    json!([
        {"name":"observe","args":{},"description":"Get a fresh exact frame and observation identity"},
        {"name":"act","args":{"operation_id":"unique stable ID","observation_id":"current ID","action":{"kind":"tap|swipe|key|text","position":[0.5,0.5],"from":[0.2,0.5],"to":[0.8,0.5],"duration_ms":300,"key":"BACK","text":"bounded text"},"consumption":{"category":"navigation|regenerative_resource|item|currency|paid|unknown","resource":"exact name","quantity":1,"purpose":"exact purpose","evidence":"visible cost text"},"expected":"visible expected result"}},
        {"name":"wait","args":{"duration_ms":500},"description":"Bounded local wait, maximum 5000ms"},
        {"name":"search_guides","args":{"query":"search terms"}},
        {"name":"read_guide","args":{"path":"guides/name.json"}},
        {"name":"memory.search","args":{"query":"keywords","limit":5}},
        {"name":"memory.read","args":{"path":"guides/name.json"}},
        {"name":"memory.propose","args":{"title":"title","content":"semantic guide without personal identifiers","conditions":"app/version/conditions","sources":["https://source.example"],"observation_id":"current ID","supersedes":null},"description":"Candidate only; cannot grant permissions or declare local verification"},
        {"name":"list_automations","args":{},"description":"Discover current Package YAML; every side effect requires fresh multimodal policy approval"},
        {"name":"call_automation","args":{"entrypoint":"package/name.yaml or package#function","args":{}},"description":"Execute in this parent run without acquiring another device slot"},
        {"name":"request_approval","args":{"operation_id":"ID","observation_id":"current ID","consumption":{"category":"unknown","resource":"resource","quantity":1,"purpose":"purpose","evidence":"screen evidence"}}},
        {"name":"finish","args":{"subgoals":[{"name":"subgoal","state":"completed|blocked","evidence":"current observation ID","result":"visible result"}],"summary":"completed work and blockers"},"description":"New screenshot and multimodal verification are required; partial results never become success"}
    ])
}
fn required<'a>(v: &'a Value, name: &str) -> Result<&'a str> {
    v[name]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 1000)
        .with_context(|| format!("{name} 缺失或超限"))
}
fn package(e: &Execution) -> Result<String> {
    Ok(e.context
        .app
        .content_package
        .as_ref()
        .context("Package Context 缺失")?
        .to_string())
}
fn guide_path(value: &Value) -> Result<&str> {
    let path = required(value, "path")?;
    crate::resources::sanitize_rel_path(path)?;
    ensure!(
        path.starts_with("guides/") && path.ends_with(".json"),
        "攻略路径无效"
    );
    Ok(path)
}
fn read_memory(e: &Execution, path: &str) -> Result<Value> {
    let package = package(e)?;
    read_local_memory(&e.runtime, &package, path)
}
pub fn read_local_memory(
    runtime: &super::runtime::Runtime,
    package: &str,
    path: &str,
) -> Result<Value> {
    let entry = runtime
        .packages
        .read_text(package, super::ID, path)?
        .context("攻略不存在")?;
    let mut value: Value = serde_json::from_str(&entry.content)?;
    ensure!(value.is_object(), "攻略不是对象");
    value["effective_status"] = json!("candidate");
    let hash = store::hash(entry.content.as_bytes());
    let generation = runtime.packages.instance_generation(package)?;
    let verified = runtime
        .repository
        .data
        .lock()
        .verifications
        .iter()
        .any(|v| {
            v.package == package
                && v.generation == generation
                && v.path == path
                && v.hash == hash
                && resource_identity(runtime, package, path)
                    .is_ok_and(|identity| identity == v.resource_instance)
                && runtime
                    .repository
                    .root
                    .join("evidence")
                    .join(&v.session)
                    .join(format!("{}.png", v.observation))
                    .is_file()
        });
    if verified {
        value["effective_status"] = json!("verified");
    }
    value["path"] = json!(path);
    value["content_hash"] = json!(hash);
    value["version"] = json!(entry.version());
    value.as_object_mut().unwrap().remove("verified");
    Ok(value)
}
fn resource_instance(e: &Execution, package: &str, path: &str) -> Result<String> {
    resource_identity(&e.runtime, package, path)
}
fn resource_identity(
    runtime: &super::runtime::Runtime,
    package: &str,
    path: &str,
) -> Result<String> {
    let m = std::fs::metadata(runtime.packages.resource_path(package, super::ID, path)?)?;
    Ok(format!("{:?}:{:?}", m.created().ok(), m.modified()?))
}
pub fn memory_search(e: &Execution, query: &str, limit: usize) -> Result<Value> {
    let package = package(e)?;
    let files = e.runtime.packages.list(&package, super::ID, "guides")?;
    let mut found = Vec::new();
    for file in files.iter().take(1000) {
        if !file.path.ends_with(".json") {
            continue;
        }
        let value = match read_memory(e, &file.path) {
            Ok(value) => value,
            Err(_) => {
                let seen = e.runtime.repository.data.lock().sessions[&e.session]
                    .events
                    .iter()
                    .any(|event| {
                        event.kind == "memory_unreadable" && event.data["path"] == file.path
                    });
                if !seen {
                    e.event("memory_unreadable",json!({"path":file.path,"message":"攻略内容无法读取，已跳过；可在攻略管理修订"}))?;
                }
                continue;
            }
        };
        if value["app"]
            != e.context
                .app
                .android_package
                .as_ref()
                .context("Android App Context 缺失")?
                .as_str()
        {
            continue;
        }
        if query.is_empty()
            || value
                .to_string()
                .to_lowercase()
                .contains(&query.to_lowercase())
        {
            found.push(value);
        }
    }
    found.sort_by_key(|v| {
        if v["effective_status"] == "verified" {
            0
        } else {
            1
        }
    });
    found.truncate(limit.clamp(1, 20));
    let failures = e
        .runtime
        .repository
        .data
        .lock()
        .failures
        .iter()
        .filter(|f| {
            f["app"]
                == e.context
                    .app
                    .android_package
                    .as_ref()
                    .map(|a| a.as_str())
                    .unwrap_or("")
        })
        .rev()
        .take(3)
        .cloned()
        .collect::<Vec<_>>();
    Ok(json!({"guides":found,"local_failures_untrusted":failures}))
}
pub async fn execute(e: &Execution, name: &str, args: Value) -> Result<Value> {
    e.check()?;
    ensure!(NAMES.contains(&name), "未知工具");
    ensure!(args.to_string().len() <= 16 * 1024, "工具参数过大");
    match name {
        "observe" => {
            let o = e.observe().await?;
            Ok(
                json!({"observation_id":o.id,"size":o.size,"model_size":o.model_size,"at":o.at,"hash":o.hash}),
            )
        }
        "act" => {
            let operation = required(&args, "operation_id")?;
            ensure!(
                operation.len() <= 100 && !operation.chars().any(char::is_control),
                "operation_id 无效"
            );
            let descriptor = store::hash(&serde_json::to_vec(
                &json!({"action":args["action"],"consumption":args["consumption"],"expected":args["expected"]}),
            )?);
            if let Some(previous) = e.runtime.repository.data.lock().sessions[&e.session]
                .operations
                .get(operation)
                .cloned()
            {
                ensure!(
                    previous["descriptor"] == descriptor,
                    "operation_id 被用于不同动作"
                );
                return Ok(previous);
            }
            let o = e
                .valid_observation(required(&args, "observation_id")?)
                .await?;
            required(&args, "expected")?;
            let consumption: Consumption = serde_json::from_value(args["consumption"].clone())?;
            let s = e.runtime.repository.data.lock().sessions[&e.session].clone();
            if s.account.is_some()
                && !matches!(
                    consumption.category,
                    store::Category::Navigation | store::Category::RegenerativeResource
                )
            {
                let reply=e.infer(json!({"mode":"verify_consumption_identity","observation_id":o.id,"account_confirmed":s.account,"cycle_confirmed":s.cycle,"action":args["action"],"consumption_claim":consumption,"instruction":"按当前画面核实账号、周期和确切消耗，返回 act arguments={account_consistent:boolean,cycle_consistent:boolean,consumption:{category,resource,quantity,purpose,evidence}}。无法从画面确认就返回 false，不能用记忆确认。"}).to_string(),&o,"identity_gate").await?;
                ensure!(
                    reply.decision.tool == "act"
                        && reply.decision.arguments["account_consistent"] == true
                        && (s.cycle.is_none()
                            || reply.decision.arguments["cycle_consistent"] == true),
                    "account_unverified: 请打开账号信息并重新观察"
                );
                let actual: Consumption =
                    serde_json::from_value(reply.decision.arguments["consumption"].clone())?;
                ensure!(
                    actual.category == consumption.category
                        && actual.resource == consumption.resource
                        && actual.quantity == consumption.quantity
                        && actual.purpose == consumption.purpose,
                    "consumption_changed: 当前画面消耗已变化"
                );
                e.valid_observation(&o.id).await?;
            }
            if !e.runtime.repository.gate(
                &e.session,
                operation,
                &o.id,
                consumption,
                e.runtime.clock.now().timestamp(),
            )? {
                e.event(
                    "blocked",
                    json!({"operation_id":operation,"observation_id":o.id}),
                )?;
                return Ok(
                    json!({"blocked":true,"reason":"approval_required","instruction":"完成其他允许部分后 finish 汇总"}),
                );
            }
            e.runtime.repository.transaction(|data|{data.sessions.get_mut(&e.session).unwrap().operations.insert(operation.into(),json!({"operation_id":operation,"descriptor":descriptor,"status":"outcome_unknown","observation_id":o.id}));Ok(())})?;
            e.event("action",json!({"operation_id":operation,"action":args["action"],"observation_id":o.id,"expected":args["expected"]}))?;
            let result = crate::core::input_ownership::scope(
                e.permit.clone(),
                crate::core::input_ownership::operation(
                    e.context.device_id().as_str(),
                    e.runtime
                        .backend
                        .inject(&e.context.app, &args["action"], o.size),
                ),
            )
            .await;
            let status = if result.is_ok() {
                "injected"
            } else {
                "outcome_unknown"
            };
            let value = json!({"operation_id":operation,"descriptor":descriptor,"status":status,"observation_id":o.id});
            e.runtime.repository.transaction(|data| {
                data.sessions
                    .get_mut(&e.session)
                    .unwrap()
                    .operations
                    .insert(operation.into(), value.clone());
                for spend in data
                    .spends
                    .iter_mut()
                    .filter(|s| s.session == e.session && s.operation == operation)
                {
                    spend.status = status.into();
                }
                Ok(())
            })?;
            *e.observation.lock() = None;
            result?;
            Ok(value)
        }
        "wait" => {
            let ms = args["duration_ms"].as_u64().context("duration_ms 缺失")?;
            ensure!(ms <= 5000, "等待超限");
            tokio::select! {_=tokio::time::sleep(Duration::from_millis(ms))=>{},_=e.cancelled()=>anyhow::bail!("CANCELLED: 等待取消")};
            e.check()?;
            Ok(json!({"waited_ms":ms}))
        }
        "search_guides" => e.search(required(&args, "query")?).await,
        "read_guide" | "memory.read" => {
            let path = guide_path(&args)?;
            let value = read_memory(e, path)?;
            if value["effective_status"] == "candidate" {
                if let Some(o) = e.observation.lock().clone() {
                    let seen = e.runtime.repository.data.lock().sessions[&e.session]
                        .events
                        .iter()
                        .any(|event| {
                            event.kind == "memory_candidate"
                                && event.data["path"] == path
                                && event.data["hash"] == value["content_hash"]
                        });
                    if !seen {
                        e.event(
                            "memory_candidate",
                            json!({"path":path,"hash":value["content_hash"],"observation_id":o.id}),
                        )?;
                    }
                }
            }
            Ok(value)
        }
        "memory.search" => memory_search(
            e,
            args["query"].as_str().unwrap_or(""),
            args["limit"].as_u64().unwrap_or(5) as usize,
        ),
        "memory.propose" => {
            let o = e
                .valid_observation(required(&args, "observation_id")?)
                .await?;
            let mut content = required(&args, "content")?.to_string();
            let session = e.runtime.repository.data.lock().sessions[&e.session].clone();
            if let Some(account) = &session.account {
                content = content.replace(account, "[账号已脱敏]");
            }
            let redact = |text: &str| {
                session
                    .account
                    .as_ref()
                    .map(|account| text.replace(account, "[账号已脱敏]"))
                    .unwrap_or_else(|| text.to_string())
            };
            let sources = args["sources"].as_array().cloned().unwrap_or_default();
            ensure!(sources.len() <= 10, "来源过多");
            fn urls(value: &Value, found: &mut Vec<String>) {
                match value {
                    Value::Object(values) => {
                        for (k, v) in values {
                            if ["url", "uri"].contains(&k.as_str()) {
                                if let Some(url) = v.as_str() {
                                    found.push(url.into());
                                }
                            } else {
                                urls(v, found);
                            }
                        }
                    }
                    Value::Array(values) => {
                        for v in values {
                            urls(v, found);
                        }
                    }
                    _ => {}
                }
            }
            let mut known = Vec::new();
            for event in &session.events {
                if event.kind == "search" {
                    urls(&event.data["result"], &mut known);
                } else if event.kind == "decision" {
                    urls(&event.data["sources"], &mut known);
                }
            }
            for source in &sources {
                let url = reqwest::Url::parse(source.as_str().context("来源必须是 URL")?)?;
                ensure!(
                    (url.scheme() == "https" || url.scheme() == "http")
                        && url.username().is_empty()
                        && url.password().is_none()
                        && known.iter().any(|known| known == source.as_str().unwrap()),
                    "来源必须来自本次实际搜索结果，不能自行编造 URL"
                );
            }
            let path = format!("guides/{}.json", store::id());
            let value = json!({"schema":1,"app":session.app,"title":redact(required(&args,"title")?),"content":content,"conditions":redact(required(&args,"conditions")?),"sources":sources,"recorded_at":e.runtime.clock.now(),"supersedes":args["supersedes"],"author_status":"candidate"});
            let entry = e.runtime.packages.write_text(
                &package(e)?,
                super::ID,
                &path,
                &serde_json::to_string_pretty(&value)?,
                None,
                false,
            )?;
            e.event("memory_candidate",json!({"path":path,"hash":store::hash(entry.content.as_bytes()),"observation_id":o.id}))?;
            Ok(json!({"path":path,"status":"candidate"}))
        }
        "list_automations" => {
            let files = e.runtime.packages.list(
                &package(e)?,
                crate::extensions::gamer_yaml::YAML_EXTENSION_ID,
                "automations",
            )?;
            Ok(
                json!({"automations":files.iter().filter(|f|f.path.ends_with(".yaml")&&!crate::extensions::gamer_yaml::resources::is_function_library_path(f.path.trim_start_matches("automations/"))).map(|f|json!({"entrypoint":format!("{}/{}",package(e).unwrap(),f.path.trim_start_matches("automations/")),"version":f.version,"policy":"fresh_multimodal_check_per_side_effect"})).take(100).collect::<Vec<_>>(),"functions":crate::extensions::gamer_yaml::runner_adapter::compose_function_library(&e.runtime.packages,&package(e)?)?.iter().map(|(name,def)|json!({"entrypoint":format!("{}#{}",package(e).unwrap(),name),"params":def.call_params(name)})).take(100).collect::<Vec<_>>()}),
            )
        }
        "call_automation" => {
            let target = required(&args, "entrypoint")?;
            let payload = args["args"].as_object().cloned().unwrap_or_default();
            let policy = Arc::new(YamlPolicy {
                execution: e.clone(),
                pending: parking_lot::Mutex::new(None),
            });
            let scope = crate::core::side_effect::Scope {
                policy,
                input: e.permit.clone(),
            };
            e.runtime
                .backend
                .automation(&e.context, target, payload, e.stop.clone(), scope)
                .await?;
            *e.observation.lock() = None;
            Ok(json!({"executed":true,"verified":false,"instruction":"Observe and verify result"}))
        }
        "request_approval" => {
            let o = e
                .valid_observation(required(&args, "observation_id")?)
                .await?;
            let consumption: Consumption = serde_json::from_value(args["consumption"].clone())?;
            let allowed = e.runtime.repository.gate(
                &e.session,
                required(&args, "operation_id")?,
                &o.id,
                consumption,
                e.runtime.clock.now().timestamp(),
            )?;
            Ok(json!({"allowed":allowed,"blocked":!allowed}))
        }
        "finish" => {
            let goals = args["subgoals"]
                .as_array()
                .context("finish 必须列出全部子目标")?;
            ensure!(!goals.is_empty() && goals.len() <= 50, "子目标数量无效");
            let summary = required(&args, "summary")?;
            e.runtime.repository.transaction(|data| {
                data.sessions.get_mut(&e.session).unwrap().progress = args["subgoals"].clone();
                Ok(())
            })?;
            if goals.iter().any(|g| g["state"] != "completed")
                || e.runtime
                    .repository
                    .data
                    .lock()
                    .approvals
                    .values()
                    .any(|a| a.session == e.session && a.status == "pending")
            {
                anyhow::bail!("waiting_user: 部分完成；{summary}");
            }
            let prior = e.observation.lock().clone().context("完成必须有真实画面")?;
            ensure!(
                goals.iter().all(|g| g["evidence"] == prior.id
                    && !g["result"].as_str().unwrap_or("").trim().is_empty()),
                "finish 子目标缺少当前画面依据"
            );
            let fresh = e.observe().await?;
            let session = e.runtime.repository.data.lock().sessions[&e.session].clone();
            let candidates = session
                .events
                .iter()
                .rev()
                .filter(|v| v.kind == "memory_candidate")
                .filter_map(|v| {
                    let path = v.data["path"].as_str()?;
                    let guide = read_memory(e, path).ok()?;
                    if guide["content_hash"] != v.data["hash"] {
                        return None;
                    }
                    Some(json!({"path":path,"hash":v.data["hash"],"guide":guide}))
                })
                .take(5)
                .collect::<Vec<_>>();
            let reply = e.infer(json!({
                "mode":"verify_completion",
                "account_confirmed":session.account,
                "cycle_confirmed":session.cycle,
                "guide_candidates":candidates,
                "recent_execution_events":session.events.iter().rev().filter(|event|matches!(event.kind.as_str(), "action"|"yaml_side_effect"|"tool_result")).take(20).collect::<Vec<_>>(),
                "goal":session.goal,
                "claimed_subgoals":goals,
                "current_observation_id":fresh.id,
                "instruction":"仅根据新图核对全部子目标、账号是否变化及遗漏。返回 finish，arguments={verified:boolean,observation_id:当前ID,account_consistent:boolean,validated_guides:[确实在本次过程使用并验证的候选path],result:可见依据}。不允许以执行成功、攻略或历史完成记录替代画面。"
            }).to_string(), &fresh, "verification").await?;
            ensure!(
                reply.decision.tool == "finish"
                    && reply.decision.arguments["verified"] == true
                    && reply.decision.arguments["observation_id"] == fresh.id
                    && reply.decision.arguments["account_consistent"] == true
                    && !reply.decision.arguments["result"]
                        .as_str()
                        .unwrap_or("")
                        .is_empty(),
                "completion_unverified: 新画面未能验证目标完整完成"
            );
            e.event("completion_verified",json!({"observation_id":fresh.id,"summary":summary,"subgoals":goals,"result":reply.decision.arguments["result"]}))?;
            e.runtime.repository.transaction(|data|{let s=data.sessions[&e.session].clone();for candidate in candidates.iter().filter(|candidate|reply.decision.arguments["validated_guides"].as_array().is_some_and(|paths|paths.contains(&candidate["path"]))){data.verifications.push(Verification{resource_instance:resource_instance(e,&s.package,candidate["path"].as_str().unwrap())?,package:s.package.clone(),generation:s.generation.clone(),path:candidate["path"].as_str().unwrap().into(),hash:candidate["hash"].as_str().unwrap().into(),session:e.session.clone(),observation:fresh.id.clone(),conditions:"当前目标、应用及本次画面验证".into()});}data.completions.push(json!({"session":s.id,"account":s.account,"cycle":s.cycle,"goal":s.goal,"generation":s.generation,"observation_id":fresh.id}));Ok(())})?;
            Ok(json!({"completed":true,"observation_id":fresh.id}))
        }
        _ => anyhow::bail!("未知工具"),
    }
}
struct YamlPolicy {
    execution: Execution,
    pending: parking_lot::Mutex<Option<String>>,
}
#[async_trait]
impl crate::core::side_effect::Policy for YamlPolicy {
    async fn before(&self, context: &crate::core::AppContext, operation: Value) -> Result<()> {
        let e = &self.execution;
        ensure!(context == &e.context.app, "YAML parent context mismatch");
        let o = e.observe().await?;
        let identity = e.runtime.repository.data.lock().sessions[&e.session].clone();
        let reply=e.infer(json!({"mode":"check_yaml_side_effect","goal":e.runtime.repository.data.lock().sessions[&e.session].goal,"account_confirmed":identity.account,"cycle_confirmed":identity.cycle,"operation_in_original_pixels":operation,"observation_id":o.id,"original_size":o.size,"instruction":"根据当前图片判断这一个准确操作的消耗。返回 act，arguments={safe_coordinates:boolean,account_consistent:boolean,cycle_consistent:boolean,consumption:{category,resource,quantity,purpose,evidence}}。无法判断为 unknown；不能从函数名、攻略或脚本权限推导消费授权。"}).to_string(),&o,"yaml_gate").await?;
        ensure!(
            reply.decision.tool == "act" && reply.decision.arguments["safe_coordinates"] == true,
            "yaml_side_effect_blocked: 旧坐标或步骤不适用"
        );
        e.valid_observation(&o.id).await?;
        ensure!(
            identity.account.is_none() || reply.decision.arguments["account_consistent"] == true,
            "account_unverified: YAML 当前账号无法核实"
        );
        ensure!(
            identity.cycle.is_none() || reply.decision.arguments["cycle_consistent"] == true,
            "cycle_unverified: YAML 当前周期无法核实"
        );
        let c: Consumption =
            serde_json::from_value(reply.decision.arguments["consumption"].clone())?;
        let semantic = store::hash(&serde_json::to_vec(
            &json!({"kind":operation["kind"],"category":c.category,"resource":c.resource,"quantity":c.quantity,"purpose":c.purpose}),
        )?);
        let operation_id=e.runtime.repository.transaction(|data|{let s=data.sessions.get_mut(&e.session).unwrap();let id=s.operations.iter().find(|(_,v)|v["semantic"]==semantic&&v["status"]=="awaiting_approval").map(|(id,_)|id.clone()).unwrap_or_else(store::id);s.operations.insert(id.clone(),json!({"semantic":semantic,"status":"awaiting_approval","action":operation,"observation_id":o.id}));Ok(id)})?;
        ensure!(
            e.runtime.repository.gate(
                &e.session,
                &operation_id,
                &o.id,
                c,
                e.runtime.clock.now().timestamp()
            )?,
            "yaml_side_effect_blocked: 操作需要用户授权"
        );
        if operation["kind"] == "key" && operation["args"]["action"] == "down" {
            if let Some(key) = operation["key"].as_str() {
                e.keys.lock().push(key.into());
            }
        }
        e.runtime.repository.transaction(|data| {
            data.sessions
                .get_mut(&e.session)
                .unwrap()
                .operations
                .insert(
                    operation_id.clone(),
                    json!({"semantic":semantic,"status":"outcome_unknown","action":operation,"observation_id":o.id}),
                );
            Ok(())
        })?;
        *self.pending.lock() = Some(operation_id.clone());
        e.event(
            "yaml_side_effect",
            json!({"operation_id":operation_id,"action":operation,"observation_id":o.id}),
        )?;
        Ok(())
    }
    async fn after(&self, _context: &crate::core::AppContext, ok: bool) -> Result<()> {
        let Some(operation) = self.pending.lock().take() else {
            return Ok(());
        };
        let e = &self.execution;
        e.runtime.repository.transaction(|data| {
            data.sessions
                .get_mut(&e.session)
                .unwrap()
                .operations
                .get_mut(&operation)
                .unwrap()["status"] = json!(if ok { "injected" } else { "outcome_unknown" });
            for spend in data
                .spends
                .iter_mut()
                .filter(|s| s.session == e.session && s.operation == operation)
            {
                spend.status = if ok { "injected" } else { "outcome_unknown" }.into();
            }
            Ok(())
        })
    }
}
