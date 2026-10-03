//! Optional, explicitly configured network services. No supplier fallback and no
//! service credentials in Package resources, model history or public diagnostics.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    net::{IpAddr, SocketAddr},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;
const BATCH_LIMIT: usize = 32;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EmbeddingConfig {
    pub enabled: bool,
    pub provider: String,
    pub protocol: String,
    pub base_url: String,
    pub endpoint: String,
    pub model: String,
    pub account_id: String,
    pub request_timeout_secs: u64,
    pub max_input_bytes: usize,
    pub query_prefix: String,
    pub document_prefix: String,
}
impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "disabled".into(),
            protocol: "openai_embeddings".into(),
            base_url: String::new(),
            endpoint: String::new(),
            model: String::new(),
            account_id: String::new(),
            request_timeout_secs: 30,
            max_input_bytes: 480,
            query_prefix: String::new(),
            document_prefix: String::new(),
        }
    }
}
impl EmbeddingConfig {
    pub fn normalize(&mut self) -> Result<()> {
        self.provider = self.provider.trim().to_string();
        self.model = self.model.trim().to_string();
        self.account_id = self.account_id.trim().to_string();
        self.base_url = optional_endpoint(&self.base_url)?
            .trim_end_matches('/')
            .to_string();
        self.endpoint = optional_endpoint(&self.endpoint)?;
        self.validate()
    }
    fn validate(&self) -> Result<()> {
        ensure!(
            (128..=24 * 1024).contains(&self.max_input_bytes),
            "embedding 输入字节上限应为128至24576"
        );
        ensure!(
            self.query_prefix.len() <= 256 && self.document_prefix.len() <= 256,
            "embedding 查询/文档前缀最多256字节"
        );
        ensure!(
            self.query_prefix.len() < self.max_input_bytes
                && self.document_prefix.len() < self.max_input_bytes,
            "embedding 前缀必须小于输入字节上限"
        );
        ensure!(
            !self.query_prefix.contains(['\0', '\r'])
                && !self.document_prefix.contains(['\0', '\r']),
            "embedding 前缀包含无效控制字符"
        );
        validate_common(
            self.enabled,
            &self.provider,
            &self.base_url,
            &self.endpoint,
            self.request_timeout_secs,
        )?;
        ensure!(
            matches!(self.protocol.as_str(), "openai_embeddings" | "cloudflare"),
            "不支持的 embedding 协议"
        );
        ensure!(
            self.model.len() <= 256 && !self.model.chars().any(char::is_control),
            "embedding 模型名无效"
        );
        ensure!(
            self.account_id.len() <= 128
                && self
                    .account_id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "embedding account_id 无效"
        );
        if self.enabled {
            ensure!(!self.model.is_empty(), "请填写 embedding 模型名");
            if self.protocol == "cloudflare" && self.endpoint.is_empty() {
                ensure!(
                    !self.account_id.is_empty(),
                    "Cloudflare 原生协议需要 account_id 或完整 endpoint"
                );
                ensure!(
                    !self.model.contains("..") && !self.model.contains(['?', '#', '\\']),
                    "Cloudflare 模型路径无效"
                );
            }
        }
        Ok(())
    }
    fn url(&self) -> Result<String> {
        if !self.endpoint.is_empty() {
            return Ok(self.endpoint.clone());
        }
        let suffix = if self.protocol == "cloudflare" {
            format!("accounts/{}/ai/run/{}", self.account_id, self.model)
        } else {
            "embeddings".into()
        };
        optional_endpoint(&format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            suffix
        ))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SearchConfig {
    pub enabled: bool,
    pub provider: String,
    pub protocol: String,
    pub base_url: String,
    pub endpoint: String,
    pub request_timeout_secs: u64,
    pub max_results: usize,
}
impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "disabled".into(),
            protocol: "custom".into(),
            base_url: String::new(),
            endpoint: String::new(),
            request_timeout_secs: 30,
            max_results: 5,
        }
    }
}
impl SearchConfig {
    pub fn normalize(&mut self) -> Result<()> {
        self.provider = self.provider.trim().to_string();
        self.base_url = optional_endpoint(&self.base_url)?
            .trim_end_matches('/')
            .to_string();
        self.endpoint = optional_endpoint(&self.endpoint)?;
        self.validate()
    }
    fn validate(&self) -> Result<()> {
        validate_common(
            self.enabled,
            &self.provider,
            &self.base_url,
            &self.endpoint,
            self.request_timeout_secs,
        )?;
        ensure!(
            matches!(
                self.protocol.as_str(),
                "tavily" | "brave" | "searxng" | "custom"
            ),
            "不支持的搜索协议"
        );
        ensure!(
            (1..=20).contains(&self.max_results),
            "搜索结果上限应为 1 至 20"
        );
        Ok(())
    }
    fn url(&self) -> Result<String> {
        if !self.endpoint.is_empty() {
            return Ok(self.endpoint.clone());
        }
        let suffix = if self.protocol == "brave" {
            "web/search"
        } else {
            "search"
        };
        optional_endpoint(&format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            suffix
        ))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct WebReadConfig {
    pub enabled: bool,
    pub request_timeout_secs: u64,
    pub max_bytes: usize,
    /// Empty allows public websites. Exact host matches, no wildcards.
    pub allowed_hosts: Vec<String>,
    /// Explicit administrator opt-in; disabled by default, including loopback.
    pub allow_private_networks: bool,
}
impl Default for WebReadConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            request_timeout_secs: 30,
            max_bytes: 256 * 1024,
            allowed_hosts: Vec::new(),
            allow_private_networks: false,
        }
    }
}
impl WebReadConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (5..=300).contains(&self.request_timeout_secs),
            "网页请求超时应为 5 至 300 秒"
        );
        ensure!(
            (1024..=4 * 1024 * 1024).contains(&self.max_bytes),
            "网页读取上限应为 1KiB 至 4MiB"
        );
        ensure!(
            self.allowed_hosts.len() <= 64
                && self.allowed_hosts.iter().all(|host| !host.is_empty()
                    && host.len() <= 253
                    && !host.chars().any(char::is_control)
                    && !host.contains(['/', '?', '#', '@', '*', ' '])),
            "网页主机允许列表无效"
        );
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingPurpose {
    Document,
    Query,
}

#[derive(Clone, Debug, Serialize)]
pub struct ServiceDiagnostics {
    pub provider: String,
    pub protocol: String,
    pub http_status: u16,
    pub request_id: Option<String>,
    pub elapsed_ms: u64,
    pub request_attempts: u32,
}
#[derive(Clone, Debug, Serialize)]
pub struct EmbeddingResult {
    pub vectors: Vec<Vec<f32>>,
    pub dimensions: usize,
    pub model: String,
    pub fingerprint: String,
    /// Null means supplier did not report usage; never infer zero billing.
    pub usage: Value,
    pub diagnostics: ServiceDiagnostics,
}
#[derive(Clone, Debug, Serialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct SearchResult {
    pub results: Vec<SearchHit>,
    pub usage: Value,
    pub diagnostics: ServiceDiagnostics,
    /// Results are untrusted reference material. In particular, Brave snippets
    /// are temporary context; persistence must retain URL/metadata only.
    pub transient: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct WebReadResult {
    pub url: String,
    pub title: Option<String>,
    pub text: String,
    pub content_type: String,
    pub truncated: bool,
    pub diagnostics: ServiceDiagnostics,
}

#[derive(Debug)]
struct ServiceFailure {
    code: &'static str,
    http_status: Option<u16>,
    request_id: Option<String>,
    detail: String,
    retryable: bool,
    usage: Value,
}
impl std::fmt::Display for ServiceFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}
impl std::error::Error for ServiceFailure {}
pub fn error_details(error: &anyhow::Error) -> Value {
    if let Some(failure) = error.downcast_ref::<ServiceFailure>() {
        json!({"code":failure.code,"http_status":failure.http_status,"request_id":failure.request_id,
            "detail":failure.detail,"retryable":failure.retryable,"usage":failure.usage})
    } else {
        json!({"code":"service_error","detail":error.to_string(),"retryable":false,"usage":null})
    }
}

// No Debug: this is a frozen private connection, not a UI config object.
pub struct ServiceConnection {
    embedding: EmbeddingConfig,
    embedding_key: String,
    search: SearchConfig,
    search_key: String,
    web_read: WebReadConfig,
}
impl ServiceConnection {
    pub fn new(
        mut embedding: EmbeddingConfig,
        embedding_key: String,
        mut search: SearchConfig,
        search_key: String,
        web_read: WebReadConfig,
    ) -> Result<Self> {
        embedding.normalize()?;
        search.normalize()?;
        web_read.validate()?;
        for key in [&embedding_key, &search_key] {
            ensure!(
                key.len() <= 4096 && !key.chars().any(char::is_control),
                "AI 服务密钥无效"
            );
        }
        Ok(Self {
            embedding,
            embedding_key,
            search,
            search_key,
            web_read,
        })
    }
    pub fn embedding_enabled(&self) -> bool {
        self.embedding.enabled
    }
    pub fn search_enabled(&self) -> bool {
        self.search.enabled
    }
    pub fn web_read_enabled(&self) -> bool {
        self.web_read.enabled
    }
    pub fn read_enabled(&self) -> bool {
        self.web_read.enabled
    }
    pub fn embedding_max_input_bytes(&self) -> usize {
        self.embedding.max_input_bytes
    }
    pub fn embedding_chunk_bytes(&self) -> usize {
        self.embedding.max_input_bytes
            - self
                .embedding
                .query_prefix
                .len()
                .max(self.embedding.document_prefix.len())
    }
    pub fn embedding_max_batch_size(&self) -> usize {
        BATCH_LIMIT
    }
    pub fn embedding_fingerprint(&self) -> String {
        // Timeout and display label do not change embeddings. Credentials are
        // deliberately excluded. Chunking/version rules belong to memory.rs.
        let identity = json!({"protocol":self.embedding.protocol,"endpoint":self.embedding.url().ok(),
            "model":self.embedding.model,"account_id":self.embedding.account_id,"max_input_bytes":self.embedding.max_input_bytes,"query_prefix":self.embedding.query_prefix,"document_prefix":self.embedding.document_prefix});
        format!("{:x}", Sha256::digest(identity.to_string().as_bytes()))
    }
    pub async fn embed(
        &self,
        texts: &[String],
        purpose: EmbeddingPurpose,
        cancel: &AtomicBool,
    ) -> Result<EmbeddingResult> {
        ensure!(
            self.embedding.protocol != "cloudflare"
                || !self.embedding.enabled
                || !self.embedding_key.is_empty(),
            "embedding_key_required: 请填写该连接的Cloudflare私密密钥"
        );
        ensure!(
            self.embedding.enabled,
            "embedding_disabled: 未启用 embedding 服务"
        );
        ensure!(
            !texts.is_empty() && texts.len() <= BATCH_LIMIT,
            "embedding 输入数量应为 1 至 32"
        );
        let prefix = match purpose {
            EmbeddingPurpose::Document => &self.embedding.document_prefix,
            EmbeddingPurpose::Query => &self.embedding.query_prefix,
        };
        ensure!(
            texts.iter().all(|text| !text.trim().is_empty()
                && text.len() + prefix.len() <= self.embedding.max_input_bytes),
            "embedding 输入加前缀超过配置的字节上限；请按完整步骤分段"
        );
        let inputs = texts
            .iter()
            .map(|text| format!("{prefix}{text}"))
            .collect::<Vec<_>>();
        let started = Instant::now();
        let http = client(self.embedding.request_timeout_secs)?;
        let body = if self.embedding.protocol == "cloudflare" {
            json!({"text":inputs})
        } else {
            json!({"model":self.embedding.model,"input":inputs,"encoding_format":"float"})
        };
        let mut request = http.post(self.embedding.url()?).json(&body);
        if !self.embedding_key.is_empty() {
            request = request.bearer_auth(&self.embedding_key);
        }
        let (value, status, request_id) = self.json_request(request, cancel).await?;
        if self.embedding.protocol == "cloudflare"
            && value.get("success").and_then(Value::as_bool) == Some(false)
        {
            return Err(self.failure(
                "provider_error",
                Some(status),
                request_id,
                "embedding 服务报告失败",
                false,
                value.get("usage").cloned().unwrap_or(Value::Null),
            ));
        }
        let vectors =
            parse_vectors(&value, &self.embedding.protocol, texts.len()).map_err(|error| {
                self.failure(
                    "invalid_response",
                    Some(status),
                    request_id.clone(),
                    &error.to_string(),
                    false,
                    value.get("usage").cloned().unwrap_or(Value::Null),
                )
            })?;
        let dimensions = vectors[0].len();
        Ok(EmbeddingResult {
            vectors,
            dimensions,
            model: self.embedding.model.clone(),
            fingerprint: self.embedding_fingerprint(),
            usage: self.safe_value(value.get("usage").cloned().unwrap_or(Value::Null)),
            diagnostics: ServiceDiagnostics {
                provider: self.embedding.provider.clone(),
                protocol: self.embedding.protocol.clone(),
                http_status: status,
                request_id,
                elapsed_ms: elapsed(started),
                request_attempts: 1,
            },
        })
    }
    pub async fn search(&self, query: &str, cancel: &AtomicBool) -> Result<SearchResult> {
        ensure!(self.search.enabled, "search_disabled: 未启用搜索服务");
        ensure!(
            !matches!(self.search.protocol.as_str(), "tavily" | "brave")
                || !self.search_key.is_empty(),
            "search_key_required: 请填写该连接的搜索服务私密密钥"
        );
        ensure!(
            !query.trim().is_empty() && query.len() <= 4096 && !query.contains('\0'),
            "搜索词为空或过长"
        );
        if self.search.protocol == "brave" {
            ensure!(
                query.chars().count() <= 600 && query.split_whitespace().count() <= 75,
                "Brave 搜索词超过 600 字符或 75 词"
            );
        }
        let started = Instant::now();
        let http = client(self.search.request_timeout_secs)?;
        let endpoint = self.search.url()?;
        let mut request = match self.search.protocol.as_str() {
            "brave" => http
                .get(endpoint)
                .query(&[
                    ("q", query),
                    ("count", &self.search.max_results.to_string()),
                ])
                .header("X-Subscription-Token", &self.search_key),
            "searxng" => http
                .get(endpoint)
                .query(&[("q", query), ("format", "json")]),
            "tavily" => http.post(endpoint).json(
                &json!({"query":query,"max_results":self.search.max_results,
                "search_depth":"basic","auto_parameters":false,"include_answer":false,
                "include_raw_content":false,"include_images":false,"include_usage":true}),
            ),
            _ => http
                .post(endpoint)
                .json(&json!({"query":query,"max_results":self.search.max_results})),
        }
        .header("Accept", "application/json");
        if self.search.protocol != "brave" && !self.search_key.is_empty() {
            request = request.bearer_auth(&self.search_key);
        }
        let (value, status, request_id) = self.json_request(request, cancel).await?;
        let rows = if self.search.protocol == "brave" {
            value.pointer("/web/results")
        } else {
            value.get("results")
        }
        .and_then(Value::as_array)
        .context("搜索服务缺少 results 数组")?;
        let mut results = Vec::new();
        for row in rows.iter().take(self.search.max_results) {
            let Some(url) = row.get("url").and_then(Value::as_str) else {
                continue;
            };
            if reference_url(url).is_err() {
                continue;
            }
            results.push(SearchHit {
                title: self
                    .safe_string(row.get("title").and_then(Value::as_str).unwrap_or(""), 512),
                url: self.safe_string(url, 4096),
                snippet: self.safe_string(
                    row.get("snippet")
                        .or_else(|| row.get("content"))
                        .or_else(|| row.get("description"))
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    12000,
                ),
            });
        }
        Ok(SearchResult {
            results,
            usage: self.safe_value(value.get("usage").cloned().unwrap_or(Value::Null)),
            diagnostics: ServiceDiagnostics {
                provider: self.search.provider.clone(),
                protocol: self.search.protocol.clone(),
                http_status: status,
                request_id,
                elapsed_ms: elapsed(started),
                request_attempts: 1,
            },
            transient: true,
        })
    }
    pub async fn read(&self, input: &str, cancel: &AtomicBool) -> Result<WebReadResult> {
        ensure!(self.web_read.enabled, "web_read_disabled: 未启用网页读取");
        let url = reference_url(input)?;
        let host = url
            .host_str()
            .context("网页地址缺少主机")?
            .trim_matches(['[', ']'])
            .to_ascii_lowercase();
        ensure!(
            self.web_read.allowed_hosts.is_empty()
                || self
                    .web_read
                    .allowed_hosts
                    .iter()
                    .any(|allowed| allowed.trim_matches(['[', ']']).eq_ignore_ascii_case(&host)),
            "web_host_denied: 网页主机不在允许列表"
        );
        let port = url.port_or_known_default().context("网页端口无效")?;
        let addresses: Vec<SocketAddr> = if let Ok(ip) = host.parse::<IpAddr>() {
            vec![SocketAddr::new(ip, port)]
        } else {
            cancellable(
                tokio::time::timeout(
                    Duration::from_secs(self.web_read.request_timeout_secs),
                    tokio::net::lookup_host((host.as_str(), port)),
                ),
                cancel,
            )
            .await?
            .map_err(|_| anyhow::anyhow!("web_dns_timeout: 网页主机解析超时"))?
            .map_err(|_| anyhow::anyhow!("web_dns_failed: 网页主机解析失败"))?
            .collect()
        };
        ensure!(
            !addresses.is_empty() && addresses.len() <= 64,
            "web_dns_failed: 网页主机解析结果无效"
        );
        ensure!(
            self.web_read.allow_private_networks
                || addresses.iter().all(|address| public_ip(address.ip())),
            "web_private_address_denied: 网页解析到本机、私网或保留地址"
        );
        // Pin the checked DNS answer, disable system proxies and redirects. No
        // credential, Cookie or Authorization is attached to arbitrary websites.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .resolve_to_addrs(&host, &addresses)
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(self.web_read.request_timeout_secs))
            .build()?;
        let started = Instant::now();
        let mut response = cancellable(
            http.get(url.clone())
                .header(
                    "Accept",
                    "text/html,text/plain,text/markdown,application/json",
                )
                .header("User-Agent", "Gamer-AI/1.0")
                .send(),
            cancel,
        )
        .await?
        .map_err(|_| anyhow::anyhow!("web_request_failed: 网页请求失败；未自动重试"))?;
        let status = response.status();
        let request_id = request_id(response.headers(), None).map(|id| self.safe_string(&id, 256));
        ensure!(
            status.is_success(),
            "web_http_error: 网页 HTTP {}；不跟随跳转",
            status.as_u16()
        );
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/plain")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        ensure!(
            matches!(
                content_type.as_str(),
                "text/html"
                    | "text/plain"
                    | "text/markdown"
                    | "application/json"
                    | "application/xhtml+xml"
            ),
            "web_content_type: 仅支持文字或 HTML 网页"
        );
        let mut bytes = Vec::new();
        let mut truncated = response
            .content_length()
            .is_some_and(|length| length > self.web_read.max_bytes as u64);
        while let Some(chunk) = cancellable(response.chunk(), cancel)
            .await?
            .map_err(|_| anyhow::anyhow!("web_response_failed: 网页读取失败"))?
        {
            let remaining = self.web_read.max_bytes.saturating_sub(bytes.len());
            if chunk.len() > remaining {
                bytes.extend_from_slice(&chunk[..remaining]);
                truncated = true;
                break;
            }
            bytes.extend_from_slice(&chunk);
        }
        let source = String::from_utf8_lossy(&bytes);
        let html = matches!(content_type.as_str(), "text/html" | "application/xhtml+xml");
        let title = html
            .then(|| html_title(&source))
            .flatten()
            .map(|title| self.safe_string(&title, 512));
        let text = if html {
            html_text(&source)
        } else {
            source.into_owned()
        };
        Ok(WebReadResult {
            url: self.safe_string(url.as_str(), 4096),
            title,
            text: self.safe_string(&text, self.web_read.max_bytes),
            content_type,
            truncated,
            diagnostics: ServiceDiagnostics {
                provider: "web".into(),
                protocol: "http_get".into(),
                http_status: status.as_u16(),
                request_id,
                elapsed_ms: elapsed(started),
                request_attempts: 1,
            },
        })
    }
    async fn json_request(
        &self,
        request: reqwest::RequestBuilder,
        cancel: &AtomicBool,
    ) -> Result<(Value, u16, Option<String>)> {
        let mut response = cancellable(request.send(), cancel)
            .await?
            .map_err(|error| {
                self.failure(
                    if error.is_timeout() {
                        "request_timeout"
                    } else {
                        "connection_failed"
                    },
                    None,
                    None,
                    "AI 服务请求失败；未自动重试",
                    true,
                    Value::Null,
                )
            })?;
        let status = response.status();
        let header_id = request_id(response.headers(), None);
        ensure!(
            response.content_length().unwrap_or(0) <= RESPONSE_LIMIT as u64,
            "AI 服务响应超过 2MiB"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = cancellable(response.chunk(), cancel).await?.map_err(|_| {
            self.failure(
                "response_read_failed",
                Some(status.as_u16()),
                header_id.clone(),
                "AI 服务响应读取失败",
                true,
                Value::Null,
            )
        })? {
            ensure!(
                bytes.len().saturating_add(chunk.len()) <= RESPONSE_LIMIT,
                "AI 服务响应超过 2MiB"
            );
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            self.failure(
                "invalid_response",
                Some(status.as_u16()),
                header_id.clone(),
                "AI 服务返回非 JSON 响应",
                status.is_server_error(),
                Value::Null,
            )
        })?;
        let id = header_id
            .or_else(|| {
                value
                    .get("request_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .map(|id| self.safe_string(&id, 256));
        if !status.is_success() {
            let message = value
                .pointer("/error/message")
                .or_else(|| value.pointer("/detail/error"))
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("服务拒绝请求");
            return Err(self.failure(
                "http_error",
                Some(status.as_u16()),
                id,
                &format!("AI 服务 HTTP {}: {}；未自动重试", status.as_u16(), message),
                status.is_server_error() || status.as_u16() == 429,
                value.get("usage").cloned().unwrap_or(Value::Null),
            ));
        }
        Ok((value, status.as_u16(), id))
    }
    fn safe_string(&self, value: &str, max: usize) -> String {
        let mut safe = value.to_string();
        for key in [&self.embedding_key, &self.search_key] {
            if !key.is_empty() {
                safe = safe.replace(key, "[redacted]");
            }
        }
        let mut safe: String = safe
            .chars()
            .filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t'))
            .collect();
        if safe.len() > max {
            let mut end = max;
            while !safe.is_char_boundary(end) {
                end -= 1;
            }
            safe.truncate(end);
        }
        safe
    }
    fn safe_value(&self, value: Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.safe_string(&s, 2048)),
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .take(64)
                    .map(|v| self.safe_value(v))
                    .collect(),
            ),
            Value::Object(items) => Value::Object(
                items
                    .into_iter()
                    .take(64)
                    .map(|(k, v)| (self.safe_string(&k, 128), self.safe_value(v)))
                    .collect(),
            ),
            other => other,
        }
    }
    fn failure(
        &self,
        code: &'static str,
        status: Option<u16>,
        request_id: Option<String>,
        detail: &str,
        retryable: bool,
        usage: Value,
    ) -> anyhow::Error {
        ServiceFailure {
            code,
            http_status: status,
            request_id: request_id.map(|id| self.safe_string(&id, 256)),
            detail: self.safe_string(detail, 1000),
            retryable,
            usage: self.safe_value(usage),
        }
        .into()
    }
}

fn validate_common(
    enabled: bool,
    provider: &str,
    base_url: &str,
    endpoint: &str,
    timeout: u64,
) -> Result<()> {
    ensure!(
        !provider.is_empty() && provider.len() <= 128 && !provider.chars().any(char::is_control),
        "AI 服务提供方名称无效"
    );
    ensure!(
        (5..=300).contains(&timeout),
        "AI 服务请求超时应为 5 至 300 秒"
    );
    optional_endpoint(base_url)?;
    optional_endpoint(endpoint)?;
    if enabled {
        ensure!(
            provider != "disabled" && (!endpoint.is_empty() || !base_url.is_empty()),
            "启用服务需要明确的提供方和地址"
        );
    }
    Ok(())
}
fn optional_endpoint(input: &str) -> Result<String> {
    if input.trim().is_empty() {
        return Ok(String::new());
    }
    let url = reference_url(input.trim())?;
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "AI 服务地址不能包含查询参数或片段；凭据请填私密密钥"
    );
    let host = url.host_str().unwrap_or("").trim_matches(['[', ']']);
    let loopback = host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    ensure!(
        url.scheme() == "https" || loopback,
        "AI 服务必须使用 HTTPS（本机自建服务可使用 HTTP）"
    );
    Ok(url.as_str().to_string())
}
fn reference_url(input: &str) -> Result<reqwest::Url> {
    ensure!(
        input.len() <= 4096 && !input.chars().any(char::is_control),
        "网页地址无效或过长"
    );
    let url = reqwest::Url::parse(input).map_err(|_| anyhow::anyhow!("网页地址无效"))?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none(),
        "网页地址必须是无凭据的 HTTP(S) URL"
    );
    Ok(url)
}
fn client(timeout: u64) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(timeout))
        .build()
        .context("无法创建 AI 服务 HTTP 客户端")
}
async fn cancellable<F: Future>(future: F, cancel: &AtomicBool) -> Result<F::Output> {
    tokio::pin!(future);
    loop {
        ensure!(
            !cancel.load(Ordering::Acquire),
            "CANCELLED: AI 服务请求已取消"
        );
        tokio::select! { output = &mut future => return Ok(output), _ = tokio::time::sleep(Duration::from_millis(25)) => {} }
    }
}
fn elapsed(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u64::MAX as u128) as u64
}
fn request_id(headers: &reqwest::header::HeaderMap, value: Option<&Value>) -> Option<String> {
    ["x-request-id", "request-id", "cf-ray"]
        .iter()
        .find_map(|name| {
            headers
                .get(*name)
                .and_then(|h| h.to_str().ok())
                .map(str::to_string)
        })
        .or_else(|| {
            value
                .and_then(|v| v.get("request_id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}
fn parse_vectors(value: &Value, protocol: &str, count: usize) -> Result<Vec<Vec<f32>>> {
    let rows = if protocol == "cloudflare" {
        value.pointer("/result/data")
    } else {
        value.get("data")
    }
    .and_then(Value::as_array)
    .context("embedding 响应缺少向量数组")?;
    ensure!(rows.len() == count, "embedding 返回数量与输入不符");
    let mut vectors: Vec<Option<Vec<f32>>> = vec![None; count];
    let mut dimension = None;
    for (position, row) in rows.iter().enumerate() {
        let (index, values) = if protocol == "cloudflare" {
            (position, row.as_array())
        } else {
            (
                row.get("index")
                    .and_then(Value::as_u64)
                    .context("embedding 响应缺少 index")? as usize,
                row.get("embedding").and_then(Value::as_array),
            )
        };
        ensure!(
            index < count && vectors[index].is_none(),
            "embedding 返回 index 越界或重复"
        );
        let values = values.context("embedding 向量不是数组")?;
        ensure!(
            !values.is_empty() && values.len() <= 65536,
            "embedding 维度无效"
        );
        ensure!(
            dimension.is_none() || dimension == Some(values.len()),
            "embedding 向量维度不一致"
        );
        dimension = Some(values.len());
        let vector = values
            .iter()
            .map(|v| {
                let value = v.as_f64().context("embedding 向量包含非数值")? as f32;
                ensure!(value.is_finite(), "embedding 向量包含非有限数值");
                Ok(value)
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(vector.iter().any(|v| *v != 0.0), "embedding 返回无效零向量");
        vectors[index] = Some(vector);
    }
    vectors
        .into_iter()
        .map(|vector| vector.context("embedding 缺少对应输入向量"))
        .collect()
}
fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || a == 0
                || a >= 240
                || (a == 100 && (64..=127).contains(&b))
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 0 && c == 2)
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(v4));
            }
            let segments = ip.segments();
            // Public unicast only; excludes ULA/link-local/multicast/loopback,
            // documentation and IPv4 transition/translation special ranges.
            (segments[0] & 0xe000) == 0x2000
                && !(segments[0] == 0x2001 && (segments[1] == 0xdb8 || segments[1] <= 0x01ff))
                && segments[0] != 0x2002
                && !(segments[0] == 0x3fff && segments[1] <= 0x0fff)
        }
    }
}
fn html_title(source: &str) -> Option<String> {
    let lower = source.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let start = start + lower[start..].find('>')? + 1;
    let end = start + lower[start..].find("</title")?;
    Some(html_text(&source[start..end]).trim().to_string())
}
fn html_text(source: &str) -> String {
    let lower = source.to_ascii_lowercase();
    let mut text = String::new();
    let mut cursor = 0;
    while cursor < source.len() {
        let rest = &source[cursor..];
        if rest.starts_with('<') {
            if lower[cursor..].starts_with("<!--") {
                cursor = lower[cursor + 4..]
                    .find("-->")
                    .map(|p| cursor + 4 + p + 3)
                    .unwrap_or(source.len());
                continue;
            }
            let Some(end) = rest.find('>') else {
                break;
            };
            let tag = lower[cursor + 1..cursor + end].trim();
            let name = tag.split_whitespace().next().unwrap_or("");
            if matches!(name, "script" | "style" | "noscript" | "template") {
                let close = format!("</{name}");
                cursor = lower[cursor + end + 1..]
                    .find(&close)
                    .map(|p| cursor + end + 1 + p)
                    .unwrap_or(source.len());
                continue;
            }
            if matches!(
                name,
                "p" | "/p"
                    | "div"
                    | "/div"
                    | "br"
                    | "br/"
                    | "li"
                    | "/li"
                    | "h1"
                    | "/h1"
                    | "h2"
                    | "/h2"
                    | "h3"
                    | "/h3"
                    | "tr"
                    | "/tr"
            ) {
                text.push('\n');
            } else {
                text.push(' ');
            }
            cursor += end + 1;
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            text.push_str(&rest[..end]);
            cursor += end;
        }
    }
    let text = decode_entities(&text);
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}
fn decode_entities(text: &str) -> String {
    let mut output = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        output.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find(';').filter(|end| *end <= 16) else {
            output.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => {
                entity[1..].parse::<u32>().ok().and_then(char::from_u32)
            }
            _ => None,
        };
        if let Some(decoded) = decoded {
            output.push(decoded);
        } else {
            output.push_str(&rest[..=end]);
        }
        rest = &rest[end + 1..];
    }
    output.push_str(rest);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    #[tokio::test]
    async fn configured_embedding_prefixes_and_total_byte_limit_are_enforced_without_extra_requests(
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let fixture = tokio::spawn(async move {
            for prefix in ["query: ", "passage: "] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (_, body) = request(&mut stream).await;
                assert_eq!(body["input"][0], format!("{prefix}原文"));
                assert!(body["input"][0].as_str().unwrap().len() <= 128);
                respond(
                    &mut stream,
                    "200 OK",
                    &json!({"data":[{"index":0,"embedding":[1.0,0.0]}]}).to_string(),
                    "application/json",
                )
                .await;
            }
        });
        let config = EmbeddingConfig {
            enabled: true,
            provider: "local".into(),
            endpoint: format!("http://{address}/embeddings"),
            model: "fixture".into(),
            max_input_bytes: 128,
            query_prefix: "query: ".into(),
            document_prefix: "passage: ".into(),
            ..Default::default()
        };
        let connection = connection(
            config.clone(),
            "",
            SearchConfig::default(),
            "",
            WebReadConfig::default(),
        );
        assert_eq!(connection.embedding_chunk_bytes(), 119);
        assert!(connection
            .embed(
                &["界".repeat(41)],
                EmbeddingPurpose::Document,
                &AtomicBool::new(false)
            )
            .await
            .is_err());
        for purpose in [EmbeddingPurpose::Query, EmbeddingPurpose::Document] {
            connection
                .embed(&["原文".into()], purpose, &AtomicBool::new(false))
                .await
                .unwrap();
        }
        fixture.await.unwrap();
        let mut different = config;
        different.query_prefix = "question: ".into();
        let different = super::ServiceConnection::new(
            different,
            String::new(),
            SearchConfig::default(),
            String::new(),
            WebReadConfig::default(),
        )
        .unwrap();
        assert_ne!(
            connection.embedding_fingerprint(),
            different.embedding_fingerprint()
        );
        let mut invalid = EmbeddingConfig {
            max_input_bytes: 128,
            query_prefix: "x".repeat(128),
            ..Default::default()
        };
        assert!(invalid.normalize().is_err());
    }

    async fn request(stream: &mut tokio::net::TcpStream) -> (String, Value) {
        let mut bytes = Vec::new();
        let (start, length, headers) = loop {
            let mut part = [0u8; 4096];
            let count = stream.read(&mut part).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&part[..count]);
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                break (end + 4, length, headers);
            }
        };
        while bytes.len() - start < length {
            let mut part = [0; 4096];
            let count = stream.read(&mut part).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&part[..count]);
        }
        (
            headers,
            if length == 0 {
                Value::Null
            } else {
                serde_json::from_slice(&bytes[start..start + length]).unwrap()
            },
        )
    }
    async fn respond(
        stream: &mut tokio::net::TcpStream,
        status: &str,
        body: &str,
        content_type: &str,
    ) {
        let headers=format!("HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nX-Request-ID: fixture-request\r\nConnection: close\r\n\r\n",body.len());
        stream.write_all(headers.as_bytes()).await.unwrap();
        stream.write_all(body.as_bytes()).await.unwrap();
    }
    fn connection(
        embedding: EmbeddingConfig,
        embedding_key: &str,
        search: SearchConfig,
        search_key: &str,
        web_read: WebReadConfig,
    ) -> ServiceConnection {
        ServiceConnection::new(
            embedding,
            embedding_key.into(),
            search,
            search_key.into(),
            web_read,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn embedding_adapters_order_validate_and_report_usage_without_fallback() {
        for protocol in ["openai_embeddings", "cloudflare"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let fixture = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (headers, body) = request(&mut stream).await;
                assert!(headers.starts_with("POST /full-endpoint "));
                assert!(headers
                    .to_ascii_lowercase()
                    .contains("authorization: bearer embedding-test-only"));
                assert_eq!(
                    body[if protocol == "cloudflare" {
                        "text"
                    } else {
                        "input"
                    }],
                    json!(["背包奖励", "领取方法"])
                );
                let response = if protocol == "cloudflare" {
                    json!({"success":true,"result":{"shape":[2,2],"data":[[1.0,2.0],[3.0,4.0]]},"usage":{"tokens":7}})
                } else {
                    assert_eq!(body["encoding_format"], "float");
                    json!({"data":[{"index":1,"embedding":[3.0,4.0]},{"index":0,"embedding":[1.0,2.0]}],"usage":{"total_tokens":7}})
                };
                respond(
                    &mut stream,
                    "200 OK",
                    &response.to_string(),
                    "application/json",
                )
                .await;
            });
            let services = connection(
                EmbeddingConfig {
                    enabled: true,
                    provider: "my-host".into(),
                    protocol: protocol.into(),
                    endpoint: format!("http://{address}/full-endpoint"),
                    model: "test".into(),
                    ..Default::default()
                },
                "embedding-test-only",
                SearchConfig::default(),
                "",
                WebReadConfig::default(),
            );
            let result = services
                .embed(
                    &["背包奖励".into(), "领取方法".into()],
                    EmbeddingPurpose::Document,
                    &AtomicBool::new(false),
                )
                .await
                .unwrap();
            fixture.await.unwrap();
            assert_eq!(result.vectors, vec![vec![1.0, 2.0], vec![3.0, 4.0]]);
            assert_eq!(result.dimensions, 2);
            assert_eq!(
                result.diagnostics.request_id.as_deref(),
                Some("fixture-request")
            );
            assert!(!result.usage.is_null());
        }
        assert!(parse_vectors(
            &json!({"data":[{"index":0,"embedding":[1.0]},{"index":0,"embedding":[2.0]}]}),
            "openai_embeddings",
            2
        )
        .is_err());
        assert!(parse_vectors(
            &json!({"data":[{"index":0,"embedding":[0.0,0.0]}]}),
            "openai_embeddings",
            1
        )
        .is_err());
        assert!(parse_vectors(
            &json!({"data":[{"index":0,"embedding":[1.0]},{"index":1,"embedding":[1.0,2.0]}]}),
            "openai_embeddings",
            2
        )
        .is_err());
    }

    #[tokio::test]
    async fn all_search_protocols_use_only_configured_endpoint_and_normalize_results() {
        for protocol in ["tavily", "brave", "searxng", "custom"] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let fixture = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (headers, body) = request(&mut stream).await;
                if matches!(protocol, "brave" | "searxng") {
                    assert!(headers.starts_with("GET /configured?"));
                    assert!(headers.contains("q="));
                } else {
                    assert!(headers.starts_with("POST /configured "));
                    assert_eq!(body["query"], "领取奖励");
                }
                if protocol == "brave" {
                    assert!(headers
                        .to_ascii_lowercase()
                        .contains("x-subscription-token: search-test-only"));
                } else {
                    assert!(headers
                        .to_ascii_lowercase()
                        .contains("authorization: bearer search-test-only"));
                }
                if protocol == "tavily" {
                    assert_eq!(body["auto_parameters"], false);
                    assert_eq!(body["search_depth"], "basic");
                }
                let hits = json!([{"title":"奖励攻略","url":"https://example.com/guide","content":"search-test-only 正文","description":"search-test-only 正文"},
                    {"title":"invalid","url":"file:///private","content":"ignore"}]);
                let response = if protocol == "brave" {
                    json!({"web":{"results":hits}})
                } else {
                    json!({"results":hits,"usage":{"credits":1}})
                };
                respond(
                    &mut stream,
                    "200 OK",
                    &response.to_string(),
                    "application/json",
                )
                .await;
            });
            let services = connection(
                EmbeddingConfig::default(),
                "",
                SearchConfig {
                    enabled: true,
                    provider: "configured".into(),
                    protocol: protocol.into(),
                    endpoint: format!("http://{address}/configured"),
                    ..Default::default()
                },
                "search-test-only",
                WebReadConfig::default(),
            );
            let result = services
                .search("领取奖励", &AtomicBool::new(false))
                .await
                .unwrap();
            fixture.await.unwrap();
            assert_eq!(result.results.len(), 1);
            assert_eq!(result.results[0].title, "奖励攻略");
            assert!(result.transient);
            assert!(!serde_json::to_string(&result)
                .unwrap()
                .contains("search-test-only"));
        }
    }

    #[tokio::test]
    async fn service_errors_keep_usage_status_and_request_id_but_redact_private_keys() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let fixture = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            request(&mut stream).await;
            respond(
                &mut stream,
                "429 Too Many Requests",
                &json!({"error":{"message":"echo search-test-only"},"usage":{"credits":2}})
                    .to_string(),
                "application/json",
            )
            .await;
        });
        let services = connection(
            EmbeddingConfig::default(),
            "",
            SearchConfig {
                enabled: true,
                provider: "fixture".into(),
                protocol: "custom".into(),
                endpoint: format!("http://{address}/search"),
                ..Default::default()
            },
            "search-test-only",
            WebReadConfig::default(),
        );
        let error = services
            .search("test", &AtomicBool::new(false))
            .await
            .unwrap_err();
        fixture.await.unwrap();
        let detail = error_details(&error);
        assert_eq!(detail["http_status"], 429);
        assert_eq!(detail["usage"]["credits"], 2);
        assert_eq!(detail["request_id"], "fixture-request");
        assert!(!detail.to_string().contains("search-test-only"));
    }

    #[tokio::test]
    async fn disabled_and_cancelled_services_do_not_retry_or_fallback() {
        let disabled = connection(
            EmbeddingConfig::default(),
            "",
            SearchConfig::default(),
            "",
            WebReadConfig::default(),
        );
        assert!(disabled
            .embed(
                &["test".into()],
                EmbeddingPurpose::Query,
                &AtomicBool::new(false)
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("embedding_disabled"));
        assert!(disabled
            .search("test", &AtomicBool::new(false))
            .await
            .unwrap_err()
            .to_string()
            .contains("search_disabled"));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let fixture = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            request(&mut stream).await;
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1024\r\n\r\n{").await.unwrap();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let services = connection(
            EmbeddingConfig {
                enabled: true,
                provider: "local".into(),
                model: "test".into(),
                endpoint: format!("http://{address}/embeddings"),
                ..Default::default()
            },
            "",
            SearchConfig::default(),
            "",
            WebReadConfig::default(),
        );
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let texts = vec!["test".to_string()];
        let request = services.embed(&texts, EmbeddingPurpose::Query, &cancel);
        let trigger = async {
            tokio::time::sleep(Duration::from_millis(80)).await;
            cancel.store(true, Ordering::Release);
        };
        let (result, _) = tokio::join!(request, trigger);
        assert!(result.unwrap_err().to_string().contains("CANCELLED"));
        assert!(started.elapsed() < Duration::from_secs(1));
        fixture.abort();
    }

    #[tokio::test]
    async fn web_read_requires_explicit_private_opt_in_and_strips_scripts_without_credentials() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let denied = connection(
            EmbeddingConfig::default(),
            "",
            SearchConfig::default(),
            "",
            WebReadConfig {
                enabled: true,
                ..Default::default()
            },
        );
        assert!(denied
            .read(&format!("http://{address}/guide"), &AtomicBool::new(false))
            .await
            .unwrap_err()
            .to_string()
            .contains("web_private_address_denied"));
        let fixture = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (headers, _) = request(&mut stream).await;
            assert!(!headers.to_ascii_lowercase().contains("authorization:"));
            assert!(!headers.to_ascii_lowercase().contains("cookie:"));
            respond(&mut stream,"200 OK","<html><title>攻略 &amp; 方法</title><script>private-command</script><style>hidden-css</style><p>领取&#x5956;励</p><p>确认成功</p></html>","text/html; charset=utf-8").await;
        });
        let services = connection(
            EmbeddingConfig::default(),
            "embedding-test-only",
            SearchConfig::default(),
            "search-test-only",
            WebReadConfig {
                enabled: true,
                allow_private_networks: true,
                allowed_hosts: vec!["127.0.0.1".into()],
                ..Default::default()
            },
        );
        let result = services
            .read(&format!("http://{address}/guide"), &AtomicBool::new(false))
            .await
            .unwrap();
        fixture.await.unwrap();
        assert_eq!(result.title.as_deref(), Some("攻略 & 方法"));
        assert!(result.text.contains("领取奖励"));
        assert!(!result.text.contains("private-command"));
        assert!(!result.text.contains("hidden-css"));
        assert!(services
            .read("https://other.example/guide", &AtomicBool::new(false))
            .await
            .unwrap_err()
            .to_string()
            .contains("web_host_denied"));
    }
    #[test]
    fn public_address_rules_reject_ipv4_mapped_and_reserved_networks() {
        for ip in [
            "127.0.0.1",
            "10.0.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "192.0.2.1",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002:7f00:1::",
        ] {
            assert!(!public_ip(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
        assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
        assert_eq!(
            html_text(
                "<p>领取&amp;奖励</p><!--hidden--><script type='x'>hidden</script><p>步骤</p>"
            ),
            "领取&奖励\n步骤"
        );
    }
}
