//! gamer-yaml 扩展边界（ADR-11/14，V1）。
//!
//! YAML 执行权威在 `yaml-interp` crate（WASM guest 与 server 测试同源，计划
//! Phase 2）。本模块职责：
//!
//! - 原生函数宿主（[`NativeYamlHost`]）：解释器 `__fn` 通道的后端——按
//!   [`native_funcs`] 注册表做 Schema 校验、权限检查与 Core capability 组合
//!   （tap/swipe/find/... 首版函数，计划 Phase 3.5）；
//! - WASM runtime 契约（[`YamlWasmRuntime`] / `NoYamlWasmRuntime`）与每 run
//!   请求/结果形态；
//! - 官方插件 manifest 参考常量（与 tools/plugins 打包源锁同步）。
//!
//! 依赖方向：本模块 → Core（capabilities / device / matcher）单向；Core 不得
//! import 本目录符号（架构守卫测试锁定）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Map as JsonMap, Value};

use crate::capabilities::{
    AppId, CapabilityRegistry, DeviceHandle, FramePoint, FrameSize, KeyAction, KeyCode, KeyInput,
    LogLevel, LogRecord, MatchOptions, MatchOutcome, ResourceId, RuntimeService, SearchRegion,
    SwipeGesture, TemplateQuery, TextInput, TouchPoint,
};
use crate::core::events::{EventSink, RuntimeEvent, RuntimeEventKind};
use crate::core::AppContext;
use crate::extensions::gamer_yaml::native_funcs::{native_function, ParamSchema};
use crate::extensions::gamer_yaml::syntax::{parse_duration_ms, point_components, ParamType};
use crate::extensions::{HostApi, Permission};

mod numeric;
mod ocr;

#[cfg(test)]
#[path = "browser_cleanup_tests.rs"]
mod browser_cleanup_tests;

pub(crate) const YAML_EXTENSION_ID: &str = "gamer-yaml";
/// 运行结构事件私有通道（guest → sink，先于权限校验拦截）。
pub(crate) const EVENT_CAPABILITY: &str = "__event";
/// 原生函数派发私有通道（guest → [`NativeYamlHost`]）。
pub(crate) const FN_CAPABILITY: &str = "__fn";
/// Reference manifest for the installable YAML guest. The server never embeds
/// its WASM bytes; package installation supplies `plugin.wasm` independently.
/// 仅测试引用：与 plugins/gamer-yaml/manifest.toml 的同步护栏 +
/// 安装/卸载面板测试以此为打包 manifest 源。
#[allow(dead_code)]
pub(crate) const YAML_EXTENSION_MANIFEST_TOML: &str = include_str!("../manifest.toml");

/// 官方市场打包源（plugins/gamer-yaml/manifest.toml）与本常量锁同步：
/// build-plugins.ps1 以文件为准打包，漂移会导致线上包与运行时语义不一致。
#[cfg(test)]
mod manifest_sync_tests {
    #[test]
    fn yaml_packaging_manifest_stays_in_sync_with_shipped_constant() {
        let packaged = include_str!("../manifest.toml");
        assert_eq!(
            super::YAML_EXTENSION_MANIFEST_TOML.trim(),
            packaged.trim(),
            "plugins/gamer-yaml/manifest.toml 与 YAML_EXTENSION_MANIFEST_TOML 不一致"
        );
    }
}

/// find/tap_template 轮询间隔下限（防止 0 间隔打爆设备）。
const MIN_POLL_INTERVAL_MS: u64 = 50;
/// 单次 sleep 上限（与 v3 一致）。
const MAX_SLEEP_MS: u64 = 3_600_000;
const MAX_VISUAL_WAIT_MS: u64 = 7_200_000;

// ---------------------------------------------------------------------------
// WASM runtime 契约
// ---------------------------------------------------------------------------

/// 一次 YAML 运行的请求：`program` 是宿主 lowering 产出的解释器 wire JSON
/// （含冻结的 Package 函数表、绑定参数与可选 start_index）。
pub(crate) struct YamlWasmRunRequest {
    /// Host-only authority; never deserialize it from the wire program.
    pub(crate) execution_policy: Option<crate::core::ExecutionPolicy>,
    pub(crate) private_templates: Option<std::collections::BTreeMap<String, Vec<u8>>>,
    pub(crate) notification: Option<super::notification::Sender>,
    pub(crate) ocr: Option<super::ocr_bridge::Recognizer>,
    pub(crate) live_switcher: Option<super::live_bridge::Switcher>,
    pub(crate) wasm: Vec<u8>,
    pub(crate) program: Value,
    pub(crate) host: HostApi,
    pub(crate) context: AppContext,
    pub(crate) stop: Arc<AtomicBool>,
    /// 运行可视化事件汇（`__event` 私有通道拦截 + 宿主侧 vision/input 补发）；
    /// `None` = 静默。
    pub(crate) sink: Option<Arc<dyn EventSink>>,
}

#[derive(Debug)]
pub(crate) struct YamlWasmRunResult {
    pub(crate) value: Value,
}

#[async_trait]
pub(crate) trait YamlWasmRuntime: Send + Sync {
    async fn run(&self, request: YamlWasmRunRequest) -> Result<YamlWasmRunResult>;

    fn is_available(&self) -> bool;
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NoYamlWasmRuntime;

#[async_trait]
impl YamlWasmRuntime for NoYamlWasmRuntime {
    async fn run(&self, _request: YamlWasmRunRequest) -> Result<YamlWasmRunResult> {
        bail!("未启用 wasm-runtime feature")
    }

    fn is_available(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// 原生函数宿主
// ---------------------------------------------------------------------------

/// 原生函数执行宿主：`__fn` 通道后端。每个 run 创建一个实例（持有设备句柄
/// 与坐标系），解释器的函数调用经 wasm_host 转发到这里。
///
/// 权限：每个函数声明所需权限，派发前逐项 `HostApi::authorize`——函数调用
/// 不能绕过插件权限（计划 Phase 3.2）。
#[derive(Default)]
pub(crate) struct YamlRunState {
    pub(crate) result_notifications: super::notification::ResultNotifications,
    pub(crate) required_checks: RwLock<std::collections::BTreeSet<String>>,
    pub(crate) completed_checks: RwLock<std::collections::BTreeSet<String>>,
    pub(crate) private_templates: RwLock<Option<std::collections::BTreeMap<String, Vec<u8>>>>,
    pub(crate) trace_enabled: AtomicBool,
    pub(crate) goal_verified: AtomicBool,
    pub(crate) enforce_template_versions: AtomicBool,
    pub(crate) trace_context: RwLock<Value>,
    pub(crate) template_versions: RwLock<std::collections::BTreeMap<String, String>>,
    cleanup_registry: std::sync::Mutex<Option<CapabilityRegistry>>,
    templates:
        tokio::sync::Mutex<std::collections::BTreeMap<String, crate::capabilities::ResourceHandle>>,
}
impl Drop for YamlRunState {
    fn drop(&mut self) {
        if let Ok(registry) = self.cleanup_registry.get_mut() {
            if let Some(resources) = registry.as_ref().and_then(|r| r.resource()) {
                for handle in self.templates.get_mut().values() {
                    resources.release_frozen(*handle);
                }
            }
        }
    }
}
impl YamlRunState {
    pub(crate) fn new() -> Self {
        let state = Self::default();
        state.trace_enabled.store(true, Ordering::Relaxed);
        state
    }
    pub(crate) fn metadata(&self, kind: &str) -> Value {
        let mut value = self.trace_context.read().unwrap().clone();
        if !value.is_object() {
            value = json!({});
        }
        value["kind"] = json!(kind);
        value["trace_enabled"] = json!(self.trace_enabled.load(Ordering::Relaxed));
        value
    }
}

pub(crate) struct NativeYamlHost {
    execution_policy: Option<crate::core::ExecutionPolicy>,
    pub(crate) run_state: Arc<YamlRunState>,
    notification: Option<super::notification::Sender>,
    ocr: Option<super::ocr_bridge::Recognizer>,
    live_switcher: Option<super::live_bridge::Switcher>,
    settings: super::settings::Settings,
    host: HostApi,
    registry: CapabilityRegistry,
    context: AppContext,
    device: DeviceHandle,
    runtime: Arc<dyn RuntimeService>,
    /// 设备坐标系（相对坐标 ⇄ 像素）：capture 后以真实帧分辨率刷新。
    screen: RwLock<FrameSize>,
    frame_stamp: RwLock<Option<crate::capabilities::FrameStamp>>,
    observed_at_ms: RwLock<Option<u64>>,
    sink: Option<Arc<dyn EventSink>>,
}

/// 按函数 Schema 校验/规整后的参数视图。
struct BoundArgs {
    values: JsonMap<String, Value>,
}

impl BoundArgs {
    fn point(&self, name: &str) -> Result<[f64; 2]> {
        point_components(
            self.values
                .get(name)
                .ok_or_else(|| anyhow!("缺少参数 {name}"))?,
        )
        .ok_or_else(|| anyhow!("参数 {name} 不是合法 point（0..1 相对坐标）"))
    }

    fn duration_ms(&self, name: &str) -> Result<u64> {
        let value = self
            .values
            .get(name)
            .ok_or_else(|| anyhow!("缺少参数 {name}"))?;
        let ms = match value {
            Value::Number(number) => number.as_f64(),
            Value::String(text) => parse_duration_ms(text),
            _ => None,
        };
        ms.map(|value| value.round().max(0.0) as u64)
            .ok_or_else(|| anyhow!("参数 {name} 不是合法 duration"))
    }

    fn string(&self, name: &str) -> Result<String> {
        self.values
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("参数 {name} 必须是字符串"))
    }

    fn opt_string(&self, name: &str) -> Result<Option<String>> {
        match self.values.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(other) => Err(anyhow!("参数 {name} 必须是字符串，得到 {other}")),
        }
    }

    fn number(&self, name: &str) -> Result<Option<f64>> {
        match self.values.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(number)) => number
                .as_f64()
                .map(Some)
                .ok_or_else(|| anyhow!("参数 {name} 非有限数字")),
            Some(other) => Err(anyhow!("参数 {name} 必须是数字，得到 {other}")),
        }
    }

    fn region(&self, name: &str) -> Result<Option<[f64; 4]>> {
        let Some(value) = self.values.get(name) else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        let items = value
            .as_array()
            .ok_or_else(|| anyhow!("region 必须是 [x, y, w, h] 相对坐标数组"))?;
        if items.len() != 4 {
            bail!("region 必须是四元数组 [x, y, w, h]");
        }
        let mut out = [0f64; 4];
        for (index, item) in items.iter().enumerate() {
            let component = item
                .as_f64()
                .ok_or_else(|| anyhow!("region 分量必须是数字"))?;
            if !(0.0..=1.0).contains(&component) {
                bail!("region 分量必须在 0..1（相对坐标），得到 {component}");
            }
            out[index] = component;
        }
        Ok(Some(out))
    }
}

/// 数值比较：JSON 数字统一按 f64 比较（整型/浮点互通）。
fn numeric_pair(a: &Value, b: &Value) -> Option<(f64, f64)> {
    Some((a.as_f64()?, b.as_f64()?))
}

fn json_equals(a: &Value, b: &Value) -> bool {
    if a == b {
        return true;
    }
    match (a, b) {
        (Value::Number(_), Value::Number(_)) => a.as_f64() == b.as_f64(),
        _ => false,
    }
}

fn message_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

impl NativeYamlHost {
    /// WASM guest 的 `__fn` 后端入口（wasm_host 调用）；无 wasm-runtime
    /// feature 时仅测试使用。
    #[cfg_attr(not(feature = "wasm-runtime"), allow(dead_code))]
    #[allow(clippy::too_many_arguments)] // Guest dispatch carries runtime context and both event channels.
    pub(crate) async fn call_function_json(
        host: HostApi,
        context: AppContext,
        stop: Arc<AtomicBool>,
        sink: Option<Arc<dyn EventSink>>,
        function: (&str, &str),
        settings: super::settings::Settings,
        notification: Option<super::notification::Sender>,
        ocr: Option<super::ocr_bridge::Recognizer>,
        live_switcher: Option<super::live_bridge::Switcher>,
        run_state: Arc<YamlRunState>,
        execution_policy: Option<crate::core::ExecutionPolicy>,
    ) -> Result<Value> {
        let (name, args_json) = function;
        if let Some(policy) = &execution_policy {
            super::private_trial::authorize_external_native(policy, name)?;
        }
        let args: Value = serde_json::from_str(args_json)
            .map_err(|error| anyhow!("函数 {name} 参数不是合法 JSON: {error}"))?;
        let mut host = Self::new(host, context, stop, sink).await?;
        host.run_state = run_state;
        host.settings = settings;
        host.notification = notification;
        host.ocr = ocr;
        host.live_switcher = live_switcher;
        host.execution_policy = execution_policy;
        // 录制输入来源标注：guest 实例线程内执行点（task-local 不跨线程），
        // 在此线程内把 YAML runner 注入的输入标为 "runner"。
        crate::capabilities::adapters::with_caller_input_source("runner", async {
            host.call_function(name, args).await
        })
        .await
    }

    pub(crate) async fn new(
        host: HostApi,
        context: AppContext,
        stop: Arc<AtomicBool>,
        sink: Option<Arc<dyn EventSink>>,
    ) -> Result<Self> {
        let registry = host.registry().clone();
        let device_service = registry
            .device()
            .ok_or_else(|| anyhow!("device capability 未注册"))?;
        let device = device_service
            .resolve(&crate::capabilities::DeviceId::new(
                context.device_id.as_str(),
            ))
            .await
            .map_err(anyhow::Error::new)?;
        Ok(Self {
            execution_policy: None,
            run_state: Arc::new(YamlRunState::new()),
            notification: None,
            ocr: None,
            live_switcher: None,
            settings: super::settings::Settings::default(),
            host,
            registry,
            context,
            device,
            runtime: Arc::new(crate::capabilities::adapters::RuntimeAdapter::new(stop)),
            screen: RwLock::new(FrameSize::new(0, 0)),
            frame_stamp: RwLock::new(None),
            observed_at_ms: RwLock::new(None),
            sink,
        })
    }

    pub(crate) fn with_runtime(mut self, runtime: Arc<dyn RuntimeService>) -> Self {
        self.runtime = runtime;
        self
    }
    pub(crate) fn with_settings(mut self, settings: super::settings::Settings) -> Self {
        self.settings = settings;
        self
    }

    pub(crate) fn with_ocr(mut self, ocr: Option<super::ocr_bridge::Recognizer>) -> Self {
        self.ocr = ocr;
        self
    }
    pub(crate) fn with_live_switcher(
        mut self,
        live_switcher: Option<super::live_bridge::Switcher>,
    ) -> Self {
        self.live_switcher = live_switcher;
        self
    }

    /// 函数派发：查注册表 → 权限 → Schema 绑定 → handler。
    pub(crate) async fn call_function(&self, name: &str, args: Value) -> Result<Value> {
        if let Some(policy) = &self.execution_policy {
            super::private_trial::authorize_external_native(policy, name)?;
        }
        let args = super::settings::bind_timeout(name, args, self.settings.default_timeout_secs);
        let Some(func) = native_function(name) else {
            bail!("未知函数: {name}");
        };
        for permission in func.permissions {
            self.host
                .authorize(*permission)
                .map_err(anyhow::Error::new)?;
        }
        // Point inputs consume a resolved match directly; its normalized center takes
        // precedence over the match's pixel-space bounding-box x/y.
        let mut args = args;
        if matches!(name, "tap" | "press" | "hover") {
            if let Some(position) = args.get_mut("position") {
                if position.get("center").is_some() {
                    let center = position.get("center").unwrap();
                    if point_components(center).is_none() {
                        bail!("点击目标的 center 必须是 0..1 范围内的中心坐标");
                    }
                    let mut center = center.clone();
                    if let Some(stamp) = position.get("_frame") {
                        center["_frame"] = stamp.clone();
                    }
                    if let Some(time) = position.get("_observed_at_ms") {
                        center["_observed_at_ms"] = time.clone();
                    }
                    *position = center;
                }
            }
        }
        let bound = self.bind_args(&func.params, args)?;
        self.emit_event(RuntimeEventKind::Detail {
            name: "effective_args".into(),
            data: json!({"function":name,"args":bound.values}),
        })
        .await;
        if matches!(
            name,
            "tap"
                | "press"
                | "swipe"
                | "key"
                | "input_text"
                | "launch"
                | "stop_app"
                | "reset_input"
                | "tap_template"
        ) {
            self.run_state.goal_verified.store(false, Ordering::Relaxed);
        }
        match name {
            "observe" | "finish" => self.observe(&bound, name == "finish").await,
            "checkpoint" => {
                let result = self.observe(&bound, false).await?;
                if !result.is_null() {
                    let id = bound.string("id")?;
                    self.run_state
                        .completed_checks
                        .write()
                        .unwrap()
                        .insert(id.clone());
                    self.emit_event(RuntimeEventKind::Detail {
                        name: "checkpoint".into(),
                        data: json!({"id":id,"verified":true,"template":bound.string("template")?}),
                    })
                    .await;
                }
                Ok(result)
            }
            "trace" => {
                let enabled = bound.values["enabled"].as_bool().expect("validated bool");
                self.run_state
                    .trace_enabled
                    .store(enabled, Ordering::Relaxed);
                if let Some(frames) = self.registry.frame() {
                    let _ = frames
                        .trace_capture(
                            &self.device,
                            self.run_state
                                .metadata(if enabled { "trace_on" } else { "trace_off" }),
                            true,
                        )
                        .await;
                }
                Ok(Value::Null)
            }
            "fail" => {
                let detail = json!({"code":bound.opt_string("code")?.unwrap_or_else(||"GOAL_FAILED".into()),
                    "stage":bound.opt_string("stage")?,"expected":bound.values.get("expected"),
                    "actual":bound.values.get("actual"),"retryable":bound.values["retryable"],"message":bound.string("message")?});
                self.emit_event(RuntimeEventKind::Detail {
                    name: "failure".into(),
                    data: detail.clone(),
                })
                .await;
                bail!("GOAL_FAILED: {detail}")
            }
            "notify" => self.notify(&bound).await,
            "switch_live_device" => self.switch_live_device(&bound).await,
            "wait_live_confirmation" => self.wait_live_confirmation(&bound).await,
            "tap" => self.tap(&bound).await,
            "hover" => self.hover(&bound).await,
            "swipe" => self.swipe(&bound).await,
            "press" => {
                let duration = bound.duration_ms("duration")?;
                if !(200..=3000).contains(&duration) {
                    bail!("press 时长必须为 200ms..3s");
                }
                let position = bound.values["position"].clone();
                if position["_observed_at_ms"]
                    .as_u64()
                    .is_some_and(|at| self.runtime.now_ms().saturating_sub(at) > 5000)
                {
                    bail!("STALE_OBSERVATION: 长按目标超过 5 秒，须重新观察");
                }
                self.swipe(&BoundArgs {
                    values: JsonMap::from_iter([
                        ("from".into(), position.clone()),
                        ("to".into(), position),
                        ("duration".into(), json!(duration)),
                    ]),
                })
                .await
            }
            "key" => self.key(&bound).await,
            "input_text" => self.input_text(&bound).await,
            "launch" => self.launch(&bound).await,
            "stop_app" => self.stop_app(&bound).await,
            "reset_input" => self.reset_input().await,
            "sleep" => self.sleep(&bound).await,
            "log" => self.log(&bound).await,
            "find" => self.find(&bound).await,
            "ocr" => self.ocr(&bound, false).await,
            "find_text" => self.ocr(&bound, true).await,
            "number" => numeric::number(
                &bound.string("text")?,
                bound.values["index"]
                    .as_u64()
                    .context("number index 必须非负")?,
                &bound.string("format")?,
            ),
            "calculate" => numeric::calculate(
                &bound.string("op")?,
                bound.number("a")?.context("缺少 a")?,
                bound.number("b")?.context("缺少 b")?,
            ),
            "find_any" => self.find_any(&bound).await,
            "wait_find" => self.wait_find(&bound).await,
            "tap_template" => self.tap_template(&bound).await,
            "wait_disappear" => self.wait_disappear(&bound).await,
            "eq" => self.compare(&bound, CompareOp::Eq),
            "ne" => self.compare(&bound, CompareOp::Ne),
            "gt" => self.compare(&bound, CompareOp::Gt),
            "ge" => self.compare(&bound, CompareOp::Ge),
            "lt" => self.compare(&bound, CompareOp::Lt),
            "le" => self.compare(&bound, CompareOp::Le),
            other => bail!("函数 {other} 未实现"),
        }
    }

    /// Schema 绑定：未知参数 / 缺必填 / 类型不符均结构化报错。
    /// 非对象实参 = 位置简写（计划 §1.3，如 `- tap: [0.5, 0.8]`、
    /// `- log: 未进入主页`），绑定到第一个参数。
    fn bind_args(&self, schema: &[ParamSchema], args: Value) -> Result<BoundArgs> {
        let args = match args {
            Value::Null => JsonMap::new(),
            Value::Object(map) => map,
            other => {
                let Some(param) = schema.first() else {
                    bail!("函数参数必须是命名参数对象，得到 {other}");
                };
                JsonMap::from_iter([(param.name.to_string(), other)])
            }
        };
        let mut values = JsonMap::new();
        for param in schema {
            match args.get(param.name) {
                None => {
                    if let Some(default) = &param.default {
                        values.insert(param.name.to_string(), default.clone());
                    } else if param.required {
                        bail!("缺少必填参数 {}", param.name);
                    }
                }
                Some(value) => {
                    check_schema_type(param, value)?;
                    if let Some(item_type) = &param.item_type {
                        for item in value
                            .as_array()
                            .ok_or_else(|| anyhow!("{} 必须是列表", param.name))?
                        {
                            check_schema_type(
                                &ParamSchema {
                                    name: param.name,
                                    ty: *item_type,
                                    required: true,
                                    default: None,
                                    desc: param.desc,
                                    item_type: None,
                                },
                                item,
                            )?;
                        }
                    }
                    values.insert(param.name.to_string(), value.clone());
                }
            }
        }
        for name in args.keys() {
            if !schema.iter().any(|param| param.name == name) {
                bail!("未知参数 {name}");
            }
        }
        Ok(BoundArgs { values })
    }

    // -- handlers ----------------------------------------------------------

    async fn hover(&self, args: &BoundArgs) -> Result<Value> {
        self.refresh_input_screen().await?;
        let position = &args.values["position"];
        ensure!(
            !position["_observed_at_ms"].as_u64().is_some_and(|at| self
                .runtime
                .now_ms()
                .saturating_sub(at)
                > 5000),
            "STALE_OBSERVATION: 悬浮目标超过 5 秒，须重新观察"
        );
        let stamp = match position.get("_frame") {
            Some(stamp) => Some(serde_json::from_value(stamp.clone())?),
            None => self.frame_stamp.read().unwrap().clone(),
        };
        self.registry
            .input()
            .ok_or_else(|| anyhow!("input capability 未注册"))?
            .hover_from_frame(
                &self.device,
                self.touch_point(args.point("position")?)?,
                stamp.as_ref(),
            )
            .await
            .map_err(anyhow::Error::new)?;
        Ok(Value::Null)
    }

    async fn tap(&self, args: &BoundArgs) -> Result<Value> {
        self.refresh_input_screen().await?;
        let point = self.touch_point(args.point("position")?)?;
        *self.observed_at_ms.write().unwrap() = args
            .values
            .get("position")
            .and_then(|p| p.get("_observed_at_ms"))
            .and_then(Value::as_u64);
        if let Some(observed) = *self.observed_at_ms.read().unwrap() {
            if self.runtime.now_ms().saturating_sub(observed) > 5_000 {
                bail!("STALE_OBSERVATION: 点击目标超过 5 秒，须重新观察");
            }
        }
        let stamp = args.values.get("position").and_then(|p| p.get("_frame"));
        if let Some(stamp) = stamp {
            *self.frame_stamp.write().unwrap() = Some(serde_json::from_value(stamp.clone())?);
        }
        self.click_point(point).await?;
        Ok(Value::Null)
    }

    /// All automation clicks pass here once; manual input keeps its existing behavior.
    async fn action_boundary(&self, kind: &str) {
        if self.run_state.trace_enabled.load(Ordering::Relaxed) {
            if let Some(frames) = self.registry.frame() {
                let _ = frames
                    .trace_capture(&self.device, self.run_state.metadata(kind), false)
                    .await;
            }
        }
    }

    async fn click_point(&self, point: TouchPoint) -> Result<()> {
        self.run_state.goal_verified.store(false, Ordering::Relaxed);
        self.action_boundary("action_before").await;
        self.input_delay("click_delay", "before", self.settings.before_click_ms)
            .await?;
        if self
            .observed_at_ms
            .read()
            .unwrap()
            .is_some_and(|observed| self.runtime.now_ms().saturating_sub(observed) > 5_000)
        {
            bail!("STALE_OBSERVATION: 动作延迟后目标已过期，须重新观察");
        }
        let stamp = self.frame_stamp.read().unwrap().clone();
        self.registry
            .input()
            .ok_or_else(|| anyhow!("input capability 未注册"))?
            .tap_from_frame(&self.device, point, stamp.as_ref())
            .await
            .map_err(anyhow::Error::new)?;
        self.action_boundary("action_after").await;
        self.emit_event(RuntimeEventKind::Tap {
            x: point.x(),
            y: point.y(),
        })
        .await;
        self.input_delay("click_delay", "after", self.settings.after_click_ms)
            .await
    }

    async fn input_delay(&self, event: &str, phase: &str, duration_ms: u64) -> Result<()> {
        if self.runtime.cancelled() {
            bail!("CANCELLED: 运行已取消");
        }
        if duration_ms > 0 {
            self.emit_event(RuntimeEventKind::Detail {
                name: event.into(),
                data: json!({"phase":phase,"duration_ms":duration_ms}),
            })
            .await;
        }
        let mut remaining = duration_ms;
        while remaining > 0 {
            let slice = remaining.min(50);
            self.runtime
                .sleep(Duration::from_millis(slice))
                .await
                .map_err(anyhow::Error::new)?;
            remaining -= slice;
        }
        if self.runtime.cancelled() {
            bail!("CANCELLED: 运行已取消");
        }
        Ok(())
    }

    async fn swipe(&self, args: &BoundArgs) -> Result<Value> {
        self.refresh_input_screen().await?;
        let from = self.touch_point(args.point("from")?)?;
        let to = self.touch_point(args.point("to")?)?;
        let duration = args.duration_ms("duration")?;
        let stamps = ["from", "to"]
            .into_iter()
            .filter_map(|name| args.values.get(name).and_then(|v| v.get("_frame")))
            .map(|v| serde_json::from_value::<crate::capabilities::FrameStamp>(v.clone()))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if stamps.len() == 2 && stamps[0] != stamps[1] {
            bail!("滑动起止点来自不同画面")
        }
        let current_stamp = self.frame_stamp.read().unwrap().clone();
        self.action_boundary("action_before").await;
        self.registry
            .input()
            .ok_or_else(|| anyhow!("input capability 未注册"))?
            .swipe_from_frame(
                &self.device,
                SwipeGesture::new(from, to, Duration::from_millis(duration)),
                stamps.first().or(current_stamp.as_ref()),
            )
            .await
            .map_err(anyhow::Error::new)?;
        self.action_boundary("action_after").await;
        self.emit_event(RuntimeEventKind::Swipe {
            x1: from.x(),
            y1: from.y(),
            x2: to.x(),
            y2: to.y(),
        })
        .await;
        Ok(Value::Null)
    }

    async fn key(&self, args: &BoundArgs) -> Result<Value> {
        let key = args.string("key")?;

        let action = match args.opt_string("action")?.unwrap_or_else(|| "press".into()) {
            value if value == "down" => KeyAction::Down,
            value if value == "up" => KeyAction::Up,
            value if value == "press" => KeyAction::Press,
            other => bail!("未知 key action: {other}"),
        };
        self.action_boundary("action_before").await;
        self.input_delay("key_delay", "before", self.settings.before_click_ms)
            .await?;
        self.registry
            .input()
            .ok_or_else(|| anyhow!("input capability 未注册"))?
            .key_named(&self.device, &key, action)
            .await
            .map_err(anyhow::Error::new)?;
        self.action_boundary("action_after").await;
        self.input_delay("key_delay", "after", self.settings.after_click_ms)
            .await?;
        Ok(Value::Null)
    }

    async fn input_text(&self, args: &BoundArgs) -> Result<Value> {
        let text = args.string("text")?;
        self.registry
            .input()
            .ok_or_else(|| anyhow!("input capability 未注册"))?
            .text(&self.device, TextInput::new(&text))
            .await
            .map_err(anyhow::Error::new)?;
        Ok(Value::Null)
    }

    async fn launch(&self, args: &BoundArgs) -> Result<Value> {
        let package = self.package(args)?;
        self.registry
            .device()
            .ok_or_else(|| anyhow!("device capability 未注册"))?
            .start_app(&self.device, &AppId::new(format!("+{package}")))
            .await
            .map_err(anyhow::Error::new)?;
        Ok(Value::Null)
    }

    async fn stop_app(&self, args: &BoundArgs) -> Result<Value> {
        let package = self.package(args)?;
        self.registry
            .device()
            .ok_or_else(|| anyhow!("device capability 未注册"))?
            .stop_app(&self.device, &AppId::new(package))
            .await
            .map_err(anyhow::Error::new)?;
        Ok(Value::Null)
    }

    async fn reset_input(&self) -> Result<Value> {
        let app = self
            .context
            .android_package
            .as_ref()
            .map(|name| AppId::new(name.as_str()));
        self.registry
            .device()
            .ok_or_else(|| anyhow!("device capability 未注册"))?
            .reset_input(&self.device, app.as_ref())
            .await
            .map_err(anyhow::Error::new)?;
        *self.frame_stamp.write().unwrap() = None;
        *self.observed_at_ms.write().unwrap() = None;
        Ok(Value::Null)
    }

    async fn sleep(&self, args: &BoundArgs) -> Result<Value> {
        let duration = args.duration_ms("duration")?.min(MAX_SLEEP_MS);
        // Keep one sleep request (including offline virtual-clock semantics),
        // but do not hold a cancelled run's lease for an hour-long host wait.
        // Epoch interruption cannot interrupt a capability future in progress.
        tokio::select! {
            result = self.runtime.sleep(Duration::from_millis(duration)) => {
                result.map_err(anyhow::Error::new)?;
            }
            _ = async {
                loop {
                    if self.runtime.cancelled() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            } => return Err(anyhow::Error::new(crate::capabilities::CapabilityError::Cancelled)),
        }
        Ok(Value::Null)
    }

    async fn notify(&self, args: &BoundArgs) -> Result<Value> {
        if self.runtime.cancelled() {
            return Err(anyhow::Error::new(
                crate::capabilities::CapabilityError::Cancelled,
            ));
        }
        let mut values = json!({"content":args.string("content")?, "title":args.opt_string("title")?.unwrap_or_default(), "channel":args.opt_string("channel")?.filter(|s| !s.is_empty()), "target_id": self.context.device_id.as_str()});
        let when = args.string("when")?;
        ensure!(
            matches!(when.as_str(), "immediate" | "success" | "failure"),
            "notify when 必须是 immediate/success/failure"
        );
        let enabled = args.values["enabled"].as_bool().expect("validated bool");
        if when != "immediate" {
            let trace = self.run_state.trace_context.read().unwrap().clone();
            let frame = trace["frame_id"].as_u64().unwrap_or(0);
            if let (Some(package), Some(path)) = (
                trace["source"]["package_id"].as_str(),
                trace["source"]["path"]
                    .as_str()
                    .and_then(|path| path.strip_prefix("automations/")),
            ) {
                values["result_scope"] =
                    json!({"package_id":package,"entrypoint":format!("{package}/{path}")});
            }
            let result = self
                .run_state
                .result_notifications
                .register(frame, &when, enabled, values);
            self.emit_event(RuntimeEventKind::Detail {
                name: "notification_policy".into(),
                data: result.clone(),
            })
            .await;
            return Ok(result);
        }
        if !enabled {
            return Ok(super::notification::skipped("此通知已关闭"));
        }
        let result = match &self.notification {
            Some(sender) => sender(values).await,
            None => super::notification::skipped("通知发送能力不可用"),
        };
        self.emit_event(RuntimeEventKind::Detail {
            name: "notification".into(),
            data: result.clone(),
        })
        .await;
        if result["accepted"] != true && self.host.authorize(Permission::LogWrite).is_ok() {
            let _ = self.log(&BoundArgs { values: serde_json::from_value(json!({"message":format!("通知未发送：{}", result["reason"].as_str().unwrap_or("能力不可用")), "level":"warn"}))? }).await;
        }
        Ok(result)
    }
    async fn switch_live_device(&self, args: &BoundArgs) -> Result<Value> {
        ensure!(!self.runtime.cancelled(), "CANCELLED");
        let switcher = self
            .live_switcher
            .as_ref()
            .ok_or_else(|| anyhow!("LIVE_UNAVAILABLE: 请安装并启用直播助手"))?;
        let timeout = args.duration_ms("timeout")?;
        ensure!(
            (1..=60_000).contains(&timeout),
            "直播切换 timeout 必须在 1ms..60s"
        );
        let device = args
            .opt_string("device_id")?
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| self.device.id().as_str().to_owned());
        let operation = switcher("stream.switch", json!({"device_id":device}));
        tokio::pin!(operation);
        let deadline = tokio::time::sleep(Duration::from_millis(timeout));
        tokio::pin!(deadline);
        loop {
            ensure!(!self.runtime.cancelled(), "CANCELLED");
            tokio::select! {
                result = &mut operation => return result,
                _ = &mut deadline => bail!("LIVE_SWITCH_TIMEOUT: 等待直播输出切换超时"),
                _ = tokio::time::sleep(Duration::from_millis(25)) => {}
            }
        }
    }

    async fn live_confirmation_action(&self, action: &'static str, values: Value) -> Result<Value> {
        ensure!(!self.runtime.cancelled(), "CANCELLED");
        let bridge = self
            .live_switcher
            .as_ref()
            .ok_or_else(|| anyhow!("LIVE_UNAVAILABLE: 请安装并启用直播助手"))?;
        let operation = bridge(action, values);
        tokio::pin!(operation);
        // Only short requests hold an extension lifecycle read lease. Waiting
        // for the viewer happens here, outside any live-plugin call/queue lock.
        let deadline = tokio::time::sleep(Duration::from_secs(2));
        tokio::pin!(deadline);
        loop {
            ensure!(!self.runtime.cancelled(), "CANCELLED");
            tokio::select! {
                result = &mut operation => {
                    ensure!(!self.runtime.cancelled(), "CANCELLED");
                    return result.map_err(|error| anyhow!("LIVE_CONFIRMATION_UNAVAILABLE: {error}"));
                }
                _ = &mut deadline => bail!("LIVE_CONFIRMATION_UNAVAILABLE: 直播确认状态读取超时"),
                _ = tokio::time::sleep(Duration::from_millis(25)) => {}
            }
        }
    }

    async fn wait_live_confirmation(&self, args: &BoundArgs) -> Result<Value> {
        ensure!(!self.runtime.cancelled(), "CANCELLED");
        let timeout_ms = args.duration_ms("timeout")?;
        ensure!(
            (1..=300_000).contains(&timeout_ms),
            "直播确认 timeout 必须在 1ms..5m"
        );
        let timeout_secs = timeout_ms.div_ceil(1000);
        let mut result = self
            .live_confirmation_action("confirmation.begin", json!({"timeout_secs": timeout_secs}))
            .await?;
        // The queue owns and persists the actual deadline (possibly shorter
        // than this request). Never manufacture a timeout decision locally.
        let unavailable_at = tokio::time::Instant::now() + Duration::from_secs(timeout_secs + 2);
        loop {
            ensure!(!self.runtime.cancelled(), "CANCELLED");
            match result["status"].as_str() {
                Some(status @ ("confirmed" | "cancelled" | "timeout")) => {
                    return Ok(json!({"status": status}));
                }
                Some("waiting" | "awaiting_confirmation") => {}
                Some("disconnected") => bail!("LIVE_CONFIRMATION_DISCONNECTED: 直播互动连接已中断"),
                _ => bail!("LIVE_CONFIRMATION_INVALID_STATE: 直播确认状态无效"),
            }
            ensure!(
                tokio::time::Instant::now() < unavailable_at,
                "LIVE_CONFIRMATION_UNAVAILABLE: 直播队列未按期限结束确认等待"
            );
            let next_poll = tokio::time::sleep(Duration::from_millis(250));
            tokio::pin!(next_poll);
            loop {
                ensure!(!self.runtime.cancelled(), "CANCELLED");
                tokio::select! {
                    _ = &mut next_poll => break,
                    _ = tokio::time::sleep(Duration::from_millis(25)) => {}
                }
            }
            result = self
                .live_confirmation_action("confirmation.poll", json!({}))
                .await?;
        }
    }

    async fn log(&self, args: &BoundArgs) -> Result<Value> {
        let level = match args.opt_string("level")?.unwrap_or_else(|| "info".into()) {
            value if value == "trace" => LogLevel::Trace,
            value if value == "debug" => LogLevel::Debug,
            value if value == "info" => LogLevel::Info,
            value if value == "warn" || value == "warning" => LogLevel::Warn,
            value if value == "error" => LogLevel::Error,
            other => bail!("未知 log level: {other}"),
        };
        let message = args
            .values
            .get("message")
            .map(message_to_text)
            .ok_or_else(|| anyhow!("缺少必填参数 message"))?;
        self.registry
            .log()
            .ok_or_else(|| anyhow!("log capability 未注册"))?
            .write(LogRecord::new(level, &message).with_device(self.device.clone()))
            .map_err(anyhow::Error::new)?;
        self.emit_event(RuntimeEventKind::Detail { name: "log".into(), data: serde_json::json!({"level": args.opt_string("level")?.unwrap_or_else(|| "info".into()), "message": message}) }).await;
        Ok(Value::Null)
    }

    /// find：单次模板匹配（简化计划 Phase 4.2：不再承担轮询语义）；未找到
    /// 返回 null。等待轮询请用 wait_find。每次尝试发 vision/hit/miss 事件
    /// （与 wait_find 同口径）。
    async fn find(&self, args: &BoundArgs) -> Result<Value> {
        let template_name = args.string("template")?;
        let (outcome, effective_px) = self.match_once(args).await?;
        let region =
            Self::relative_region_echo(effective_px, self.screen().width, self.screen().height);
        self.emit_vision_outcome(&template_name, outcome, effective_px)
            .await;
        Ok(match outcome {
            MatchOutcome::Found(_) => self.match_value(outcome, region, self.screen()),
            MatchOutcome::NotFound => Value::Null,
        })
    }

    async fn observe(&self, args: &BoundArgs, finish: bool) -> Result<Value> {
        let timeout = args.duration_ms("timeout")?;
        if timeout > MAX_VISUAL_WAIT_MS {
            bail!("等待超出有限预算 7200000ms");
        }
        let threshold = args.number("threshold")?.unwrap_or(0.8);
        if !(0.0..=1.0).contains(&threshold) {
            bail!("threshold 必须在 0..1");
        }
        let result = self.poll_match(args, timeout, "observe").await?;
        match result {
            Some(value) => {
                if finish {
                    self.run_state.goal_verified.store(true, Ordering::Relaxed);
                }
                Ok(value)
            }
            None if finish || args.values["required"].as_bool().unwrap_or(true) => bail!(
                "VISUAL_TIMEOUT: 必需视觉目标 {} 在 {}ms 内未出现",
                args.string("template")?,
                timeout
            ),
            None => Ok(Value::Null),
        }
    }

    async fn wait_find(&self, args: &BoundArgs) -> Result<Value> {
        let click = args.values.get("click").and_then(Value::as_bool) == Some(true);
        if click
            || args
                .values
                .get("obstacles")
                .and_then(Value::as_array)
                .is_some_and(|v| !v.is_empty())
        {
            // 仅等待不需要输入权限；自动点击在开始轮询前校验。
            self.host
                .authorize(Permission::InputTap)
                .map_err(anyhow::Error::new)?;
        }
        let timeout = args.duration_ms("timeout")?;
        if !args
            .values
            .get("forever")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && timeout > MAX_VISUAL_WAIT_MS
        {
            bail!("WAIT_BUDGET_EXCEEDED: timeout 超出 7200000ms");
        }
        let Some(matched) = self.poll_match(args, timeout, "wait_find").await? else {
            return Ok(Value::Null);
        };
        if click {
            self.tap_match(&matched).await?;
        }
        Ok(matched)
    }

    async fn tap_template(&self, args: &BoundArgs) -> Result<Value> {
        let timeout = args.duration_ms("timeout")?;
        if timeout > MAX_VISUAL_WAIT_MS {
            bail!("WAIT_BUDGET_EXCEEDED: timeout 超出 7200000ms");
        }
        let Some(matched) = self.poll_match(args, timeout, "tap_template").await? else {
            return Ok(Value::Null);
        };
        self.tap_match(&matched).await?;
        Ok(matched)
    }

    async fn tap_match(&self, matched: &Value) -> Result<()> {
        let Some(center) = matched.get("center").and_then(point_components) else {
            bail!("模板匹配结果缺少 center");
        };
        self.click_point(self.touch_point(center)?).await
    }

    async fn wait_disappear(&self, args: &BoundArgs) -> Result<Value> {
        let timeout = args.duration_ms("timeout")?;
        if timeout > MAX_VISUAL_WAIT_MS {
            bail!("WAIT_BUDGET_EXCEEDED: timeout 超出 7200000ms");
        }
        let interval = args.duration_ms("interval")?.max(MIN_POLL_INTERVAL_MS);
        let started = self.runtime.now_ms();
        loop {
            let (outcome, _) = self.match_once(args).await?;
            let found = matches!(outcome, MatchOutcome::Found(_));
            if !found {
                return Ok(Value::Bool(true));
            }
            if self.runtime.now_ms().saturating_sub(started) >= timeout {
                return Ok(Value::Bool(false));
            }
            self.runtime
                .sleep(Duration::from_millis(interval.min(MAX_SLEEP_MS).min(
                    timeout.saturating_sub(self.runtime.now_ms().saturating_sub(started)),
                )))
                .await
                .map_err(anyhow::Error::new)?;
        }
    }

    fn compare(&self, args: &BoundArgs, op: CompareOp) -> Result<Value> {
        let a = args
            .values
            .get("a")
            .cloned()
            .ok_or_else(|| anyhow!("缺少必填参数 a"))?;
        let b = args
            .values
            .get("b")
            .cloned()
            .ok_or_else(|| anyhow!("缺少必填参数 b"))?;
        let result = match op {
            CompareOp::Eq => json_equals(&a, &b),
            CompareOp::Ne => !json_equals(&a, &b),
            CompareOp::Gt => numeric_pair(&a, &b).is_some_and(|(a, b)| a > b),
            CompareOp::Ge => numeric_pair(&a, &b).is_some_and(|(a, b)| a >= b),
            CompareOp::Lt => numeric_pair(&a, &b).is_some_and(|(a, b)| a < b),
            CompareOp::Le => numeric_pair(&a, &b).is_some_and(|(a, b)| a <= b),
        };
        Ok(Value::Bool(result))
    }

    // -- 视觉轮询 -----------------------------------------------------------

    /// 单次截图匹配（不发事件）；返回结果与本次生效的像素搜索区域。
    async fn match_once(&self, args: &BoundArgs) -> Result<(MatchOutcome, Option<[u32; 4]>)> {
        let frame = self.capture().await?;
        self.match_on_frame(args, frame).await
    }

    async fn match_on_frame(
        &self,
        args: &BoundArgs,
        frame: crate::capabilities::FrameHandle,
    ) -> Result<(MatchOutcome, Option<[u32; 4]>)> {
        let outcome = self.match_on_frame_inner(args, frame).await;
        if let Some(frames) = self.registry.frame() {
            let mut metadata = self.run_state.metadata("consumed");
            if let Err(error) = &outcome {
                metadata["error"] = json!(error.to_string());
            }
            let _ = frames.trace_frame(frame, metadata, false).await;
        }
        outcome
    }

    async fn match_on_frame_inner(
        &self,
        args: &BoundArgs,
        frame: crate::capabilities::FrameHandle,
    ) -> Result<(MatchOutcome, Option<[u32; 4]>)> {
        let template_name = args.string("template")?;
        let template = self.template(&template_name).await?;
        let threshold = args.number("threshold")?;
        let explicit_px = args
            .region("region")?
            .map(|region| self.pixel_region(region));
        let template_file = self.template_file_name(&template).await;
        let effective_px = crate::matcher::effective_search_region(
            explicit_px,
            template_file.as_deref(),
            self.screen().width,
            self.screen().height,
        );
        let options = MatchOptions {
            threshold: threshold.map(|value| value as f32),
            region: effective_px
                .map(|[x, y, width, height]| SearchRegion::new(x, y, width, height)),
            color_check: false,
        };
        let outcome = self
            .registry
            .vision()
            .ok_or_else(|| anyhow!("vision capability 未注册"))?
            .match_template(frame, TemplateQuery::new(template, options))
            .await
            .map_err(anyhow::Error::new)?;
        let _ = template_name;
        Ok((outcome, effective_px))
    }

    /// 顺序匹配共用同一帧，首个命中即返回；不产生输入操作。
    async fn find_any(&self, args: &BoundArgs) -> Result<Value> {
        let templates = args
            .values
            .get("templates")
            .and_then(Value::as_array)
            .expect("Schema validated list");
        if templates.is_empty() || templates.len() > 64 {
            bail!("templates 必须包含 1..64 项");
        }
        if !args
            .number("threshold")?
            .is_some_and(|v| (0.0..=1.0).contains(&v))
        {
            bail!("threshold 必须为 0..1 数字");
        }
        let frame = self.capture().await?;
        for (index, template) in templates.iter().enumerate() {
            if self.runtime.cancelled() {
                bail!("CANCELLED");
            }
            let mut values = args.values.clone();
            values.insert("template".into(), template.clone());
            let (outcome, effective_px) = self.match_on_frame(&BoundArgs { values }, frame).await?;
            self.emit_vision_outcome(
                template.as_str().expect("Schema validated template"),
                outcome,
                effective_px,
            )
            .await;
            if matches!(outcome, MatchOutcome::Found(_)) {
                let region = Self::relative_region_echo(
                    effective_px,
                    self.screen().width,
                    self.screen().height,
                );
                let mut result = self.match_value(outcome, region, self.screen());
                result["index"] = json!(index);
                result["template"] = template.clone();
                return Ok(result);
            }
        }
        Ok(Value::Null)
    }

    /// wait_find/tap_template 共用轮询及 match_on_frame 匹配语义。
    async fn poll_match(
        &self,
        args: &BoundArgs,
        timeout_ms: u64,
        _fn_name: &str,
    ) -> Result<Option<Value>> {
        let interval = args.duration_ms("interval")?.max(MIN_POLL_INTERVAL_MS);
        let template_name = args.string("template")?;
        let started = self.runtime.now_ms();
        let obstacles = args
            .values
            .get("obstacles")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let forever = args
            .values
            .get("forever")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        loop {
            if self.runtime.cancelled() {
                bail!("CANCELLED");
            }
            let frame = self.capture().await?;
            let mut cleared = false;
            for obstacle in &obstacles {
                let mut values = args.values.clone();
                values.insert("template".into(), obstacle.clone());
                values.remove("region"); // 障碍按自己的模板区域搜索，不继承目标区域。
                let obstacle_args = BoundArgs { values };
                let (outcome, effective_px) = self.match_on_frame(&obstacle_args, frame).await?;
                self.emit_vision_outcome(
                    obstacle.as_str().expect("Schema 已校验模板列表"),
                    outcome,
                    effective_px,
                )
                .await;
                if matches!(outcome, MatchOutcome::Found(_)) {
                    let region = Self::relative_region_echo(
                        effective_px,
                        self.screen().width,
                        self.screen().height,
                    );
                    self.tap_match(&self.match_value(outcome, region, self.screen()))
                        .await?;
                    cleared = true;
                    break;
                }
                if !forever
                    && timeout_ms > 0
                    && self.runtime.now_ms().saturating_sub(started) >= timeout_ms
                {
                    return Ok(None);
                }
                if self.runtime.cancelled() {
                    bail!("CANCELLED");
                }
            }
            let (outcome, effective_px) = if cleared {
                (MatchOutcome::NotFound, None)
            } else {
                self.match_on_frame(args, frame).await?
            };
            let region =
                Self::relative_region_echo(effective_px, self.screen().width, self.screen().height);
            if !cleared {
                self.emit_vision_outcome(&template_name, outcome, effective_px)
                    .await;
            }
            if let MatchOutcome::Found(_) = outcome {
                return Ok(Some(self.match_value(outcome, region, self.screen())));
            }
            if !forever && self.runtime.now_ms().saturating_sub(started) >= timeout_ms {
                return Ok(None);
            }
            self.runtime
                .sleep(Duration::from_millis(if forever {
                    interval.min(MAX_SLEEP_MS)
                } else {
                    interval.min(MAX_SLEEP_MS).min(
                        timeout_ms.saturating_sub(self.runtime.now_ms().saturating_sub(started)),
                    )
                }))
                .await
                .map_err(anyhow::Error::new)?;
            // Observe at the deadline as well; only a failed observation at
            // that time is a timeout. All time comes from the runtime clock.
        }
    }

    // -- 共用辅助 -----------------------------------------------------------

    fn package(&self, args: &BoundArgs) -> Result<String> {
        match args.opt_string("package")? {
            Some(value) if !value.trim().is_empty() => Ok(value),
            _ => self
                .context
                .android_package
                .as_ref()
                .map(|p| p.as_str().to_string())
                .ok_or_else(|| anyhow!("当前目标不支持 Android 应用操作")),
        }
    }

    fn touch_point(&self, relative: [f64; 2]) -> Result<TouchPoint> {
        let [x, y] = relative;
        if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
            bail!("point 坐标超出 0..1");
        }
        let size = self.screen();
        if size.width == 0 || size.height == 0 {
            bail!("设备画面尺寸尚未就绪，无法转换点击坐标");
        }
        Ok(TouchPoint::new(
            (x * size.width as f64).round() as u32,
            (y * size.height as f64).round() as u32,
            1.0,
        ))
    }

    fn pixel_region(&self, relative: [f64; 4]) -> [u32; 4] {
        let [x, y, width, height] = relative;
        let screen = self.screen();
        [
            (x * screen.width as f64).round() as u32,
            (y * screen.height as f64).round() as u32,
            (width * screen.width as f64).round() as u32,
            (height * screen.height as f64).round() as u32,
        ]
    }

    async fn template(&self, name: &str) -> Result<crate::capabilities::ResourceHandle> {
        self.host
            .authorize(Permission::ResourceRead)
            .map_err(anyhow::Error::new)?;
        let package = self
            .context
            .content_package
            .as_ref()
            .ok_or_else(|| anyhow!("当前上下文没有 content package"))?;
        let mut cache = self.run_state.templates.lock().await;
        if let Some(handle) = cache.get(name) {
            return Ok(*handle);
        }
        let private = self.run_state.private_templates.read().unwrap().clone();
        if let Some(templates) = private {
            let filename = crate::resources::resolve_short_relative_path(
                name,
                templates.keys().map(String::as_str),
            )?
            .to_string();
            let resources = self
                .registry
                .resource()
                .ok_or_else(|| anyhow!("resource capability 未注册"))?;
            let id = ResourceId::new(
                package.as_str().to_string(),
                YAML_EXTENSION_ID,
                format!("templates/{filename}"),
            )?;
            let handle = resources
                .freeze_bytes(&id, templates[&filename].clone())
                .await?;
            *self.run_state.cleanup_registry.lock().unwrap() = Some(self.registry.clone());
            cache.insert(name.to_string(), handle);
            return Ok(handle);
        }
        let resource = self
            .registry
            .resource()
            .ok_or_else(|| anyhow!("resource capability 未注册"))?
            .resolve(
                &ResourceId::new(
                    package.as_str().to_string(),
                    YAML_EXTENSION_ID,
                    format!("templates/{name}"),
                )
                .map_err(anyhow::Error::new)?,
            )
            .await
            .map_err(anyhow::Error::new)?;
        let resources = self.registry.resource().unwrap();
        let expected = self.run_state.template_versions.read().unwrap().clone();
        let resource = if !self
            .run_state
            .enforce_template_versions
            .load(Ordering::Relaxed)
        {
            resource
        } else {
            let frozen = resources
                .freeze(resource)
                .await
                .map_err(anyhow::Error::new)?;
            let filename = resources
                .resolved_path(frozen)
                .await
                .map_err(anyhow::Error::new)?;
            let digest = resources
                .fingerprint(frozen)
                .await
                .map_err(anyhow::Error::new)?;
            if expected.get(&filename) != Some(&digest) {
                resources.release_frozen(frozen);
                bail!("RESOURCE_CHANGED: 模板 {name} 在运行开始后变化，或未包含在冻结版本中");
            }
            frozen
        };
        *self.run_state.cleanup_registry.lock().unwrap() = Some(self.registry.clone());
        cache.insert(name.to_string(), resource);
        Ok(resource)
    }

    async fn capture(&self) -> Result<crate::capabilities::FrameHandle> {
        let frame = self
            .registry
            .frame()
            .ok_or_else(|| anyhow!("frame capability 未注册"))?
            .capture(&self.device)
            .await
            .map_err(anyhow::Error::new)?;
        self.refresh_screen(&frame).await?;
        *self.frame_stamp.write().unwrap() = self
            .registry
            .frame()
            .unwrap()
            .stamp(frame)
            .await
            .map_err(anyhow::Error::new)?;
        Ok(frame)
    }

    /// Each __fn call creates a host. Input must read the active coordinate
    /// space itself, including taps before any find and rotation between calls.
    async fn refresh_input_screen(&self) -> Result<()> {
        let (size, stamp) = self
            .registry
            .frame()
            .ok_or_else(|| anyhow!("frame capability 未注册"))?
            .coordinate_space(&self.device)
            .await
            .map_err(anyhow::Error::new)?;
        *self.frame_stamp.write().unwrap() = stamp;
        self.set_screen(size)
    }

    /// Vision results use the exact captured frame's dimensions.
    async fn refresh_screen(&self, frame: &crate::capabilities::FrameHandle) -> Result<()> {
        let size = self
            .registry
            .frame()
            .ok_or_else(|| anyhow!("frame capability 未注册"))?
            .size(*frame)
            .await
            .map_err(anyhow::Error::new)?;
        self.set_screen(size)
    }

    fn set_screen(&self, size: FrameSize) -> Result<()> {
        if size.width == 0 || size.height == 0 {
            bail!("设备画面尺寸尚未就绪");
        }
        *self.screen.write().unwrap() = size;
        Ok(())
    }

    fn screen(&self) -> FrameSize {
        *self.screen.read().unwrap()
    }

    /// 模板 handle → 解析后的实际文件名（`#` 后缀区域推断用；失败 = None）。
    async fn template_file_name(
        &self,
        handle: &crate::capabilities::ResourceHandle,
    ) -> Option<String> {
        self.registry
            .resource()?
            .resolved_file_name(*handle)
            .await
            .ok()
    }

    fn match_value(&self, outcome: MatchOutcome, region: Value, screen: FrameSize) -> Value {
        match outcome {
            MatchOutcome::Found(found) => {
                *self.observed_at_ms.write().unwrap() = Some(self.runtime.now_ms());
                let center = [
                    (found.x + found.width / 2) as f64 / screen.width as f64,
                    (found.y + found.height / 2) as f64 / screen.height as f64,
                ];
                let mut value = json!({
                    "x": found.x,
                    "y": found.y,
                    "width": found.width,
                    "height": found.height,
                    "score": found.score,
                    "center": { "x": center[0], "y": center[1] },
                    "region": region,
                });
                if let Some(stamp) = self.frame_stamp.read().unwrap().clone() {
                    value["_frame"] = json!(stamp);
                    value["center"]["_frame"] = json!(stamp);
                    value["_observed_at_ms"] = json!(self.runtime.now_ms());
                    value["center"]["_observed_at_ms"] = json!(self.runtime.now_ms());
                }
                value
            }
            MatchOutcome::NotFound => Value::Null,
        }
    }

    /// 本次实际搜索区域的回显值（相对坐标 map）。
    fn relative_region_echo(px: Option<[u32; 4]>, w: u32, h: u32) -> Value {
        let (x, y, width, height) = match px {
            Some([x, y, width, height]) => (x, y, width, height),
            None => (0, 0, w, h),
        };
        json!({
            "x": x as f64 / w as f64,
            "y": y as f64 / h as f64,
            "width": width as f64 / w as f64,
            "height": height as f64 / h as f64,
        })
    }

    /// 尽力而为的事件旁路：发射失败只记 debug，不影响能力执行结果。
    async fn emit_event(&self, kind: RuntimeEventKind) {
        let Some(sink) = &self.sink else {
            return;
        };
        if let Err(error) = sink
            .emit(RuntimeEvent::new(self.context.device_id.clone(), kind))
            .await
        {
            tracing::debug!(%error, "yaml runtime event emit failed");
        }
    }

    /// vision 结果可视化旁路：`vision` 结构事件 + `hit`/`miss` 投屏标记。
    async fn emit_vision_outcome(
        &self,
        template: &str,
        outcome: MatchOutcome,
        region_px: Option<[u32; 4]>,
    ) {
        let region_px = region_px.unwrap_or([0, 0, self.screen().width, self.screen().height]);
        match outcome {
            MatchOutcome::Found(found) => {
                let center = [
                    (found.x + found.width / 2) as f64 / self.screen().width as f64,
                    (found.y + found.height / 2) as f64 / self.screen().height as f64,
                ];
                self.emit_event(RuntimeEventKind::Vision {
                    template: template.to_string(),
                    found: true,
                    score: Some(found.score),
                    center: Some(center),
                })
                .await;
                self.emit_event(RuntimeEventKind::Hit {
                    tpl: template.to_string(),
                    x: found.x,
                    y: found.y,
                    w: found.width,
                    h: found.height,
                    score: found.score,
                })
                .await;
            }
            MatchOutcome::NotFound => {
                self.emit_event(RuntimeEventKind::Vision {
                    template: template.to_string(),
                    found: false,
                    score: None,
                    center: None,
                })
                .await;
                self.emit_event(RuntimeEventKind::Miss {
                    tpl: template.to_string(),
                    x: region_px[0],
                    y: region_px[1],
                    w: region_px[2],
                    h: region_px[3],
                })
                .await;
            }
        }
    }
}

#[derive(Clone, Copy)]
enum CompareOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

/// Schema 类型检查（绑定；值为规整前原文）。
fn check_schema_type(param: &ParamSchema, value: &Value) -> Result<()> {
    let ok = |condition: bool| {
        if condition {
            Ok(())
        } else {
            Err(anyhow!(
                "参数 {} 与类型 {} 不符，得到 {value}",
                param.name,
                param.ty.canonical()
            ))
        }
    };
    match param.ty {
        ParamType::Any => Ok(()),
        ParamType::Boolean => ok(value.is_boolean()),
        ParamType::Integer => ok(value.is_i64() || value.is_u64()),
        ParamType::Number => ok(value.is_number()),
        ParamType::String | ParamType::Template | ParamType::Key => {
            ok(value.as_str().is_some_and(|text| !text.trim().is_empty()))
        }
        ParamType::List => ok(value.is_array()),
        ParamType::Object => ok(value.is_object()),
        ParamType::Duration => ok(match value {
            Value::Number(number) => number.as_f64().is_some_and(|ms| ms >= 0.0),
            Value::String(text) => parse_duration_ms(text).is_some(),
            _ => false,
        }),
        ParamType::Point => ok(point_components(value).is_some()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::capabilities::{
        CapabilityResult, ColorSample, FrameHandle, FrameService, LogRecord, LogService,
        MatchManyRequest, MatchManyResult, ResourceHandle, ResourceId, ResourceLease,
        ResourceService, VisionService,
    };
    use crate::extensions::HostApiCatalog;
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    /// device+input 记录桩。
    #[derive(Default)]
    pub(crate) struct Trace {
        pub(crate) text: Mutex<Vec<String>>,
        pub(crate) taps: Mutex<Vec<[u32; 2]>>,
        pub(crate) apps: Mutex<Vec<(String, String)>>,
        pub(crate) swipes: Mutex<Vec<SwipeGesture>>,
        pub(crate) keys: Mutex<Vec<KeyInput>>,
        expected_stamp: Mutex<Option<crate::capabilities::FrameStamp>>,
    }

    #[async_trait]
    impl crate::capabilities::DeviceService for Trace {
        async fn resolve(
            &self,
            id: &crate::capabilities::DeviceId,
        ) -> CapabilityResult<DeviceHandle> {
            Ok(DeviceHandle::new(id.clone()))
        }

        async fn start_app(&self, _: &DeviceHandle, app: &AppId) -> CapabilityResult<()> {
            self.apps
                .lock()
                .unwrap()
                .push(("launch".into(), app.as_str().into()));
            Ok(())
        }

        async fn stop_app(&self, _: &DeviceHandle, app: &AppId) -> CapabilityResult<()> {
            self.apps
                .lock()
                .unwrap()
                .push(("stop_app".into(), app.as_str().into()));
            Ok(())
        }

        async fn reset_input(&self, _: &DeviceHandle, app: Option<&AppId>) -> CapabilityResult<()> {
            self.apps.lock().unwrap().push((
                "reset_input".into(),
                app.map(|app| app.as_str()).unwrap_or_default().into(),
            ));
            Ok(())
        }
    }

    #[async_trait]
    impl crate::capabilities::InputService for Trace {
        async fn tap_from_frame(
            &self,
            device: &DeviceHandle,
            point: TouchPoint,
            stamp: Option<&crate::capabilities::FrameStamp>,
        ) -> CapabilityResult<()> {
            if stamp.is_some() && stamp != self.expected_stamp.lock().unwrap().as_ref() {
                return Err(crate::capabilities::CapabilityError::InvalidRequest(
                    "STALE_FRAME".into(),
                ));
            }
            self.tap(device, point).await
        }
        async fn tap(&self, _: &DeviceHandle, point: TouchPoint) -> CapabilityResult<()> {
            self.taps.lock().unwrap().push([point.x(), point.y()]);
            Ok(())
        }

        async fn swipe(&self, _: &DeviceHandle, gesture: SwipeGesture) -> CapabilityResult<()> {
            self.swipes.lock().unwrap().push(gesture);
            Ok(())
        }

        async fn key(&self, _: &DeviceHandle, key: KeyInput) -> CapabilityResult<()> {
            self.keys.lock().unwrap().push(key);
            Ok(())
        }

        async fn text(&self, _: &DeviceHandle, value: TextInput) -> CapabilityResult<()> {
            self.text.lock().unwrap().push(value.as_str().to_string());
            Ok(())
        }
    }

    pub(crate) struct LogTrace {
        logs: Mutex<Vec<(String, String)>>,
        devices: Mutex<Vec<String>>,
    }

    impl LogTrace {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self {
                logs: Mutex::new(Vec::new()),
                devices: Mutex::new(Vec::new()),
            })
        }

        pub(crate) fn messages(&self) -> Vec<String> {
            self.logs
                .lock()
                .unwrap()
                .iter()
                .map(|(_, message)| message.clone())
                .collect()
        }
    }

    impl LogService for LogTrace {
        fn write(&self, record: LogRecord) -> CapabilityResult<()> {
            self.devices.lock().unwrap().push(
                record
                    .device()
                    .map(|d| d.id().as_str().to_owned())
                    .unwrap_or_default(),
            );
            self.logs.lock().unwrap().push((
                format!("{:?}", record.level()),
                record.message().to_string(),
            ));
            Ok(())
        }
    }

    /// frame+vision+resource 桩：按队列逐次返回匹配结果（缺省 NotFound）。
    pub(crate) struct VisionStub {
        pub(crate) size: FrameSize,
        input_size: RwLock<FrameSize>,
        pub(crate) captures: AtomicU64,
        pub(crate) outcomes: Mutex<VecDeque<MatchOutcome>>,
        pub(crate) match_calls: AtomicU64,
        released: Mutex<Vec<FrameHandle>>,
        frames: Mutex<Vec<FrameHandle>>,
        regions: Mutex<Vec<Option<SearchRegion>>>,
        stamp: Mutex<Option<crate::capabilities::FrameStamp>>,
    }

    impl VisionStub {
        pub(crate) fn new(size: FrameSize) -> Arc<Self> {
            Arc::new(Self {
                size,
                input_size: RwLock::new(size),
                captures: AtomicU64::new(0),
                outcomes: Mutex::new(VecDeque::new()),
                match_calls: AtomicU64::new(0),
                released: Mutex::new(Vec::new()),
                frames: Mutex::new(Vec::new()),
                regions: Mutex::new(Vec::new()),
                stamp: Mutex::new(None),
            })
        }

        pub(crate) fn push_outcome(&self, outcome: MatchOutcome) {
            self.outcomes.lock().unwrap().push_back(outcome);
        }
    }

    #[async_trait]
    impl FrameService for VisionStub {
        fn release(&self, frame: FrameHandle) {
            self.released.lock().unwrap().push(frame);
        }
        async fn stamp(
            &self,
            _frame: FrameHandle,
        ) -> CapabilityResult<Option<crate::capabilities::FrameStamp>> {
            Ok(self.stamp.lock().unwrap().clone())
        }
        async fn device_size(&self, _device: &DeviceHandle) -> CapabilityResult<FrameSize> {
            Ok(*self.input_size.read().unwrap())
        }

        async fn latest(&self, _device: &DeviceHandle) -> CapabilityResult<Option<FrameHandle>> {
            Ok(Some(FrameHandle::new()))
        }

        async fn capture(&self, _device: &DeviceHandle) -> CapabilityResult<FrameHandle> {
            self.captures.fetch_add(1, Ordering::SeqCst);
            Ok(FrameHandle::new())
        }

        async fn size(&self, _frame: FrameHandle) -> CapabilityResult<FrameSize> {
            Ok(self.size)
        }
        async fn image_png(
            &self,
            frame: FrameHandle,
            region: Option<SearchRegion>,
        ) -> CapabilityResult<Vec<u8>> {
            self.frames.lock().unwrap().push(frame);
            self.regions.lock().unwrap().push(region);
            let image = image::RgbImage::from_fn(self.size.width, self.size.height, |x, y| {
                image::Rgb([x as u8, y as u8, 17])
            });
            crate::capabilities::adapters::encode_frame_png(
                &crate::matcher::DecodedFrame::from_rgb(image),
                region,
            )
        }
    }

    pub(super) fn stub_outcome() -> MatchOutcome {
        MatchOutcome::Found(crate::capabilities::MatchBox {
            x: 10,
            y: 20,
            width: 200,
            height: 100,
            score: 0.93,
        })
    }

    #[async_trait]
    impl VisionService for VisionStub {
        async fn match_template(
            &self,
            _frame: FrameHandle,
            _template: TemplateQuery,
        ) -> CapabilityResult<MatchOutcome> {
            self.match_calls.fetch_add(1, Ordering::Relaxed);
            self.frames.lock().unwrap().push(_frame);
            self.regions
                .lock()
                .unwrap()
                .push(_template.options().region);
            Ok(self
                .outcomes
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(MatchOutcome::NotFound))
        }

        async fn match_many(
            &self,
            request: &MatchManyRequest,
        ) -> CapabilityResult<Vec<MatchManyResult>> {
            Ok(request
                .templates()
                .iter()
                .map(|query| MatchManyResult {
                    template: query.template(),
                    outcome: MatchOutcome::NotFound,
                })
                .collect())
        }

        async fn sample_color(
            &self,
            _frame: FrameHandle,
            _point: FramePoint,
        ) -> CapabilityResult<ColorSample> {
            Ok(ColorSample {
                red: 1,
                green: 2,
                blue: 3,
            })
        }
    }

    #[async_trait]
    impl ResourceService for VisionStub {
        async fn resolve(&self, _id: &ResourceId) -> CapabilityResult<ResourceHandle> {
            Ok(ResourceHandle::new())
        }

        async fn open(&self, resource: ResourceHandle) -> CapabilityResult<ResourceLease> {
            Ok(ResourceLease::new(resource, Some(0)))
        }

        async fn resolved_file_name(&self, _handle: ResourceHandle) -> CapabilityResult<String> {
            Ok("template.png".to_string())
        }
    }

    pub(crate) struct EventCollect {
        events: Mutex<Vec<Value>>,
    }

    impl EventCollect {
        pub(crate) fn new() -> Arc<Self> {
            Arc::new(Self {
                events: Mutex::new(Vec::new()),
            })
        }

        pub(crate) fn of(&self, ev: &str) -> Vec<Value> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|event| event["ev"] == ev)
                .cloned()
                .collect()
        }
    }

    /// Core EventSink 适配：RuntimeEvent → `{"ev":…}` JSON（与 wasm_host 的
    /// `__event` 通道出参同形，测试断言词表一致）。
    impl crate::core::events::EventSink for EventCollect {
        fn emit(
            &self,
            event: crate::core::events::RuntimeEvent,
        ) -> futures_util::future::BoxFuture<'_, anyhow::Result<()>> {
            let mut payload = serde_json::to_value(event.kind).unwrap_or(Value::Null);
            if let Some(trace) = event.trace {
                payload["trace"] = trace;
            }
            self.events.lock().unwrap().push(payload);
            Box::pin(std::future::ready(Ok(())))
        }
    }

    pub(super) fn vision_host(
        trace: Arc<Trace>,
        stub: &Arc<VisionStub>,
        logs: Arc<LogTrace>,
        permissions: &[&str],
    ) -> HostApi {
        let permissions = permissions
            .iter()
            .map(|permission| format!("\"{permission}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let manifest = crate::extensions::parse_manifest(
            format!(
                r#"manifest_version = 2
id = "gamer-yaml"
version = "3.0.0"
name = "自动化"
entry = "plugin.wasm"
permissions = [{permissions}]
[host_api]
device = "^1.0"
vision = "^1.0"
input = "^1.0"
resource = "^1.0"
runtime = "^1.0"
log = "^1.0"
"#
            )
            .as_bytes(),
        )
        .unwrap();
        HostApi::for_manifest(
            CapabilityRegistry::builder()
                .with_device_service(trace.clone() as Arc<dyn crate::capabilities::DeviceService>)
                .with_input_service(trace as Arc<dyn crate::capabilities::InputService>)
                .with_frame_service(stub.clone() as Arc<dyn FrameService>)
                .with_vision_service(stub.clone() as Arc<dyn VisionService>)
                .with_resource_service(stub.clone() as Arc<dyn ResourceService>)
                .with_log_service(logs as Arc<dyn LogService>)
                .build(),
            HostApiCatalog::default(),
            &manifest,
        )
        .unwrap()
    }

    fn test_context() -> AppContext {
        AppContext::for_test("device-1", "com.example.game").unwrap()
    }

    fn call(name: &str, args: Value, host: &HostApi) -> Result<Value> {
        let sink: Option<Arc<dyn EventSink>> = None;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let host_impl = NativeYamlHost::new(
                host.clone(),
                test_context(),
                Arc::new(AtomicBool::new(false)),
                sink,
            )
            .await
            .unwrap();
            host_impl.call_function(name, args).await
        })
    }

    #[test]
    fn comparisons_and_pure_functions_work() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let host = vision_host(
            trace,
            &stub,
            LogTrace::new(),
            &["vision.match", "resource.read"],
        );
        assert_eq!(
            call("eq", json!({"a": 1, "b": 1.0}), &host).unwrap(),
            Value::Bool(true),
            "整型/浮点数字相等"
        );
        assert_eq!(
            call("gt", json!({"a": 3, "b": 2}), &host).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            call("ne", json!({"a": "x", "b": "y"}), &host).unwrap(),
            Value::Bool(true)
        );
        let error = call("gt", json!({"a": "x", "b": 2}), &host).unwrap_err();
        assert!(error.to_string().contains("number"), "{error}");
    }

    #[test]
    fn notification_function_remains_callable_without_optional_plugin() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let host = vision_host(trace, &stub, LogTrace::new(), &[]);
        let result = call("notify", json!({"content":"完成"}), &host).unwrap();
        assert_eq!(result["accepted"], false);
        assert_eq!(result["status"], "skipped");
        assert_eq!(call("eq", json!({"a":1,"b":1}), &host).unwrap(), true);
        assert!(call("notify", json!({"content":42}), &host).is_err());
        assert_eq!(
            call("notify", json!({"content":"完成","enabled":false}), &host).unwrap()["status"],
            "skipped"
        );
        assert!(call("notify", json!({"content":"完成","when":"unknown"}), &host).is_err());
    }

    #[tokio::test]
    async fn external_native_dispatch_rejects_guest_calls_absent_from_the_frozen_program() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let host = vision_host(trace, &stub, LogTrace::new(), &[]);
        let policy = crate::core::ExecutionPolicy::external(
            "fixture".into(),
            ["gamer-yaml".into()].into(),
            ["run.submit".into()].into(),
        );
        // A guest can ignore an empty program and send __fn itself. Exercise
        // that native entry directly, with no parser or static closure scan.
        for name in [
            "notify",
            "switch_live_device",
            "wait_live_confirmation",
            "ocr",
            "find_text",
        ] {
            let error = NativeYamlHost::call_function_json(
                host.clone(),
                test_context(),
                Arc::new(AtomicBool::new(false)),
                None,
                (name, r#"{"content":"forged","execution_policy":null}"#),
                super::super::settings::Settings::default(),
                None,
                None,
                None,
                Arc::new(YamlRunState::new()),
                Some(policy.clone()),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("external_execution_denied"),
                "{name}: {error}"
            );
        }
        let value = NativeYamlHost::call_function_json(
            host.clone(),
            test_context(),
            Arc::new(AtomicBool::new(false)),
            None,
            ("eq", r#"{"a":1,"b":1}"#),
            super::super::settings::Settings::default(),
            None,
            None,
            None,
            Arc::new(YamlRunState::new()),
            Some(policy.clone()),
        )
        .await
        .unwrap();
        assert_eq!(value, true);
        // The actual native dispatcher also enforces the typed policy when
        // called independently of the JSON guest bridge.
        let mut native = NativeYamlHost::new(
            host.clone(),
            test_context(),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await
        .unwrap();
        native.execution_policy = Some(policy);
        assert!(native
            .call_function("notify", json!({"content":"forged"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("external_execution_denied"));
        native.execution_policy = None;
        assert_eq!(
            native
                .call_function("notify", json!({"content":"management"}))
                .await
                .unwrap()["status"],
            "skipped"
        );
    }

    #[test]
    fn notification_result_policy_follows_called_script_arguments_and_keeps_parent_steps() {
        struct Bridge<'a> {
            native: &'a NativeYamlHost,
            runtime: &'a tokio::runtime::Runtime,
        }
        impl yaml_interp::HostFunctions for Bridge<'_> {
            fn invoke(&self, name: &str, args: Value) -> Result<Value, yaml_interp::HostError> {
                self.runtime
                    .block_on(self.native.call_function(name, args))
                    .map_err(|e| {
                        yaml_interp::HostError::new(
                            yaml_interp::HostErrorKind::Failed,
                            e.to_string(),
                        )
                    })
            }
        }
        impl yaml_interp::EventSink for Bridge<'_> {
            fn emit(&self, event: Value) {
                *self.native.run_state.trace_context.write().unwrap() = event["trace"].clone();
                if event["ev"] == "call_end" {
                    self.runtime
                        .block_on(self.native.run_state.result_notifications.settle(
                            event["trace"]["frame_id"].as_u64(),
                            event["ok"] == true,
                            event["error"].as_str(),
                            self.native.notification.as_ref(),
                        ));
                }
            }
        }
        let short = "params:\n  notify_success: {type: boolean, default: true}\n  notify_failure: {type: boolean, default: true}\n  simulate_failure: {type: boolean, default: false}\nrun:\n  - notify: {when: success, enabled: $notify_success, content: success}\n  - notify: {when: failure, enabled: $notify_failure, content: failure}\n  - reset_input: {}\n  - if: $simulate_failure\n    then: [{fail: {message: deliberate test failure, code: LIVE_DAILY_TEST_FAILURE}}]\n";
        let library = super::super::syntax::callable_source("short.yaml", short).unwrap();
        for (success, failure, simulate, expected) in [
            (true, true, false, vec!["success"]),
            (false, true, false, vec![]),
            (true, true, true, vec!["failure"]),
            (true, false, true, vec![]),
        ] {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let trace = Arc::new(Trace::default());
            let stub = VisionStub::new(FrameSize::new(1000, 1000));
            let logs = LogTrace::new();
            let sent = Arc::new(Mutex::new(Vec::<Value>::new()));
            let captured = sent.clone();
            let sender: super::super::notification::Sender = Arc::new(move |values| {
                captured.lock().unwrap().push(values);
                Box::pin(async { json!({"accepted":true,"status":"queued"}) })
            });
            let mut native = runtime
                .block_on(NativeYamlHost::new(
                    vision_host(
                        trace.clone(),
                        &stub,
                        logs.clone(),
                        &["device.read", "device.app", "input.tap", "log.write"],
                    ),
                    test_context(),
                    Arc::new(AtomicBool::new(false)),
                    None,
                ))
                .unwrap();
            native.notification = Some(sender);
            let caller = super::super::syntax::parse_script(&format!("run:\n  - notify: {{when: failure, content: parent failure}}\n  - short.yaml: {{notify_success: {success}, notify_failure: {failure}, simulate_failure: {simulate}}}\n  - log: {{message: parent continued, level: info}}\n")).unwrap();
            let mut wire =
                super::super::syntax::build_program(&caller, &library, Default::default(), 0);
            wire["trace"] = json!({"run_id":"notification-test"});
            let program: yaml_interp::Program = serde_json::from_value(wire).unwrap();
            let bridge = Bridge {
                native: &native,
                runtime: &runtime,
            };
            let outcome = yaml_interp::run(&program, &bridge, Some(&bridge));
            assert_eq!(outcome.is_err(), simulate, "{outcome:?}");
            let error = outcome.as_ref().err().map(String::as_str);
            runtime.block_on(native.run_state.result_notifications.settle(
                None,
                outcome.is_ok(),
                error,
                native.notification.as_ref(),
            ));
            let sent = sent.lock().unwrap();
            // An explicitly disabled child failure allows the enclosing script's
            // independently enabled policy to report its own failed task.
            let expected = if simulate && !failure {
                vec!["parent failure"]
            } else {
                expected
            };
            assert_eq!(sent.len(), expected.len());
            for (sent, expected) in sent.iter().zip(expected) {
                assert!(sent["content"].as_str().unwrap().starts_with(expected));
            }
            assert_eq!(
                trace
                    .apps
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(action, _)| action == "reset_input")
                    .count(),
                1
            );
            assert_eq!(
                logs.logs
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|(_, message)| message == "parent continued"),
                !simulate
            );
        }
    }

    #[tokio::test]
    async fn live_switch_uses_run_target_and_checks_permission_provider_and_timeout() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let requests = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let captured = requests.clone();
        let switcher: super::super::live_bridge::Switcher = Arc::new(move |action, values| {
            assert_eq!(action, "stream.switch");
            captured.lock().unwrap().push(values.clone());
            Box::pin(
                async move { Ok(json!({"device_id":values["device_id"],"state":"streaming"})) },
            )
        });
        let make =
            |permissions: &[&str]| vision_host(trace.clone(), &stub, LogTrace::new(), permissions);
        let native = NativeYamlHost::new(
            make(&["media.stream"]),
            test_context(),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await
        .unwrap()
        .with_live_switcher(Some(switcher.clone()));
        let current = native
            .call_function("switch_live_device", json!({}))
            .await
            .unwrap();
        assert_eq!(current["device_id"], test_context().device_id.as_str());
        let explicit = native
            .call_function("switch_live_device", json!({"device_id":"browser-other"}))
            .await
            .unwrap();
        assert_eq!(explicit["device_id"], "browser-other");
        let denied = NativeYamlHost::new(
            make(&[]),
            test_context(),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await
        .unwrap()
        .with_live_switcher(Some(switcher));
        assert!(denied
            .call_function("switch_live_device", json!({}))
            .await
            .is_err());
        assert_eq!(requests.lock().unwrap().len(), 2);
        let missing = NativeYamlHost::new(
            make(&["media.stream"]),
            test_context(),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await
        .unwrap();
        assert!(missing
            .call_function("switch_live_device", json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("LIVE_UNAVAILABLE"));
        let waiting: super::super::live_bridge::Switcher =
            Arc::new(|_, _| Box::pin(std::future::pending()));
        let stopped = Arc::new(AtomicBool::new(false));
        let slow = NativeYamlHost::new(
            make(&["media.stream"]),
            test_context(),
            stopped.clone(),
            None,
        )
        .await
        .unwrap()
        .with_live_switcher(Some(waiting));
        assert!(slow
            .call_function("switch_live_device", json!({"timeout":"5ms"}))
            .await
            .unwrap_err()
            .to_string()
            .contains("LIVE_SWITCH_TIMEOUT"));
        stopped.store(true, Ordering::SeqCst);
        assert!(slow
            .call_function("switch_live_device", json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("CANCELLED"));
    }

    #[tokio::test]
    async fn live_confirmation_returns_only_persisted_terminal_status_without_input() {
        for status in ["confirmed", "cancelled", "timeout"] {
            let trace = Arc::new(Trace::default());
            let stub = VisionStub::new(FrameSize::new(1000, 1000));
            let calls = Arc::new(Mutex::new(Vec::new()));
            let captured = calls.clone();
            let bridge: super::super::live_bridge::Switcher = Arc::new(move |action, values| {
                captured.lock().unwrap().push((action, values.clone()));
                Box::pin(async move {
                    match action {
                        "confirmation.begin" => {
                            assert_eq!(values, json!({"timeout_secs":300}));
                            Ok(
                                json!({"status":"awaiting_confirmation", "open_id":"private-viewer"}),
                            )
                        }
                        "confirmation.poll" => Ok(
                            json!({"status":status, "item_id":"private-item", "open_id":"private-viewer"}),
                        ),
                        _ => panic!("confirmation must not switch output"),
                    }
                })
            });
            let native = NativeYamlHost::new(
                vision_host(trace.clone(), &stub, LogTrace::new(), &["runtime.sleep"]),
                test_context(),
                Arc::new(AtomicBool::new(false)),
                None,
            )
            .await
            .unwrap()
            .with_live_switcher(Some(bridge));
            let result = native
                .call_function("wait_live_confirmation", json!({}))
                .await
                .unwrap();
            assert_eq!(result, json!({"status":status}));
            assert_eq!(
                calls.lock().unwrap().as_slice(),
                &[
                    ("confirmation.begin", json!({"timeout_secs":300})),
                    ("confirmation.poll", json!({})),
                ]
            );
            assert!(trace.apps.lock().unwrap().is_empty());
            assert!(trace.taps.lock().unwrap().is_empty());
            assert!(trace.swipes.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn live_confirmation_validates_timeout_permission_and_provider() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let captured = calls.clone();
        let bridge: super::super::live_bridge::Switcher = Arc::new(move |action, values| {
            assert_eq!(action, "confirmation.begin");
            assert_eq!(values, json!({"timeout_secs":2}));
            captured.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(json!({"status":"confirmed"})) })
        });
        let native = NativeYamlHost::new(
            vision_host(trace.clone(), &stub, LogTrace::new(), &["runtime.sleep"]),
            test_context(),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await
        .unwrap()
        .with_live_switcher(Some(bridge.clone()));
        for args in [
            json!({"timeout":"0ms"}),
            json!({"timeout":"301s"}),
            json!({"run_id":"forged"}),
            json!({"device_id":"forged"}),
            json!({"open_id":"forged"}),
        ] {
            assert!(native
                .call_function("wait_live_confirmation", args)
                .await
                .is_err());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            native
                .call_function("wait_live_confirmation", json!({"timeout":"1501ms"}))
                .await
                .unwrap(),
            json!({"status":"confirmed"})
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let denied = NativeYamlHost::new(
            vision_host(trace.clone(), &stub, LogTrace::new(), &[]),
            test_context(),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await
        .unwrap()
        .with_live_switcher(Some(bridge));
        assert!(denied
            .call_function("wait_live_confirmation", json!({}))
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let missing = NativeYamlHost::new(
            vision_host(trace, &stub, LogTrace::new(), &["runtime.sleep"]),
            test_context(),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await
        .unwrap();
        assert!(missing
            .call_function("wait_live_confirmation", json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("LIVE_UNAVAILABLE"));
    }

    #[tokio::test]
    async fn live_confirmation_cancels_inflight_rpc_and_between_polls() {
        struct Dropped(Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        for pending_rpc in [true, false] {
            let trace = Arc::new(Trace::default());
            let stub = VisionStub::new(FrameSize::new(1000, 1000));
            let stopped = Arc::new(AtomicBool::new(false));
            let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let captured_drops = drops.clone();
            let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let captured_calls = calls.clone();
            let bridge: super::super::live_bridge::Switcher = Arc::new(move |action, _| {
                assert_eq!(action, "confirmation.begin");
                captured_calls.fetch_add(1, Ordering::SeqCst);
                let guard = Dropped(captured_drops.clone());
                Box::pin(async move {
                    let _guard = guard;
                    if pending_rpc {
                        std::future::pending::<()>().await;
                    }
                    Ok(json!({"status":"waiting"}))
                })
            });
            let native = NativeYamlHost::new(
                vision_host(trace.clone(), &stub, LogTrace::new(), &["runtime.sleep"]),
                test_context(),
                stopped.clone(),
                None,
            )
            .await
            .unwrap()
            .with_live_switcher(Some(bridge));
            let cancel = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(40)).await;
                stopped.store(true, Ordering::SeqCst);
            });
            let error = tokio::time::timeout(
                Duration::from_millis(500),
                native.call_function("wait_live_confirmation", json!({})),
            )
            .await
            .unwrap()
            .unwrap_err();
            cancel.await.unwrap();
            assert!(error.to_string().contains("CANCELLED"), "{error}");
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
            assert!(trace.apps.lock().unwrap().is_empty());
            assert!(trace.taps.lock().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn live_confirmation_unavailable_disconnected_and_invalid_state_fail_closed() {
        for response in [None, Some("disconnected"), Some("success"), Some("")] {
            let trace = Arc::new(Trace::default());
            let stub = VisionStub::new(FrameSize::new(1000, 1000));
            let bridge: super::super::live_bridge::Switcher = Arc::new(move |_, _| {
                Box::pin(async move {
                    match response {
                        None => Err(anyhow!("not a queue main run")),
                        Some(status) => Ok(json!({"status":status})),
                    }
                })
            });
            let native = NativeYamlHost::new(
                vision_host(trace.clone(), &stub, LogTrace::new(), &["runtime.sleep"]),
                test_context(),
                Arc::new(AtomicBool::new(false)),
                None,
            )
            .await
            .unwrap()
            .with_live_switcher(Some(bridge));
            assert!(native
                .call_function("wait_live_confirmation", json!({}))
                .await
                .unwrap_err()
                .to_string()
                .contains("LIVE_CONFIRMATION_"));
            assert!(trace.apps.lock().unwrap().is_empty());
            assert!(trace.taps.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn matched_coordinates_retain_frame_identity_when_center_is_extracted() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let stamp = crate::capabilities::FrameStamp {
            target: "browser-a".into(),
            epoch: "epoch".into(),
            revision: 7,
        };
        *stub.stamp.lock().unwrap() = Some(stamp.clone());
        stub.push_outcome(stub_outcome());
        let host = vision_host(
            trace.clone(),
            &stub,
            LogTrace::new(),
            &["vision.match", "resource.read", "input.tap"],
        );
        let found = call("find", json!({"template":"reward.png"}), &host).unwrap();
        assert_eq!(found["_frame"], json!(stamp));
        assert_eq!(found["center"]["_frame"], json!(stamp));
        // An input implementation without this frame source must reject both forms.
        for point in [found.clone(), found["center"].clone()] {
            assert!(call("tap", json!({"position":point}), &host).is_err());
        }
        assert!(trace.taps.lock().unwrap().is_empty());
        call("tap", json!({"position":{"x":0.2,"y":0.3}}), &host).unwrap();
        assert_eq!(trace.taps.lock().unwrap().len(), 1);
    }

    fn ocr_stamp() -> crate::capabilities::FrameStamp {
        crate::capabilities::FrameStamp {
            target: "device-1".into(),
            epoch: "ocr-test".into(),
            revision: 3,
        }
    }

    fn ocr_reply(width: u32, height: u32) -> Value {
        let word =
            json!({"text":"排队 123", "confidence":0.95, "x":10,"y":20,"width":80,"height":30});
        json!({"text":"排队 123","confidence":0.95,"width":width,"height":height,"lines":[word.clone()],"words":[word]})
    }

    #[tokio::test]
    async fn ocr_uses_original_roi_and_maps_text_center_to_guarded_tap() {
        use base64::Engine;
        let trace = Arc::new(Trace::default());
        *trace.expected_stamp.lock().unwrap() = Some(ocr_stamp());
        let stub = VisionStub::new(FrameSize::new(1000, 500));
        *stub.stamp.lock().unwrap() = Some(ocr_stamp());
        let host = vision_host(
            trace.clone(),
            &stub,
            LogTrace::new(),
            &["device.read", "vision.ocr", "input.tap"],
        );
        let received = Arc::new(Mutex::new(Vec::<Value>::new()));
        let requests = received.clone();
        let recognize: super::super::ocr_bridge::Recognizer = Arc::new(move |request| {
            requests.lock().unwrap().push(request);
            Box::pin(async { Ok(ocr_reply(400, 100)) })
        });
        let mut native =
            NativeYamlHost::new(host, test_context(), Arc::new(AtomicBool::new(false)), None)
                .await
                .unwrap()
                .with_ocr(Some(recognize));
        native.settings.before_click_ms = 0;
        native.settings.after_click_ms = 0;
        let result = native
            .call_function(
                "find_text",
                json!({"text":"排队", "region":[0.2,0.3,0.4,0.2]}),
            )
            .await
            .unwrap();
        assert_eq!(stub.captures.load(Ordering::Relaxed), 1);
        assert_eq!(stub.released.lock().unwrap().len(), 1);
        assert_eq!(
            *stub.regions.lock().unwrap(),
            vec![Some(SearchRegion::new(200, 150, 400, 100))]
        );
        let request = received.lock().unwrap()[0].clone();
        let png = base64::engine::general_purpose::STANDARD
            .decode(request["png_base64"].as_str().unwrap())
            .unwrap();
        let image = crate::matcher::DecodedFrame::from_png(&png).unwrap();
        assert_eq!(image.dimensions(), (400, 100));
        assert_eq!(image.pixel(0, 0), Some([200, 150, 17]));
        assert_eq!(request["scale"], 2);
        assert_eq!(request["timeout_ms"], 10_000);
        assert_eq!(result["x"], 210);
        assert_eq!(result["y"], 170);
        assert_eq!(result["center"]["x"], 0.25);
        assert_eq!(result["center"]["y"], 0.37);
        assert_eq!(result["_frame"], json!(ocr_stamp()));
        native
            .call_function("tap", json!({"position":result.clone()}))
            .await
            .unwrap();
        assert_eq!(*trace.taps.lock().unwrap(), vec![[250, 185]]);
        trace.expected_stamp.lock().unwrap().as_mut().unwrap().epoch = "new-connection".into();
        assert!(native
            .call_function("tap", json!({"position":result}))
            .await
            .unwrap_err()
            .to_string()
            .contains("STALE_FRAME"));
        assert_eq!(trace.taps.lock().unwrap().len(), 1);
        for args in [
            json!({"text":"缺失", "region":[0.2,0.3,0.4,0.2]}),
            json!({"text":"排队", "match":"exact", "region":[0.2,0.3,0.4,0.2]}),
            json!({"text":"排队", "min_confidence":0.99, "region":[0.2,0.3,0.4,0.2]}),
        ] {
            assert_eq!(
                native.call_function("find_text", args).await.unwrap(),
                Value::Null
            );
        }
    }

    #[tokio::test]
    async fn ocr_permission_and_provider_errors_never_become_empty_success() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 500));
        *stub.stamp.lock().unwrap() = Some(ocr_stamp());
        for permissions in [&["device.read"][..], &["device.read", "vision.ocr"][..]] {
            let host = vision_host(trace.clone(), &stub, LogTrace::new(), permissions);
            let native =
                NativeYamlHost::new(host, test_context(), Arc::new(AtomicBool::new(false)), None)
                    .await
                    .unwrap();
            let error = native
                .call_function("ocr", json!({}))
                .await
                .unwrap_err()
                .to_string();
            if permissions.len() == 1 {
                assert!(error.contains("vision.ocr"), "{error}");
            } else {
                assert!(error.contains("OCR_UNAVAILABLE"), "{error}");
            }
        }
        assert_eq!(stub.captures.load(Ordering::Relaxed), 0);
        let host = vision_host(
            trace,
            &stub,
            LogTrace::new(),
            &["device.read", "vision.ocr"],
        );
        let recognize: super::super::ocr_bridge::Recognizer =
            Arc::new(|_| Box::pin(async { bail!("OCR_ENGINE_MISSING") }));
        let native =
            NativeYamlHost::new(host, test_context(), Arc::new(AtomicBool::new(false)), None)
                .await
                .unwrap()
                .with_ocr(Some(recognize));
        assert!(native
            .call_function("ocr", json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("OCR_ENGINE_MISSING"));
        assert_eq!(stub.released.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn ocr_cancel_drops_recognition_without_advancing_runtime_clock() {
        struct InFlight(Arc<AtomicBool>);
        impl Drop for InFlight {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 500));
        *stub.stamp.lock().unwrap() = Some(ocr_stamp());
        let host = vision_host(
            trace.clone(),
            &stub,
            LogTrace::new(),
            &["device.read", "vision.ocr"],
        );
        let clock = Arc::new(ClickClock {
            trace,
            sleeps: Mutex::new(vec![]),
            stop: AtomicBool::new(false),
            cancel_on_sleep: false,
        });
        let dropped = Arc::new(AtomicBool::new(false));
        let marker = dropped.clone();
        let started = Arc::new(tokio::sync::Notify::new());
        let signal = started.clone();
        let recognize: super::super::ocr_bridge::Recognizer = Arc::new(move |_| {
            let marker = marker.clone();
            let signal = signal.clone();
            Box::pin(async move {
                let _guard = InFlight(marker);
                signal.notify_one();
                std::future::pending::<Result<Value>>().await
            })
        });
        let native =
            NativeYamlHost::new(host, test_context(), Arc::new(AtomicBool::new(false)), None)
                .await
                .unwrap()
                .with_runtime(clock.clone())
                .with_ocr(Some(recognize));
        let cancel_clock = clock.clone();
        let (result, ()) = tokio::join!(native.call_function("ocr", json!({})), async move {
            started.notified().await;
            cancel_clock.stop.store(true, Ordering::SeqCst);
        });
        let error = result.unwrap_err().to_string();
        assert!(error.to_ascii_lowercase().contains("cancelled"), "{error}");
        assert!(dropped.load(Ordering::SeqCst));
        assert!(clock.sleeps.lock().unwrap().is_empty());
        assert_eq!(stub.released.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn ocr_rejects_unsafe_regions_modes_deadlines_and_provider_geometry() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 500));
        *stub.stamp.lock().unwrap() = Some(ocr_stamp());
        let host = vision_host(
            trace,
            &stub,
            LogTrace::new(),
            &["device.read", "vision.ocr"],
        );
        let recognize: super::super::ocr_bridge::Recognizer =
            Arc::new(|_| Box::pin(async { Ok(ocr_reply(400, 100)) }));
        let native =
            NativeYamlHost::new(host, test_context(), Arc::new(AtomicBool::new(false)), None)
                .await
                .unwrap()
                .with_ocr(Some(recognize));
        for args in [
            json!({"region":[0.8,0.2,0.3,0.5]}),
            json!({"region":[0.2,0.2,0.0,0.3]}),
            json!({"mode":"auto"}),
            json!({"scale":4}),
            json!({"timeout":"16s"}),
        ] {
            assert!(native
                .call_function("ocr", args)
                .await
                .unwrap_err()
                .to_string()
                .contains("OCR_INVALID_ARGUMENT"));
        }
        assert!(native
            .call_function("ocr", json!({}))
            .await
            .unwrap_err()
            .to_string()
            .contains("OCR_INVALID_RESULT"));
        assert_eq!(
            stub.released.lock().unwrap().len() as u64,
            stub.captures.load(Ordering::Relaxed)
        );
    }

    #[test]
    fn schema_binding_rejects_unknown_missing_and_mistyped_args() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let host = vision_host(trace, &stub, LogTrace::new(), &["input.tap"]);
        let error = call("tap", json!({}), &host).unwrap_err();
        assert!(error.to_string().contains("position"), "{error}");
        let error = call("tap", json!({"position": [0.5, 0.5], "nope": 1}), &host).unwrap_err();
        assert!(error.to_string().contains("未知参数 nope"), "{error}");
        let error = call("tap", json!({"position": [0.5, 5.0]}), &host).unwrap_err();
        assert!(error.to_string().contains("point"), "{error}");
    }

    #[test]
    fn input_coordinates_use_current_non_square_size_without_capture() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1920, 1080));
        let host = vision_host(
            trace.clone(),
            &stub,
            LogTrace::new(),
            &["input.tap", "input.swipe"],
        );
        // call() deliberately creates a fresh host each time, as __fn does.
        call("tap", json!({"position":{"center":{"x":350.0/1920.0,"y":871.0/1080.0},"x":242,"y":839,"width":217,"height":64}}), &host).unwrap();
        assert_eq!(*trace.taps.lock().unwrap(), vec![[350, 871]]);
        *stub.input_size.write().unwrap() = FrameSize::new(1080, 1920);
        call("tap", json!([0.25, 0.75]), &host).unwrap();
        assert_eq!(*trace.taps.lock().unwrap(), vec![[350, 871], [270, 1440]]);
        call(
            "swipe",
            json!({"from":[0.25,0.75],"to":[0.5,0.5],"duration":"1ms"}),
            &host,
        )
        .unwrap();
        let swipes = trace.swipes.lock().unwrap();
        assert_eq!(swipes[0].start().x(), 270);
        assert_eq!(swipes[0].start().y(), 1440);
        assert_eq!(swipes[0].end().x(), 540);
        assert_eq!(swipes[0].end().y(), 960);
        assert_eq!(
            stub.captures.load(Ordering::SeqCst),
            0,
            "输入只读取会话尺寸，不截图或重新匹配"
        );
        *stub.input_size.write().unwrap() = FrameSize::new(0, 0);
        assert!(call("tap", json!([0.5, 0.5]), &host).is_err());
        assert!(call(
            "swipe",
            json!({"from":[0.25,0.75],"to":[0.5,0.5],"duration":"1ms"}),
            &host
        )
        .is_err());
        assert_eq!(
            trace.taps.lock().unwrap().len(),
            2,
            "尺寸未知不能使用默认坐标点击"
        );
    }

    #[test]
    fn press_accepts_resolved_match_center_without_rematching() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1920, 1080));
        let host = vision_host(trace.clone(), &stub, LogTrace::new(), &["input.swipe"]);
        let matched = json!({"center":{"x":1844.0/1920.0,"y":892.0/1080.0},"x":1833,"y":872,"width":36,"height":30,"score":0.98,"template":"plus.png"});
        call(
            "press",
            json!({"position":matched,"duration":"800ms"}),
            &host,
        )
        .unwrap();
        let swipes = trace.swipes.lock().unwrap();
        assert_eq!(swipes.len(), 1);
        assert_eq!(swipes[0].start(), swipes[0].end());
        assert_eq!(swipes[0].start().x(), 1844);
        assert_eq!(swipes[0].start().y(), 892);
        assert_eq!(swipes[0].duration(), Duration::from_millis(800));
        assert_eq!(stub.match_calls.load(Ordering::SeqCst), 0);
        drop(swipes);
        assert!(call(
            "press",
            json!({"position":{"center":{"x":1.5,"y":0.5}},"duration":"800ms"}),
            &host
        )
        .is_err());
        assert_eq!(trace.swipes.lock().unwrap().len(), 1);
    }
    #[test]
    fn tap_accepts_match_center_without_rematching_and_rejects_invalid_targets() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let host = vision_host(trace.clone(), &stub, LogTrace::new(), &["input.tap"]);
        let hit = json!({"center":{"x":0.25,"y":0.75},"x":242,"y":839,"width":217,"height":64,"template":"reward.png","score":0.97});
        for position in [hit, json!({"x":0.25,"y":0.75}), json!([0.25, 0.75])] {
            call("tap", json!({"position":position}), &host).unwrap();
        }
        let taps = trace.taps.lock().unwrap().clone();
        assert_eq!(taps.len(), 3);
        assert!(taps.iter().all(|point| point == &taps[0]));
        assert_eq!(
            stub.match_calls.load(Ordering::SeqCst),
            0,
            "已有匹配结果不能重新识别"
        );
        for position in [
            Value::Null,
            json!({"center":null,"x":0.25,"y":0.75}),
            json!({"center":{"x":1.2,"y":0.5}}),
            json!("reward.png"),
        ] {
            assert!(call("tap", json!({"position":position}), &host).is_err());
        }
        assert_eq!(
            trace.taps.lock().unwrap().len(),
            3,
            "非法或空目标不能产生点击"
        );
    }

    #[test]
    fn permission_denial_surfaces_denied_error() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let host = vision_host(trace, &stub, LogTrace::new(), &["device.read"]);
        let error = call("tap", json!({"position": [0.5, 0.5]}), &host).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("denied") || text.contains("权限"), "{text}");
    }

    #[test]
    fn find_returns_match_then_null_and_tap_template_clicks_center() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        stub.push_outcome(stub_outcome());
        let host = vision_host(
            trace.clone(),
            &stub,
            LogTrace::new(),
            &["vision.match", "resource.read", "input.tap"],
        );
        let matched = call("find", json!({"template": "home"}), &host).unwrap();
        assert_eq!(matched["center"]["x"], 0.11, "中心 = (10+100)/1000");
        assert_eq!(matched["center"]["y"], 0.07, "中心 = (20+50)/1000");
        assert!(
            matched["score"].as_f64().unwrap() > 0.9,
            "score = {}（f32 经 wire 放大）",
            matched["score"]
        );

        let miss = call("find", json!({"template": "home"}), &host).unwrap();
        assert_eq!(miss, Value::Null);

        // find 是单次匹配：timeout/interval 已从 Schema 删除（等待轮询用 wait_find）
        let error = call("find", json!({"template": "home", "timeout": "1s"}), &host).unwrap_err();
        assert!(error.to_string().contains("未知参数 timeout"), "{error}");

        stub.push_outcome(stub_outcome());
        let matched = call(
            "tap_template",
            json!({"template": "home", "timeout": "0ms"}),
            &host,
        )
        .unwrap();
        assert_eq!(matched["center"]["x"], 0.11);
        let taps = trace.taps.lock().unwrap();
        assert_eq!(taps.len(), 1);
        assert_eq!(taps[0], [110, 70], "点击像素坐标 = center×屏");

        let disappeared = call(
            "wait_disappear",
            json!({"template": "home", "timeout": "100ms", "interval": "50ms"}),
            &host,
        )
        .unwrap();
        assert_eq!(disappeared, Value::Bool(true));
    }

    /// find 只执行一次；wait_find 才轮询，并在 timeout 到达后返回 null。
    #[test]
    fn wait_find_polls_until_match_and_times_out_without_match() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        stub.push_outcome(MatchOutcome::NotFound);
        stub.push_outcome(stub_outcome());
        let host = vision_host(
            trace,
            &stub,
            LogTrace::new(),
            &["vision.match", "resource.read"],
        );
        let matched = call(
            "wait_find",
            json!({"template": "home", "click": false, "timeout": "100ms", "interval": "1ms"}),
            &host,
        )
        .unwrap();
        assert_eq!(matched["center"]["x"], 0.11);
        assert_eq!(stub.match_calls.load(Ordering::Relaxed), 2);

        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let host = vision_host(
            trace,
            &stub,
            LogTrace::new(),
            &["vision.match", "resource.read"],
        );
        let result = call(
            "wait_find",
            json!({"template": "home", "click": false, "timeout": "0ms", "interval": "1ms"}),
            &host,
        )
        .unwrap();
        assert_eq!(result, Value::Null, "超时未命中必须返回 null");
        assert_eq!(
            stub.match_calls.load(Ordering::Relaxed),
            1,
            "timeout=0 仍只尝试一次"
        );
    }

    #[test]
    fn required_visual_observations_fail_optional_absence_only_and_explicit_failure() {
        let trace = Arc::new(Trace::default());
        let vision = VisionStub::new(FrameSize::new(1000, 1000));
        let host = vision_host(
            trace.clone(),
            &vision,
            LogTrace::new(),
            &["vision.match", "resource.read", "runtime.sleep"],
        );
        assert_eq!(
            call(
                "observe",
                json!({"template":"home","timeout":"0ms","required":false}),
                &host
            )
            .unwrap(),
            Value::Null
        );
        for function in ["observe", "finish"] {
            assert!(
                call(function, json!({"template":"home","timeout":"0ms"}), &host)
                    .unwrap_err()
                    .to_string()
                    .contains("VISUAL_TIMEOUT")
            );
        }
        assert!(call(
            "finish",
            json!({"template":"home","timeout":"0ms","required":false}),
            &host
        )
        .is_err());
        assert!(call(
            "observe",
            json!({"template":"home","timeout":"0ms","required":false,"threshold":"invalid"}),
            &host
        )
        .is_err());
        assert!(call("fail", json!("deliberate failure"), &host)
            .unwrap_err()
            .to_string()
            .contains("GOAL_FAILED"));
        assert!(trace.taps.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn visual_assertions_are_independent_and_fail_keeps_structured_reason() {
        let trace = Arc::new(Trace::default());
        let vision = VisionStub::new(FrameSize::new(1000, 1000));
        let api = vision_host(
            trace,
            &vision,
            LogTrace::new(),
            &["vision.match", "resource.read", "runtime.sleep"],
        );
        let host = NativeYamlHost::new(
            api,
            AppContext::for_test("d", "com.test").unwrap(),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await
        .unwrap();
        host.run_state
            .required_checks
            .write()
            .unwrap()
            .insert("daily".into());
        let found = || {
            MatchOutcome::Found(crate::capabilities::MatchBox {
                x: 10,
                y: 10,
                width: 9,
                height: 9,
                score: 0.99,
            })
        };
        vision.push_outcome(found());
        assert!(host
            .call_function("finish", json!({"template":"world","timeout":"0ms"}))
            .await
            .is_ok());
        vision.push_outcome(found());
        host.call_function(
            "checkpoint",
            json!({"id":"daily","template":"claimed","timeout":"0ms"}),
        )
        .await
        .unwrap();
        assert!(host
            .run_state
            .completed_checks
            .read()
            .unwrap()
            .contains("daily"));
        vision.push_outcome(found());
        host.call_function("finish", json!({"template":"world","timeout":"0ms"}))
            .await
            .unwrap();
        assert!(host.run_state.goal_verified.load(Ordering::Relaxed));
        let error=host.call_function("fail",json!({"code":"OCR_INVALID","stage":"stamina","expected":300,"actual":999,"retryable":false,"message":"bad reading"})).await.unwrap_err().to_string();
        assert!(
            error.contains("OCR_INVALID") && error.contains("stamina") && error.contains("999")
        );
    }

    #[test]
    fn wait_find_forever_keeps_polling_past_timeout_and_refreshes_obstacles() {
        let trace = Arc::new(Trace::default());
        let vision = VisionStub::new(FrameSize::new(1000, 1000));
        for outcome in [
            stub_outcome(),
            MatchOutcome::NotFound,
            MatchOutcome::NotFound,
            MatchOutcome::NotFound,
            stub_outcome(),
        ] {
            vision.push_outcome(outcome);
        }
        let host = vision_host(
            trace.clone(),
            &vision,
            LogTrace::new(),
            &[
                "vision.match",
                "resource.read",
                "runtime.sleep",
                "input.tap",
            ],
        );
        let result = call("wait_find", json!({"template":"home", "obstacles":["expired"], "forever":true, "timeout":"0ms", "interval":"50ms"}), &host).unwrap();
        assert!(!result.is_null());
        assert_eq!(trace.taps.lock().unwrap().len(), 1);
        assert_eq!(vision.match_calls.load(Ordering::Relaxed), 5);
    }

    #[tokio::test]
    async fn wait_find_forever_is_cancelled_without_hitting_timeout() {
        let trace = Arc::new(Trace::default());
        let vision = VisionStub::new(FrameSize::new(1000, 1000));
        let api = vision_host(
            trace,
            &vision,
            LogTrace::new(),
            &["vision.match", "resource.read"],
        );
        let stop = Arc::new(AtomicBool::new(false));
        let host = NativeYamlHost::new(api, test_context(), stop.clone(), None)
            .await
            .unwrap();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            stop.store(true, Ordering::SeqCst);
        });
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            host.call_function(
                "wait_find",
                json!({"template":"missing", "forever":true, "timeout":"0ms", "interval":"50ms"}),
            ),
        )
        .await
        .unwrap();
        let error = result.unwrap_err().to_string();
        assert!(error.to_ascii_lowercase().contains("cancelled"), "{error}");
        assert!(vision.match_calls.load(Ordering::Relaxed) >= 2);
    }

    #[test]
    fn wait_find_defaults_to_observation_and_click_is_explicit() {
        for click in [None, Some(true), Some(false)] {
            let trace = Arc::new(Trace::default());
            let stub = VisionStub::new(FrameSize::new(1000, 1000));
            stub.push_outcome(stub_outcome());
            let permissions = if click != Some(true) {
                vec!["vision.match", "resource.read"]
            } else {
                vec!["vision.match", "resource.read", "input.tap"]
            };
            let host = vision_host(trace.clone(), &stub, LogTrace::new(), &permissions);
            let mut args = json!({"template": "home", "timeout": "0ms"});
            if let Some(click) = click {
                args["click"] = json!(click);
            }
            let matched = call("wait_find", args.clone(), &host).unwrap();
            assert_eq!(matched["center"], json!({"x": 0.11, "y": 0.07}));
            let expected_taps = if click != Some(true) {
                vec![]
            } else {
                vec![[110, 70]]
            };
            assert_eq!(*trace.taps.lock().unwrap(), expected_taps);
            assert_eq!(call("wait_find", args, &host).unwrap(), Value::Null);
            assert_eq!(*trace.taps.lock().unwrap(), expected_taps, "未命中不能点击");
        }
    }

    #[test]
    fn wait_find_click_requires_permission_and_boolean_argument() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let host = vision_host(
            trace.clone(),
            &stub,
            LogTrace::new(),
            &["vision.match", "resource.read"],
        );
        let error = call(
            "wait_find",
            json!({"template": "home", "click":true}),
            &host,
        )
        .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("denied") || text.contains("权限"), "{text}");
        let error = call(
            "wait_find",
            json!({"template": "home", "click": "false"}),
            &host,
        )
        .unwrap_err();
        assert!(error.to_string().contains("boolean"), "{error}");
        assert_eq!(stub.match_calls.load(Ordering::Relaxed), 0);
        assert!(trace.taps.lock().unwrap().is_empty());
    }

    #[test]
    fn find_any_stops_at_first_hit_shares_frame_and_never_taps() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        stub.push_outcome(MatchOutcome::NotFound);
        stub.push_outcome(stub_outcome());
        stub.push_outcome(stub_outcome());
        let host = vision_host(
            trace.clone(),
            &stub,
            LogTrace::new(),
            &["vision.match", "resource.read"],
        );
        let matched = call(
            "find_any",
            json!({"templates":["notice", "login", "home"],"region":[0.2,0.1,0.4,0.3]}),
            &host,
        )
        .unwrap();
        assert_eq!(matched["index"], 1);
        assert_eq!(matched["template"], "login");
        assert!(trace.taps.lock().unwrap().is_empty());
        let frames = stub.frames.lock().unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0], frames[1]);
        drop(frames);
        assert_eq!(
            *stub.regions.lock().unwrap(),
            vec![
                Some(SearchRegion {
                    x: 200,
                    y: 100,
                    width: 400,
                    height: 300
                });
                2
            ]
        );
        for invalid in [json!([]), json!([""]), json!([1]), json!("home")] {
            assert!(call("find_any", json!({"templates":invalid}), &host).is_err());
        }
        assert_eq!(stub.match_calls.load(Ordering::Relaxed), 2);
        stub.outcomes.lock().unwrap().clear();
        assert_eq!(
            call("find_any", json!({"templates":["missing"]}), &host).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn wait_find_keeps_checking_late_obstacles_until_the_goal_appears() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        // First poll: no popup or goal. Then the popup appears, remains after
        // the first click, and finally clears so the goal can be observed.
        for outcome in [
            MatchOutcome::NotFound,
            MatchOutcome::NotFound,
            stub_outcome(),
            stub_outcome(),
            MatchOutcome::NotFound,
            stub_outcome(),
        ] {
            stub.push_outcome(outcome);
        }
        let host = vision_host(
            trace.clone(),
            &stub,
            LogTrace::new(),
            &["vision.match", "resource.read", "input.tap"],
        );
        let matched=call("wait_find",json!({"template":"world","obstacles":["supply"],"click":false,"timeout":"5min","interval":"2s","region":[0.75,0.18,0.25,0.08]}),&host).unwrap();
        assert!(matched.is_object());
        assert_eq!(trace.taps.lock().unwrap().len(), 2);
        let frames = stub.frames.lock().unwrap();
        assert_eq!(frames[0], frames[1]);
        assert_ne!(frames[1], frames[2]);
        assert_ne!(frames[2], frames[3]);
        assert_ne!(frames[3], frames[4]);
        assert_eq!(frames[4], frames[5]);
        let regions = stub.regions.lock().unwrap();
        assert!(
            regions[0].is_none()
                && regions[2].is_none()
                && regions[3].is_none()
                && regions[4].is_none()
        );
        assert!(regions[1].is_some() && regions[5].is_some());
    }
    #[test]
    fn wait_find_obstacles_click_first_then_refresh_and_share_next_frame() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        for outcome in [
            stub_outcome(),
            MatchOutcome::NotFound,
            MatchOutcome::NotFound,
            stub_outcome(),
        ] {
            stub.push_outcome(outcome);
        }
        let host = vision_host(
            trace.clone(),
            &stub,
            LogTrace::new(),
            &["vision.match", "resource.read", "input.tap"],
        );
        let matched = call(
            "wait_find",
            json!({"template":"home", "obstacles":["close", "confirm"],
            "click":false, "timeout":"2s", "interval":"50ms", "region":[0.5,0.5,0.5,0.5]}),
            &host,
        )
        .unwrap();
        assert_eq!(matched["center"], json!({"x":0.11,"y":0.07}));
        assert_eq!(
            *trace.taps.lock().unwrap(),
            vec![[110, 70]],
            "click=false 仍清障碍，不点击目标"
        );
        let frames = stub.frames.lock().unwrap();
        assert_eq!(frames.len(), 4);
        assert_ne!(frames[0], frames[1], "点击后必须取新帧");
        assert_eq!(frames[1], frames[2]);
        assert_eq!(frames[2], frames[3], "本轮障碍和目标共用同一帧");
        let regions = stub.regions.lock().unwrap();
        assert!(
            regions[..3].iter().all(Option::is_none),
            "障碍不继承目标区域"
        );
        assert!(regions[3].is_some());
    }

    #[test]
    fn wait_find_obstacle_time_counts_toward_timeout_and_validates_input() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        stub.push_outcome(stub_outcome());
        let host = vision_host(
            trace.clone(),
            &stub,
            LogTrace::new(),
            &["vision.match", "resource.read", "input.tap"],
        );
        let result = call("wait_find", json!({"template":"home", "obstacles":["close", "confirm"], "click":false,"timeout":"100ms"}), &host).unwrap();
        assert_eq!(result, Value::Null);
        assert_eq!(stub.match_calls.load(Ordering::Relaxed), 1);
        assert_eq!(trace.taps.lock().unwrap().len(), 1);
        for obstacles in [json!("close"), json!([1]), json!([""])] {
            assert!(call(
                "wait_find",
                json!({"template":"home","obstacles":obstacles}),
                &host
            )
            .is_err());
        }
        let no_input = vision_host(
            trace,
            &stub,
            LogTrace::new(),
            &["vision.match", "resource.read"],
        );
        assert!(call(
            "wait_find",
            json!({"template":"home","obstacles":["close"],"click":false}),
            &no_input
        )
        .is_err());
        assert_eq!(stub.match_calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn log_function_writes_and_stringifies_non_text() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let logs = LogTrace::new();
        let host = vision_host(trace, &stub, logs.clone(), &["log.write"]);
        call("log", json!({"message": "文本"}), &host).unwrap();
        call("log", json!({"message": {"k": 1}}), &host).unwrap();
        assert!(logs.devices.lock().unwrap().iter().all(|d| !d.is_empty()));
        assert_eq!(
            logs.messages(),
            vec!["文本".to_string(), "{\"k\":1}".to_string()]
        );
    }

    struct ClickClock {
        trace: Arc<Trace>,
        sleeps: Mutex<Vec<(usize, u64)>>,
        stop: AtomicBool,
        cancel_on_sleep: bool,
    }
    #[async_trait]
    impl RuntimeService for ClickClock {
        async fn sleep(&self, duration: Duration) -> crate::capabilities::CapabilityResult<()> {
            self.sleeps.lock().unwrap().push((
                self.trace.taps.lock().unwrap().len() + self.trace.keys.lock().unwrap().len(),
                duration.as_millis() as u64,
            ));
            if self.cancel_on_sleep {
                self.stop.store(true, Ordering::SeqCst);
                return Err(crate::capabilities::CapabilityError::Cancelled);
            }
            Ok(())
        }
        fn cancelled(&self) -> bool {
            self.stop.load(Ordering::SeqCst)
        }
    }

    #[tokio::test]
    async fn native_sleep_preserves_single_virtual_clock_wait_and_cancel_error() {
        for cancel_on_sleep in [false, true] {
            let trace = Arc::new(Trace::default());
            let stub = VisionStub::new(FrameSize::new(1000, 1000));
            let host = vision_host(trace.clone(), &stub, LogTrace::new(), &["runtime.sleep"]);
            let mut native =
                NativeYamlHost::new(host, test_context(), Arc::new(AtomicBool::new(false)), None)
                    .await
                    .unwrap();
            let clock = Arc::new(ClickClock {
                trace,
                sleeps: Mutex::new(vec![]),
                stop: AtomicBool::new(false),
                cancel_on_sleep,
            });
            native.runtime = clock.clone();
            let result = native.call_function("sleep", json!("5s")).await;
            assert_eq!(*clock.sleeps.lock().unwrap(), vec![(0, 5000)]);
            if cancel_on_sleep {
                assert!(matches!(
                    result
                        .unwrap_err()
                        .downcast_ref::<crate::capabilities::CapabilityError>(),
                    Some(crate::capabilities::CapabilityError::Cancelled)
                ));
            } else {
                assert_eq!(result.unwrap(), Value::Null);
            }
        }
    }

    #[tokio::test]
    async fn input_delays_wrap_every_automation_click_and_key_exactly_once() {
        for name in [
            "tap",
            "wait_find",
            "tap_template",
            "obstacle",
            "press",
            "down",
            "up",
        ] {
            let trace = Arc::new(Trace::default());
            let stub = VisionStub::new(FrameSize::new(1000, 1000));
            stub.push_outcome(stub_outcome());
            let events = EventCollect::new();
            let host = vision_host(
                trace.clone(),
                &stub,
                LogTrace::new(),
                &["input.tap", "input.key", "vision.match", "resource.read"],
            );
            let mut native = NativeYamlHost::new(
                host,
                test_context(),
                Arc::new(AtomicBool::new(false)),
                Some(events.clone()),
            )
            .await
            .unwrap();
            native.settings.before_click_ms = 125;
            native.settings.after_click_ms = 225;
            let clock = Arc::new(ClickClock {
                trace: trace.clone(),
                sleeps: Mutex::new(vec![]),
                stop: AtomicBool::new(false),
                cancel_on_sleep: false,
            });
            native.runtime = clock.clone();
            let (function, args) = match name {
                "press" | "down" | "up" => ("key", json!({"key":"HOME","action":name})),
                "tap" => (name, json!([0.5, 0.5])),
                "obstacle" => (
                    "wait_find",
                    json!({"template":"home","obstacles":["close"],"click":false,"timeout":"0ms"}),
                ),
                "wait_find" => (
                    name,
                    json!({"template":"home","timeout":"0ms","click":true}),
                ),
                _ => (name, json!({"template":"home","timeout":"0ms"})),
            };
            native.call_function(function, args).await.unwrap();
            if function == "key" {
                let keys = trace.keys.lock().unwrap();
                assert_eq!(keys.len(), 1, "{name}");
                assert_eq!(
                    keys[0].action(),
                    match name {
                        "down" => KeyAction::Down,
                        "up" => KeyAction::Up,
                        _ => KeyAction::Press,
                    }
                );
                assert!(trace.taps.lock().unwrap().is_empty());
            } else {
                assert_eq!(trace.taps.lock().unwrap().len(), 1, "{name}");
            }
            let sleeps = clock.sleeps.lock().unwrap();
            assert_eq!(
                sleeps
                    .iter()
                    .filter(|(t, _)| *t == 0)
                    .map(|(_, ms)| ms)
                    .sum::<u64>(),
                125,
                "{name} before"
            );
            assert_eq!(
                sleeps
                    .iter()
                    .filter(|(t, _)| *t == 1)
                    .map(|(_, ms)| ms)
                    .sum::<u64>(),
                225,
                "{name} after"
            );
            let delays: Vec<_> = events
                .of("detail")
                .into_iter()
                .filter(|e| {
                    e["name"]
                        == if function == "key" {
                            "key_delay"
                        } else {
                            "click_delay"
                        }
                })
                .collect();
            assert_eq!(delays.len(), 2);
            assert_eq!(
                delays[0]["data"],
                json!({"phase":"before","duration_ms":125})
            );
            assert_eq!(
                delays[1]["data"],
                json!({"phase":"after","duration_ms":225})
            );
        }
    }

    #[tokio::test]
    async fn input_delays_can_be_disabled_and_cancel_before_or_after_input() {
        for function in ["tap", "key"] {
            for (before, after, cancel, expected_taps) in
                [(0, 0, false, 1), (300, 300, true, 0), (0, 300, true, 1)]
            {
                let trace = Arc::new(Trace::default());
                let stub = VisionStub::new(FrameSize::new(1000, 1000));
                let host = vision_host(
                    trace.clone(),
                    &stub,
                    LogTrace::new(),
                    &["input.tap", "input.key"],
                );
                let mut native = NativeYamlHost::new(
                    host,
                    test_context(),
                    Arc::new(AtomicBool::new(false)),
                    None,
                )
                .await
                .unwrap();
                native.settings.before_click_ms = before;
                native.settings.after_click_ms = after;
                let clock = Arc::new(ClickClock {
                    trace: trace.clone(),
                    sleeps: Mutex::new(vec![]),
                    stop: AtomicBool::new(false),
                    cancel_on_sleep: cancel,
                });
                native.runtime = clock.clone();
                assert_eq!(
                    native
                        .call_function(
                            function,
                            if function == "key" {
                                json!("HOME")
                            } else {
                                json!([0.5, 0.5])
                            }
                        )
                        .await
                        .is_err(),
                    cancel
                );
                assert_eq!(
                    trace.taps.lock().unwrap().len() + trace.keys.lock().unwrap().len(),
                    expected_taps
                );
                assert_eq!(
                    clock.sleeps.lock().unwrap().len(),
                    if cancel { 1 } else { 0 }
                );
            }
        }
    }

    #[tokio::test]
    async fn matching_without_click_never_uses_click_delays() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let host = vision_host(
            trace.clone(),
            &stub,
            LogTrace::new(),
            &["vision.match", "resource.read"],
        );
        let mut native =
            NativeYamlHost::new(host, test_context(), Arc::new(AtomicBool::new(false)), None)
                .await
                .unwrap();
        let clock = Arc::new(ClickClock {
            trace: trace.clone(),
            sleeps: Mutex::new(vec![]),
            stop: AtomicBool::new(false),
            cancel_on_sleep: false,
        });
        native.runtime = clock.clone();
        for (name, args) in [
            ("find", json!("home")),
            ("find_any", json!({"templates":["home"]})),
            (
                "wait_find",
                json!({"template":"home","click":false,"timeout":"0ms"}),
            ),
        ] {
            stub.push_outcome(stub_outcome());
            assert!(!native.call_function(name, args).await.unwrap().is_null());
        }
        assert!(clock.sleeps.lock().unwrap().is_empty());
        assert!(trace.taps.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn diagnostics_include_bound_defaults_and_preserve_explicit_timeout() {
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let host = vision_host(
            Arc::new(Trace::default()),
            &stub,
            LogTrace::new(),
            &[
                "device.read",
                "vision.match",
                "resource.read",
                "input.tap",
                "runtime.sleep",
            ],
        );
        let events = EventCollect::new();
        let native = NativeYamlHost::new(
            host,
            test_context(),
            Arc::new(AtomicBool::new(false)),
            Some(events.clone()),
        )
        .await
        .unwrap();
        let _ = native
            .call_function("wait_find", json!({"template":"home","timeout":"0ms"}))
            .await;
        let details = events.of("detail");
        let args = &details
            .iter()
            .find(|e| e["name"] == "effective_args")
            .unwrap()["data"]["args"];
        assert_eq!(args["template"], "home");
        assert_eq!(args["timeout"], "0ms");
        assert_eq!(args["click"], false);
        assert!(args["threshold"].is_number());
        assert!(args.get("interval").is_some());
    }

    #[test]
    fn input_text_and_launch_flow_through_capabilities() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let host = vision_host(
            trace.clone(),
            &stub,
            LogTrace::new(),
            &["input.text", "device.app", "device.read"],
        );
        call("input_text", json!({"text": "你好"}), &host).unwrap();
        assert_eq!(trace.text.lock().unwrap().as_slice(), ["你好"]);
        call("launch", json!({}), &host).unwrap();
    }

    #[tokio::test]
    async fn reset_input_uses_run_target_app_and_requires_app_permission() {
        let trace = Arc::new(Trace::default());
        let stub = VisionStub::new(FrameSize::new(1000, 1000));
        let host = vision_host(trace.clone(), &stub, LogTrace::new(), &["device.app"]);
        let native = NativeYamlHost::new(
            host.clone(),
            test_context(),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            native
                .call_function("reset_input", json!({}))
                .await
                .unwrap(),
            Value::Null
        );
        let mut browser_context = test_context();
        browser_context.android_package = None;
        let browser = NativeYamlHost::new(
            host,
            browser_context,
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await
        .unwrap();
        browser
            .call_function("reset_input", json!({}))
            .await
            .unwrap();
        assert_eq!(
            *trace.apps.lock().unwrap(),
            [
                ("reset_input".into(), "com.example.game".into()),
                ("reset_input".into(), "".into()),
            ]
        );
        let denied = NativeYamlHost::new(
            vision_host(trace.clone(), &stub, LogTrace::new(), &[]),
            test_context(),
            Arc::new(AtomicBool::new(false)),
            None,
        )
        .await
        .unwrap();
        assert!(denied
            .call_function("reset_input", json!({}))
            .await
            .is_err());
        assert_eq!(trace.apps.lock().unwrap().len(), 2);
    }
}

#[cfg(all(test, feature = "wasm-runtime"))]
pub(crate) mod wasm_tests {
    use super::super::wasm_host::LazyYamlWasmtimeRuntime;
    use super::tests;
    use super::*;
    use crate::extensions::gamer_yaml::syntax;
    fn parse_script(source: &str) -> Result<syntax::Script, Vec<syntax::Diagnostic>> {
        syntax::parse_script(source)
    }
    fn parse_function_library(
        source: &str,
    ) -> Result<syntax::FunctionLibrary, Vec<syntax::Diagnostic>> {
        syntax::parse_function_library(source)
    }
    fn build_program(
        script: &syntax::Script,
        library: &syntax::FunctionLibrary,
        vars: JsonMap<String, Value>,
        start: usize,
    ) -> Value {
        let program = syntax::build_program(script, library, vars, start);
        // These fixtures test isolated native/control-flow instructions. Full
        // visual completion stays mandatory at production build_program.
        program
    }
    use async_trait::async_trait;
    use std::fs;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::sync::{atomic::Ordering, OnceLock};
    use zip::write::SimpleFileOptions;

    include!("acceptance_tests.rs");
    include!("concurrency_tests.rs");

    /// The YAML production registrar supplies the same execution-model
    /// declaration, but this focused guest test does not need a Scheduler.
    struct InstanceFreeRegistrar;

    #[async_trait]
    impl crate::extensions::TimerRunnerRegistrar for InstanceFreeRegistrar {
        async fn extension_started(&self, _extension_id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn extension_stopped(&self, _extension_id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        fn executes_without_instance(&self, _extension_id: &str) -> bool {
            true
        }
    }

    fn guest_source_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../plugins/gamer-yaml/guest")
    }

    fn guest_target_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("yaml-guest")
    }

    fn guest_module_path() -> PathBuf {
        guest_target_dir()
            .join("wasm32-unknown-unknown")
            .join("release")
            .join("gamer_yaml_guest.wasm")
    }

    fn run_guest_cargo(args: &[String]) -> Output {
        let (subcommand, rest) = args
            .split_first()
            .expect("yaml guest cargo 子进程缺少 subcommand");
        let guest_dir = guest_source_dir();
        let target_dir = guest_target_dir();
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut command = Command::new(cargo);
        command
            .current_dir(&guest_dir)
            .arg(subcommand)
            .arg("--manifest-path")
            .arg(guest_dir.join("Cargo.toml"))
            // 不继承 server 的 target 目录（CI 常设 CARGO_TARGET_DIR）。
            .arg("--target-dir")
            .arg(&target_dir);
        for arg in rest {
            command.arg(arg);
        }
        command.output().unwrap_or_else(|error| {
            panic!(
                "无法启动 yaml guest cargo 子进程: {error}; guest_dir={}; target_dir={}",
                guest_dir.display(),
                target_dir.display()
            )
        })
    }

    fn assert_guest_command(output: Output, stage: &str) {
        if output.status.success() {
            return;
        }
        panic!(
            "yaml guest {stage} 失败: status={:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn guest_module() -> Vec<u8> {
        static MODULE: OnceLock<Vec<u8>> = OnceLock::new();
        MODULE
            .get_or_init(|| {
                let output = run_guest_cargo(&[
                    "build".into(),
                    "--locked".into(),
                    "--quiet".into(),
                    "--release".into(),
                    "--lib".into(),
                    "--target".into(),
                    "wasm32-unknown-unknown".into(),
                ]);
                assert_guest_command(output, "guest wasm 构建");
                let path = guest_module_path();
                fs::read(&path).unwrap_or_else(|error| {
                    panic!("yaml guest wasm 不存在: {}: {error}", path.display())
                })
            })
            .clone()
    }

    fn componentize_guest(output_path: &Path) -> Vec<u8> {
        let module_path = guest_module_path();
        let output = run_guest_cargo(&[
            "run".into(),
            "--locked".into(),
            "--quiet".into(),
            "--release".into(),
            "--bin".into(),
            "componentize".into(),
            "--".into(),
            module_path.to_string_lossy().into_owned(),
            output_path.to_string_lossy().into_owned(),
        ]);
        assert_guest_command(output, "WIT Component 封装");
        fs::read(output_path).unwrap_or_else(|error| {
            panic!(
                "yaml guest Component 输出不存在: {}: {error}",
                output_path.display()
            )
        })
    }

    pub(crate) fn guest_component() -> Vec<u8> {
        static COMPONENT: OnceLock<Vec<u8>> = OnceLock::new();
        COMPONENT
            .get_or_init(|| {
                guest_module();
                let temp = tempfile::tempdir().expect("无法创建 YAML Component 临时目录");
                componentize_guest(&temp.path().join("yaml-guest.component.wasm"))
            })
            .clone()
    }

    #[test]
    fn yaml_guest_builds_wasm_module() {
        let module = guest_module();
        assert!(module.len() >= 8, "guest wasm 太短");
        assert_eq!(&module[..4], b"\0asm");
        assert_eq!(&module[4..8], [1, 0, 0, 0]);
    }

    #[test]
    fn yaml_guest_componentizes_with_checked_in_wit() {
        let component = guest_component();
        assert!(component.len() >= 8, "YAML Component 太短");
        assert_eq!(&component[..4], b"\0asm");
        assert_eq!(&component[4..8], [13, 0, 1, 0]);
    }

    #[test]
    fn yaml_componentizer_releases_output_file_before_returning() {
        guest_module();
        let temp = tempfile::tempdir().expect("无法创建 YAML Component 生命周期临时目录");
        let output = temp.path().join("yaml-guest.component.wasm");
        componentize_guest(&output);
        let moved = temp.path().join("yaml-guest.component.moved.wasm");
        fs::rename(&output, &moved).expect("Componentizer 返回后输出文件仍被占用");
        fs::remove_file(&moved).expect("无法删除已关闭的 Component 输出文件");
    }

    /// V1 源 → wire 程序（无 Package 函数）。
    fn wire(source: &str) -> Value {
        let script = parse_script(source).unwrap();
        let library = parse_function_library("functions: {}\n").unwrap();
        build_program(&script, &library, Default::default(), 0)
    }

    fn host_with_permissions(trace: Arc<tests::Trace>, permissions: &[&str]) -> HostApi {
        let permissions = permissions
            .iter()
            .map(|permission| format!("\"{permission}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let manifest = crate::extensions::parse_manifest(
            format!(
                r#"manifest_version = 2
id = "gamer-yaml"
version = "3.0.0"
name = "自动化"
entry = "plugin.wasm"
permissions = [{permissions}]
[host_api]
device = "^1.0"
input = "^1.0"
runtime = "^1.0"
"#
            )
            .as_bytes(),
        )
        .unwrap();
        HostApi::for_manifest(
            CapabilityRegistry::builder()
                .with_device_service(trace.clone() as Arc<dyn crate::capabilities::DeviceService>)
                .with_input_service(trace as Arc<dyn crate::capabilities::InputService>)
                .with_frame_service(tests::VisionStub::new(FrameSize::new(1920, 1080)))
                .build(),
            crate::extensions::HostApiCatalog::default(),
            &manifest,
        )
        .unwrap()
    }

    fn run_request(
        program: Value,
        host: HostApi,
        stop: Arc<AtomicBool>,
        sink: Option<Arc<dyn EventSink>>,
    ) -> YamlWasmRunRequest {
        YamlWasmRunRequest {
            execution_policy: None,
            private_templates: None,
            notification: None,
            ocr: None,
            live_switcher: None,
            wasm: guest_component(),
            program,
            host,
            context: AppContext::for_test("device-1", "com.example.game").unwrap(),
            stop,
            sink,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn real_yaml_component_switches_live_source_through_native_function() {
        let trace = Arc::new(tests::Trace::default());
        let host = tests::vision_host(
            trace,
            &tests::VisionStub::new(FrameSize::new(1920, 1080)),
            tests::LogTrace::new(),
            &["media.stream"],
        );
        let script = parse_script("run:\n  - switch_live_device: {device_id: browser-next}\n    as: switched\n  - return: $switched\n").unwrap();
        let mut request = run_request(
            build_program(&script, &vec![], Default::default(), 0),
            host,
            Arc::new(AtomicBool::new(false)),
            None,
        );
        request.live_switcher = Some(Arc::new(|action, values| {
            Box::pin(async move {
                assert_eq!(action, "stream.switch");
                assert_eq!(values, json!({"device_id":"browser-next"}));
                Ok(
                    json!({"device_id":"browser-next","state":"streaming","output_id":"existing-output"}),
                )
            })
        }));
        let result = LazyYamlWasmtimeRuntime::new().run(request).await.unwrap();
        assert_eq!(
            result.value,
            json!({"device_id":"browser-next","state":"streaming","output_id":"existing-output"})
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn real_yaml_component_waits_for_live_confirmation_through_native_function() {
        let trace = Arc::new(tests::Trace::default());
        let host = tests::vision_host(
            trace.clone(),
            &tests::VisionStub::new(FrameSize::new(1920, 1080)),
            tests::LogTrace::new(),
            &["runtime.sleep"],
        );
        let script = parse_script(
            "run:\n  - wait_live_confirmation: {}\n    as: decision\n  - return: $decision\n",
        )
        .unwrap();
        let mut request = run_request(
            build_program(&script, &vec![], Default::default(), 0),
            host,
            Arc::new(AtomicBool::new(false)),
            None,
        );
        request.live_switcher = Some(Arc::new(|action, values| {
            Box::pin(async move {
                assert_eq!(action, "confirmation.begin");
                assert_eq!(values, json!({"timeout_secs":300}));
                Ok(json!({"status":"cancelled", "open_id":"must-not-leak"}))
            })
        }));
        let result = LazyYamlWasmtimeRuntime::new().run(request).await.unwrap();
        assert_eq!(result.value, json!({"status":"cancelled"}));
        assert!(trace.apps.lock().unwrap().is_empty());
        assert!(trace.taps.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn real_yaml_component_match_then_tap_uses_real_frame_dimensions() {
        let trace = Arc::new(tests::Trace::default());
        let vision = tests::VisionStub::new(FrameSize::new(1920, 1080));
        vision.push_outcome(MatchOutcome::Found(crate::capabilities::MatchBox {
            x: 242,
            y: 839,
            width: 217,
            height: 64,
            score: 0.97,
        }));
        let host = tests::vision_host(
            trace.clone(),
            &vision,
            tests::LogTrace::new(),
            &["device.read", "vision.match", "resource.read", "input.tap"],
        );
        let program = wire("run:\n  - match_templates:\n      cases:\n        - template: reward.png\n          as: recv\n          do:\n            - tap: $recv\n            - tap: $recv.center\n");
        LazyYamlWasmtimeRuntime::new()
            .run(run_request(
                program,
                host,
                Arc::new(AtomicBool::new(false)),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(*trace.taps.lock().unwrap(), vec![[350, 871]; 2]);
        assert_eq!(vision.match_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            vision.captures.load(Ordering::SeqCst),
            1,
            "点击不应额外截图"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn real_yaml_component_polls_fresh_frames_and_breaks_after_actions() {
        let trace = Arc::new(tests::Trace::default());
        let vision = tests::VisionStub::new(FrameSize::new(1920, 1080));
        vision.push_outcome(MatchOutcome::NotFound);
        vision.push_outcome(MatchOutcome::Found(crate::capabilities::MatchBox {
            x: 100,
            y: 200,
            width: 20,
            height: 20,
            score: 0.97,
        }));
        let host = tests::vision_host(
            trace.clone(),
            &vision,
            tests::LogTrace::new(),
            &[
                "device.read",
                "vision.match",
                "resource.read",
                "input.tap",
                "runtime.sleep",
            ],
        );
        let program = wire("run:\n  - match_templates:\n      times: 999\n      interval: 1ms\n      cases:\n        - template: reward.png\n          as: hit\n          do:\n            - tap: $hit\n            - break: {}\n  - return: done\n");
        assert_eq!(program["run"][0]["op"], "match_templates_loop");
        let sink = tests::EventCollect::new();
        let result = LazyYamlWasmtimeRuntime::new()
            .run(run_request(
                program,
                host,
                Arc::new(AtomicBool::new(false)),
                Some(sink.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(result.value, json!("done"));
        assert_eq!(*trace.taps.lock().unwrap(), vec![[110, 210]]);
        assert_eq!(vision.captures.load(Ordering::SeqCst), 2);
        assert_eq!(vision.match_calls.load(Ordering::SeqCst), 2);
        let sleeps: Vec<_> = sink
            .of("detail")
            .into_iter()
            .filter(|event| event["name"] == "arguments" && event["data"]["function"] == "sleep")
            .collect();
        assert_eq!(sleeps.len(), 1, "should wait only between the two captures");
        assert_eq!(sleeps[0]["data"]["args"]["duration"], "1ms");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn real_yaml_component_tap_accepts_match_reference_and_center() {
        let trace = Arc::new(tests::Trace::default());
        let runtime = LazyYamlWasmtimeRuntime::new();
        let mut program =
            wire("run:\n  - tap: $hit\n  - tap: {position: $hit}\n  - tap: $hit.center\n");
        program["vars"] = json!({"hit":{"center":{"x":350.0/1920.0,"y":871.0/1080.0},"x":242,"y":839,"template":"reward.png"}});
        runtime
            .run(run_request(
                program,
                host_with_permissions(trace.clone(), &["device.read", "input.tap"]),
                Arc::new(AtomicBool::new(false)),
                None,
            ))
            .await
            .unwrap();
        let taps = trace.taps.lock().unwrap();
        assert_eq!(*taps, vec![[350, 871]; 3]);
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires an explicit pre-upgrade component in GAMER_OLD_YAML_COMPONENT"]
    async fn old_yaml_component_is_rejected_before_any_host_input() {
        let old =
            std::env::var("GAMER_OLD_YAML_COMPONENT").expect("provide preserved old component");
        let trace = Arc::new(tests::Trace::default());
        let events = tests::EventCollect::new();
        let mut request = run_request(
            wire("run: [{input_text: must_not_send}]"),
            host_with_permissions(trace.clone(), &["device.read", "input.text"]),
            Arc::new(AtomicBool::new(false)),
            Some(events.clone()),
        );
        request.wasm = std::fs::read(old).unwrap();
        let error = LazyYamlWasmtimeRuntime::new()
            .run(request)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("YAML_GUEST_UPGRADE_REQUIRED"),
            "{error}"
        );
        assert!(trace.text.lock().unwrap().is_empty());
        assert!(trace.taps.lock().unwrap().is_empty());
        assert!(trace.keys.lock().unwrap().is_empty());
        assert!(
            events.of("run_start").is_empty(),
            "the probe must not emit a misleading run"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn real_yaml_component_v2_observes_once_taps_once_and_checks_goal() {
        let trace = Arc::new(tests::Trace::default());
        let vision = tests::VisionStub::new(FrameSize::new(1000, 1000));
        vision.push_outcome(tests::stub_outcome());
        vision.push_outcome(tests::stub_outcome());
        let host = tests::vision_host(
            trace.clone(),
            &vision,
            tests::LogTrace::new(),
            &[
                "device.read",
                "vision.match",
                "resource.read",
                "input.tap",
                "runtime.sleep",
            ],
        );
        let script = syntax::parse_script("targets: {button: {template: button.png}, done: {template: done.png}}\nrun: [{trace: false}, {wait: button, then: [{tap: button}]}, {trace: true}, {finish: done}]\n").unwrap();
        let program = syntax::build_program(&script, &vec![], Default::default(), 0);
        let result = LazyYamlWasmtimeRuntime::new()
            .run(run_request(
                program,
                host,
                Arc::new(AtomicBool::new(false)),
                None,
            ))
            .await;
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(trace.taps.lock().unwrap().len(), 1);
        assert_eq!(vision.match_calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn real_yaml_component_runs_v1_program_with_native_functions() {
        let trace = Arc::new(tests::Trace::default());
        let runtime = LazyYamlWasmtimeRuntime::new();
        let mut program =
            wire("run:\n  - input_text: from-real-wasm\n  - tap: [0.5, 0.5]\n  - return: done\n");
        program["_native_settings"] =
            json!({"default_timeout_secs":10,"before_click_ms":17,"after_click_ms":29});
        let sink = tests::EventCollect::new();
        let result = runtime
            .run(run_request(
                program,
                host_with_permissions(trace.clone(), &["device.read", "input.text", "input.tap"]),
                Arc::new(AtomicBool::new(false)),
                Some(sink.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(result.value, Value::String("done".into()));
        assert_eq!(trace.text.lock().unwrap().as_slice(), ["from-real-wasm"]);
        let delays: Vec<_> = sink
            .of("detail")
            .into_iter()
            .filter(|e| e["name"] == "click_delay")
            .map(|e| e["data"]["duration_ms"].as_u64().unwrap())
            .collect();
        assert_eq!(
            delays,
            vec![17, 29],
            "每次原生调用必须使用本次运行的全局快照"
        );
        assert_eq!(trace.taps.lock().unwrap().len(), 1);
        assert!(runtime.is_available());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn yaml_component_runs_package_function_from_frozen_table() {
        let trace = Arc::new(tests::Trace::default());
        let runtime = LazyYamlWasmtimeRuntime::new();
        let script =
            parse_script("run:\n  - greet:\n      who: V1\n    as: out\n  - return: $out\n")
                .unwrap();
        let library = parse_function_library(
            "functions:\n  greet:\n    params:\n      who:\n        type: string\n        default: world\n    run:\n      - input_text: $who\n      - return: $who\n",
        )
        .unwrap();
        let program = build_program(&script, &library, Default::default(), 0);
        let result = runtime
            .run(run_request(
                program,
                host_with_permissions(trace.clone(), &["device.read", "input.text"]),
                Arc::new(AtomicBool::new(false)),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(result.value, Value::String("V1".into()));
        assert_eq!(trace.text.lock().unwrap().as_slice(), ["V1"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn yaml_component_preserves_permission_and_cancellation_kinds() {
        let runtime = LazyYamlWasmtimeRuntime::new();
        let denied = runtime
            .run(run_request(
                wire("run:\n  - input_text: denied\n"),
                host_with_permissions(Arc::new(tests::Trace::default()), &["device.read"]),
                Arc::new(AtomicBool::new(false)),
                None,
            ))
            .await
            .unwrap_err();
        assert!(
            denied.to_string().contains("kind=denied"),
            "permission denial lost its WIT kind: {denied:#}"
        );

        // stop 先于运行置位：sleep 函数的 runtime.sleep 报 kind=cancelled。
        let cancelled = runtime
            .run(run_request(
                wire("run:\n  - sleep: 1s\n"),
                host_with_permissions(
                    Arc::new(tests::Trace::default()),
                    &["device.read", "runtime.sleep"],
                ),
                Arc::new(AtomicBool::new(true)),
                None,
            ))
            .await
            .unwrap_err();
        let message = cancelled.to_string();
        assert!(
            message.contains("kind=cancelled") || message.contains("CANCELLED"),
            "cancellation must surface as a cancel-shaped error: {message}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn yaml_component_emits_run_events_and_budget_code() {
        let runtime = LazyYamlWasmtimeRuntime::new();
        let sink = tests::EventCollect::new();
        let trace = Arc::new(tests::Trace::default());
        let mut program =
            wire("run:\n  - input_text: one\n  - repeat: 2\n    do:\n      - input_text: tick\n");
        program["trace"] = json!({"run_id":"trace-qa", "entry":{"package_id":"qa","plugin_id":"gamer-yaml","path":"automations/main.yaml","version":"v1"}, "functions":{}});
        let _ = runtime
            .run(run_request(
                program,
                host_with_permissions(trace, &["device.read", "input.text"]),
                Arc::new(AtomicBool::new(false)),
                Some(sink.clone()),
            ))
            .await
            .unwrap();
        let paths: Vec<String> = sink
            .of("step_start")
            .iter()
            .filter_map(|event| event["path"].as_str().map(str::to_string))
            .collect();
        for event in sink.of("step_start") {
            assert_eq!(event["trace"]["run_id"], "trace-qa");
            assert_eq!(event["trace"]["source"]["version"], "v1");
            assert_eq!(event["trace"]["frame_id"], 0);
        }
        assert_eq!(
            paths,
            vec!["run[0]", "run[1]", "run[1].do[0]", "run[1].do[0]"]
        );
        let ends = sink.of("run_end");
        assert_eq!(
            ends.last().map(|event| event["ok"].clone()),
            Some(Value::Bool(true))
        );

        // 巨大次数空转 repeat → STEP_BUDGET_EXCEEDED + budget 事件
        let sink = tests::EventCollect::new();
        let trace = Arc::new(tests::Trace::default());
        let error = runtime
            .run(run_request(
                wire("run:\n  - repeat: 4294967295\n    do: []\n"),
                host_with_permissions(trace, &["device.read"]),
                Arc::new(AtomicBool::new(false)),
                Some(sink.clone()),
            ))
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("STEP_BUDGET_EXCEEDED"),
            "{error:#}"
        );
        assert!(sink
            .of("budget")
            .iter()
            .any(|event| event["kind"] == "STEP_BUDGET_EXCEEDED"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn yaml_component_honors_top_level_start_index() {
        let runtime = LazyYamlWasmtimeRuntime::new();
        let trace = Arc::new(tests::Trace::default());
        let script =
            parse_script("run:\n  - input_text: first\n  - input_text: second\n  - return: done\n")
                .unwrap();
        let library = parse_function_library("functions: {}\n").unwrap();
        let program = build_program(&script, &library, Default::default(), 1);
        let result = runtime
            .run(run_request(
                program,
                host_with_permissions(trace.clone(), &["device.read", "input.text"]),
                Arc::new(AtomicBool::new(false)),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(result.value, Value::String("done".into()));
        assert_eq!(trace.text.lock().unwrap().as_slice(), ["second"]);
    }

    /// 生命周期 e2e：安装 → 启用 → V1 脚本经 run_yaml_program 全链跑通；卸载
    /// 后同脚本明确失败。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn installed_yaml_extension_runs_v1_program_end_to_end() {
        let temp = tempfile::tempdir().expect("无法创建 yaml 扩展临时目录");
        let logs = tests::LogTrace::new();
        let registry = CapabilityRegistry::builder()
            .with_device_service(
                Arc::new(tests::Trace::default()) as Arc<dyn crate::capabilities::DeviceService>
            )
            .with_log_service(logs.clone() as Arc<dyn crate::capabilities::LogService>)
            .build();
        let service = Arc::new(
            crate::extensions::ExtensionService::for_data_root(temp.path(), registry)
                .with_runner_registrar(Arc::new(InstanceFreeRegistrar))
                .with_builtin_service(Arc::new(
                    crate::extensions::notify::NotifyService::new(temp.path()).unwrap(),
                )),
        );

        let mut archive = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut archive));
            let options = SimpleFileOptions::default();
            writer.start_file("manifest.toml", options).unwrap();
            writer
                .write_all(YAML_EXTENSION_MANIFEST_TOML.as_bytes())
                .unwrap();
            writer.start_file("plugin.wasm", options).unwrap();
            writer.write_all(&guest_component()).unwrap();
            writer.start_file("ui/plugin.js", options).unwrap();
            writer.write_all(b"export const sdkVersion = 1;").unwrap();
            super::super::external_schemas::append_schemas(&mut writer);
            writer.finish().unwrap();
        }
        let installed = service.install(&archive).await.unwrap();
        let id = crate::extensions::ExtensionId::parse(YAML_EXTENSION_ID).unwrap();
        service.enable(&id).await.unwrap();
        service.start(&id).await.unwrap();

        let value = super::super::run_yaml_program(
            &service,
            wire("run:\n  - notify: {content: 通知测试}\n    as: delivery\n  - log: from-v1-e2e\n  - return: $delivery.status\n"),
            AppContext::for_test("device-1", "com.example.game").unwrap(),
            Arc::new(AtomicBool::new(false)),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(value, json!("skipped"));
        assert_eq!(logs.messages().last().unwrap(), "from-v1-e2e");

        // The same step reaches the optional plugin once it is installed,
        // without changing the script or its Package dependencies.
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file("manifest.toml", SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(include_str!("../../gamer-notify/manifest.toml").as_bytes())
            .unwrap();
        writer
            .start_file("ui/plugin.js", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"export const sdkVersion = 1;").unwrap();
        let archive = writer.finish().unwrap().into_inner();
        let notify_id = crate::extensions::ExtensionId::parse("gamer-notify").unwrap();
        service.install(&archive).await.unwrap();
        service.enable(&notify_id).await.unwrap();
        service.start(&notify_id).await.unwrap();
        let delivery = super::super::run_yaml_program(
            &service,
            wire(
                "run:\n  - notify: {content: 缺少通道}\n    as: delivery\n  - return: $delivery\n",
            ),
            AppContext::for_test("device-1", "com.example.game").unwrap(),
            Arc::new(AtomicBool::new(false)),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(delivery["status"], "skipped");
        assert!(delivery["id"].is_string());
        let history = service
            .call_extension(&notify_id, "records.read", json!({}))
            .await
            .unwrap();
        assert_eq!(history["records"][0]["source"], "script");
        service.disable(&notify_id).await.unwrap();

        service.disable(&id).await.unwrap();
        assert!(service
            .uninstall(&id, installed.active_version())
            .await
            .unwrap());
        assert!(super::super::run_yaml_program(
            &service,
            wire("run: []\n"),
            AppContext::for_test("device-1", "com.example.game").unwrap(),
            Arc::new(AtomicBool::new(false)),
            None,
            None,
        )
        .await
        .is_err());
    }
}
