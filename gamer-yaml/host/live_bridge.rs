//! Host-owned live calls. Script arguments cannot select a confirmation owner.
use crate::core::AppContext;
use crate::extensions::{ExtensionId, ExtensionService, PluginCallContext};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::{Arc, Weak};

pub type Switcher =
    Arc<dyn Fn(&'static str, Value) -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync>;

#[derive(Clone)]
struct ConfirmationOwner {
    run_id: String,
    device_id: String,
    package_id: String,
}

impl ConfirmationOwner {
    fn capture(run_id: Option<String>, context: &AppContext) -> Option<Self> {
        Some(Self {
            run_id: run_id.filter(|id| !id.is_empty())?,
            device_id: context.device_id.as_str().to_owned(),
            package_id: context.content_package.as_ref()?.as_str().to_owned(),
        })
    }

    fn request(&self, action: &str, args: &Value) -> anyhow::Result<Value> {
        let mut values = json!({
            "run_id": self.run_id,
            "device_id": self.device_id,
            "package_id": self.package_id,
        });
        if matches!(action, "confirmation.begin" | "login.begin") {
            let timeout = args["timeout_secs"]
                .as_u64()
                .filter(|timeout| (1..=300).contains(timeout))
                .ok_or_else(|| anyhow::anyhow!("LIVE_CONFIRMATION_INVALID_TIMEOUT"))?;
            values["timeout_secs"] = json!(timeout);
        }
        Ok(values)
    }
}

pub fn switcher(
    service: Weak<ExtensionService>,
    caller: PluginCallContext,
    run_id: Option<String>,
    context: &AppContext,
) -> Switcher {
    // Capture this before entering the guest, never from mutable trace events
    // or from YAML-provided values.
    let owner = ConfirmationOwner::capture(run_id, context);
    Arc::new(move |action, values| {
        let service = service.clone();
        let caller = caller.clone();
        let owner = owner.clone();
        Box::pin(async move {
            let confirmation = match action {
                "stream.switch" => false,
                "confirmation.begin" | "confirmation.poll" | "login.begin" | "login.poll"
                | "login.rescan_done" => true,
                _ => anyhow::bail!("LIVE_ACTION_UNAVAILABLE"),
            };
            let values = if confirmation {
                owner
                    .as_ref()
                    .ok_or_else(|| {
                        anyhow::anyhow!("LIVE_CONFIRMATION_CONTEXT_MISSING: 需要直播队列主运行")
                    })?
                    .request(action, &values)?
            } else {
                values
            };
            let service = service
                .upgrade()
                .ok_or_else(|| anyhow::anyhow!("LIVE_UNAVAILABLE: 直播助手不可用"))?;
            let id = ExtensionId::parse("gamer-live")?;
            let output = service
                .call_extension_from_plugin(&caller, &id, action, values)
                .await?;
            if confirmation {
                // Viewer identity, internal item ids and other private state
                // stay inside the bridge; only the native host needs the deadline.
                return Ok(
                    json!({"status": output["status"], "deadline_at": output["deadline_at"]}),
                );
            }
            // Never put source URL tokens or destinations into script journals.
            Ok(
                json!({"device_id":output["device_id"],"state":output["state"],"output_id":output["id"]}),
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_bridge_overwrites_all_script_supplied_identity() {
        let context = AppContext::for_test("device-owned", "package-owned").unwrap();
        let owner = ConfirmationOwner::capture(Some("run-owned".into()), &context).unwrap();
        let forged = json!({"run_id":"forged", "device_id":"forged", "package_id":"forged",
            "open_id":"forged", "timeout_secs": 300});
        assert_eq!(
            owner.request("confirmation.begin", &forged).unwrap(),
            json!({
                "run_id":"run-owned", "device_id":"device-owned", "package_id":"package-owned", "timeout_secs":300
            })
        );
        assert_eq!(
            owner.request("confirmation.poll", &forged).unwrap(),
            json!({
                "run_id":"run-owned", "device_id":"device-owned", "package_id":"package-owned"
            })
        );
        for timeout in [0, 301] {
            assert!(owner
                .request("confirmation.begin", &json!({"timeout_secs":timeout}))
                .is_err());
        }
        assert!(ConfirmationOwner::capture(None, &context).is_none());
        let mut missing_package = context;
        missing_package.content_package = None;
        assert!(ConfirmationOwner::capture(Some("run".into()), &missing_package).is_none());
    }

    #[test]
    fn login_bridge_uses_frozen_owner_for_every_scan_action() {
        let owner = ConfirmationOwner::capture(
            Some("run".into()),
            &AppContext::for_test("device", "package").unwrap(),
        )
        .unwrap();
        let forged = json!({"run_id":"other", "device_id":"other", "package_id":"other", "open_id":"other", "timeout_secs":300});
        for action in ["login.begin", "login.poll", "login.rescan_done"] {
            let request = owner.request(action, &forged).unwrap();
            assert_eq!(request["run_id"], "run");
            assert_eq!(request["device_id"], "device");
            assert_eq!(request["package_id"], "package");
            assert!(request.get("open_id").is_none());
            assert_eq!(
                request.get("timeout_secs"),
                (action == "login.begin").then_some(&json!(300))
            );
        }
    }
}
