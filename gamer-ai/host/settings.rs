//! Account-scoped model credentials. None of this storage belongs to a Package.
use super::prompts::{self, PromptConfig};
use super::services::{EmbeddingConfig, SearchConfig, ServiceConnection, WebReadConfig};
use anyhow::{ensure, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

pub(super) const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 16384;
fn default_max_output_tokens() -> u32 {
    DEFAULT_MAX_OUTPUT_TOKENS
}
fn default_public_reasoning_content() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SettingsConfig {
    pub base_url: String,
    pub model: String,
    pub protocol: String,
    pub request_timeout_secs: u64,
    #[serde(default = "default_public_reasoning_content")]
    pub public_reasoning_content: bool,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
}

impl Default for SettingsConfig {
    fn default() -> Self {
        Self {
            base_url: "https://open.bigmodel.cn/api/v1".into(),
            model: "glm-5.3-flash".into(),
            protocol: "responses".into(),
            request_timeout_secs: 90,
            public_reasoning_content: true,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        }
    }
}

// Deliberately no Debug: a failed test or log must not print the credential.
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PrivateSettings {
    pub version: Option<String>,
    #[serde(default)]
    pub config: SettingsConfig,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub probe: Option<Value>,
    #[serde(default)]
    pub services: PrivateServices,
    #[serde(default)]
    pub(super) prompts: PromptConfig,
}

// Separate versions prevent an unrelated services edit from invalidating an
// in-flight model probe or a model settings form. All credentials share one gate.
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PrivateServices {
    pub version: Option<String>,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    #[serde(default)]
    pub embedding_api_key: String,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub search_api_key: String,
    #[serde(default)]
    pub web_read: WebReadConfig,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServicesSaveRequest {
    expected_version: Option<String>,
    embedding: EmbeddingSave,
    search: SearchSave,
    web_read: WebReadConfig,
}
#[derive(Deserialize)]
struct EmbeddingSave {
    #[serde(flatten)]
    config: EmbeddingConfig,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    clear_key: bool,
}
#[derive(Deserialize)]
struct SearchSave {
    #[serde(flatten)]
    config: SearchConfig,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    clear_key: bool,
}

#[derive(Clone)]
pub struct ConnectionConfig {
    pub base_url: String,
    pub model: String,
    pub protocol: String,
    pub request_timeout_secs: u64,
    pub api_key: String,
    pub public_reasoning_content: bool,
    pub max_output_tokens: u32,
}

pub struct Settings {
    path: PathBuf,
    gate: Mutex<()>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveRequest {
    expected_version: Option<String>,
    base_url: String,
    model: String,
    protocol: String,
    request_timeout_secs: u64,
    #[serde(default)]
    api_key: String,
    #[serde(default)]
    clear_key: bool,
    #[serde(default = "default_public_reasoning_content")]
    public_reasoning_content: bool,
    #[serde(default = "default_max_output_tokens")]
    max_output_tokens: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptsSaveRequest {
    expected_version: Option<String>,
    chat_system_prompt: String,
    game_system_prompt: String,
    import_system_prompt: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptsResetRequest {
    expected_version: Option<String>,
    scope: String,
}

impl Settings {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            gate: Mutex::new(()),
        }
    }

    pub fn read(&self) -> Result<Value> {
        let _guard = self.gate.lock();
        Ok(self.load()?.public())
    }

    pub fn services_read(&self) -> Result<Value> {
        let _guard = self.gate.lock();
        Ok(self.load()?.services.public())
    }

    pub fn prompts_read(&self) -> Result<Value> {
        Ok(self.prompts()?.public())
    }
    pub fn prompts(&self) -> Result<PromptConfig> {
        let _guard = self.gate.lock();
        Ok(self.load()?.prompts)
    }
    pub fn prompts_save(&self, values: Value) -> Result<Value> {
        let request: PromptsSaveRequest = serde_json::from_value(values)
            .map_err(|_| anyhow::anyhow!("系统提示词配置字段无效"))?;
        for text in [
            &request.chat_system_prompt,
            &request.game_system_prompt,
            &request.import_system_prompt,
        ] {
            prompts::validate_prompt(text)?;
        }
        let _guard = self.gate.lock();
        let mut settings = self.load()?;
        ensure!(
            settings.prompts.version == request.expected_version,
            "version_conflict: 系统提示词已变更，请刷新后保存"
        );
        settings.prompts = PromptConfig {
            version: Some(uuid::Uuid::new_v4().to_string()),
            chat_system_prompt: (request.chat_system_prompt != prompts::CHAT_DEFAULT)
                .then_some(request.chat_system_prompt),
            game_system_prompt: (request.game_system_prompt != prompts::GAME_DEFAULT)
                .then_some(request.game_system_prompt),
            import_system_prompt: (request.import_system_prompt != prompts::IMPORT_DEFAULT)
                .then_some(request.import_system_prompt),
        };
        self.persist(&settings)?;
        Ok(settings.prompts.public())
    }
    pub fn prompts_reset(&self, values: Value) -> Result<Value> {
        let request: PromptsResetRequest = serde_json::from_value(values)
            .map_err(|_| anyhow::anyhow!("系统提示词重置字段无效"))?;
        ensure!(
            matches!(request.scope.as_str(), "chat" | "game" | "import" | "all"),
            "系统提示词重置范围无效"
        );
        let _guard = self.gate.lock();
        let mut settings = self.load()?;
        ensure!(
            settings.prompts.version == request.expected_version,
            "version_conflict: 系统提示词已变更，请刷新后重置"
        );
        if matches!(request.scope.as_str(), "chat" | "all") {
            settings.prompts.chat_system_prompt = None;
        }
        if matches!(request.scope.as_str(), "game" | "all") {
            settings.prompts.game_system_prompt = None;
        }
        if matches!(request.scope.as_str(), "import" | "all") {
            settings.prompts.import_system_prompt = None;
        }
        settings.prompts.version = Some(uuid::Uuid::new_v4().to_string());
        self.persist(&settings)?;
        Ok(settings.prompts.public())
    }
    pub fn redact_snapshot(&self, snapshot: &Value) -> Result<Value> {
        let _guard = self.gate.lock();
        let settings = self.load()?;
        Ok(prompts::sanitize(
            snapshot,
            &[
                &settings.api_key,
                &settings.services.embedding_api_key,
                &settings.services.search_api_key,
            ],
        ))
    }

    pub fn services_save(&self, values: Value) -> Result<Value> {
        // serde(flatten) cannot safely combine deny_unknown_fields with a secret
        // sibling. Check the public field set before deserializing instead.
        for (section, fields) in [
            (
                "embedding",
                &[
                    "enabled",
                    "provider",
                    "protocol",
                    "base_url",
                    "endpoint",
                    "model",
                    "account_id",
                    "request_timeout_secs",
                    "max_input_bytes",
                    "query_prefix",
                    "document_prefix",
                    "api_key",
                    "clear_key",
                ][..],
            ),
            (
                "search",
                &[
                    "enabled",
                    "provider",
                    "protocol",
                    "base_url",
                    "endpoint",
                    "request_timeout_secs",
                    "max_results",
                    "api_key",
                    "clear_key",
                ][..],
            ),
        ] {
            let object = values
                .get(section)
                .and_then(Value::as_object)
                .context("AI 服务配置字段无效")?;
            ensure!(
                object.keys().all(|key| fields.contains(&key.as_str())),
                "AI 服务配置包含未知字段"
            );
        }
        let mut request: ServicesSaveRequest =
            serde_json::from_value(values).map_err(|_| anyhow::anyhow!("AI 服务配置字段无效"))?;
        request.embedding.config.normalize()?;
        request.search.config.normalize()?;
        request.web_read.validate()?;
        let _guard = self.gate.lock();
        let mut settings = self.load()?;
        ensure!(
            settings.services.version == request.expected_version,
            "version_conflict: AI 服务配置已变更，请刷新后保存"
        );
        let embedding_key = update_key(
            if settings.services.embedding.provider == request.embedding.config.provider
                && settings.services.embedding.protocol == request.embedding.config.protocol
                && settings.services.embedding.base_url == request.embedding.config.base_url
                && settings.services.embedding.endpoint == request.embedding.config.endpoint
                && settings.services.embedding.account_id == request.embedding.config.account_id
            {
                &settings.services.embedding_api_key
            } else {
                ""
            },
            &request.embedding.api_key,
            request.embedding.clear_key,
        )?;
        let search_key = update_key(
            if settings.services.search.provider == request.search.config.provider
                && settings.services.search.protocol == request.search.config.protocol
                && settings.services.search.base_url == request.search.config.base_url
                && settings.services.search.endpoint == request.search.config.endpoint
            {
                &settings.services.search_api_key
            } else {
                ""
            },
            &request.search.api_key,
            request.search.clear_key,
        )?;
        // Validate the exact frozen connection without issuing any requests.
        ServiceConnection::new(
            request.embedding.config.clone(),
            embedding_key.clone(),
            request.search.config.clone(),
            search_key.clone(),
            request.web_read.clone(),
        )?;
        settings.services = PrivateServices {
            version: Some(uuid::Uuid::new_v4().to_string()),
            embedding: request.embedding.config,
            embedding_api_key: embedding_key,
            search: request.search.config,
            search_api_key: search_key,
            web_read: request.web_read,
        };
        self.persist(&settings)?;
        Ok(settings.services.public())
    }

    pub fn service_connection(&self) -> Result<ServiceConnection> {
        let _guard = self.gate.lock();
        let services = self.load()?.services;
        ServiceConnection::new(
            services.embedding,
            services.embedding_api_key,
            services.search,
            services.search_api_key,
            services.web_read,
        )
    }

    pub fn save(&self, values: Value) -> Result<Value> {
        let request: SaveRequest =
            serde_json::from_value(values).map_err(|_| anyhow::anyhow!("AI 配置字段无效"))?;
        let config = SettingsConfig {
            base_url: normalize_base_url(&request.base_url)?,
            model: request.model.trim().to_string(),
            protocol: request.protocol,
            request_timeout_secs: request.request_timeout_secs,
            public_reasoning_content: request.public_reasoning_content,
            max_output_tokens: request.max_output_tokens,
        };
        validate_config(&config)?;
        ensure!(request.api_key.len() <= 4096, "AI 密钥过长");
        ensure!(
            !request.api_key.chars().any(char::is_control),
            "AI 密钥不能包含控制字符"
        );
        ensure!(
            !request.clear_key || request.api_key.trim().is_empty(),
            "清除密钥时不能同时填写新密钥"
        );
        let _guard = self.gate.lock();
        let mut settings = self.load()?;
        ensure!(
            settings.version == request.expected_version,
            "version_conflict: AI 配置已变更，请刷新后保存"
        );
        let key = if request.clear_key {
            String::new()
        } else if !request.api_key.trim().is_empty() {
            request.api_key.trim().to_string()
        } else if same_origin(&settings.config.base_url, &config.base_url) {
            settings.api_key.clone()
        } else {
            String::new()
        };
        if settings.config != config || settings.api_key != key {
            settings.probe = None;
        }
        settings.config = config;
        settings.api_key = key;
        self.write(&mut settings)?;
        Ok(settings.public())
    }

    /// A probe is attached only to exactly the connection which was tested.
    pub fn save_probe(&self, probe: Value, expected_version: Option<&str>) -> Result<Value> {
        let _guard = self.gate.lock();
        let mut settings = self.load()?;
        ensure!(
            settings.version.as_deref() == expected_version,
            "version_conflict: 测试期间 AI 配置已变更，请重新测试"
        );
        ensure!(probe.is_object(), "AI 能力测试结果无效");
        // Whitelist the public diagnostic fields. Never persist request bodies or
        // a supplier response which might echo an Authorization header.
        let mut safe = serde_json::Map::new();
        for key in [
            "ok",
            "protocol",
            "model",
            "checked_at",
            "checks",
            "usage",
            "error",
        ] {
            if let Some(value) = probe.get(key) {
                let encoded = value.to_string();
                ensure!(
                    settings.api_key.is_empty() || !encoded.contains(&settings.api_key),
                    "AI 能力测试结果含私密信息"
                );
                safe.insert(key.to_string(), value.clone());
            }
        }
        settings.probe = Some(Value::Object(safe));
        self.write(&mut settings)?;
        Ok(settings.public())
    }

    pub fn connection(&self) -> Result<ConnectionConfig> {
        let _guard = self.gate.lock();
        let settings = self.load()?;
        validate_config(&settings.config)?;
        ensure!(!settings.api_key.is_empty(), "请先保存 AI API 密钥");
        Ok(ConnectionConfig {
            base_url: settings.config.base_url,
            model: settings.config.model,
            protocol: settings.config.protocol,
            request_timeout_secs: settings.config.request_timeout_secs,
            api_key: settings.api_key,
            public_reasoning_content: settings.config.public_reasoning_content,
            max_output_tokens: settings.config.max_output_tokens,
        })
    }

    fn load(&self) -> Result<PrivateSettings> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PrivateSettings::default())
            }
            Err(_) => anyhow::bail!("无法读取 AI 私密配置"),
        };
        ensure!(bytes.len() <= 256 * 1024, "AI 私密配置过大");
        let bytes = crate::core::secrets::protect(&bytes, false)
            .context("无法解密 AI 配置，请使用保存配置的系统账号")?;
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("AI 私密配置损坏"))
    }

    fn write(&self, settings: &mut PrivateSettings) -> Result<()> {
        settings.version = Some(uuid::Uuid::new_v4().to_string());
        self.persist(settings)
    }

    fn persist(&self, settings: &PrivateSettings) -> Result<()> {
        let parent = self.path.parent().context("AI 配置路径无效")?;
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        let bytes = crate::core::secrets::protect(&serde_json::to_vec(settings)?, true)?;
        crate::core::fs::atomic_write(&self.path, &bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

impl PrivateServices {
    fn public(&self) -> Value {
        let mut embedding =
            serde_json::to_value(&self.embedding).expect("serializable embedding config");
        embedding["has_key"] = json!(!self.embedding_api_key.is_empty());
        let mut search = serde_json::to_value(&self.search).expect("serializable search config");
        search["has_key"] = json!(!self.search_api_key.is_empty());
        json!({"version":self.version,"embedding":embedding,"search":search,"web_read":self.web_read})
    }
}

fn update_key(current: &str, incoming: &str, clear: bool) -> Result<String> {
    ensure!(
        incoming.len() <= 4096 && !incoming.chars().any(char::is_control),
        "AI 服务密钥无效"
    );
    ensure!(
        !clear || incoming.trim().is_empty(),
        "清除密钥时不能同时填写新密钥"
    );
    Ok(if clear {
        String::new()
    } else if incoming.trim().is_empty() {
        current.to_string()
    } else {
        incoming.trim().to_string()
    })
}

impl PrivateSettings {
    fn public(&self) -> Value {
        json!({
            "version": self.version,
            "base_url": self.config.base_url,
            "model": self.config.model,
            "protocol": self.config.protocol,
            "request_timeout_secs": self.config.request_timeout_secs,
            "public_reasoning_content": self.config.public_reasoning_content,
            "max_output_tokens": self.config.max_output_tokens,
            "has_key": !self.api_key.is_empty(),
            "probe": self.probe,
        })
    }
}
fn same_origin(left: &str, right: &str) -> bool {
    reqwest::Url::parse(left)
        .ok()
        .zip(reqwest::Url::parse(right).ok())
        .is_some_and(|(left, right)| left.origin() == right.origin())
}

fn normalize_base_url(input: &str) -> Result<String> {
    let input = input.trim().trim_end_matches('/');
    ensure!(input.len() <= 2048, "AI API 地址过长");
    let url = reqwest::Url::parse(input).map_err(|_| anyhow::anyhow!("AI API 地址无效"))?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "AI API 地址不能包含账号、查询参数或片段"
    );
    let loopback = url
        .host_str()
        .map(|host| {
            host == "localhost"
                || host == "[::1]"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
        .unwrap_or(false);
    ensure!(
        url.scheme() == "https" || (url.scheme() == "http" && loopback),
        "AI API 必须使用 HTTPS（本机测试地址可使用 HTTP）"
    );
    ensure!(url.host_str().is_some(), "AI API 地址缺少主机");
    Ok(url.as_str().trim_end_matches('/').to_string())
}

pub(super) fn validate_config(config: &SettingsConfig) -> Result<()> {
    normalize_base_url(&config.base_url)?;
    ensure!(
        !config.model.is_empty()
            && config.model.len() <= 128
            && !config.model.chars().any(char::is_control),
        "AI 模型名称无效"
    );
    ensure!(
        matches!(config.protocol.as_str(), "responses" | "chat_completions"),
        "AI 协议必须是 responses 或 chat_completions"
    );
    ensure!(
        (5..=300).contains(&config.request_timeout_secs),
        "AI 请求超时应为 5 至 300 秒"
    );
    ensure!(
        config.max_output_tokens <= 131072,
        "AI 单次输出上限应为 0 至 131072（0 使用供应商默认值）"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_have_independent_versions_and_preserve_credentials_and_connection_probe() {
        let directory = tempfile::tempdir().unwrap();
        let settings = Settings::new(directory.path().join("config.json"));
        let model = settings
            .save(request(Value::Null, "model-test-only"))
            .unwrap();
        let services = settings.services_read().unwrap();
        let original = settings.prompts_read().unwrap();
        assert_eq!(original["custom"]["chat"], false);
        let changed = settings.prompts_save(json!({"expected_version":original["version"],"chat_system_prompt":"用简洁中文回答","game_system_prompt":"先截图再行动","import_system_prompt":"按来源整理攻略"})).unwrap();
        assert_eq!(changed["custom"]["chat"], true);
        assert_eq!(settings.connection().unwrap().api_key, "model-test-only");
        assert_eq!(settings.read().unwrap(), model);
        assert_eq!(settings.services_read().unwrap(), services);
        assert!(settings.prompts_save(json!({"expected_version":original["version"],"chat_system_prompt":"旧表单","game_system_prompt":"先截图","import_system_prompt":"整理"})).unwrap_err().to_string().contains("version_conflict"));
        let reloaded = Settings::new(directory.path().join("config.json"));
        assert_eq!(reloaded.prompts_read().unwrap(), changed);
        let reset = settings
            .prompts_reset(json!({"expected_version":changed["version"],"scope":"chat"}))
            .unwrap();
        assert_eq!(
            reset["chat_system_prompt"],
            reset["defaults"]["chat_system_prompt"]
        );
        assert_eq!(reset["custom"]["chat"], false);
        assert_eq!(reset["game_system_prompt"], "先截图再行动");
        assert_eq!(settings.read().unwrap(), model);
        let defaults = settings.prompts_save(json!({"expected_version":reset["version"],"chat_system_prompt":reset["defaults"]["chat_system_prompt"],"game_system_prompt":reset["defaults"]["game_system_prompt"],"import_system_prompt":reset["defaults"]["import_system_prompt"]})).unwrap();
        assert_eq!(
            defaults["custom"],
            json!({"chat":false,"game":false,"import":false})
        );
        assert!(settings.prompts_save(json!({"expected_version":reset["version"],"chat_system_prompt":"x".repeat(32*1024+1),"game_system_prompt":"先截图","import_system_prompt":"整理"})).is_err());
    }

    #[test]
    fn public_thinking_defaults_on_and_per_request_output_limit_remains_optional() {
        let directory = tempfile::tempdir().unwrap();
        let settings = Settings::new(directory.path().join("config.json"));
        assert_eq!(settings.read().unwrap()["public_reasoning_content"], true);
        assert_eq!(
            settings.read().unwrap()["max_output_tokens"],
            DEFAULT_MAX_OUTPUT_TOKENS
        );
        let saved = settings.save(request(Value::Null, "test-key")).unwrap();
        assert_eq!(saved["public_reasoning_content"], true);
        assert_eq!(saved["max_output_tokens"], DEFAULT_MAX_OUTPUT_TOKENS);
        let mut changed = request(saved["version"].clone(), "");
        changed["public_reasoning_content"] = json!(false);
        changed["max_output_tokens"] = json!(0);
        let saved = settings.save(changed).unwrap();
        assert_eq!(saved["public_reasoning_content"], false);
        assert_eq!(settings.connection().unwrap().max_output_tokens, 0);
        let mut invalid = request(saved["version"].clone(), "");
        invalid["max_output_tokens"] = json!(131073);
        assert!(settings.save(invalid).is_err());
    }

    fn request(version: Value, key: &str) -> Value {
        json!({"expected_version":version,"base_url":"https://example.com/v1/",
            "model":"test-vision","protocol":"responses","request_timeout_secs":30,
            "api_key":key})
    }
    #[test]
    fn new_service_endpoint_clears_only_its_credential_and_model_same_origin_keeps_key() {
        let directory = tempfile::tempdir().unwrap();
        let settings = Settings::new(directory.path().join("config.json"));
        let model = settings
            .save(request(Value::Null, "model-test-only"))
            .unwrap();
        let services = settings
            .services_save(service_request(Value::Null))
            .unwrap();
        let mut changed = service_request(services["version"].clone());
        changed["embedding"]["endpoint"] = json!("https://different.example/embeddings");
        changed["embedding"]["api_key"] = json!("");
        changed["search"]["api_key"] = json!("");
        let saved = settings.services_save(changed).unwrap();
        assert_eq!(saved["embedding"]["has_key"], false);
        assert_eq!(saved["search"]["has_key"], true);
        let mut model_changed = request(model["version"].clone(), "");
        model_changed["base_url"] = json!("https://example.com/other");
        model_changed["protocol"] = json!("chat_completions");
        model_changed["public_reasoning_content"] = json!(true);
        let saved = settings.save(model_changed).unwrap();
        assert_eq!(saved["has_key"], true);
        assert_eq!(saved["public_reasoning_content"], true);
        let mut crossed = request(saved["version"].clone(), "");
        crossed["base_url"] = json!("https://other.example/v1");
        let crossed = settings.save(crossed).unwrap();
        assert_eq!(crossed["has_key"], false);
        assert_eq!(settings.services_read().unwrap()["search"]["has_key"], true);
    }

    #[test]
    fn credentials_stay_private_and_stale_updates_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let settings = Settings::new(directory.path().join("private/config.json"));
        let saved = settings
            .save(request(Value::Null, "test-secret-value"))
            .unwrap();
        assert_eq!(saved["has_key"], true);
        assert!(!saved.to_string().contains("test-secret-value"));
        assert_eq!(saved["base_url"], "https://example.com/v1");
        assert!(settings.save(request(Value::Null, "wrong")).is_err());
        let second = settings
            .save(request(saved["version"].clone(), ""))
            .unwrap();
        assert_eq!(settings.connection().unwrap().api_key, "test-secret-value");
        assert_ne!(saved["version"], second["version"]);
        let mut clear = request(second["version"].clone(), "");
        clear["clear_key"] = json!(true);
        settings.save(clear).unwrap();
        assert!(settings.connection().is_err());
    }

    #[test]
    fn changed_connection_invalidates_probe_and_probe_uses_optimistic_lock() {
        let directory = tempfile::tempdir().unwrap();
        let settings = Settings::new(directory.path().join("config.json"));
        let saved = settings
            .save(request(Value::Null, "secret-test-only"))
            .unwrap();
        let tested = settings
            .save_probe(
                json!({"ok":true,"checks":[],"private":"omit"}),
                saved["version"].as_str(),
            )
            .unwrap();
        assert_eq!(tested["probe"]["ok"], true);
        assert!(tested["probe"].get("private").is_none());
        assert!(settings
            .save_probe(json!({"ok":true}), saved["version"].as_str())
            .is_err());
        let mut changed = request(tested["version"].clone(), "");
        changed["protocol"] = json!("chat_completions");
        let changed = settings.save(changed).unwrap();
        assert_eq!(changed["probe"], Value::Null);
    }

    #[test]
    fn rejects_credential_urls_and_insecure_remote_http() {
        assert!(normalize_base_url("https://user:password@example.com/v1").is_err());
        assert!(normalize_base_url("https://example.com/v1?key=test").is_err());
        assert!(normalize_base_url("http://example.com/v1").is_err());
        assert!(normalize_base_url("http://127.0.0.1:1234/v1").is_ok());
        assert!(normalize_base_url("http://[::1]:1234/v1").is_ok());
    }

    fn service_request(version: Value) -> Value {
        json!({"expected_version":version,
            "embedding":{"enabled":true,"provider":"my-embedding","protocol":"openai_embeddings",
                "endpoint":"https://example.com/custom-embeddings","model":"my-model","api_key":"embedding-test-only"},
            "search":{"enabled":true,"provider":"my-search","protocol":"custom",
                "endpoint":"https://example.com/custom-search","api_key":"search-test-only"},
            "web_read":{"enabled":false}})
    }

    #[test]
    fn independent_services_versions_and_keys_preserve_existing_private_model_data() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        // Simulate the exact previous protected file layout, without services.
        let original = json!({"version":"legacy-model-version","config":{"base_url":"https://example.com/v1",
            "model":"test","protocol":"responses","request_timeout_secs":30},"api_key":"model-test-only","probe":{"ok":true}});
        std::fs::write(
            &path,
            crate::core::secrets::protect(&serde_json::to_vec(&original).unwrap(), true).unwrap(),
        )
        .unwrap();
        let settings = Settings::new(path);
        assert_eq!(
            settings.services_read().unwrap()["embedding"]["enabled"],
            false
        );
        let services = settings
            .services_save(service_request(Value::Null))
            .unwrap();
        assert!(!services.to_string().contains("test-only"));
        assert_eq!(services["embedding"]["has_key"], true);
        assert_eq!(services["search"]["has_key"], true);
        assert_eq!(settings.read().unwrap()["version"], "legacy-model-version");
        assert_eq!(settings.read().unwrap()["probe"]["ok"], true);
        assert_eq!(settings.connection().unwrap().api_key, "model-test-only");
        assert!(settings
            .services_save(service_request(Value::Null))
            .is_err());
        let model = settings
            .save(request(json!("legacy-model-version"), ""))
            .unwrap();
        assert_eq!(
            settings.services_read().unwrap()["version"],
            services["version"]
        );
        let mut next = service_request(services["version"].clone());
        next["embedding"]["api_key"] = json!("");
        next["search"]["api_key"] = json!("");
        next["search"]["clear_key"] = json!(true);
        let saved = settings.services_save(next).unwrap();
        assert_eq!(saved["embedding"]["has_key"], true);
        assert_eq!(saved["search"]["has_key"], false);
        assert_eq!(settings.read().unwrap()["version"], model["version"]);
        assert!(settings.service_connection().unwrap().embedding_enabled());
    }

    #[test]
    fn service_save_rejects_unknown_fields_secret_urls_and_clear_with_new_key() {
        let directory = tempfile::tempdir().unwrap();
        let settings = Settings::new(directory.path().join("config.json"));
        let mut request = service_request(Value::Null);
        request["embedding"]["api_keey"] = json!("would-have-been-lost");
        assert!(settings.services_save(request).is_err());
        let mut request = service_request(Value::Null);
        request["embedding"]["clear_key"] = json!(true);
        assert!(settings.services_save(request).is_err());
        let mut request = service_request(Value::Null);
        request["search"]["endpoint"] = json!("https://example.com/search?api_key=hidden");
        assert!(settings.services_save(request).is_err());
    }
}
