//! Optional cross-plugin bridge; the notify function remains available without a sender.
use crate::extensions::{ExtensionId, ExtensionService, PluginCallContext};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::{Arc, Weak};

#[derive(Default)]
pub(crate) struct ResultNotifications {
    pending: std::sync::Mutex<std::collections::BTreeMap<u64, Vec<Policy>>>,
    failure_attempted: std::sync::atomic::AtomicBool,
}
struct Policy {
    when: String,
    enabled: bool,
    values: Value,
}
impl ResultNotifications {
    pub(crate) fn register(&self, frame: u64, when: &str, enabled: bool, values: Value) -> Value {
        self.pending
            .lock()
            .unwrap()
            .entry(frame)
            .or_default()
            .push(Policy {
                when: when.into(),
                enabled,
                values,
            });
        json!({"accepted":false,"status":"registered","when":when,"enabled":enabled,"frame_id":frame})
    }

    fn take(&self, frame: Option<u64>, ok: bool, error: Option<&str>) -> Vec<Value> {
        let policies = {
            let mut pending = self.pending.lock().unwrap();
            match frame {
                Some(frame) => pending.remove(&frame).unwrap_or_default(),
                // A guest trap has no call_end. Prefer the deepest active scope
                // so an enclosing script does not send the same failure twice.
                None => std::mem::take(&mut *pending)
                    .into_iter()
                    .rev()
                    .flat_map(|(_, p)| p)
                    .collect(),
            }
        };
        let mut values = Vec::new();
        for policy in policies {
            if !policy.enabled || (policy.when == "success") != ok {
                continue;
            }
            if !ok
                && self
                    .failure_attempted
                    .swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                continue;
            }
            let mut value = policy.values;
            if let Some(error) = error.filter(|e| !e.is_empty()) {
                let content = value["content"].as_str().unwrap_or("");
                let mut end = error.len().min(768);
                while !error.is_char_boundary(end) {
                    end -= 1;
                }
                value["content"] = json!(format!("{content}\n错误：{}", &error[..end]));
            }
            values.push(value);
        }
        values
    }

    pub(crate) async fn settle(
        &self,
        frame: Option<u64>,
        ok: bool,
        error: Option<&str>,
        sender: Option<&Sender>,
    ) -> Vec<Value> {
        let mut results = Vec::new();
        for values in self.take(frame, ok, error) {
            let result = match sender {
                Some(sender) => sender(values).await,
                None => skipped("通知发送能力不可用"),
            };
            results.push(result);
        }
        results
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn trapped_guest_reports_most_specific_active_failure_once() {
        let policies = ResultNotifications::default();
        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = sent.clone();
        let sender: Sender = Arc::new(move |value| {
            captured.lock().unwrap().push(value);
            Box::pin(async { json!({"accepted":true}) })
        });
        for (frame, text) in [(0, "parent"), (1, "child"), (2, "deepest")] {
            policies.register(frame, "failure", true, json!({"content":text}));
            policies.register(
                frame,
                "success",
                true,
                json!({"content":"should not send success"}),
            );
        }
        assert_eq!(
            policies
                .settle(None, false, Some("host trap"), Some(&sender))
                .await
                .len(),
            1
        );
        assert!(policies
            .settle(None, false, Some("host trap"), Some(&sender))
            .await
            .is_empty());
        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0]["content"], "deepest\n错误：host trap");
    }

    #[tokio::test]
    async fn independently_completed_calls_send_success_each_time_and_optional_sender_skips() {
        let policies = ResultNotifications::default();
        policies.register(1, "success", true, json!({"content":"one"}));
        policies.register(2, "success", true, json!({"content":"two"}));
        policies.register(0, "success", false, json!({"content":"disabled"}));
        for frame in [1, 2] {
            let result = policies.settle(Some(frame), true, None, None).await;
            assert_eq!(result.len(), 1);
            assert_eq!(result[0]["status"], "skipped");
        }
        assert!(policies.settle(None, true, None, None).await.is_empty());
    }
}
