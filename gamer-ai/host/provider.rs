//! Cancellable JSON/SSE adapters with a single Responses-shaped history.
//! MCP images become real image inputs, never ordinary base64 text.
use super::settings::{validate_config, ConnectionConfig, SettingsConfig};
use anyhow::{ensure, Context, Result};
use base64::Engine;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

const RESPONSE_BYTES_LIMIT: usize = 2 * 1024 * 1024;
// SSE has an envelope around each delta; wire bytes are not decoded text bytes.
// Keep ordinary JSON and any individual SSE line/event bounded separately.
const STREAM_BYTES_LIMIT: usize = 32 * 1024 * 1024;
const SSE_EVENT_BYTES_LIMIT: usize = 2 * 1024 * 1024;

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
    /// Explicitly public summary_text only; never raw reasoning or encrypted content.
    pub summary: Vec<String>,
    pub usage: Option<Value>,
    pub request_attempts: u32,
    pub diagnostics: Value,
}

/// Only supplier-published output. Responses reasoning_text is accepted only
/// from the documented official GLM endpoint; encrypted content is never emitted.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelStreamEvent {
    RequestSnapshot { snapshot: Value },
    TextDelta { delta: String },
    SummaryDelta { delta: String },
    ThinkingDelta { delta: String },
    Usage { usage: Value },
    Diagnostics { diagnostics: Value },
}

#[derive(Debug)]
struct ApiFailure {
    code: &'static str,
    http_status: Option<u16>,
    detail: String,
    retryable: bool,
    usage: Option<Value>,
    request_id: Option<String>,
    diagnostics: Value,
}
impl std::fmt::Display for ApiFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}
impl std::error::Error for ApiFailure {}
fn api_failure(
    code: &'static str,
    http_status: Option<u16>,
    detail: impl Into<String>,
    retryable: bool,
) -> anyhow::Error {
    ApiFailure {
        code,
        http_status,
        detail: detail.into(),
        retryable,
        usage: None,
        request_id: None,
        diagnostics: Value::Null,
    }
    .into()
}
pub(super) fn error_details(error: &anyhow::Error) -> Value {
    if let Some(failure) = error.downcast_ref::<ApiFailure>() {
        json!({"code":failure.code,"http_status":failure.http_status,"detail":failure.detail,"retryable":failure.retryable,
            "request_id":failure.request_id,"diagnostics":failure.diagnostics})
    } else {
        json!({"code":"model_response_error","detail":error.to_string(),"retryable":true})
    }
}
pub(super) fn error_usage(error: &anyhow::Error) -> Option<&Value> {
    error
        .downcast_ref::<ApiFailure>()
        .and_then(|failure| failure.usage.as_ref())
}
pub(super) fn interrupted_error(
    code: &'static str,
    detail: &str,
    usage: Option<Value>,
    diagnostics: Value,
) -> anyhow::Error {
    ApiFailure {
        code,
        detail: detail.into(),
        retryable: false,
        http_status: diagnostics["http_status"].as_u64().map(|v| v as u16),
        request_id: diagnostics["request_id"].as_str().map(str::to_owned),
        usage,
        diagnostics,
    }
    .into()
}
fn redact_error(error: anyhow::Error, key: &str) -> anyhow::Error {
    let message = if key.is_empty() {
        error.to_string()
    } else {
        error.to_string().replace(key, "[redacted]")
    };
    let safe: String = message.chars().take(1000).collect();
    if let Some(failure) = error.downcast_ref::<ApiFailure>() {
        let mut redacted = ApiFailure {
            code: failure.code,
            http_status: failure.http_status,
            detail: safe,
            retryable: failure.retryable,
            usage: failure.usage.clone().map(|value| redact_value(value, key)),
            request_id: failure.request_id.clone().map(|id| redact_string(&id, key)),
            diagnostics: redact_value(failure.diagnostics.clone(), key),
        };
        redacted.detail = redact_string(&redacted.detail, key);
        redacted.into()
    } else {
        api_failure("model_response_error", None, safe, true)
    }
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
            public_reasoning_content: config.public_reasoning_content,
            max_output_tokens: config.max_output_tokens,
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
        let max_output_tokens = config.max_output_tokens;
        Ok(Self {
            config,
            http,
            max_output_tokens,
        })
    }

    pub async fn turn(
        &self,
        history: &[Value],
        tools: &[Value],
        cancel: &AtomicBool,
    ) -> Result<ModelTurn> {
        self.turn_with_choice(history, tools, None, cancel).await
    }

    /// Real incremental SSE. Tool fragments are never callable: only a terminal
    /// response, with complete JSON arguments, can produce ModelTurn::calls.
    pub async fn turn_stream<F: FnMut(ModelStreamEvent)>(
        &self,
        history: &[Value],
        tools: &[Value],
        cancel: &AtomicBool,
        mut on_event: F,
    ) -> Result<ModelTurn> {
        ensure!(!cancel.load(Ordering::Acquire), "CANCELLED: AI 请求已取消");
        let (suffix, mut body) = self.request_body(history, tools, None, true)?;
        // Chat usage is delivered separately after the last choice, when the
        // configured compatible supplier implements this standard option.
        if self.config.protocol == "chat_completions" {
            body["stream_options"] = json!({"include_usage":true});
        }
        // The request has now been fully converted to the chosen wire protocol.
        // Emit a sanitized display copy before dispatch, including requests that
        // fail before the supplier returns any diagnostics or output.
        on_event(ModelStreamEvent::RequestSnapshot {
            snapshot: super::prompts::sanitize(
                &json!({
                    "protocol":self.config.protocol,"model":self.config.model,
                    "endpoint":format!("{}/{}",self.config.base_url.trim_end_matches('/'),suffix),
                    "request_body":body,"redactions":["credentials","image_payloads","typed_text","encrypted_reasoning"],
                    "capture":"before_http_dispatch","headers_included":false
                }),
                &[&self.config.api_key],
            ),
        });
        let started = std::time::Instant::now();
        let response = cancellable(
            self.http
                .post(format!(
                    "{}/{}",
                    self.config.base_url.trim_end_matches('/'),
                    suffix
                ))
                .bearer_auth(&self.config.api_key)
                .header("Accept", "text/event-stream")
                .json(&body)
                .send(),
            cancel,
        )
        .await?
        .map_err(|error| transport_error(&error))?;
        let status = response.status();
        let request_id = response_request_id(response.headers())
            .map(|id| redact_string(&id, &self.config.api_key));
        let mut diagnostics = json!({"protocol":self.config.protocol,"model":self.config.model,
            "http_status":status.as_u16(),"request_id":request_id,"streamed":true,"request_attempts":1});
        on_event(ModelStreamEvent::Diagnostics {
            diagnostics: diagnostics.clone(),
        });
        let mut state = StreamState::new(self.config.protocol == "responses", &self.config.api_key);
        state.public_reasoning_content = self.config.public_reasoning_content;
        state.published_responses_reasoning = self.published_responses_reasoning();
        let result = self
            .consume_stream(response, cancel, &mut state, &mut on_event)
            .await;
        // Flush safe suffixes even on cancellation; this contains only complete
        // public text already received, never speculative function arguments.
        state.flush(&mut on_event);
        diagnostics["elapsed_ms"] =
            json!(started.elapsed().as_millis().min(u64::MAX as u128) as u64);
        diagnostics["streamed"] = json!(state.streamed);
        if let Some(id) = &state.response_id {
            diagnostics["response_id"] = json!(redact_string(id, &self.config.api_key));
        }
        if let Some(usage) = &state.usage {
            on_event(ModelStreamEvent::Usage {
                usage: redact_value(usage.clone(), &self.config.api_key),
            });
        }
        on_event(ModelStreamEvent::Diagnostics {
            diagnostics: diagnostics.clone(),
        });
        let mut turn = result.map_err(|error| {
            failure_metadata(
                redact_error(error, &self.config.api_key),
                state
                    .usage
                    .clone()
                    .map(|value| redact_value(value, &self.config.api_key)),
                request_id.clone(),
                diagnostics.clone(),
            )
        })?;
        turn.diagnostics = diagnostics;
        turn.request_attempts = 1;
        Ok(turn)
    }

    fn request_body(
        &self,
        history: &[Value],
        tools: &[Value],
        choice: Option<Value>,
        stream: bool,
    ) -> Result<(&'static str, Value)> {
        let (suffix, mut body) = if self.config.protocol == "responses" {
            (
                "responses",
                json!({"model":self.config.model,"input":responses_history(history)?,
                "tools":tools,"stream":stream,"store":false,"parallel_tool_calls":false}),
            )
        } else {
            let converted: Result<Vec<_>> = tools.iter().map(chat_tool).collect();
            (
                "chat/completions",
                json!({"model":self.config.model,"messages":chat_history(history)?,
                "tools":converted?,"stream":stream,"parallel_tool_calls":false}),
            )
        };
        if self.max_output_tokens > 0 {
            let key = if self.config.protocol == "responses" {
                "max_output_tokens"
            } else {
                "max_tokens"
            };
            body[key] = json!(self.max_output_tokens);
        }
        // GLM documents public thinking for Chat Completions. Its Responses
        // API already thinks by default and has no reasoning.summary option.
        if self.config.protocol == "chat_completions"
            && self.config.public_reasoning_content
            && official_glm_endpoint(
                &self.config.base_url,
                &self.config.model,
                "chat_completions",
            )
            && ["glm-5", "glm-4.5", "glm-4.6", "glm-4.7"]
                .iter()
                .any(|prefix| self.config.model.to_ascii_lowercase().starts_with(*prefix))
        {
            body["thinking"] = json!({"type":"enabled"});
        }
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
        Ok((suffix, body))
    }

    fn published_responses_reasoning(&self) -> bool {
        self.config.public_reasoning_content
            && official_glm_endpoint(&self.config.base_url, &self.config.model, "responses")
    }

    async fn consume_stream<F: FnMut(ModelStreamEvent)>(
        &self,
        mut response: reqwest::Response,
        cancel: &AtomicBool,
        state: &mut StreamState,
        on_event: &mut F,
    ) -> Result<ModelTurn> {
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|header| header.to_str().ok())
            .unwrap_or("");
        if !status.is_success() || !content_type.starts_with("text/event-stream") {
            state.streamed = false;
            let bytes = read_limited(response, cancel).await?;
            let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
                api_failure(
                    "invalid_response",
                    Some(status.as_u16()),
                    format!("AI API 返回非 JSON/SSE 响应（HTTP {}）", status.as_u16()),
                    status.is_server_error(),
                )
            })?;
            state.usage = value
                .get("usage")
                .filter(|value| value.is_object())
                .cloned();
            state.response_id = value.get("id").and_then(Value::as_str).map(str::to_string);
            if !status.is_success() {
                let message = value
                    .pointer("/error/message")
                    .or_else(|| value.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("供应商拒绝请求");
                return Err(api_failure(
                    "http_error",
                    Some(status.as_u16()),
                    format!("AI API HTTP {}: {}（未自动重试）", status.as_u16(), message),
                    status.is_server_error() || status.as_u16() == 429,
                ));
            }
            // Some compatible suppliers return complete JSON despite stream.
            // Preserve their explicitly configured protocol; do not reconnect.
            if state.responses && state.published_responses_reasoning {
                state.publish_response_reasoning(&value, on_event);
            }
            if !state.responses && state.public_reasoning_content {
                if let Some(thinking) = value
                    .pointer("/choices/0/message/reasoning_content")
                    .and_then(Value::as_str)
                {
                    on_event(ModelStreamEvent::ThinkingDelta {
                        delta: redact_string(thinking, &self.config.api_key),
                    });
                }
            }
            let mut turn = if state.responses {
                parse_responses(redact_value(value, &self.config.api_key))?
            } else {
                parse_chat(redact_value(value, &self.config.api_key))?
            };
            if !turn.text.is_empty() {
                on_event(ModelStreamEvent::TextDelta {
                    delta: turn.text.clone(),
                });
            }
            for delta in &turn.summary {
                on_event(ModelStreamEvent::SummaryDelta {
                    delta: delta.clone(),
                });
            }
            turn.diagnostics = json!({"streamed":false});
            return Ok(turn);
        }
        let mut decoder = SseDecoder::default();
        let mut total = 0usize;
        while let Some(chunk) = cancellable(response.chunk(), cancel)
            .await?
            .map_err(|error| transport_error(&error))?
        {
            total = total.saturating_add(chunk.len());
            ensure!(
                total <= STREAM_BYTES_LIMIT,
                "AI API 流响应超过 32 MiB 大小限制"
            );
            for event in decoder.push(&chunk)? {
                if state.event(event, on_event)? {
                    return state.finish();
                }
            }
        }
        for event in decoder.finish()? {
            if state.event(event, on_event)? {
                return state.finish();
            }
        }
        state.finish()
    }

    async fn turn_with_choice(
        &self,
        history: &[Value],
        tools: &[Value],
        choice: Option<Value>,
        cancel: &AtomicBool,
    ) -> Result<ModelTurn> {
        ensure!(!cancel.load(Ordering::Acquire), "CANCELLED: AI 请求已取消");
        let (suffix, body) = self.request_body(history, tools, choice, false)?;
        let started = std::time::Instant::now();
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
        let request_id = response_request_id(response.headers())
            .map(|id| redact_string(&id, &self.config.api_key));
        let bytes = read_limited(response, cancel).await?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            api_failure(
                "invalid_response",
                Some(status.as_u16()),
                format!("AI API 返回非 JSON 响应（HTTP {}）", status.as_u16()),
                status.is_server_error() || status.is_success(),
            )
        })?;
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
            return Err(failure_metadata(
                api_failure(
                    "http_error",
                    Some(status.as_u16()),
                    format!("AI API HTTP {}: {}（未自动重试）", status.as_u16(), safe),
                    status.is_server_error() || status.as_u16() == 429,
                ),
                value
                    .get("usage")
                    .filter(|usage| usage.is_object())
                    .cloned()
                    .map(|v| redact_value(v, &self.config.api_key)),
                request_id,
                json!({"protocol":self.config.protocol,"model":self.config.model,"http_status":status.as_u16()}),
            ));
        }
        ensure!(!cancel.load(Ordering::Acquire), "CANCELLED: AI 请求已取消");
        let usage = value
            .get("usage")
            .filter(|usage| usage.is_object())
            .cloned();
        let parsed = if self.config.protocol == "responses" {
            parse_responses(redact_value(value, &self.config.api_key))
        } else {
            parse_chat(redact_value(value, &self.config.api_key))
        };
        let diagnostics = json!({"protocol":self.config.protocol,"model":self.config.model,"http_status":status.as_u16(),
            "request_id":request_id,"streamed":false,"elapsed_ms":started.elapsed().as_millis().min(u64::MAX as u128) as u64});
        let mut turn = parsed.map_err(|error| {
            failure_metadata(
                redact_error(error, &self.config.api_key),
                usage.map(|value| redact_value(value, &self.config.api_key)),
                request_id.clone(),
                diagnostics.clone(),
            )
        })?;
        turn.request_attempts = 1;
        turn.diagnostics = diagnostics;
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

fn redact_string(value: &str, key: &str) -> String {
    if key.is_empty() {
        value.to_string()
    } else {
        value.replace(key, "[redacted]")
    }
}
fn redact_value(value: Value, key: &str) -> Value {
    match value {
        Value::String(text) => Value::String(redact_string(&text, key)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|item| redact_value(item, key))
                .collect(),
        ),
        Value::Object(items) => Value::Object(
            items
                .into_iter()
                .map(|(name, value)| (redact_string(&name, key), redact_value(value, key)))
                .collect(),
        ),
        other => other,
    }
}
fn response_request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    ["x-request-id", "request-id", "cf-ray"]
        .iter()
        .find_map(|name| {
            headers
                .get(*name)
                .and_then(|header| header.to_str().ok())
                .map(|id| id.chars().take(256).collect())
        })
}
fn failure_metadata(
    error: anyhow::Error,
    usage: Option<Value>,
    request_id: Option<String>,
    diagnostics: Value,
) -> anyhow::Error {
    let mut failure = match error.downcast::<ApiFailure>() {
        Ok(failure) => failure,
        Err(error) => ApiFailure {
            code: if error.to_string().contains("CANCELLED:") {
                "cancelled"
            } else {
                "model_response_error"
            },
            http_status: None,
            detail: error.to_string(),
            retryable: true,
            usage: None,
            request_id: None,
            diagnostics: Value::Null,
        },
    };
    if usage.is_some() {
        failure.usage = usage;
    }
    failure.request_id = request_id;
    failure.diagnostics = diagnostics;
    failure.into()
}

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
    name: String,
    data: Vec<String>,
    skip_lf: bool,
    started: bool,
    event_bytes: usize,
}
struct SseEvent {
    name: String,
    data: String,
}
impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>> {
        let mut result = Vec::new();
        for byte in bytes {
            if self.skip_lf {
                self.skip_lf = false;
                if *byte == b'\n' {
                    continue;
                }
            }
            if matches!(*byte, b'\r' | b'\n') {
                let line = std::mem::take(&mut self.buffer);
                self.line(&line, &mut result)?;
                self.skip_lf = *byte == b'\r';
            } else {
                ensure!(
                    self.buffer.len() < SSE_EVENT_BYTES_LIMIT,
                    "AI SSE 单行超过 2 MiB 大小限制"
                );
                self.buffer.push(*byte);
            }
        }
        Ok(result)
    }
    fn line(&mut self, line: &[u8], result: &mut Vec<SseEvent>) -> Result<()> {
        self.event_bytes = self.event_bytes.saturating_add(line.len());
        ensure!(
            self.event_bytes <= SSE_EVENT_BYTES_LIMIT,
            "AI SSE 单事件超过 2 MiB 大小限制"
        );
        let line = if !self.started {
            self.started = true;
            line.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(line)
        } else {
            line
        };
        let line = std::str::from_utf8(line).context("AI SSE 包含无效 UTF-8")?;
        if line.is_empty() {
            self.event_bytes = 0;
            if !self.data.is_empty() {
                result.push(SseEvent {
                    name: std::mem::take(&mut self.name),
                    data: std::mem::take(&mut self.data).join("\n"),
                });
            } else {
                self.name.clear();
            }
        } else if !line.starts_with(':') {
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => self.name = value.to_string(),
                "data" => self.data.push(value.to_string()),
                _ => {}
            }
        }
        Ok(())
    }
    fn finish(&mut self) -> Result<Vec<SseEvent>> {
        let mut result = Vec::new();
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            self.line(&line, &mut result)?;
        }
        self.line(b"", &mut result)?;
        Ok(result)
    }
}

struct DeltaRedactor {
    key: String,
    pending: String,
}
impl DeltaRedactor {
    fn new(key: &str) -> Self {
        Self {
            key: key.into(),
            pending: String::new(),
        }
    }
    fn push(&mut self, delta: &str) -> String {
        self.pending.push_str(delta);
        self.pending = redact_string(&self.pending, &self.key);
        if self.key.is_empty() {
            return std::mem::take(&mut self.pending);
        }
        // Retain only a suffix which could be the start of a split credential.
        let keep = self
            .pending
            .char_indices()
            .filter_map(|(start, _)| {
                let suffix = &self.pending[start..];
                (suffix.len() < self.key.len() && self.key.starts_with(suffix)).then_some(start)
            })
            .next()
            .unwrap_or(self.pending.len());
        let tail = self.pending.split_off(keep);
        std::mem::replace(&mut self.pending, tail)
    }
    fn finish(&mut self) -> String {
        redact_string(&std::mem::take(&mut self.pending), &self.key)
    }
}
#[derive(Default)]
struct ChatCallDelta {
    id: String,
    name: String,
    arguments: String,
}

/// The meaning of reasoning_text is supplier-specific. GLM explicitly publishes
/// this output in its Responses schema; an OpenAI-compatible URL alone is not
/// evidence that raw reasoning from another supplier is public.
fn official_glm_endpoint(base_url: &str, model: &str, protocol: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return false;
    };
    if url.scheme() != "https"
        || url.host_str() != Some("open.bigmodel.cn")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !model.to_ascii_lowercase().starts_with("glm-")
    {
        return false;
    }
    match protocol {
        "responses" => url.path().trim_end_matches('/') == "/api/v1",
        "chat_completions" => matches!(
            url.path().trim_end_matches('/'),
            "/api/paas/v4" | "/api/coding/paas/v4"
        ),
        _ => false,
    }
}

fn published_glm_reasoning(value: &Value) -> Vec<(usize, String)> {
    let Some(output) = value.get("output").and_then(Value::as_array) else {
        return Vec::new();
    };
    output
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            if item.get("type").and_then(Value::as_str) != Some("reasoning") {
                return None;
            }
            let content = item.get("content")?;
            let parts: Vec<&Value> = if let Some(parts) = content.as_array() {
                parts.iter().collect()
            } else if content.is_object() {
                vec![content]
            } else {
                return None;
            };
            let text = parts
                .iter()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("reasoning_text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some((index, text))
        })
        .collect()
}

struct StreamState {
    responses: bool,
    streamed: bool,
    key: String,
    final_response: Option<Value>,
    response_id: Option<String>,
    usage: Option<Value>,
    text: String,
    summary: String,
    calls: BTreeMap<usize, ChatCallDelta>,
    finish_reason: Option<String>,
    role_seen: bool,
    text_redactor: DeltaRedactor,
    summary_redactor: DeltaRedactor,
    thinking_redactor: DeltaRedactor,
    public_reasoning_content: bool,
    published_responses_reasoning: bool,
    published_reasoning_by_index: BTreeMap<usize, String>,
}
impl StreamState {
    fn new(responses: bool, key: &str) -> Self {
        Self {
            responses,
            streamed: true,
            key: key.into(),
            final_response: None,
            response_id: None,
            usage: None,
            text: String::new(),
            summary: String::new(),
            calls: BTreeMap::new(),
            finish_reason: None,
            role_seen: false,
            text_redactor: DeltaRedactor::new(key),
            summary_redactor: DeltaRedactor::new(key),
            thinking_redactor: DeltaRedactor::new(key),
            public_reasoning_content: false,
            published_responses_reasoning: false,
            published_reasoning_by_index: BTreeMap::new(),
        }
    }

    fn published_reasoning_delta<F: FnMut(ModelStreamEvent)>(
        &mut self,
        index: usize,
        delta: &str,
        on_event: &mut F,
    ) {
        self.published_reasoning_by_index
            .entry(index)
            .or_default()
            .push_str(delta);
        self.delta("thinking", delta, on_event);
    }

    fn published_reasoning_done<F: FnMut(ModelStreamEvent)>(
        &mut self,
        index: usize,
        text: &str,
        on_event: &mut F,
    ) {
        let previous = self
            .published_reasoning_by_index
            .get(&index)
            .map(String::as_str)
            .unwrap_or("");
        // Done/completed may repeat deltas. Only publish a missing suffix, and
        // never invent a replacement when the supplier gives inconsistent text.
        if let Some(suffix) = text.strip_prefix(previous) {
            let suffix = suffix.to_string();
            self.published_reasoning_delta(index, &suffix, on_event);
        }
    }

    fn publish_response_reasoning<F: FnMut(ModelStreamEvent)>(
        &mut self,
        response: &Value,
        on_event: &mut F,
    ) {
        for (index, text) in published_glm_reasoning(response) {
            self.published_reasoning_done(index, &text, on_event);
        }
    }
    fn delta<F: FnMut(ModelStreamEvent)>(&mut self, channel: &str, delta: &str, on_event: &mut F) {
        let safe = match channel {
            "summary" => self.summary_redactor.push(delta),
            "thinking" => self.thinking_redactor.push(delta),
            _ => self.text_redactor.push(delta),
        };
        if safe.is_empty() {
            return;
        }
        on_event(match channel {
            "summary" => ModelStreamEvent::SummaryDelta { delta: safe },
            "thinking" => ModelStreamEvent::ThinkingDelta { delta: safe },
            _ => ModelStreamEvent::TextDelta { delta: safe },
        });
    }
    fn flush<F: FnMut(ModelStreamEvent)>(&mut self, on_event: &mut F) {
        for (channel, redactor) in [
            ("text", &mut self.text_redactor),
            ("summary", &mut self.summary_redactor),
            ("thinking", &mut self.thinking_redactor),
        ] {
            let safe = redactor.finish();
            if safe.is_empty() {
                continue;
            }
            on_event(match channel {
                "summary" => ModelStreamEvent::SummaryDelta { delta: safe },
                "thinking" => ModelStreamEvent::ThinkingDelta { delta: safe },
                _ => ModelStreamEvent::TextDelta { delta: safe },
            });
        }
    }
    fn event<F: FnMut(ModelStreamEvent)>(
        &mut self,
        event: SseEvent,
        on_event: &mut F,
    ) -> Result<bool> {
        if event.data.trim() == "[DONE]" {
            return Ok(true);
        }
        if event.data.trim().is_empty() {
            return Ok(false);
        }
        let value: Value =
            serde_json::from_str(&event.data).context("AI SSE data 不是有效 JSON")?;
        if let Some(usage) = value.get("usage").filter(|usage| usage.is_object()) {
            self.usage = Some(usage.clone());
            on_event(ModelStreamEvent::Usage {
                usage: redact_value(usage.clone(), &self.key),
            });
        }
        if let Some(id) = value.get("id").and_then(Value::as_str) {
            self.response_id = Some(id.to_string());
        }
        if self.responses {
            let kind = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or(&event.name);
            if let Some(response) = value.get("response") {
                if let Some(id) = response.get("id").and_then(Value::as_str) {
                    self.response_id = Some(id.to_string());
                }
                if let Some(usage) = response.get("usage").filter(|usage| usage.is_object()) {
                    self.usage = Some(usage.clone());
                }
            }
            match kind {
                "response.output_text.delta" => self.delta(
                    "text",
                    value
                        .get("delta")
                        .and_then(Value::as_str)
                        .context("AI 文本 delta 无效")?,
                    on_event,
                ),
                "response.reasoning_summary_text.delta" => self.delta(
                    "summary",
                    value
                        .get("delta")
                        .and_then(Value::as_str)
                        .context("AI 公开摘要 delta 无效")?,
                    on_event,
                ),
                "response.reasoning_text.delta" if self.published_responses_reasoning => {
                    self.published_reasoning_delta(
                        value
                            .get("output_index")
                            .and_then(Value::as_u64)
                            .unwrap_or(0) as usize,
                        value
                            .get("delta")
                            .and_then(Value::as_str)
                            .context("GLM 公开思考 delta 无效")?,
                        on_event,
                    );
                }
                "response.reasoning_text.done" if self.published_responses_reasoning => {
                    self.published_reasoning_done(
                        value
                            .get("output_index")
                            .and_then(Value::as_u64)
                            .unwrap_or(0) as usize,
                        value
                            .get("text")
                            .and_then(Value::as_str)
                            .context("GLM 公开思考 text 无效")?,
                        on_event,
                    );
                }
                "response.thinking_text.delta"
                    if self.public_reasoning_content
                        && value.get("visibility").and_then(Value::as_str) == Some("public") =>
                {
                    self.delta(
                        "thinking",
                        value
                            .get("delta")
                            .and_then(Value::as_str)
                            .context("AI 公开 thinking delta 无效")?,
                        on_event,
                    )
                }
                "response.completed" | "response.incomplete" | "response.failed" => {
                    let response = value.get("response").context("AI SSE 终态缺少 response")?;
                    ensure!(
                        response.get("status").and_then(Value::as_str)
                            == kind.strip_prefix("response."),
                        "AI SSE 终态与 status 不一致"
                    );
                    if self.published_responses_reasoning {
                        self.publish_response_reasoning(response, on_event);
                    }
                    self.final_response = Some(response.clone());
                    return Ok(true);
                }
                "error" => {
                    return Err(api_failure(
                        "stream_error",
                        None,
                        value
                            .get("message")
                            .or_else(|| value.pointer("/error/message"))
                            .and_then(Value::as_str)
                            .unwrap_or("AI 流式响应失败"),
                        true,
                    ));
                }
                _ => {}
            }
        } else {
            if value.get("error").is_some_and(|error| !error.is_null()) {
                return Err(api_failure(
                    "stream_error",
                    None,
                    value
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("AI 流式响应失败"),
                    true,
                ));
            }
            let choices = value
                .get("choices")
                .and_then(Value::as_array)
                .context("AI Chat SSE 缺少 choices")?;
            ensure!(choices.len() <= 1, "AI Chat SSE 必须仅返回一个 choice");
            if let Some(choice) = choices.first() {
                let already_finished = self.finish_reason.is_some();
                ensure!(
                    choice
                        .get("index")
                        .and_then(Value::as_u64)
                        .is_none_or(|index| index == 0),
                    "AI Chat SSE choice index 无效"
                );
                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                    ensure!(self.finish_reason.is_none(), "AI Chat SSE 重复终态");
                    self.finish_reason = Some(reason.into());
                }
                if let Some(delta) = choice.get("delta") {
                    ensure!(
                        !already_finished
                            || delta.is_null()
                            || delta.as_object().is_some_and(|object| object.is_empty()),
                        "AI Chat SSE 在终态后修改回答或工具调用"
                    );
                    if let Some(role) = delta.get("role").and_then(Value::as_str) {
                        ensure!(role == "assistant", "AI Chat SSE 角色无效");
                        self.role_seen = true;
                    }
                    if let Some(content) = delta.get("content").filter(|content| !content.is_null())
                    {
                        let text = message_text(content)?;
                        self.text.push_str(&text);
                        self.delta("text", &text, on_event);
                    }
                    if let Some(summary) = delta.get("summary_text").and_then(Value::as_str) {
                        self.summary.push_str(summary);
                        self.delta("summary", summary, on_event);
                    }
                    if self.public_reasoning_content {
                        if let Some(thinking) = delta.get("public_thinking").and_then(Value::as_str)
                        {
                            self.delta("thinking", thinking, on_event);
                        }
                        if let Some(thinking) =
                            delta.get("reasoning_content").and_then(Value::as_str)
                        {
                            self.delta("thinking", thinking, on_event);
                        }
                    }
                    ensure!(
                        !delta
                            .get("refusal")
                            .is_some_and(|value| value.is_string()
                                && !value.as_str().unwrap_or("").is_empty()),
                        "模型拒绝本次请求"
                    );
                    if let Some(calls) = delta.get("tool_calls") {
                        for call in calls.as_array().context("AI Chat SSE tool_calls 无效")? {
                            let index = call
                                .get("index")
                                .and_then(Value::as_u64)
                                .context("AI Chat SSE tool index 缺失")?
                                as usize;
                            ensure!(index < 128, "AI Chat SSE 工具数量超限");
                            ensure!(
                                call.get("type")
                                    .and_then(Value::as_str)
                                    .is_none_or(|kind| kind == "function"),
                                "AI Chat SSE 工具类型无效"
                            );
                            let pending = self.calls.entry(index).or_default();
                            if let Some(id) = call.get("id").and_then(Value::as_str) {
                                ensure!(
                                    pending.id.is_empty() || pending.id == id,
                                    "AI Chat SSE tool id 改变"
                                );
                                pending.id = id.into();
                            }
                            if let Some(function) = call.get("function") {
                                if let Some(name) = function.get("name").and_then(Value::as_str) {
                                    pending.name.push_str(name);
                                }
                                if let Some(arguments) =
                                    function.get("arguments").and_then(Value::as_str)
                                {
                                    pending.arguments.push_str(arguments);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(false)
    }
    fn finish(&self) -> Result<ModelTurn> {
        if self.responses {
            let value = self.final_response.clone().ok_or_else(|| {
                api_failure(
                    "incomplete_response",
                    None,
                    "AI Responses SSE 在终态之前断开；本轮未执行未完成的操作",
                    true,
                )
            })?;
            parse_responses(redact_value(value, &self.key))
        } else {
            ensure!(self.role_seen, "AI Chat SSE 缺少 assistant 角色");
            let calls: Vec<_> = self
                .calls
                .values()
                .map(|call| {
                    json!({"id":call.id,"type":"function",
                "function":{"name":call.name,"arguments":call.arguments}})
                })
                .collect();
            let mut turn = parse_chat(redact_value(
                json!({"id":self.response_id,"choices":[{"finish_reason":self.finish_reason,
                "message":{"role":"assistant","content":self.text,"tool_calls":calls}}],"usage":self.usage}),
                &self.key,
            ))?;
            if !self.summary.trim().is_empty() {
                turn.summary.push(redact_string(&self.summary, &self.key));
            }
            Ok(turn)
        }
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
    let (code, reason) = if error.is_timeout() {
        ("request_timeout", "请求超时")
    } else if error.is_connect() {
        ("connection_failed", "无法连接")
    } else if error.is_body() {
        ("response_read_failed", "读取响应失败")
    } else {
        ("network_error", "网络请求失败")
    };
    api_failure(code, None, format!("AI API {}（未自动重试）", reason), true)
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
        // MCP commonly includes the same JSON in content.text and
        // structuredContent. Send that payload once, while preserving any
        // distinct explanation, error notice and observation images.
        if !text
            .iter()
            .any(|part| serde_json::from_str::<Value>(part).is_ok_and(|value| value == *structured))
        {
            text.push(structured.to_string());
        }
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
    let blocks: Vec<&Value> = if let Some(blocks) = content.as_array() {
        blocks.iter().collect()
    } else if content.is_object() {
        vec![content]
    } else {
        anyhow::bail!("模型回答 content 无效");
    };
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
        let message = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("供应商未提供错误详情");
        return Err(api_failure(
            "response_failed",
            None,
            format!(
                "AI Responses 返回错误：{}",
                message.chars().take(512).collect::<String>()
            ),
            true,
        ));
    }
    if let Some(status) = value.get("status").and_then(Value::as_str) {
        if status != "completed" {
            let reason = value
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str);
            let (code, detail, retryable) = match reason {
                Some("max_output_tokens") => (
                    "output_limit",
                    "模型输出达到本轮响应 token 上限，回答或工具调用未完整生成；本轮未执行这些操作"
                        .to_string(),
                    false,
                ),
                Some("content_filter") => (
                    "content_filter",
                    "供应商内容过滤导致回答未完整生成；请调整指令或场景".to_string(),
                    false,
                ),
                _ => (
                    "incomplete_response",
                    format!(
                        "AI Responses 未完整结束：{}；本轮未执行未完成的操作",
                        status.chars().take(64).collect::<String>()
                    ),
                    true,
                ),
            };
            return Err(api_failure(code, None, detail, retryable));
        }
    }
    let output = value
        .get("output")
        .and_then(Value::as_array)
        .context("AI Responses 缺少 output 数组")?;
    let mut items = Vec::new();
    let mut calls = Vec::new();
    let mut texts = Vec::new();
    let mut summary = Vec::new();
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
            Some("reasoning") => {
                // Suppliers may omit a public summary. Do not use content,
                // reasoning_content, encrypted_content, or invent a substitute.
                if let Some(parts) = item.get("summary").and_then(Value::as_array) {
                    for part in parts.iter().take(8) {
                        if part.get("type").and_then(Value::as_str) == Some("summary_text") {
                            if let Some(text) = part
                                .get("text")
                                .and_then(Value::as_str)
                                .filter(|text| !text.trim().is_empty())
                            {
                                if summary.len() < 8 {
                                    summary.push(text.chars().take(4000).collect());
                                }
                            }
                        }
                    }
                }
            }
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
        summary,
        usage,
        request_attempts: 1,
        diagnostics: Value::Null,
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
    let finish_reason = choice.get("finish_reason").and_then(Value::as_str);
    if !matches!(finish_reason, Some("stop" | "tool_calls")) {
        let (code, detail, retryable) = match finish_reason {
            Some("length") => (
                "output_limit",
                "模型输出达到本轮响应 token 上限，回答或工具调用未完整生成；本轮未执行这些操作",
                false,
            ),
            Some("content_filter") => (
                "content_filter",
                "供应商内容过滤导致回答未完整生成；请调整指令或场景",
                false,
            ),
            _ => (
                "incomplete_response",
                "AI Chat Completions 未完整结束；本轮未执行未完成的操作",
                true,
            ),
        };
        return Err(api_failure(code, None, detail, retryable));
    }
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
        summary: Vec::new(),
        usage: value
            .get("usage")
            .filter(|value| value.is_object())
            .cloned(),
        request_attempts: 1,
        diagnostics: Value::Null,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glm_provider(protocol: &str) -> Provider {
        Provider::new(ConnectionConfig {
            base_url: if protocol == "responses" {
                "https://open.bigmodel.cn/api/v1".into()
            } else {
                "https://open.bigmodel.cn/api/paas/v4".into()
            },
            model: "glm-5.3-flash".into(),
            protocol: protocol.into(),
            request_timeout_secs: 90,
            api_key: "test-only".into(),
            public_reasoning_content: true,
            max_output_tokens: super::super::settings::DEFAULT_MAX_OUTPUT_TOKENS,
        })
        .unwrap()
    }

    #[test]
    fn glm_published_reasoning_is_limited_to_the_documented_official_endpoint() {
        let provider = glm_provider("responses");
        assert!(provider.published_responses_reasoning());
        for (url, model) in [
            ("https://api.openai.com/v1", "glm-5.3-flash"),
            (
                "https://open.bigmodel.cn.evil.example/api/v1",
                "glm-5.3-flash",
            ),
            ("https://open.bigmodel.cn/api/other", "glm-5.3-flash"),
            ("https://open.bigmodel.cn:8443/api/v1", "glm-5.3-flash"),
            ("http://open.bigmodel.cn/api/v1", "glm-5.3-flash"),
            ("https://open.bigmodel.cn/api/v1", "gpt-5"),
        ] {
            assert!(!official_glm_endpoint(url, model, "responses"));
        }
        let mut provider = provider;
        provider.config.public_reasoning_content = false;
        assert!(!provider.published_responses_reasoning());
    }

    #[test]
    fn per_request_output_limit_is_configurable_and_zero_uses_supplier_default() {
        for protocol in ["responses", "chat_completions"] {
            let mut provider = glm_provider(protocol);
            let field = if protocol == "responses" {
                "max_output_tokens"
            } else {
                "max_tokens"
            };
            let (_, body) = provider.request_body(&[], &[], None, true).unwrap();
            assert_eq!(body[field], 16384);
            if protocol == "responses" {
                // GLM's schema only declares effort; summary is not supported.
                assert!(body.get("reasoning").is_none());
            } else {
                assert_eq!(body["thinking"]["type"], "enabled");
            }
            provider.max_output_tokens = 0;
            let (_, body) = provider.request_body(&[], &[], None, false).unwrap();
            assert!(body.get(field).is_none());
        }
    }

    #[test]
    fn glm_public_reasoning_streams_once_and_never_enters_model_history() {
        for enabled in [false, true] {
            let mut state = StreamState::new(true, "test-secret");
            state.published_responses_reasoning = enabled;
            let mut events = Vec::new();
            let mut emit = |event| events.push(event);
            for value in [
                json!({"type":"response.reasoning_text.delta","output_index":0,"delta":"先核对 test-"}),
                json!({"type":"response.reasoning_text.delta","output_index":0,"delta":"secret 画面。"}),
                json!({"type":"response.reasoning_text.done","output_index":0,"text":"先核对 test-secret 画面。"}),
                json!({"type":"response.completed","response":{"status":"completed","output":[
                    {"type":"reasoning","content":{"type":"reasoning_text","text":"先核对 test-secret 画面。"},"encrypted_content":"opaque-private"},
                    {"type":"message","role":"assistant","content":{"type":"output_text","text":"已经确认。"}}
                ]}}),
            ] {
                state
                    .event(
                        SseEvent {
                            name: String::new(),
                            data: value.to_string(),
                        },
                        &mut emit,
                    )
                    .unwrap();
            }
            state.flush(&mut emit);
            let thinking = events
                .iter()
                .filter_map(|event| {
                    if let ModelStreamEvent::ThinkingDelta { delta } = event {
                        Some(delta.as_str())
                    } else {
                        None
                    }
                })
                .collect::<String>();
            assert_eq!(
                thinking,
                if enabled {
                    "先核对 [redacted] 画面。"
                } else {
                    ""
                }
            );
            let turn = state.finish().unwrap();
            assert_eq!(turn.text, "已经确认。");
            assert!(turn.summary.is_empty());
            let history = serde_json::to_string(&turn.items).unwrap();
            assert!(!history.contains("先核对"));
            assert!(!history.contains("opaque-private"));
        }
    }

    #[test]
    fn glm_completed_reasoning_is_emitted_when_deltas_are_omitted() {
        let mut state = StreamState::new(true, "secret");
        state.published_responses_reasoning = true;
        let mut events = Vec::new();
        state.event(SseEvent { name: String::new(), data: json!({"type":"response.completed","response":{"status":"completed","output":[
            {"type":"reasoning","content":[{"type":"reasoning_text","text":"补发公开思考 secret"}]},
            {"type":"message","role":"assistant","content":{"type":"output_text","text":"完成"}}
        ]}}).to_string() }, &mut |event| events.push(event)).unwrap();
        state.flush(&mut |event| events.push(event));
        assert!(serde_json::to_string(&events)
            .unwrap()
            .contains("补发公开思考 [redacted]"));
        assert!(!serde_json::to_string(&state.finish().unwrap().items)
            .unwrap()
            .contains("补发"));
    }

    #[tokio::test]
    async fn glm_json_fallback_emits_public_reasoning_without_reconnecting_or_history_leak() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let fixture = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            let body = json!({"status":"completed","output":[
                {"type":"reasoning","content":{"type":"reasoning_text","text":"JSON公开思考 test-only"}},
                {"type":"message","role":"assistant","content":{"type":"output_text","text":"JSON回答"}}
            ]}).to_string();
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        let provider = fixture_provider(address, "responses");
        // The transport is a local fixture. The policy boundary is covered
        // separately; here the state simulates a verified official GLM request.
        let response = provider
            .http
            .post(format!("http://{address}/v1/responses"))
            .bearer_auth("test-only")
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        let mut state = StreamState::new(true, "test-only");
        state.published_responses_reasoning = true;
        let mut events = Vec::new();
        let turn = provider
            .consume_stream(
                response,
                &AtomicBool::new(false),
                &mut state,
                &mut |event| events.push(event),
            )
            .await
            .unwrap();
        state.flush(&mut |event| events.push(event));
        fixture.await.unwrap();
        assert_eq!(turn.text, "JSON回答");
        assert!(serde_json::to_string(&events)
            .unwrap()
            .contains("JSON公开思考 [redacted]"));
        assert!(!serde_json::to_string(&turn.items)
            .unwrap()
            .contains("JSON公开思考"));
    }
    #[test]
    fn published_chat_reasoning_setting_controls_visibility_and_never_enters_history() {
        for enabled in [false, true] {
            let mut state = StreamState::new(false, "test-secret");
            state.public_reasoning_content = enabled;
            let mut events = Vec::new();
            state.event(SseEvent{name:String::new(),data:json!({"id":"r","choices":[{"index":0,"delta":{"role":"assistant","content":"答案","reasoning_content":"公开过程 test-secret"},"finish_reason":"stop"}]}).to_string()},&mut |event|events.push(event)).unwrap();
            state.flush(&mut |event| events.push(event));
            let thinking = events
                .iter()
                .filter_map(|event| {
                    if let ModelStreamEvent::ThinkingDelta { delta } = event {
                        Some(delta.as_str())
                    } else {
                        None
                    }
                })
                .collect::<String>();
            assert_eq!(thinking.contains("公开过程"), enabled);
            assert!(!thinking.contains("test-secret"));
            let turn = state.finish().unwrap();
            assert!(!serde_json::to_string(&turn.items)
                .unwrap()
                .contains("公开过程"));
        }
    }
    #[test]
    fn sse_accepts_cr_only_split_crlf_and_initial_bom() {
        for ending in ["\r", "\r\n", "\n"] {
            let bytes=format!("\u{feff}:comment{ending}event: done{ending}data: 中文{ending}data: tail{ending}{ending}");
            let mut decoder = SseDecoder::default();
            let mut events = Vec::new();
            for byte in bytes.as_bytes() {
                events.extend(decoder.push(&[*byte]).unwrap());
            }
            events.extend(decoder.finish().unwrap());
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].name, "done");
            assert_eq!(events[0].data, "中文\ntail");
        }
    }

    #[test]
    fn public_summary_is_separate_from_private_reasoning() {
        let turn = parse_responses(json!({"status":"completed","output":[
            {"type":"reasoning","encrypted_content":"private-encrypted","content":[{"type":"reasoning_text","text":"private-thought"}],
             "summary":[{"type":"summary_text","text":"先核对当前画面，再选择入口。"},{"type":"reasoning_text","text":"private-ignored"}]},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"准备打开设置。"}]}
        ]})).unwrap();
        assert_eq!(turn.summary, ["先核对当前画面，再选择入口。"]);
        assert_eq!(turn.text, "准备打开设置。");
        assert!(!serde_json::to_string(&turn.items)
            .unwrap()
            .contains("private"));
        let chat = parse_chat(json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"公开回答","reasoning_content":"private-thought"}}]})).unwrap();
        assert!(chat.summary.is_empty());
        assert!(!serde_json::to_string(&chat.items)
            .unwrap()
            .contains("private"));
    }

    fn fixture_provider(address: std::net::SocketAddr, protocol: &str) -> Provider {
        Provider::new(ConnectionConfig {
            base_url: format!("http://{address}/v1"),
            model: "fixture".into(),
            protocol: protocol.into(),
            request_timeout_secs: 5,
            api_key: "test-only".into(),
            public_reasoning_content: false,
            max_output_tokens: super::super::settings::DEFAULT_MAX_OUTPUT_TOKENS,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn streaming_snapshot_is_the_actual_wire_body_before_any_supplier_output() {
        use tokio::io::AsyncWriteExt;
        for protocol in ["responses", "chat_completions"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let responses = protocol == "responses";
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let request = read_request(&mut stream).await;
                sender.send(request).unwrap();
                let body = if responses { json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]}]}) } else { json!({"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"done"}}]}) }.to_string();
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).as_bytes()).await.unwrap();
            });
            let history = [
                json!({"role":"system","content":"base"}),
                json!({"role":"developer","content":"application context"}),
                json!({"role":"user","content":"current user"}),
                json!({"role":"user","content":"reference injected afterwards"}),
            ];
            let tools = [
                json!({"type":"function","name":"memory_get","description":"full tool schema","parameters":{"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false},"strict":false}),
            ];
            let mut events = Vec::new();
            fixture_provider(address, protocol)
                .turn_stream(&history, &tools, &AtomicBool::new(false), |event| {
                    events.push(event)
                })
                .await
                .unwrap();
            let ModelStreamEvent::RequestSnapshot { snapshot } = &events[0] else {
                panic!("snapshot must precede diagnostics and supplier output")
            };
            let wire = receiver.await.unwrap();
            assert_eq!(snapshot["request_body"], wire);
            assert!(wire.to_string().contains("reference injected afterwards"));
            assert!(wire.to_string().contains("full tool schema"));
            assert_eq!(snapshot["headers_included"], false);
            assert!(!snapshot.to_string().contains("test-only"));
            server.await.unwrap();
        }
    }
    #[tokio::test]
    async fn frequent_sse_envelopes_can_exceed_json_limit_and_keep_terminal_usage() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            let padding = "p".repeat(8 * 1024);
            let mut wire = String::new();
            for _ in 0..400 {
                wire.push_str(&event(
                    json!({"type":"response.output_text.delta","delta":"x","metadata":padding}),
                ));
            }
            wire.push_str(&event(json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"x".repeat(400)}]}],"usage":{"input_tokens":20,"output_tokens":25,"total_tokens":45}}})));
            assert!(wire.len() > RESPONSE_BYTES_LIMIT && wire.len() < STREAM_BYTES_LIMIT);
            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",wire.len()).as_bytes()).await.unwrap();
            stream.write_all(wire.as_bytes()).await.unwrap();
        });
        let mut emitted = String::new();
        let turn = fixture_provider(address, "responses")
            .turn_stream(&[], &[], &AtomicBool::new(false), |event| {
                if let ModelStreamEvent::TextDelta { delta } = event {
                    emitted.push_str(&delta)
                }
            })
            .await
            .unwrap();
        assert_eq!(emitted, "x".repeat(400));
        assert_eq!(turn.text, emitted);
        assert_eq!(turn.usage.unwrap()["total_tokens"], 45);
        server.await.unwrap();
    }
    #[test]
    fn sse_line_and_multiline_event_have_independent_bounded_buffers() {
        let mut line = SseDecoder::default();
        assert!(line.push(&vec![b'x'; SSE_EVENT_BYTES_LIMIT]).is_ok());
        assert!(line.push(b"x").err().unwrap().to_string().contains("单行"));
        let mut event = SseDecoder::default();
        let payload = "x".repeat(SSE_EVENT_BYTES_LIMIT / 2);
        event.push(format!("data: {payload}\n").as_bytes()).unwrap();
        assert!(event
            .push(format!("data: {payload}\n").as_bytes())
            .err()
            .unwrap()
            .to_string()
            .contains("单事件"));
        assert_eq!(RESPONSE_BYTES_LIMIT, 2 * 1024 * 1024);
        assert_eq!(STREAM_BYTES_LIMIT, 32 * 1024 * 1024);
    }
    fn event(value: Value) -> String {
        format!("data: {}\r\n\r\n", value)
    }

    #[test]
    fn sse_decoder_supports_split_utf8_comments_crlf_and_multiline_data() {
        let bytes=": keepalive\r\nevent: custom\r\ndata: {\"text\":\r\ndata: \"中文\"}\r\n\r\ndata: [DONE]\r\n\r\n".as_bytes();
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        for byte in bytes {
            events.extend(decoder.push(&[*byte]).unwrap());
        }
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].name, "custom");
        assert_eq!(
            serde_json::from_str::<Value>(&events[0].data).unwrap()["text"],
            "中文"
        );
        assert_eq!(events[1].data, "[DONE]");
    }

    #[test]
    fn streamed_redaction_hides_credentials_split_across_deltas() {
        let mut redactor = DeltaRedactor::new("secret-value");
        let mut emitted = String::new();
        for part in ["公开 sec", "ret-", "value 正文"] {
            emitted.push_str(&redactor.push(part));
        }
        emitted.push_str(&redactor.finish());
        assert_eq!(emitted, "公开 [redacted] 正文");
        assert!(!emitted.contains("secret-value"));
    }

    #[tokio::test]
    async fn responses_sse_emits_before_completion_and_only_terminal_calls_are_executable() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let observed = std::sync::Arc::new(tokio::sync::Notify::new());
        let fixture_observed = observed.clone();
        let fixture = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let body = read_request(&mut stream).await;
            assert_eq!(body["stream"], true);
            let first=[event(json!({"type":"response.created","response":{"id":"resp-fixture","status":"in_progress"}})),
                event(json!({"type":"response.reasoning_summary_text.delta","delta":"先核对画面。"})),
                event(json!({"type":"response.reasoning_text.delta","delta":"private-raw-reasoning"})),
                event(json!({"type":"response.output_text.delta","delta":"公开回答"})),
                event(json!({"type":"response.function_call_arguments.delta","item_id":"fc1","delta":"{\"x\":"}))].concat();
            let last = event(
                json!({"type":"response.completed","response":{"id":"resp-fixture","status":"completed","usage":{"input_tokens":5,"output_tokens":3,"total_tokens":8},"output":[
                {"type":"reasoning","summary":[{"type":"summary_text","text":"先核对画面。"}],"encrypted_content":"private-encrypted"},
                {"type":"message","role":"assistant","content":[{"type":"output_text","text":"公开回答"}]},
                {"type":"function_call","call_id":"call-one","name":"input_tap","arguments":"{\"x\":2}","status":"completed"}]}}),
            );
            let headers=format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nX-Request-ID: request-fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",first.len()+last.len());
            stream.write_all(headers.as_bytes()).await.unwrap();
            stream.write_all(first.as_bytes()).await.unwrap();
            // A buffered implementation cannot release this barrier.
            tokio::time::timeout(Duration::from_secs(2), fixture_observed.notified())
                .await
                .expect("public delta must arrive before terminal response");
            stream.write_all(last.as_bytes()).await.unwrap();
        });
        let provider = fixture_provider(address, "responses");
        let mut events = Vec::new();
        let turn = provider
            .turn_stream(&[], &[], &AtomicBool::new(false), |event| {
                if matches!(event, ModelStreamEvent::TextDelta { .. }) {
                    observed.notify_one();
                }
                events.push(event);
            })
            .await
            .unwrap();
        fixture.await.unwrap();
        assert_eq!(turn.calls.len(), 1);
        assert_eq!(turn.calls[0].arguments["x"], 2);
        assert_eq!(turn.text, "公开回答");
        assert_eq!(turn.summary, ["先核对画面。"]);
        assert_eq!(turn.diagnostics["request_id"], "request-fixture");
        assert_eq!(turn.diagnostics["response_id"], "resp-fixture");
        let public = serde_json::to_string(&events).unwrap();
        assert!(public.contains("summary_delta"));
        assert!(!public.contains("private-"));
        assert_eq!(turn.usage.unwrap()["total_tokens"], 8);
    }

    #[tokio::test]
    async fn chat_sse_accumulates_tool_fragments_and_preserves_trailing_usage() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let fixture = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let body = read_request(&mut stream).await;
            assert_eq!(body["stream"], true);
            assert_eq!(body["stream_options"]["include_usage"], true);
            let data=[event(json!({"id":"chat-fixture","choices":[{"index":0,"delta":{"role":"assistant","content":"准备","reasoning_content":"private-raw","summary_text":"公开摘要"},"finish_reason":null}]})),
                event(json!({"id":"chat-fixture","choices":[{"index":0,"delta":{"content":"操作","tool_calls":[{"index":0,"id":"call-one","type":"function","function":{"name":"input_","arguments":"{\"x\":"}}]},"finish_reason":null}]})),
                event(json!({"id":"chat-fixture","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"tap","arguments":"3}"}}]},"finish_reason":"tool_calls"}]})),
                event(json!({"id":"chat-fixture","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}})),
                "data: [DONE]\r\n\r\n".into()].concat();
            let headers=format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",data.len());
            stream.write_all(headers.as_bytes()).await.unwrap();
            for part in data.as_bytes().chunks(7) {
                stream.write_all(part).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        let mut events = Vec::new();
        let turn = fixture_provider(address, "chat_completions")
            .turn_stream(&[], &[], &AtomicBool::new(false), |event| {
                events.push(event)
            })
            .await
            .unwrap();
        fixture.await.unwrap();
        assert_eq!(turn.text, "准备操作");
        assert_eq!(turn.calls[0].name, "input_tap");
        assert_eq!(turn.calls[0].arguments["x"], 3);
        assert_eq!(turn.usage.unwrap()["total_tokens"], 15);
        assert_eq!(turn.summary, ["公开摘要"]);
        assert!(!serde_json::to_string(&events)
            .unwrap()
            .contains("private-raw"));
    }

    #[tokio::test]
    async fn cancelled_sse_keeps_received_usage_and_request_diagnostics_without_partial_calls() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let fixture = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nX-Request-ID: cancelled-fixture\r\nContent-Length: 10000\r\n\r\n").await.unwrap();
            stream.write_all(event(json!({"id":"partial","choices":[{"index":0,"delta":{"role":"assistant","content":"公开进度","tool_calls":[{"index":0,"id":"call-one","type":"function","function":{"name":"input_tap","arguments":"{\"x\":"}}]},"finish_reason":null}],"usage":{"total_tokens":3}})).as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let cancel = AtomicBool::new(false);
        let mut events = Vec::new();
        let provider = fixture_provider(address, "chat_completions");
        let request = provider.turn_stream(&[], &[], &cancel, |event| events.push(event));
        let trigger = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.store(true, Ordering::Release);
        };
        let (result, _) = tokio::join!(request, trigger);
        let error = result.unwrap_err();
        fixture.abort();
        assert_eq!(error_details(&error)["request_id"], "cancelled-fixture");
        assert_eq!(error_usage(&error).unwrap()["total_tokens"], 3);
        assert!(error.to_string().contains("CANCELLED"));
        assert!(events
            .iter()
            .any(|event| matches!(event, ModelStreamEvent::TextDelta { .. })));
    }

    #[tokio::test]
    async fn incomplete_sse_reports_billed_usage_and_never_returns_calls() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let fixture = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            let data = event(
                json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},
                "usage":{"input_tokens":2,"output_tokens":4,"total_tokens":6},"output":[{"type":"function_call","call_id":"partial","name":"input_tap","arguments":"{}"}]}}),
            );
            let headers=format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nX-Request-ID: incomplete-fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",data.len());
            stream.write_all(headers.as_bytes()).await.unwrap();
            stream.write_all(data.as_bytes()).await.unwrap();
        });
        let error = fixture_provider(address, "responses")
            .turn_stream(&[], &[], &AtomicBool::new(false), |_| {})
            .await
            .unwrap_err();
        fixture.await.unwrap();
        assert_eq!(error_details(&error)["code"], "output_limit");
        assert_eq!(error_details(&error)["request_id"], "incomplete-fixture");
        assert_eq!(error_usage(&error).unwrap()["total_tokens"], 6);
    }

    #[test]
    fn incomplete_responses_explain_the_limit_and_never_execute_partial_calls() {
        let error = parse_responses(json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},
            "output":[{"type":"function_call","call_id":"partial","name":"input_tap","arguments":"{}"}]})).unwrap_err();
        let details = error_details(&error);
        assert_eq!(details["code"], "output_limit");
        assert_eq!(details["retryable"], false);
        assert!(details["detail"].as_str().unwrap().contains("本轮未执行"));
        let chat = parse_chat(json!({"choices":[{"finish_reason":"length","message":{"role":"assistant","content":""}}]})).unwrap_err();
        assert_eq!(error_details(&chat)["code"], "output_limit");
    }

    #[test]
    fn provider_error_details_redact_credentials_and_keep_actionable_fields() {
        let error = api_failure(
            "http_error",
            Some(401),
            "AI API HTTP 401: echoed-test-key",
            false,
        );
        let error = redact_error(error, "echoed-test-key");
        let details = error_details(&error);
        assert_eq!(details["http_status"], 401);
        assert_eq!(details["retryable"], false);
        assert!(!details.to_string().contains("echoed-test-key"));
        assert!(details["detail"].as_str().unwrap().contains("[redacted]"));
    }

    #[test]
    fn duplicate_structured_tool_json_is_sent_once_without_losing_text_images_or_errors() {
        let metadata =
            json!({"frame_id":"frame-7","items":[{"id":"remembered-guide","excerpt":"短攻略"}]});
        let output = json!({"content":[
            {"type":"text","text":serde_json::to_string_pretty(&metadata).unwrap()},
            {"type":"text","text":"额外说明：请核对新画面"},
            {"type":"image","mimeType":"image/png","data":"AQID"}
        ],"structuredContent":metadata,"isError":true});
        let (text, images) = tool_output(&output, "call-7").unwrap();
        assert_eq!(text.matches("remembered-guide").count(), 1);
        assert!(text.contains("额外说明：请核对新画面"));
        assert!(text.starts_with("Tool execution failed"));
        assert_eq!(images.len(), 2);
        assert_eq!(images[1]["type"], "input_image");
        assert_eq!(images[1]["image_url"], "data:image/png;base64,AQID");
        let different = json!({"content":[{"type":"text","text":"{\"id\":\"first-payload\"}"}],"structuredContent":{"id":"second-payload"}});
        let (text, images) = tool_output(&different, "different").unwrap();
        assert!(text.contains("first-payload") && text.contains("second-payload"));
        assert!(images.is_empty());
        let plain = json!({"content":[{"type":"text","text":"纯文本说明"}],"structuredContent":{"id":"structured-only"}});
        let (text, _) = tool_output(&plain, "plain").unwrap();
        assert!(text.contains("纯文本说明") && text.contains("structured-only"));
    }
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
                public_reasoning_content: false,
                max_output_tokens: super::super::settings::DEFAULT_MAX_OUTPUT_TOKENS,
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
                        super::super::settings::DEFAULT_MAX_OUTPUT_TOKENS
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
                public_reasoning_content: false,
                max_output_tokens: super::super::settings::DEFAULT_MAX_OUTPUT_TOKENS,
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
