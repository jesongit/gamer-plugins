//! gamer-yaml 原生（插件）函数注册表——V2 函数库两种来源之一（计划 Phase 3.2）。
//!
//! 原生函数在宿主（扩展边界）内以 Rust 实现，组合 Core capability 原语；
//! 解释器经 `__fn` 通道按名派发（`tap`、`find`、`sleep` 等不是语法关键字）。
//! Schema（名称/描述/参数/返回/权限）是执行校验、entrypoint 提示与前端
//! 参数表单的共同来源；新增函数只改本表 + 一个 handler，不动解释器。
//!
//! Package 函数（`automations/_function*.yaml`，简化计划 Phase 1 前缀识别）
//! 是另一种来源，由 YAML 写成、解释器本地执行；两种来源调用语法一致，
//! 同名即冲突（组合期拒绝）。

use serde_json::{json, Value};

use crate::extensions::permissions::Permission;

use super::syntax::ParamType;

/// 参数 Schema（静态表形态）。
pub struct ParamSchema {
    pub name: &'static str,
    pub ty: ParamType,
    pub required: bool,
    pub default: Option<Value>,
    pub desc: &'static str,
    pub item_type: Option<ParamType>,
}

/// 原生函数声明。
pub struct NativeFunction {
    pub name: &'static str,
    pub display_name: &'static str,
    pub description: &'static str,
    pub params: Vec<ParamSchema>,
    pub returns: &'static str,
    /// 运行前必须通过的插件权限（解释器调用不绕过权限）。
    pub permissions: &'static [Permission],
}

fn p(
    name: &'static str,
    ty: ParamType,
    required: bool,
    default: Option<Value>,
    desc: &'static str,
) -> ParamSchema {
    ParamSchema {
        item_type: None,
        name,
        ty,
        required,
        default,
        desc,
    }
}

const RETURN_NULL: &str = "null";
const RETURN_MATCH: &str = "match?";
const RETURN_BOOL: &str = "boolean";

fn ocr_params() -> Vec<ParamSchema> {
    vec![
        p(
            "region",
            ParamType::List,
            false,
            None,
            "原始截图区域 [x,y,w,h]（0..1 相对坐标）",
        ),
        p(
            "language",
            ParamType::String,
            false,
            Some(json!("chi_sim+eng")),
            "已安装的 Tesseract 语言，例如 chi_sim+eng / eng",
        ),
        p(
            "mode",
            ParamType::String,
            false,
            Some(json!("block")),
            "版面模式：line / block / sparse",
        ),
        p(
            "whitelist",
            ParamType::String,
            false,
            None,
            "可选字符白名单，例如 0123456789:",
        ),
        p(
            "scale",
            ParamType::Integer,
            false,
            Some(json!(2)),
            "识别前放大倍数 1..3；结果坐标始终对应原图",
        ),
        p(
            "timeout",
            ParamType::Duration,
            false,
            Some(json!("10s")),
            "本地识别时间上限，最多 15s；超时明确失败",
        ),
    ]
}

pub(crate) fn native_functions() -> &'static [NativeFunction] {
    static FUNCTIONS: std::sync::LazyLock<Vec<NativeFunction>> = std::sync::LazyLock::new(|| {
        let mut functions = vec![
            NativeFunction {
                name: "trace",
                display_name: "图片记录",
                description: "开关自动图片采集；边界和异常仍保留证据",
                params: vec![p(
                    "enabled",
                    ParamType::Boolean,
                    true,
                    None,
                    "是否启用自动图片",
                )],
                returns: RETURN_NULL,
                permissions: &[],
            },
            NativeFunction {
                name: "fail",
                display_name: "明确失败",
                description: "立即失败并记录原因",
                params: vec![p("message", ParamType::String, true, None, "失败原因"),
                    p("code", ParamType::String, false, None, "稳定错误码"),
                    p("stage", ParamType::String, false, None, "失败阶段"),
                    p("expected", ParamType::Any, false, None, "预期状态"),
                    p("actual", ParamType::Any, false, None, "实际状态"),
                    p("retryable", ParamType::Boolean, false, Some(json!(false)), "是否可通过环境恢复重试")],
                returns: RETURN_NULL,
                permissions: &[],
            },
            NativeFunction {
                name: "hover",
                display_name: "鼠标悬浮",
                description: "浏览器移到指定位置而不点击，复用输入屏障与截图校验；Android 不支持",
                params: vec![p("position", ParamType::Point, true, None, "坐标或匹配结果")],
                returns: RETURN_NULL,
                permissions: &[Permission::InputTap],
            },
            NativeFunction {
                name: "tap",
                display_name: "点击",
                description: "点击坐标或匹配结果；匹配结果自动点击中心，不重新识别",
                params: vec![p(
                    "position",
                    ParamType::Point,
                    true,
                    None,
                    "坐标 [x, y] / {x, y}，或匹配结果（自动取 center）",
                )],
                returns: RETURN_NULL,
                permissions: &[Permission::InputTap],
            },
            NativeFunction {
                name: "swipe",
                display_name: "滑动",
                description: "从起点滑动到终点",
                params: vec![
                    p("from", ParamType::Point, true, None, "起点"),
                    p("to", ParamType::Point, true, None, "终点"),
                    p(
                        "duration",
                        ParamType::Duration,
                        false,
                        Some(json!("300ms")),
                        "滑动时长",
                    ),
                ],
                returns: RETURN_NULL,
                permissions: &[Permission::InputSwipe],
            },
            NativeFunction {
                name: "press",
                display_name: "长按",
                description: "在新鲜匹配位置按住后释放；位置不移动，用于录制中的长按控件",
                params: vec![
                    p(
                        "position",
                        ParamType::Point,
                        true,
                        None,
                        "匹配结果或相对坐标",
                    ),
                    p(
                        "duration",
                        ParamType::Duration,
                        false,
                        Some(json!("800ms")),
                        "按住时长 200ms..3s",
                    ),
                ],
                returns: RETURN_NULL,
                permissions: &[Permission::InputSwipe],
            },
            NativeFunction {
                name: "key",
                display_name: "按键",
                description: "发送按键（HOME/BACK/…或数字 keycode）",
                params: vec![
                    p(
                        "key",
                        ParamType::Key,
                        true,
                        None,
                        "按键名（浏览器支持字母/方向键等）或 Android keycode",
                    ),
                    p(
                        "action",
                        ParamType::String,
                        false,
                        Some(json!("press")),
                        "press/down/up",
                    ),
                ],
                returns: RETURN_NULL,
                permissions: &[Permission::InputKey],
            },
            NativeFunction {
                name: "input_text",
                display_name: "输入文本",
                description: "向设备输入文本",
                params: vec![p("text", ParamType::String, true, None, "文本内容")],
                returns: RETURN_NULL,
                permissions: &[Permission::InputText],
            },
            NativeFunction {
                name: "launch",
                display_name: "启动应用",
                description: "冷启动应用（缺省为设备配置的应用）",
                params: vec![p(
                    "package",
                    ParamType::String,
                    false,
                    None,
                    "Android 包名，缺省用设备配置",
                )],
                returns: RETURN_NULL,
                permissions: &[Permission::DeviceApp],
            },
            NativeFunction {
                name: "stop_app",
                display_name: "停止应用",
                description: "停止应用（缺省为设备配置的应用）",
                params: vec![p(
                    "package",
                    ParamType::String,
                    false,
                    None,
                    "Android 包名，缺省用设备配置",
                )],
                returns: RETURN_NULL,
                permissions: &[Permission::DeviceApp],
            },
            NativeFunction {
                name: "reset_input",
                display_name: "重置输入入口",
                description: "浏览器刷新当前绑定网页，Android 冷启动当前配置 App；清理按住状态并使旧截图失效，随后仍须等待并识别入口画面",
                params: vec![],
                returns: RETURN_NULL,
                permissions: &[Permission::DeviceApp],
            },
            NativeFunction {
                name: "sleep",
                display_name: "等待",
                description: "等待指定时长（取消可达）",
                params: vec![p(
                    "duration",
                    ParamType::Duration,
                    true,
                    None,
                    "如 500ms / 1.5s / 2min",
                )],
                returns: RETURN_NULL,
                permissions: &[Permission::RuntimeSleep],
            },
            NativeFunction {
                name: "log",
                display_name: "日志",
                description: "写运行日志（非字符串值自动转 JSON 文本）",
                params: vec![
                    p("message", ParamType::Any, true, None, "日志内容"),
                    p(
                        "level",
                        ParamType::String,
                        false,
                        Some(json!("info")),
                        "info/debug/warn/error",
                    ),
                ],
                returns: RETURN_NULL,
                permissions: &[Permission::LogWrite],
            },
            NativeFunction {
                name: "find",
                display_name: "查找模板",
                description: "单次模板匹配；未找到返回 null（等待轮询用 wait_find）",
                params: vec![
                    p("template", ParamType::Template, true, None, "模板短名"),
                    p(
                        "threshold",
                        ParamType::Number,
                        false,
                        Some(json!(0.8)),
                        "匹配阈值 0..1",
                    ),
                    p(
                        "region",
                        ParamType::List,
                        false,
                        None,
                        "搜索区域 [x, y, w, h]（相对坐标）",
                    ),
                ],
                returns: RETURN_MATCH,
                permissions: &[Permission::VisionMatch, Permission::ResourceRead],
            },
            NativeFunction {
                name: "find_any",
                display_name: "查找首个模板",
                description:
                    "共用一帧按顺序匹配，返回首个命中及 index，不点击；全部未命中返回 null",
                params: vec![
                    ParamSchema {
                        item_type: Some(ParamType::Template),
                        ..p(
                            "templates",
                            ParamType::List,
                            true,
                            None,
                            "按优先级排序的模板列表（1..64 项）",
                        )
                    },
                    p(
                        "threshold",
                        ParamType::Number,
                        false,
                        Some(json!(0.8)),
                        "匹配阈值 0..1",
                    ),
                    p(
                        "region",
                        ParamType::List,
                        false,
                        None,
                        "所有模板共用的搜索区域 [x, y, w, h]（相对坐标）；缺省沿用各模板文件名区域",
                    ),
                ],
                returns: RETURN_MATCH,
                permissions: &[Permission::VisionMatch, Permission::ResourceRead],
            },
            NativeFunction {
                name: "wait_find",
                display_name: "等待模板出现",
                description: "等待模板出现，默认仅观察（click: true 才点击）；超时返回 null",
                params: vec![
                    p("template", ParamType::Template, true, None, "模板短名"),
                    p("forever", ParamType::Boolean, false, Some(json!(false)), "持续等待直到命中或取消；忽略 timeout，设备和模板错误仍立即失败"),
                    ParamSchema {
                        item_type: Some(ParamType::Template),
                        ..p(
                            "obstacles",
                            ParamType::List,
                            false,
                            Some(json!([])),
                            "障碍模板：按顺序命中首个就点击，下轮重新检查；计入总超时",
                        )
                    },
                    p(
                        "click",
                        ParamType::Boolean,
                        false,
                        Some(json!(false)),
                        "命中后是否点击模板中心（前后延迟使用自动化设置）",
                    ),
                    p(
                        "threshold",
                        ParamType::Number,
                        false,
                        Some(json!(0.8)),
                        "匹配阈值 0..1",
                    ),
                    p(
                        "timeout",
                        ParamType::Duration,
                        false,
                        Some(json!(format!("{}s", super::settings::DEFAULT_TIMEOUT_SECS))),
                        "等待上限",
                    ),
                    p(
                        "interval",
                        ParamType::Duration,
                        false,
                        Some(json!("250ms")),
                        "轮询间隔",
                    ),
                    p(
                        "region",
                        ParamType::List,
                        false,
                        None,
                        "搜索区域 [x, y, w, h]（相对坐标）",
                    ),
                ],
                returns: RETURN_MATCH,
                permissions: &[Permission::VisionMatch, Permission::ResourceRead],
            },
            NativeFunction {
                name: "tap_template",
                display_name: "点击模板",
                description: "查找模板并点击其中心（未找到不点击，返回 null）",
                params: vec![
                    p("template", ParamType::Template, true, None, "模板短名"),
                    p(
                        "threshold",
                        ParamType::Number,
                        false,
                        Some(json!(0.8)),
                        "匹配阈值 0..1",
                    ),
                    p(
                        "timeout",
                        ParamType::Duration,
                        false,
                        Some(json!(format!("{}s", super::settings::DEFAULT_TIMEOUT_SECS))),
                        "轮询上限；0 = 只试一次",
                    ),
                    p(
                        "interval",
                        ParamType::Duration,
                        false,
                        Some(json!("100ms")),
                        "轮询间隔",
                    ),
                    p(
                        "region",
                        ParamType::List,
                        false,
                        None,
                        "搜索区域 [x, y, w, h]（相对坐标）",
                    ),
                ],
                returns: RETURN_MATCH,
                permissions: &[
                    Permission::VisionMatch,
                    Permission::ResourceRead,
                    Permission::InputTap,
                ],
            },
            NativeFunction {
                name: "wait_disappear",
                display_name: "等待模板消失",
                description: "等待模板消失；超时仍存在返回 false",
                params: vec![
                    p("template", ParamType::Template, true, None, "模板短名"),
                    p(
                        "threshold",
                        ParamType::Number,
                        false,
                        Some(json!(0.8)),
                        "匹配阈值 0..1",
                    ),
                    p(
                        "timeout",
                        ParamType::Duration,
                        false,
                        Some(json!(format!("{}s", super::settings::DEFAULT_TIMEOUT_SECS))),
                        "等待上限",
                    ),
                    p(
                        "interval",
                        ParamType::Duration,
                        false,
                        Some(json!("250ms")),
                        "轮询间隔",
                    ),
                    p(
                        "region",
                        ParamType::List,
                        false,
                        None,
                        "搜索区域 [x, y, w, h]（相对坐标）",
                    ),
                ],
                returns: RETURN_BOOL,
                permissions: &[Permission::VisionMatch, Permission::ResourceRead],
            },
            NativeFunction {
                name: "eq",
                display_name: "等于",
                description: "相等比较（数字跨整型/浮点，其余按值）",
                params: vec![
                    p("a", ParamType::Any, true, None, "左值"),
                    p("b", ParamType::Any, true, None, "右值"),
                ],
                returns: RETURN_BOOL,
                permissions: &[],
            },
            NativeFunction {
                name: "ne",
                display_name: "不等于",
                description: "不等比较",
                params: vec![
                    p("a", ParamType::Any, true, None, "左值"),
                    p("b", ParamType::Any, true, None, "右值"),
                ],
                returns: RETURN_BOOL,
                permissions: &[],
            },
            NativeFunction {
                name: "gt",
                display_name: "大于",
                description: "大于（仅数字）",
                params: vec![
                    p("a", ParamType::Number, true, None, "左值"),
                    p("b", ParamType::Number, true, None, "右值"),
                ],
                returns: RETURN_BOOL,
                permissions: &[],
            },
            NativeFunction {
                name: "ge",
                display_name: "大于等于",
                description: "大于等于（仅数字）",
                params: vec![
                    p("a", ParamType::Number, true, None, "左值"),
                    p("b", ParamType::Number, true, None, "右值"),
                ],
                returns: RETURN_BOOL,
                permissions: &[],
            },
            NativeFunction {
                name: "lt",
                display_name: "小于",
                description: "小于（仅数字）",
                params: vec![
                    p("a", ParamType::Number, true, None, "左值"),
                    p("b", ParamType::Number, true, None, "右值"),
                ],
                returns: RETURN_BOOL,
                permissions: &[],
            },
            NativeFunction {
                name: "le",
                display_name: "小于等于",
                description: "小于等于（仅数字）",
                params: vec![
                    p("a", ParamType::Number, true, None, "左值"),
                    p("b", ParamType::Number, true, None, "右值"),
                ],
                returns: RETURN_BOOL,
                permissions: &[],
            },
        ];
        functions.push(NativeFunction {
            name: "notify",
            display_name: "发送通知",
            description: "即时发送或登记当前脚本/函数结束时的通知；通知插件不可用时跳过并返回原因",
            params: vec![
                p("content", ParamType::String, true, None, "通知正文"),
                p(
                    "title",
                    ParamType::String,
                    false,
                    Some(json!("")),
                    "通知标题",
                ),
                p(
                    "channel",
                    ParamType::String,
                    false,
                    Some(json!("")),
                    "全局通道 ID；留空使用默认通道",
                ),
                p(
                    "when",
                    ParamType::String,
                    false,
                    Some(json!("immediate")),
                    "immediate 立即发送；success/failure 在当前脚本或函数结束时发送",
                ),
                p(
                    "enabled",
                    ParamType::Boolean,
                    false,
                    Some(json!(true)),
                    "是否发送此通知；关闭的结果通知仍覆盖全局默认规则",
                ),
            ],
            returns: "object",
            permissions: &[],
        });
        functions.push(NativeFunction {
            name: "switch_live_device",
            display_name: "切换直播输出设备",
            description: "切换已启动直播助手的画面来源，保持输出地址和参数；省略设备时使用当前脚本目标，支持 Android 和浏览器",
            params: vec![
                p("device_id", ParamType::String, false, Some(json!("")), "目标设备 ID；留空使用当前脚本运行目标"),
                p("timeout", ParamType::Duration, false, Some(json!("10s")), "等待新设备画面并切换的上限"),
            ],
            returns: "object",
            permissions: &[Permission::MediaStream],
        });
        functions.push(NativeFunction {
            name: "wait_live_confirmation",
            display_name: "等待直播观众确认",
            description: "登录就绪后等待当前队列任务触发者确认或取消；返回 status 为 confirmed、cancelled 或 timeout，停止时中断",
            params: vec![p(
                "timeout",
                ParamType::Duration,
                false,
                Some(json!("5m")),
                "确认等待上限，1ms..5m，向上取整到秒；不能延长队列规则冻结的期限",
            )],
            returns: "object",
            permissions: &[Permission::RuntimeSleep],
        });
        functions.push(NativeFunction {
            name: "ocr",
            display_name: "识别文字",
            description:
                "按需识别原始截图文字；返回文字、置信度、行和单词；OCR 插件不可用时明确失败",
            params: ocr_params(),
            returns: "object",
            permissions: &[Permission::DeviceRead, Permission::VisionOcr],
        });
        functions.push(NativeFunction {
            name: "number",
            display_name: "提取整数",
            description: "从 OCR 文字按顺序提取非负整数；没有数字明确失败，不把识别失败当成零",
            params: vec![
                p("text", ParamType::String, true, None, "小区域 OCR 的 text"),
                p(
                    "index",
                    ParamType::Integer,
                    false,
                    Some(json!(0)),
                    "从零开始的数字序号，例如 60/300 的体力为 0",
                ),
                p(
                    "format",
                    ParamType::String,
                    false,
                    Some(json!("tokens")),
                    "tokens 按顺序提取；fraction 严格要求完整整数/整数，index 0 为分子、1 为分母，缺项或零分母明确失败",
                ),
            ],
            returns: "integer",
            permissions: &[],
        });
        functions.push(NativeFunction {
            name: "calculate", display_name: "数值计算", description: "纯数值运算：add/sub/mul/div/floor_div/min/max；可计算剩余体力可刷次数，不引入表达式语法",
            params: vec![p("op", ParamType::String, true, None, "运算名"), p("a", ParamType::Number, true, None, "左值"), p("b", ParamType::Number, true, None, "右值")],
            returns: "number", permissions: &[],
        });
        let mut text_params = vec![
            p("text", ParamType::String, true, None, "要查找的文字"),
            p(
                "match",
                ParamType::String,
                false,
                Some(json!("contains")),
                "contains 包含 / exact 完全匹配；按阅读顺序返回首个行或词",
            ),
            p(
                "min_confidence",
                ParamType::Number,
                false,
                Some(json!(0.5)),
                "最低置信度 0..1",
            ),
        ];
        text_params.extend(ocr_params());
        functions.push(NativeFunction {
            name: "find_text",
            display_name: "查找文字",
            description:
                "单次识别并查找文字；未找到返回 null，命中可直接传给 tap；等待可使用 repeat",
            params: text_params,
            returns: "match?",
            permissions: &[Permission::DeviceRead, Permission::VisionOcr],
        });
        for name in ["observe", "finish", "checkpoint"] {
            functions.push(NativeFunction {
                name,
                display_name: if name == "finish" {
                    "验证完成画面"
                } else {
                    "观察视觉目标"
                },
                description: "只观察，不自动点击；必需目标超时明确失败，可选缺失返回 null",
                params: vec![
                    p("template", ParamType::Template, true, None, "模板"),
                    p(
                        "threshold",
                        ParamType::Number,
                        false,
                        Some(json!(0.8)),
                        "匹配阈值 0..1",
                    ),
                    p("region", ParamType::List, false, None, "相对区域 [x,y,w,h]"),
                    p(
                        "timeout",
                        ParamType::Duration,
                        false,
                        Some(json!("10s")),
                        "最长等待时间",
                    ),
                    p(
                        "interval",
                        ParamType::Duration,
                        false,
                        Some(json!("250ms")),
                        "轮询间隔",
                    ),
                    p(
                        "required",
                        ParamType::Boolean,
                        false,
                        Some(json!(true)),
                        "未命中是否失败",
                    ),
                ],
                returns: RETURN_MATCH,
                permissions: &[
                    Permission::VisionMatch,
                    Permission::ResourceRead,
                    Permission::RuntimeSleep,
                ],
            });
        }
        let checkpoint = functions
            .iter_mut()
            .find(|f| f.name == "checkpoint")
            .unwrap();
        checkpoint.display_name = "验证任务完成项";
        checkpoint.description =
            "实际观察到模板后记录完成项；只读，未观察到模板明确失败，不要求统一结束标记";
        checkpoint
            .params
            .push(p("id", ParamType::String, true, None, "稳定完成项 ID"));
        for func in &mut functions {
            func.params.push(p(
                "name",
                ParamType::String,
                false,
                Some(json!(func.display_name)),
                "可视化显示名称",
            ));
        }
        functions
    });
    &FUNCTIONS
}

/// 按名查原生函数。
pub fn native_function(name: &str) -> Option<&'static NativeFunction> {
    native_functions().iter().find(|func| func.name == name)
}

/// 原生函数名集合（函数注册表组合用）。
pub fn native_names() -> std::collections::BTreeSet<String> {
    native_functions()
        .iter()
        .map(|func| func.name.to_string())
        .collect()
}

/// 原生函数 Schema → descriptor JSON（`GET /api/extensions` 函数清单与
/// entrypoint 提示共用形态）。
pub fn native_schema_json(func: &NativeFunction) -> Value {
    json!({
        "name": func.name,
        "description": func.description,
        "source": "plugin",
        "optional_extension": match func.name { "notify" => Some("gamer-notify"), "ocr" | "find_text" => Some("gamer-ocr"), "switch_live_device" | "wait_live_confirmation" => Some("gamer-live"), _ => None },
        "params": func.params.iter().map(|param| {
            let mut schema = json!({
            "name": param.name,
            "type": param.ty.canonical(),
            "required": param.required,
            "default": param.default,
            "desc": param.desc,
            });
            if let Some(item_type) = &param.item_type {
                schema["items"] = json!({"type": item_type.canonical()});
            }
            schema
        }).collect::<Vec<_>>(),
        "returns": func.returns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_functions_have_display_names_and_declared_timeouts() {
        let mut timeouts = 0;
        for function in native_functions() {
            let names: Vec<_> = function
                .params
                .iter()
                .filter(|p| p.name == "name")
                .collect();
            assert_eq!(names.len(), 1, "{}", function.name);
            assert_eq!(names[0].default, Some(json!(function.display_name)));
            assert_eq!(names[0].ty, ParamType::String);
            assert!(!names[0].required);
            if function.params.len() > 1 {
                assert_ne!(function.params[0].name, "name", "位置简写不能变");
            }
            for param in function.params.iter().filter(|p| p.name == "timeout") {
                let expected = if function.name == "wait_live_confirmation" {
                    "5m"
                } else {
                    "10s"
                };
                assert_eq!(param.default, Some(json!(expected)), "{}", function.name);
                timeouts += 1;
            }
        }
        assert_eq!(timeouts, 10);
    }
}
