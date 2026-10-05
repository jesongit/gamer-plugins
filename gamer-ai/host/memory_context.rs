//! Bounded automatic memory context, distinct from explicit detail reads.
use super::State;
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;

const FULL_ENTRIES: usize = 8;
const FULL_BYTES: usize = 24 * 1024;
const DIRECTORY_BYTES: usize = 8 * 1024;

fn preview(text: &str, bytes: usize) -> &str {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn definition_summary(entry: &Value) -> Value {
    let applicability = entry["applicability"].as_str().unwrap_or("");
    let tags: Vec<_> = entry["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .take(8)
        .cloned()
        .collect();
    let source_count = entry["source_count"].as_u64().unwrap_or(0);
    let source_conflict_count = entry["source_conflicts"].as_array().map_or(0, Vec::len);
    json!({
        "id":entry["id"],"title":entry["title"],"version":entry["version"],
        "revision":entry["revision"],"validation":entry["validation"],
        "game_version":entry["game_version"],"kind":entry["kind"],
        "applicability":preview(applicability, 400),
        "applicability_requires_memory_get":applicability.len()>400,
        "tags":tags,"tags_requires_memory_get":entry["tags"].as_array().is_some_and(|all| all.len()>8),
        "effective_validation":entry["effective_validation"],
        "protected_fields":entry["protected_fields"],
        "summary":preview(entry["summary"].as_str().unwrap_or(""), 400),
        "reference":{"id":entry["id"],"revision":entry["revision"],"version":entry["version"]},
        "source_count":source_count,"source_conflict_count":source_conflict_count,
        "source_details_reference":{"id":entry["id"],"revision":entry["revision"]},
        "source_details_requires_memory_get":source_count>0 || source_conflict_count>0,
        "body_requires_memory_get":true
    })
}

impl State {
    pub(super) async fn protected_memory_context(
        &self,
        package: &str,
        kind: Option<&str>,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        self.authorize(Some(crate::extensions::Permission::ResourceRead))?;
        let mut args =
            json!({"status":"active","validation":"any","protected_only":true,"limit":30});
        if let Some(kind) = kind {
            args["kind"] = json!(kind);
        }
        let listed = self
            .memory
            .call_cancellable("memory_list", package, args, None, false, cancel)
            .await?;
        let mut context = Vec::new();
        let mut full_bytes = 0usize;
        let mut directory_bytes = 0usize;
        let mut details_required = false;
        for (position, entry) in listed["items"].as_array().into_iter().flatten().enumerate() {
            let summary = definition_summary(entry);
            let size = summary.to_string().len();
            if directory_bytes + size > DIRECTORY_BYTES {
                details_required = true;
                break;
            }
            directory_bytes += size;
            let mut projected = summary;
            if position < FULL_ENTRIES {
                let current = self
                    .memory
                    .call_cancellable(
                        "memory_get",
                        package,
                        json!({"id":entry["id"],"revision":entry["revision"]}),
                        None,
                        false,
                        cancel,
                    )
                    .await?;
                let mut complete = projected.clone();
                complete.as_object_mut().unwrap().remove("summary");
                complete["body"] = current["memory"]["body"].clone();
                complete["version"] = current["version"].clone();
                complete["revision"] = current["revision"].clone();
                complete["validation"] = current["memory"]["validation"].clone();
                complete["effective_validation"] =
                    current["memory"]["effective_validation"].clone();
                complete["protected_fields"] = current["memory"]["protected_fields"].clone();
                for field in ["applicability", "game_version", "kind", "tags"] {
                    complete[field] = current["memory"][field].clone();
                }
                complete["applicability_requires_memory_get"] = json!(false);
                complete["tags_requires_memory_get"] = json!(false);
                let source_count = current["memory"]["sources"].as_array().map_or(0, Vec::len);
                let source_conflict_count =
                    current["source_conflicts"].as_array().map_or(0, Vec::len);
                complete["source_count"] = json!(source_count);
                complete["source_conflict_count"] = json!(source_conflict_count);
                complete["source_details_reference"] =
                    json!({"id":current["reference"]["id"],"revision":current["revision"]});
                complete["source_details_requires_memory_get"] =
                    json!(source_count > 0 || source_conflict_count > 0);
                complete["reference"] = current["reference"].clone();
                complete["body_requires_memory_get"] = json!(false);
                let size = complete.to_string().len();
                if full_bytes + size <= FULL_BYTES {
                    full_bytes += size;
                    projected = complete;
                } else {
                    details_required = true;
                }
            } else {
                details_required = true;
            }
            context.push(projected);
        }
        if details_required || listed["total"].as_u64().unwrap_or(0) > context.len() as u64 {
            context.push(json!({
                "context_notice":"保护定义正文/目录有自动加载上限。仅有摘要或未列出的定义不能视为没有约束；若当前操作可能受其影响，应先memory_get读取相关原文，目录不足时按需memory_list。",
                "total":listed["total"],"shown":context.len(),"details_required":true
            }));
        }
        Ok(json!(context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn protected_context_bounds_details_without_changing_authoritative_memory() {
        let (_root, ai, _extensions) = super::super::tests::fixture().await;
        ai.state
            .background_cancel
            .lock()
            .store(true, std::sync::atomic::Ordering::Release);
        let large_body = format!(
            "超大保护定义起点{}超大保护定义结尾",
            "完整原文条件与步骤。".repeat(2000)
        );
        for index in 0..10 {
            ai.state.memory.call("memory_create", "default", json!({"id":format!("rule-{index}"),"operation_id":format!("create-rule-{index}"),"title":format!("保护定义{index}"),"body":format!("定义正文{index}"),"kind":"definition"}), None, true).await.unwrap();
        }
        let large_applicability = "仅竞技场。".repeat(1000);
        ai.state.memory.call("memory_create", "default", json!({"id":"large-rule","operation_id":"create-large-rule","title":"超大保护定义","body":large_body,"kind":"definition","applicability":large_applicability,"game_version":"2.0","sources":[{"log":"完整来源日志不应自动加载".repeat(1000)}],"reason":"修改理由不应自动加载"}), None, true).await.unwrap();
        ai.state.memory.call("memory_create", "default", json!({"id":"short-rule","operation_id":"create-short-rule","title":"当前按钮定义","body":"当前保护定义必须可见：优先用药","kind":"definition","applicability":"仅竞技场","game_version":"1.2","tags":["竞技场","药品"],"sources":[{"log":"完整来源日志不应自动加载".repeat(1000)}],"reason":"修改理由不应自动加载"}), None, true).await.unwrap();
        ai.state.memory.call("memory_create", "default", json!({"id":"ordinary","operation_id":"create-ordinary","title":"普通攻略","body":"未保护的全库正文不应加载","kind":"definition"}), None, false).await.unwrap();
        let context = ai
            .state
            .protected_memory_context("default", Some("definition"), &AtomicBool::new(false))
            .await
            .unwrap();
        let text = context.to_string();
        assert!(text.contains("当前保护定义必须可见"));
        assert!(!text.contains("完整来源日志不应自动加载"));
        assert!(!text.contains("修改理由不应自动加载"));
        assert!(!text.contains("未保护的全库正文不应加载"));
        assert!(!text.contains("超大保护定义结尾"));
        let entries = context.as_array().unwrap();
        let large = entries
            .iter()
            .find(|entry| entry["id"] == "large-rule")
            .unwrap();
        assert_eq!(large["reference"]["id"], "large-rule");
        assert_eq!(large["reference"]["revision"], 1);
        assert_eq!(large["body_requires_memory_get"], true);
        assert_eq!(large["game_version"], "2.0");
        assert_eq!(large["applicability_requires_memory_get"], true);
        assert!(large["applicability"].as_str().unwrap().len() <= 400);
        assert!(large["summary"]
            .as_str()
            .unwrap()
            .contains("超大保护定义起点"));
        let short = entries
            .iter()
            .find(|entry| entry["id"] == "short-rule")
            .unwrap();
        assert_eq!(short["body_requires_memory_get"], false);
        assert_eq!(short["applicability"], "仅竞技场");
        assert_eq!(short["applicability_requires_memory_get"], false);
        assert_eq!(short["game_version"], "1.2");
        assert_eq!(short["kind"], "definition");
        assert_eq!(short["tags"], json!(["竞技场", "药品"]));
        for field in ["body", "applicability", "game_version", "tags"] {
            assert!(short["protected_fields"]
                .as_array()
                .unwrap()
                .contains(&json!(field)));
        }
        assert_eq!(short["source_count"], 1);
        assert!(short["source_conflict_count"].is_u64());
        assert_eq!(short["source_details_reference"]["id"], "short-rule");
        assert_eq!(short["source_details_requires_memory_get"], true);
        let full: Vec<_> = entries
            .iter()
            .filter(|entry| entry.get("body").is_some())
            .collect();
        assert!(full.len() <= FULL_ENTRIES);
        assert!(
            full.iter()
                .map(|entry| entry.to_string().len())
                .sum::<usize>()
                <= FULL_BYTES
        );
        assert!(entries
            .iter()
            .any(|entry| entry["details_required"] == true));
        let authoritative = ai
            .state
            .memory
            .call(
                "memory_get",
                "default",
                json!({"id":"large-rule","revision":1}),
                None,
                false,
            )
            .await
            .unwrap();
        assert_eq!(authoritative["memory"]["body"], large_body);
        assert_eq!(
            authoritative["memory"]["applicability"],
            large_applicability
        );
        assert!(authoritative["memory"]["sources"]
            .to_string()
            .contains("完整来源日志不应自动加载"));
        ai.state.stop_all().await;
    }
}
