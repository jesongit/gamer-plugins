//! MCP protocol only. Authorization and target-bound execution belong to AiService.
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const PROTOCOL_VERSION: &str = "2025-11-25";

#[derive(Debug)]
pub enum Request {
    Initialize {
        id: Value,
        protocol_version: String,
    },
    Ping {
        id: Value,
    },
    ToolsList {
        id: Value,
    },
    ToolsCall {
        id: Value,
        name: String,
        arguments: Value,
    },
    Notification,
    Error(Value),
}

pub fn parse(value: Value) -> Request {
    let Some(object) = value.as_object() else {
        return invalid_request();
    };
    let id = match object.get("id") {
        Some(Value::String(value)) if !value.is_empty() => Some(json!(value)),
        Some(Value::Number(value)) if value.is_i64() || value.is_u64() => Some(json!(value)),
        None => None,
        _ => return invalid_request(),
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return invalid_request();
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return invalid_request();
    };
    // JSON-RPC notifications never receive a JSON-RPC response. The HTTP route
    // acknowledges them with 202, including unknown notification methods.
    let Some(id) = id else {
        return Request::Notification;
    };
    let params = match object.get("params") {
        Some(Value::Object(params)) => params.clone(),
        None => serde_json::Map::new(),
        _ => return Request::Error(error(id, -32602, "params must be an object")),
    };
    match method {
        "initialize" => {
            let Some(protocol_version) = params.get("protocolVersion").and_then(Value::as_str)
            else {
                return Request::Error(error(id, -32602, "protocolVersion is required"));
            };
            let client_info = params.get("clientInfo").and_then(Value::as_object);
            if protocol_version.is_empty()
                || protocol_version.len() > 64
                || !params.get("capabilities").is_some_and(Value::is_object)
                || !client_info.is_some_and(|info| {
                    info.get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|name| !name.is_empty())
                        && info.get("version").and_then(Value::as_str).is_some()
                })
            {
                return Request::Error(error(id, -32602, "invalid initialize parameters"));
            }
            Request::Initialize {
                id,
                protocol_version: protocol_version.to_string(),
            }
        }
        "ping" if params.is_empty() => Request::Ping { id },
        "tools/list" => {
            // The bounded V1 catalogue fits one page. A nonempty cursor is never
            // silently ignored, which could make a client repeat the first page.
            if params.keys().any(|key| key != "cursor" && key != "_meta")
                || params
                    .get("cursor")
                    .is_some_and(|cursor| cursor.as_str() != Some(""))
            {
                Request::Error(error(id, -32602, "unsupported tools cursor"))
            } else {
                Request::ToolsList { id }
            }
        }
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Request::Error(error(id, -32602, "tool name is required"));
            };
            if name.is_empty()
                || name.len() > 128
                || !name.bytes().all(|character| {
                    character.is_ascii_alphanumeric() || matches!(character, b'_' | b'-' | b'.')
                })
                || params
                    .keys()
                    .any(|key| !matches!(key.as_str(), "name" | "arguments" | "_meta"))
            {
                return Request::Error(error(id, -32602, "invalid tool call parameters"));
            }
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !arguments.is_object() {
                return Request::Error(error(id, -32602, "tool arguments must be an object"));
            }
            Request::ToolsCall {
                id,
                name: name.to_string(),
                arguments,
            }
        }
        "ping" => Request::Error(error(id, -32602, "ping does not accept parameters")),
        _ => Request::Error(error(id, -32601, "method not found")),
    }
}

fn invalid_request() -> Request {
    Request::Error(error(Value::Null, -32600, "invalid JSON-RPC request"))
}

pub fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

pub fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}

pub fn initialize_result(requested_version: &str) -> Value {
    let manifest: toml::Value = toml::from_str(include_str!("../manifest.toml"))
        .expect("builtin AI manifest must be valid");
    let plugin_version = manifest["version"]
        .as_str()
        .expect("builtin AI manifest must declare a version");
    let version = match requested_version {
        "2025-03-26" | "2025-06-18" | "2025-11-25" => requested_version,
        _ => PROTOCOL_VERSION,
    };
    json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},
        "serverInfo":{"name":"gamer-ai","version":plugin_version},
        "instructions":"Observe with screen_capture before input. Input requires the user's active MCP control session. A paused session cannot be resumed by tools."})
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    pub content: Vec<Value>,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub structured_content: Option<Value>,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![json!({"type":"text","text":text.into()})],
            is_error: false,
            structured_content: None,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        let mut result = Self::text(text);
        result.is_error = true;
        result
    }

    pub fn json(value: Value) -> Self {
        let mut result = Self::text(value.to_string());
        if value.is_object() {
            result.structured_content = Some(value);
        }
        result
    }

    pub fn image(bytes: &[u8], mime_type: &str, metadata: Value) -> Self {
        let mut result = Self::json(metadata);
        result.content.push(image_content(bytes, mime_type));
        result
    }

    pub fn value(&self) -> Value {
        // Every field is already a JSON value; serialization cannot fail.
        serde_json::to_value(self).expect("MCP tool result contains only JSON values")
    }
}

pub fn image_content(bytes: &[u8], mime_type: &str) -> Value {
    json!({"type":"image","data":base64::engine::general_purpose::STANDARD.encode(bytes),
        "mimeType":mime_type})
}

/// Shared internal history shape. Provider converts MCP image blocks into
/// actual multimodal input, keeping the tool-call association intact.
pub fn history_output(call_id: &str, result: &ToolResult) -> Value {
    json!({"type":"function_call_output","call_id":call_id,"output":result.value()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_rpc_id_and_tool_parameters_and_silences_notifications() {
        for value in [
            json!([]),
            json!({"jsonrpc":"2.0","id":null,"method":"ping"}),
            json!({"jsonrpc":"2.0","id":1.5,"method":"ping"}),
            json!({"jsonrpc":"1.0","id":1,"method":"ping"}),
        ] {
            let Request::Error(value) = parse(value) else {
                panic!("invalid request accepted")
            };
            assert_eq!(value["error"]["code"], -32600);
            assert_eq!(value["id"], Value::Null);
        }
        assert!(matches!(
            parse(json!({"jsonrpc":"2.0","method":"notifications/initialized"})),
            Request::Notification
        ));
        assert!(matches!(
            parse(json!({"jsonrpc":"2.0","id":"p","method":"ping"})),
            Request::Ping { .. }
        ));
        let Request::Error(value) = parse(json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"input_tap","arguments":[]}}))
        else {
            panic!("array arguments accepted")
        };
        assert_eq!(value["error"]["code"], -32602);
        assert_eq!(value["id"], 3);
    }

    #[test]
    fn returns_standard_image_and_structured_text_blocks() {
        let result = ToolResult::image(&[1, 2, 3], "image/png", json!({"width":320,"height":240}));
        let value = result.value();
        assert_eq!(value["isError"], false);
        assert_eq!(
            value["content"][1],
            json!({"type":"image","data":"AQID","mimeType":"image/png"})
        );
        assert_eq!(value["structuredContent"]["width"], 320);
        assert_eq!(history_output("call-1", &result)["call_id"], "call-1");
    }

    #[test]
    fn negotiates_protocol_and_rejects_incomplete_initialize() {
        let request = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"1"}}});
        assert!(matches!(parse(request), Request::Initialize { .. }));
        assert_eq!(
            initialize_result("2025-06-18")["protocolVersion"],
            "2025-06-18"
        );
        assert_eq!(
            initialize_result("unknown")["protocolVersion"],
            PROTOCOL_VERSION
        );
        assert!(matches!(
            parse(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}})),
            Request::Error(_)
        ));
    }
}
