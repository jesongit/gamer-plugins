//! Non-streaming, cancellable adapters with a single Responses-shaped history.
//! MCP images become real image inputs, never ordinary base64 text.
use super::settings::{validate_config, ConnectionConfig, SettingsConfig};
use anyhow::{ensure, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

const RESPONSE_BYTES_LIMIT: usize = 2 * 1024 * 1024;
const DEFAULT_OUTPUT_LIMIT: u32 = 2048;

#[derive(Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug)]
pub struct ModelTurn {
    pub items: Vec<Value>,
    pub calls: Vec<ToolCall>,
    pub text: String,
    pub usage: Option<Value>,
    pub request_attempts: u32,
}

pub struct Provider {
    config: ConnectionConfig,
    http: reqwest::Client,
    max_output_tokens: u32,
}

impl Provider {
    pub fn new(config: ConnectionConfig) -> Result<Self> {
        validate_config(&SettingsConfig {
            base_url: config.base_url.clone(),
            model: config.model.clone(),
            protocol: config.protocol.clone(),
            request_timeout_secs: config.request_timeout_secs,
        })?;
        ensure!(!config.api_key.is_empty(), "AI API 密钥未配置");
        let http = reqwest::Client::builder()
            // Never forward a credential to a redirect destination, even on a
            // supplier misconfiguration. Protocol changes are always explicit.
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(config.request_timeout_secs))
            .build()
            .context("无法创建 AI HTTP 客户端")?;
        Ok(Self {
            config,
            http,
            max_output_tokens: DEFAULT_OUTPUT_LIMIT,
        })
    }

    pub fn with_output_limit(mut self, limit: u32) -> Self {
        self.max_output_tokens = limit.clamp(128, 4096);
        self
    }

    pub async fn turn(
        &self,
        history: &[Value],
        tools: &[Value],
        cancel: &AtomicBool,
    ) -> Result<ModelTurn> {
        self.turn_with_choice(history, tools, None, cancel).await
    }

    async fn turn_with_choice(
        &self,
        history: &[Value],
        tools: &[Value],
        choice: Option<Value>,
        cancel: &AtomicBool,
    ) -> Result<ModelTurn> {
        ensure!(!cancel.load(Ordering::Acquire), "CANCELLED: AI 请求已取消");
        let (suffix, mut body) = if self.config.protocol == "responses" {
            (
                "responses",
                json!({"model":self.config.model,"input":responses_history(history)?,
                "tools":tools,"stream":false,"store":false,"max_output_tokens":self.max_output_tokens,
                "parallel_tool_calls":false}),
            )
        } else {
            let converted: Result<Vec<_>> = tools.iter().map(chat_tool).collect();
            (
                "chat/completions",
                json!({"model":self.config.model,"messages":chat_history(history)?,
                "tools":converted?,"stream":false,"max_tokens":self.max_output_tokens,
                "parallel_tool_calls":false}),
            )
        };
        if tools.is_empty() {
            body.as_object_mut().unwrap().remove("tools");
            body.as_object_mut().unwrap().remove("parallel_tool_calls");
        }
        if let Some(choice) = choice {
            body["tool_choice"] = if self.config.protocol == "chat_completions"
                && choice.get("type").and_then(Value::as_str) == Some("function")
            {
                json!({"type":"function","function":{"name":choice["name"]}})
            } else {
                choice
            };
        }
        let endpoint = format!("{}/{}", self.config.base_url.trim_end_matches('/'), suffix);
        // No automatic retries: a timeout or lost response can already have
        // consumed tokens. The executor counts each explicit turn against its
        // budget and can request another decision without replaying device input.
        let response = cancellable(
            self.http
                .post(endpoint)
                .bearer_auth(&self.config.api_key)
                .json(&body)
                .send(),
            cancel,
        )
        .await?
        .map_err(|error| transport_error(&error))?;
        let status = response.status();
        let bytes = read_limited(response, cancel).await?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("AI API 返回非 JSON 响应（HTTP {}）", status.as_u16()))?;
        if !status.is_success() {
            let message = value
                .pointer("/error/message")
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("供应商拒绝请求");
            let safe: String = message
                .replace(&self.config.api_key, "[redacted]")
                .chars()
                .take(512)
                .collect();
            anyhow::bail!("AI API HTTP {}: {}（未自动重试）", status.as_u16(), safe);
        }
        ensure!(!cancel.load(Ordering::Acquire), "CANCELLED: AI 请求已取消");
        let mut turn = if self.config.protocol == "responses" {
            parse_responses(value)?
        } else {
            parse_chat(value)?
        };
        turn.request_attempts = 1;
        Ok(turn)
    }

    /// Tests real visual input and a tool-result image with synthetic pixels.
    /// No target, screenshot capability or device input is used.
    pub async fn probe(&self, cancel: &AtomicBool) -> Result<Value> {
        let checked_at = chrono::Utc::now().to_rfc3339();
        let mut checks = Vec::new();
        let mut usages = Vec::new();
        let result = self.probe_steps(cancel, &mut checks, &mut usages).await;
        if cancel.load(Ordering::Acquire) {
            anyhow::bail!("CANCELLED: AI 能力测试已取消")
        }
        let error = result.err().map(|error| {
            let safe: String = error
                .to_string()
                .replace(&self.config.api_key, "[redacted]")
                .chars()
                .take(1024)
                .collect();
            safe
        });
        Ok(
            json!({"ok":error.is_none(),"protocol":self.config.protocol,"model":self.config.model,
            "checked_at":checked_at,"checks":checks,"usage":usages,"error":error}),
        )
    }

    async fn probe_steps(
        &self,
        cancel: &AtomicBool,
        checks: &mut Vec<Value>,
        usages: &mut Vec<Value>,
    ) -> Result<()> {
        let nonce = format!("READY_{}", uuid::Uuid::new_v4().simple());
        let ready = self.turn(&[json!({"role":"user","content":[{"type":"input_text",
            "text":format!("This is a connection test. Reply with exactly {nonce} and nothing else.")} ]})], &[], cancel).await;
        let ready = record_check("model", ready, checks, usages)?;
        if ready.text.trim() != nonce {
            return fail_check("model", "模型未返回连接测试标记", checks);
        }
        let colors = [
            ("red", [235, 15, 15]),
            ("green", [10, 210, 20]),
            ("blue", [10, 20, 235]),
            ("yellow", [230, 230, 10]),
        ];
        let index = rand::random::<u32>() as usize % colors.len();
        let (first_color, first_rgb) = colors[index];
        let (next_color, next_rgb) = colors[(index + 1) % colors.len()];
        let first_image = synthetic_image(first_rgb)?;
        let vision_history = vec![json!({"role":"user","content":[
            {"type":"input_text","text":"Inspect the attached synthetic image. Reply only its dominant color: red, green, blue, or yellow."},
            {"type":"input_image","image_url":first_image}
        ]})];
        let vision = self.turn(&vision_history, &[], cancel).await;
        let vision = record_check("image_input", vision, checks, usages)?;
        if !color_reply(&vision.text, first_color) {
            return fail_check("image_input", "模型未正确识别合成图片颜色", checks);
        }
        let tools = vec![json!({"type":"function","name":"probe_observe",
            "description":"Non-device test. Report the image's dominant color and the user's marker.",
            "parameters":{"type":"object","properties":{
                "color":{"type":"string","enum":["red","green","blue","yellow"]},
                "marker":{"type":"string"}},"required":["color","marker"],"additionalProperties":false}})];
        let marker = uuid::Uuid::new_v4().simple().to_string();
        let mut history = vec![json!({"role":"user","content":[
            {"type":"input_text","text":format!("Call probe_observe exactly once with the attached image's dominant color and marker {marker}. After the tool replies with another image, report only that NEW image's dominant color.")},
            {"type":"input_image","image_url":first_image}
        ]})];
        let called = self
            .turn_with_choice(
                &history,
                &tools,
                Some(json!({"type":"function","name":"probe_observe"})),
                cancel,
            )
            .await;
        let called = record_check("function_calling", called, checks, usages)?;
        if called.calls.len() != 1
            || called.calls[0].name != "probe_observe"
            || called.calls[0].arguments["color"].as_str() != Some(first_color)
            || called.calls[0].arguments["marker"].as_str() != Some(&marker)
        {
            return fail_check(
                "function_calling",
                "模型未生成有效且基于图片的测试函数调用",
                checks,
            );
        }
        let call_id = called.calls[0].id.clone();
        history.extend(called.items);
        let next_image = synthetic_image(next_rgb)?;
        let data = next_image.strip_prefix("data:image/png;base64,").unwrap();
        history.push(json!({"type":"function_call_output","call_id":call_id,"output":{
            "content":[{"type":"text","text":"Tool succeeded. Inspect the NEW image returned by this tool. Reply only its dominant color."},
                {"type":"image","data":data,"mimeType":"image/png"}],"isError":false}}));
        let followup = self
            .turn_with_choice(&history, &tools, Some(json!("none")), cancel)
            .await;
        let followup = record_check("tool_image_feedback", followup, checks, usages)?;
        if !followup.calls.is_empty() || !color_reply(&followup.text, next_color) {
            return fail_check(
                "tool_image_feedback",
                "模型未正确识别工具回传的新图片",
                checks,
            );
        }
        checks.push(json!({"name":"tool_result_roundtrip","ok":true}));
        Ok(())
    }
}

fn color_reply(text: &str, expected: &str) -> bool {
    let normalized = text
        .trim()
        .trim_matches(|c: char| c.is_ascii_punctuation())
        .to_lowercase();
    normalized == expected
}

fn record_check(
    name: &str,
    result: Result<ModelTurn>,
    checks: &mut Vec<Value>,
    usages: &mut Vec<Value>,
) -> Result<ModelTurn> {
    match result {
        Ok(turn) => {
            checks.push(json!({"name":name,"ok":true}));
            usages.push(turn.usage.clone().unwrap_or(Value::Null));
            Ok(turn)
        }
        Err(error) => {
            checks.push(json!({"name":name,"ok":false,"error":error.to_string()}));
            Err(error)
        }
    }
}

fn fail_check<T>(name: &str, message: &str, checks: &mut [Value]) -> Result<T> {
    if let Some(check) = checks.iter_mut().rev().find(|check| check["name"] == name) {
        check["ok"] = json!(false);
        check["error"] = json!(message);
    }
    anyhow::bail!("{}: {}", name, message)
}

fn synthetic_image(rgb: [u8; 3]) -> Result<String> {
    let image = image::RgbImage::from_pixel(96, 96, image::Rgb(rgb));
    let mut cursor = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image).write_to(&mut cursor, image::ImageFormat::Png)?;
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(cursor.into_inner())
    ))
}

async fn cancellable<F: Future>(future: F, cancel: &AtomicBool) -> Result<F::Output> {
    tokio::pin!(future);
    loop {
        ensure!(!cancel.load(Ordering::Acquire), "CANCELLED: AI 请求已取消");
        tokio::select! {
            output = &mut future => return Ok(output),
            _ = tokio::time::sleep(Duration::from_millis(25)) => {},
        }
    }
}

async fn read_limited(mut response: reqwest::Response, cancel: &AtomicBool) -> Result<Vec<u8>> {
    ensure!(
        response.content_length().unwrap_or(0) <= RESPONSE_BYTES_LIMIT as u64,
        "AI API 响应超过大小限制"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = cancellable(response.chunk(), cancel)
        .await?
        .map_err(|error| transport_error(&error))?
    {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= RESPONSE_BYTES_LIMIT,
            "AI API 响应超过大小限制"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn transport_error(error: &reqwest::Error) -> anyhow::Error {
    // reqwest's Display can include URLs. Keep supplier diagnostics separate
    // and never include headers, response bodies or the connection credential.
    let reason = if error.is_timeout() {
        "请求超时"
    } else if error.is_connect() {
        "无法连接"
    } else if error.is_body() {
        "读取响应失败"
    } else {
        "网络请求失败"
    };
    anyhow::anyhow!("AI API {}（未自动重试）", reason)
}

fn tool_output(output: &Value, call_id: &str) -> Result<(String, Vec<Value>)> {
    if let Some(text) = output.as_str() {
        return Ok((text.to_string(), vec![]));
    }
    let content = output
        .as_array()
        .or_else(|| output.get("content").and_then(Value::as_array));
    let Some(content) = content else {
        return Ok((output.to_string(), vec![]));
    };
    let mut text = Vec::new();
    let mut images = Vec::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text" | "input_text" | "output_text") => {
                text.push(
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .context("工具文本结果无效")?
                        .to_string(),
                );
            }
            Some("image") => {
                let mime = block
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .context("工具图片缺少 mimeType")?;
                ensure!(
                    matches!(mime, "image/png" | "image/jpeg" | "image/webp"),
                    "工具图片格式不支持"
                );
                let data = block
                    .get("data")
                    .and_then(Value::as_str)
                    .context("工具图片缺少 data")?;
                ensure!(data.len() <= 8 * 1024 * 1024, "工具图片超过大小限制");
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .context("工具图片 Base64 无效")?;
                images.push(
                    json!({"type":"input_image","image_url":format!("data:{mime};base64,{data}")}),
                );
            }
            Some("input_image") => {
                images.push(block.clone());
            }
            _ => anyhow::bail!("工具返回不支持的内容类型"),
        }
    }
    if output.get("isError").and_then(Value::as_bool) == Some(true) {
        text.insert(
            0,
            "Tool execution failed; no action should be inferred as successful.".into(),
        );
    }
    if let Some(structured) = output.get("structuredContent") {
        text.push(structured.to_string());
    }
    if !images.is_empty() {
        images.insert(0, json!({"type":"input_text",
            "text":format!("Observation images returned by tool call {call_id}; these belong to its preceding tool result.")}));
    }
    Ok((text.join("\n"), images))
}

fn responses_history(history: &[Value]) -> Result<Vec<Value>> {
    let mut result = Vec::new();
    let mut pending_images = Vec::new();
    for item in history {
        if item.get("type").and_then(Value::as_str) == Some("function_call_output") {
            let id = item
                .get("call_id")
                .and_then(Value::as_str)
                .context("工具结果缺少 call_id")?;
            let (text, images) =
                tool_output(item.get("output").context("工具结果缺少 output")?, id)?;
            result.push(json!({"type":"function_call_output","call_id":id,"output":text}));
            pending_images.extend(images);
        } else {
            flush_images(&mut result, &mut pending_images);
            result.push(item.clone());
        }
    }
    flush_images(&mut result, &mut pending_images);
    Ok(result)
}

fn flush_images(result: &mut Vec<Value>, pending_images: &mut Vec<Value>) {
    if !pending_images.is_empty() {
        result.push(json!({"role":"user","content":std::mem::take(pending_images)}));
    }
}

fn chat_tool(tool: &Value) -> Result<Value> {
    ensure!(
        tool.get("type").and_then(Value::as_str) == Some("function"),
        "仅支持函数工具"
    );
    let mut function = serde_json::Map::new();
    for field in ["name", "description", "parameters", "strict"] {
        if let Some(value) = tool.get(field) {
            function.insert(field.to_string(), value.clone());
        }
    }
    ensure!(
        function.get("name").is_some_and(Value::is_string)
            && function.get("parameters").is_some_and(Value::is_object),
        "函数工具定义无效"
    );
    Ok(json!({"type":"function","function":function}))
}

fn chat_history(history: &[Value]) -> Result<Vec<Value>> {
    let normalized = responses_history(history)?;
    let mut result = Vec::new();
    let mut assistant_text = Vec::new();
    let mut assistant_calls = Vec::new();
    for item in normalized {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                let call = parse_call(&item)?;
                assistant_calls.push(json!({"id":call.id,"type":"function",
                    "function":{"name":call.name,"arguments":call.arguments.to_string()}}));
            }
            Some("function_call_output") => {
                flush_assistant(&mut result, &mut assistant_text, &mut assistant_calls);
                result.push(
                    json!({"role":"tool","tool_call_id":item["call_id"],"content":item["output"]}),
                );
            }
            Some("reasoning") => {}
            _ => {
                let role = item
                    .get("role")
                    .and_then(Value::as_str)
                    .context("AI 历史消息缺少 role")?;
                ensure!(
                    matches!(role, "user" | "assistant" | "system" | "developer"),
                    "AI 历史消息角色无效"
                );
                let content = item.get("content").context("AI 历史消息缺少 content")?;
                if role == "assistant" {
                    assistant_text.push(message_text(content)?);
                } else {
                    flush_assistant(&mut result, &mut assistant_text, &mut assistant_calls);
                    let role = if role == "developer" { "system" } else { role };
                    result.push(json!({"role":role,"content":chat_content(content)?}));
                }
            }
        }
    }
    flush_assistant(&mut result, &mut assistant_text, &mut assistant_calls);
    Ok(result)
}

fn flush_assistant(result: &mut Vec<Value>, texts: &mut Vec<String>, calls: &mut Vec<Value>) {
    if texts.is_empty() && calls.is_empty() {
        return;
    }
    let text = std::mem::take(texts).join("\n");
    let mut message = json!({"role":"assistant","content":if text.is_empty() { Value::Null } else { json!(text) }});
    if !calls.is_empty() {
        message["tool_calls"] = json!(std::mem::take(calls));
    }
    result.push(message);
}

fn chat_content(content: &Value) -> Result<Value> {
    if content.is_string() {
        return Ok(content.clone());
    }
    let blocks = content.as_array().context("AI 消息内容必须为文本或数组")?;
    let mut result = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("input_text" | "output_text" | "text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .context("AI 消息文本无效")?;
                result.push(json!({"type":"text","text":text}));
            }
            Some("input_image") => {
                let url = block
                    .get("image_url")
                    .and_then(Value::as_str)
                    .context("AI 消息图片无效")?;
                let mut image = json!({"url":url});
                if let Some(detail) = block.get("detail") {
                    image["detail"] = detail.clone();
                }
                result.push(json!({"type":"image_url","image_url":image}));
            }
            _ => anyhow::bail!("AI 消息包含不支持的内容类型"),
        }
    }
    Ok(json!(result))
}

fn message_text(content: &Value) -> Result<String> {
    if let Some(text) = content.as_str() {
        return Ok(text.to_string());
    }
    let blocks = content.as_array().context("模型回答 content 无效")?;
    let mut result = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("output_text" | "input_text" | "text") => {
                result.push(
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .context("模型回答文本无效")?
                        .to_string(),
                );
            }
            Some("refusal") => anyhow::bail!("模型拒绝本次请求"),
            _ => anyhow::bail!("模型回答内容类型不支持"),
        }
    }
    Ok(result.join("\n"))
}

fn parse_call(item: &Value) -> Result<ToolCall> {
    let id = item
        .get("call_id")
        .and_then(Value::as_str)
        .context("模型工具调用缺少 call_id")?;
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .context("模型工具调用缺少 name")?;
    ensure!(!id.is_empty() && id.len() <= 256, "模型工具调用 ID 无效");
    ensure!(
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.')),
        "模型工具调用名称无效"
    );
    let raw = item
        .get("arguments")
        .context("模型工具调用缺少 arguments")?;
    let arguments = if let Some(text) = raw.as_str() {
        serde_json::from_str(text)
            .map_err(|_| anyhow::anyhow!("模型工具调用 arguments 不是有效 JSON"))?
    } else {
        raw.clone()
    };
    ensure!(arguments.is_object(), "模型工具调用 arguments 必须为对象");
    Ok(ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments,
    })
}

fn parse_responses(value: Value) -> Result<ModelTurn> {
    if value.get("error").is_some_and(|error| !error.is_null()) {
        anyhow::bail!("AI Responses 返回错误");
    }
    if let Some(status) = value.get("status").and_then(Value::as_str) {
        ensure!(status == "completed", "AI Responses 未完整结束：{}", status);
    }
    let output = value
        .get("output")
        .and_then(Value::as_array)
        .context("AI Responses 缺少 output 数组")?;
    let mut items = Vec::new();
    let mut calls = Vec::new();
    let mut texts = Vec::new();
    let mut seen = HashSet::new();
    for item in output {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                ensure!(
                    item.get("status")
                        .and_then(Value::as_str)
                        .is_none_or(|status| status == "completed"),
                    "模型函数调用未完整结束"
                );
                let call = parse_call(item)?;
                ensure!(seen.insert(call.id.clone()), "模型工具调用 ID 重复");
                items.push(
                    json!({"type":"function_call","call_id":call.id,"name":call.name,
                    "arguments":call.arguments.to_string()}),
                );
                calls.push(call);
            }
            Some("message") => {
                ensure!(
                    item.get("role").and_then(Value::as_str) == Some("assistant"),
                    "模型返回消息角色无效"
                );
                let text = message_text(item.get("content").context("模型回答缺少 content")?)?;
                if !text.is_empty() {
                    texts.push(text.clone());
                    items.push(json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}));
                }
            }
            // Hidden reasoning is intentionally not logged or requested. Public
            // assistant text and function calls are enough for this V1 adapter.
            Some("reasoning") => {}
            _ => anyhow::bail!("AI Responses 返回不支持的 output 类型"),
        }
    }
    ensure!(!items.is_empty(), "模型未返回回答或工具调用");
    let usage = value
        .get("usage")
        .filter(|value| value.is_object())
        .cloned();
    Ok(ModelTurn {
        items,
        calls,
        text: texts.join("\n"),
        usage,
        request_attempts: 1,
    })
}

fn parse_chat(value: Value) -> Result<ModelTurn> {
    let choices = value
        .get("choices")
        .and_then(Value::as_array)
        .context("AI Chat Completions 缺少 choices")?;
    ensure!(
        choices.len() == 1,
        "AI Chat Completions 必须返回一个 choice"
    );
    let choice = &choices[0];
    ensure!(
        matches!(
            choice.get("finish_reason").and_then(Value::as_str),
            Some("stop" | "tool_calls")
        ),
        "AI Chat Completions 未完整结束"
    );
    let message = choice
        .get("message")
        .context("AI Chat Completions 缺少 message")?;
    ensure!(
        message.get("role").and_then(Value::as_str) == Some("assistant"),
        "模型返回消息角色无效"
    );
    ensure!(
        !message.get("refusal").is_some_and(|value| !value.is_null()),
        "模型拒绝本次请求"
    );
    let text = match message.get("content") {
        None | Some(Value::Null) => String::new(),
        Some(content) => message_text(content)?,
    };
    let mut items = Vec::new();
    if !text.is_empty() {
        items.push(json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}));
    }
    let mut calls = Vec::new();
    let mut seen = HashSet::new();
    if let Some(raw) = message.get("tool_calls") {
        for item in raw.as_array().context("模型 tool_calls 必须是数组")? {
            ensure!(
                item.get("type").and_then(Value::as_str) == Some("function"),
                "模型调用类型不支持"
            );
            let function = item.get("function").context("模型调用缺少 function")?;
            let normalized = json!({"type":"function_call","call_id":item["id"],
                "name":function["name"],"arguments":function["arguments"]});
            let call = parse_call(&normalized)?;
            ensure!(seen.insert(call.id.clone()), "模型工具调用 ID 重复");
            items.push(
                json!({"type":"function_call","call_id":call.id,"name":call.name,
                "arguments":call.arguments.to_string()}),
            );
            calls.push(call);
        }
    }
    ensure!(!items.is_empty(), "模型未返回回答或工具调用");
    Ok(ModelTurn {
        items,
        calls,
        text,
        usage: value
            .get("usage")
            .filter(|value| value.is_object())
            .cloned(),
        request_attempts: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_images_are_real_visual_input_in_both_protocols() {
        let history = vec![
            json!({"type":"function_call","call_id":"call-1","name":"screen_capture","arguments":"{}"}),
            json!({"type":"function_call_output","call_id":"call-1","output":[
                {"type":"text","text":"Frame 7"},{"type":"image","mimeType":"image/png","data":"AQID"}]} ),
        ];
        let responses = responses_history(&history).unwrap();
        assert_eq!(responses[1]["output"], "Frame 7");
        assert!(!responses[1].to_string().contains("AQID"));
        assert_eq!(responses[2]["content"][1]["type"], "input_image");
        assert_eq!(
            responses[2]["content"][1]["image_url"],
            "data:image/png;base64,AQID"
        );
        let chat = chat_history(&history).unwrap();
        assert_eq!(chat[0]["tool_calls"][0]["id"], "call-1");
        assert_eq!(chat[1]["role"], "tool");
        assert_eq!(chat[1]["tool_call_id"], "call-1");
        assert_eq!(chat[2]["content"][1]["type"], "image_url");
        assert_eq!(
            chat[2]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AQID"
        );
    }

    #[test]
    fn chat_preserves_assistant_tool_calls_and_defers_images_until_all_outputs() {
        let history = vec![
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"Observe"}]}),
            json!({"type":"function_call","call_id":"a","name":"screen_capture","arguments":"{}"}),
            json!({"type":"function_call","call_id":"b","name":"context_get","arguments":"{}"}),
            json!({"type":"function_call_output","call_id":"a","output":{"content":[{"type":"image","mimeType":"image/png","data":"AQID"}]}}),
            json!({"type":"function_call_output","call_id":"b","output":"ready"}),
        ];
        let chat = chat_history(&history).unwrap();
        assert_eq!(chat.len(), 4);
        assert_eq!(chat[0]["content"], "Observe");
        assert_eq!(chat[0]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(chat[1]["role"], "tool");
        assert_eq!(chat[2]["role"], "tool");
        assert_eq!(chat[3]["role"], "user");
    }

    #[test]
    fn refuses_partial_calls_duplicate_ids_and_preserves_unknown_usage() {
        let raw = json!({"status":"completed","output":[{"type":"function_call","call_id":"a",
            "name":"input_tap","arguments":"{\"x\":3,\"y\":4}"}]});
        let turn = parse_responses(raw.clone()).unwrap();
        assert!(turn.usage.is_none());
        assert_eq!(turn.calls[0].arguments["x"], 3);
        let mut incomplete = raw.clone();
        incomplete["status"] = json!("incomplete");
        assert!(parse_responses(incomplete).is_err());
        let mut duplicate = raw.clone();
        duplicate["output"]
            .as_array_mut()
            .unwrap()
            .push(raw["output"][0].clone());
        assert!(parse_responses(duplicate).is_err());
        let mut broken = raw;
        broken["output"][0]["arguments"] = json!("{\"x\":");
        assert!(parse_responses(broken).is_err());
        assert!(parse_chat(json!({"choices":[{"finish_reason":"length","message":{"role":"assistant",
            "tool_calls":[{"id":"a","type":"function","function":{"name":"input_tap","arguments":"{}"}}]}}]})).is_err());
    }

    #[tokio::test]
    async fn cancel_interrupts_waiting_for_headers_and_partial_body() {
        use tokio::io::AsyncWriteExt;
        for partial_body in [false, true] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                // TCP may split headers and body across reads; consume the complete request.
                let request = read_request(&mut stream).await;
                assert_eq!(request["model"], "test");
                if partial_body {
                    stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1000\r\n\r\n{").await.unwrap();
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            });
            let provider = Provider::new(ConnectionConfig {
                base_url: format!("http://{address}/v1"),
                model: "test".into(),
                protocol: "responses".into(),
                request_timeout_secs: 5,
                api_key: "test-only".into(),
            })
            .unwrap();
            let cancelled = AtomicBool::new(false);
            let started = tokio::time::Instant::now();
            let trigger = async {
                tokio::time::sleep(Duration::from_millis(80)).await;
                cancelled.store(true, Ordering::Release);
            };
            let request = provider.turn(&[], &[], &cancelled);
            let (result, _) = tokio::join!(request, trigger);
            assert!(result.unwrap_err().to_string().contains("CANCELLED"));
            assert!(started.elapsed() < Duration::from_secs(1));
            server.abort();
        }
    }

    async fn read_request(stream: &mut tokio::net::TcpStream) -> Value {
        use tokio::io::AsyncReadExt;
        let mut bytes = Vec::new();
        let (body_start, body_length) = loop {
            let mut buffer = [0; 4096];
            let length = stream.read(&mut buffer).await.unwrap();
            assert!(length > 0);
            bytes.extend_from_slice(&buffer[..length]);
            if let Some(start) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&bytes[..start]).unwrap();
                assert!(headers
                    .to_ascii_lowercase()
                    .contains("authorization: bearer test-only"));
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                break (start + 4, length);
            }
            assert!(bytes.len() < 64 * 1024);
        };
        while bytes.len() - body_start < body_length {
            let mut buffer = [0; 4096];
            let length = stream.read(&mut buffer).await.unwrap();
            assert!(length > 0);
            bytes.extend_from_slice(&buffer[..length]);
        }
        serde_json::from_slice(&bytes[body_start..body_start + body_length]).unwrap()
    }

    fn observed_color(messages: &[Value], chat: bool) -> &'static str {
        let image_url = messages
            .iter()
            .rev()
            .filter_map(|message| message["content"].as_array())
            .flat_map(|content| content.iter().rev())
            .find_map(|block| {
                if chat {
                    block.pointer("/image_url/url").and_then(Value::as_str)
                } else if block["type"] == "input_image" {
                    block["image_url"].as_str()
                } else {
                    None
                }
            })
            .expect("model must receive a real image input");
        let encoded = image_url.strip_prefix("data:image/png;base64,").unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let image = image::load_from_memory(&bytes).unwrap().to_rgb8();
        match image.get_pixel(0, 0).0 {
            [235, 15, 15] => "red",
            [10, 210, 20] => "green",
            [10, 20, 235] => "blue",
            [230, 230, 10] => "yellow",
            _ => panic!("unexpected synthetic pixels"),
        }
    }

    #[tokio::test]
    async fn connection_probe_covers_images_calling_and_tool_image_feedback_over_http() {
        use tokio::io::AsyncWriteExt;
        for protocol in ["responses", "chat_completions"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let chat = protocol == "chat_completions";
            let server = tokio::spawn(async move {
                let mut initial_color = String::new();
                for step in 0..4 {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let request = read_request(&mut stream).await;
                    assert_eq!(request["stream"], false);
                    assert_eq!(
                        request[if chat {
                            "max_tokens"
                        } else {
                            "max_output_tokens"
                        }],
                        2048
                    );
                    let messages = request[if chat { "messages" } else { "input" }]
                        .as_array()
                        .unwrap();
                    let prompt = messages[0]["content"][0]["text"].as_str().unwrap();
                    let mut calls = Vec::new();
                    let text = match step {
                        0 => prompt
                            .split("exactly ")
                            .nth(1)
                            .unwrap()
                            .split_whitespace()
                            .next()
                            .unwrap()
                            .to_string(),
                        1 => {
                            initial_color = observed_color(messages, chat).to_string();
                            initial_color.clone()
                        }
                        2 => {
                            let color = observed_color(messages, chat);
                            let marker = prompt
                                .split("marker ")
                                .nth(1)
                                .unwrap()
                                .split('.')
                                .next()
                                .unwrap();
                            assert_eq!(color, initial_color);
                            let arguments = json!({"color":color,"marker":marker}).to_string();
                            if chat {
                                assert_eq!(
                                    request["tool_choice"]["function"]["name"],
                                    "probe_observe"
                                );
                                calls.push(json!({"id":"probe-call","type":"function","function":{
                                    "name":"probe_observe","arguments":arguments}}));
                            } else {
                                assert_eq!(request["tool_choice"]["name"], "probe_observe");
                                calls.push(json!({"type":"function_call","call_id":"probe-call",
                                    "name":"probe_observe","arguments":arguments,"status":"completed"}));
                            }
                            String::new()
                        }
                        3 => {
                            assert_eq!(request["tool_choice"], "none");
                            let tool = messages
                                .iter()
                                .find(|item| {
                                    if chat {
                                        item["role"] == "tool"
                                    } else {
                                        item["type"] == "function_call_output"
                                    }
                                })
                                .unwrap();
                            assert_eq!(
                                tool[if chat { "tool_call_id" } else { "call_id" }],
                                "probe-call"
                            );
                            assert!(!tool.to_string().contains("base64"));
                            let color = observed_color(messages, chat);
                            assert_ne!(color, initial_color, "tool feedback must be a new image");
                            color.to_string()
                        }
                        _ => unreachable!(),
                    };
                    let response = if chat {
                        let mut message = json!({"role":"assistant","content":text});
                        if !calls.is_empty() {
                            message["tool_calls"] = json!(calls);
                        }
                        json!({"choices":[{"finish_reason":if step == 2 {"tool_calls"} else {"stop"},"message":message}],
                            "usage":{"prompt_tokens":12,"completion_tokens":4,"total_tokens":16}})
                    } else {
                        let output = if calls.is_empty() {
                            vec![
                                json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}),
                            ]
                        } else {
                            calls
                        };
                        json!({"status":"completed","output":output,"usage":{"input_tokens":12,"output_tokens":4,"total_tokens":16}})
                    };
                    let bytes = response.to_string();
                    let headers = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len());
                    stream.write_all(headers.as_bytes()).await.unwrap();
                    stream.write_all(bytes.as_bytes()).await.unwrap();
                }
            });
            let provider = Provider::new(ConnectionConfig {
                base_url: format!("http://{address}/v1"),
                model: "mock-vision".into(),
                protocol: protocol.into(),
                request_timeout_secs: 5,
                api_key: "test-only".into(),
            })
            .unwrap();
            let result = provider.probe(&AtomicBool::new(false)).await.unwrap();
            server.await.unwrap();
            assert_eq!(result["ok"], true, "{result}");
            assert_eq!(result["checks"].as_array().unwrap().len(), 5);
            assert_eq!(result["usage"].as_array().unwrap().len(), 4);
        }
    }
}
