//! Optional cross-plugin bridge; the notify function remains available without a sender.
use crate::extensions::{ExtensionId, ExtensionService, PluginCallContext};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::{Arc, Weak};

pub type Sender = Arc<dyn Fn(Value) -> BoxFuture<'static, Value> + Send + Sync>;
pub fn sender(
    service: Weak<ExtensionService>,
    caller: PluginCallContext,
    run_id: Option<String>,
) -> Sender {
    Arc::new(move |mut values| {
        let service = service.clone();
        let caller = caller.clone();
        let run_id = run_id.clone();
        Box::pin(async move {
            let Some(service) = service.upgrade() else {
                return skipped("通知发送能力不可用");
            };
            // Every execution of a script step is a distinct business notification.
            values["source"] = json!("script");
            values["source_id"] = json!(format!(
                "{}:{}",
                run_id.as_deref().unwrap_or("script"),
                uuid::Uuid::new_v4()
            ));
            let id = ExtensionId::parse("gamer-notify").expect("notification extension id");
            // A stopping target can hold its lifecycle gate while HTTP workers drain.
            // Notification admission must not stall the script behind that drain.
            match tokio::time::timeout(
                std::time::Duration::from_secs(1),
                service.call_extension_from_plugin(&caller, &id, "notification.send", values),
            )
            .await
            {
                Ok(Ok(response)) => {
                    json!({"accepted":response["accepted"], "id":response["record"]["id"], "status":response["record"]["status"], "reason":response["record"]["message"]})
                }
                Ok(Err(_)) => skipped("通知插件未安装、未启用、版本不兼容或发送权限／能力不可用"),
                Err(_) => skipped("通知发送能力繁忙或正在停用，已跳过发送"),
            }
        })
    })
}
pub fn skipped(reason: &str) -> Value {
    json!({"accepted":false,"status":"skipped","reason":reason})
}
