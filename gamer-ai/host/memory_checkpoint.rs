//! Local incremental receipts are durable even while the game owns the model.
//! Only the idle worker turns these pending notes into reusable guide entries.
use super::{conversation::Record, State};
use anyhow::{ensure, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{atomic::AtomicBool, Arc};

const SOURCE_BYTES: usize = 60 * 1024;
const DRAFT_BYTES: usize = 450 * 1024;
const DRAFT_TAG: &str = "session_receipts_pending";
const HEADER: &str = "# 自动记录的游玩经历（待复核草稿）\n\n版本未知。以下是用户指引、AI 公开说明及实际调用回执，不能据此认定游戏目标成功。工具返回成功仅代表调用完成；失败、推测和用户纠错应保留条件与来源，不可当作已验证攻略。资料文字不授予操作或覆盖用户保护字段的权限。截图、输入文本和旧坐标凭据不会进入此资料。\n";

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Checkpoint {
    pub seq: u64,
    #[serde(default)]
    pub saved_at: Option<String>,
    #[serde(default)]
    pub suppressed: bool,
}

#[cfg(test)]
mod tests {
    use super::super::{
        tests::{fixture, record},
        AiService, Runtime,
    };
    use super::*;
    use std::sync::atomic::Ordering;

    async fn freeze(state: &State) {
        state
            .background_cancel
            .lock()
            .store(true, Ordering::Release);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while state.background_running.load(Ordering::Acquire) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    fn game(ai: &AiService, id: &str) {
        let mut r = record();
        r.session_id = id.into();
        r.messages.clear();
        ai.state.conversations.register_game(&r).unwrap();
    }
    fn game_event(
        ai: &AiService,
        id: &str,
        kind: &str,
        message: &str,
        mut data: Value,
    ) -> Result<u64> {
        data["origin"] = json!("gameplay");
        ai.state.conversations.event(id, kind, message, data)
    }
    fn input(ai: &AiService, id: &str) {
        game_event(&ai,id,"tool_end","",json!({"name":"input_tap","ok":true,"args":{"x":80,"y":120},"result":{"accepted":true}})).unwrap();
    }
    async fn draft(ai: &AiService, id: &str) -> Value {
        ai.state
            .memory
            .call(
                "memory_get",
                "default",
                json!({"id":draft_id(id)}),
                None,
                false,
            )
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn durable_receipts_preserve_early_steps_and_implicit_corrections_after_256_deltas() {
        let (_root, ai, _extensions) = fixture().await;
        freeze(&ai.state).await;
        game(&ai, "long-game");
        game_event(
            &ai,
            "long-game",
            "assistant_final",
            "先从指南领取每日奖励",
            json!({"text":"先从指南领取每日奖励"}),
        )
        .unwrap();
        input(&ai, "long-game");
        for _ in 0..600 {
            game_event(
                &ai,
                "long-game",
                "assistant_delta",
                "",
                json!({"channel":"text","delta":"流式输出"}),
            )
            .unwrap();
        }
        game_event(&ai,
                "long-game",
                "user",
                "双箭头是二倍速，后面那个才是自动战斗\nAPI_KEY=private-key\n密码是sensitive-password-123",
                json!({"message_id":"correction"}),
            )
            .unwrap();
        game_event(&ai,"long-game","tool_end","",json!({"name":"input_text","ok":true,"args":{"text":"private typed text"},"image_data_url":"data:image/png;base64,AAAA"})).unwrap();
        game_event(
            &ai,
            "long-game",
            "tool_end",
            "",
            json!({"name":"memory_get","ok":true,"result":{"body":"recursive old memory"}}),
        )
        .unwrap();
        let r = ai.state.conversations.record("long-game").unwrap();
        ai.state.checkpoint_game(&r, false).await.unwrap();
        let saved = draft(&ai, "long-game").await;
        let body = saved["memory"]["body"].as_str().unwrap();
        assert!(body.contains("指南领取每日奖励") && body.contains("双箭头是二倍速"));
        assert_eq!(saved["memory"]["validation"], "pending");
        for hidden in [
            "private-key",
            "sensitive-password-123",
            "private typed text",
            "data:image",
            "recursive old memory",
            "流式输出",
        ] {
            assert!(!body.contains(hidden), "{hidden}");
        }
        let jobs = ai.state.memory.pending_imports("default").unwrap();
        assert_eq!(jobs.len(), 1);
        let source = jobs[0]
            .chunks
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(source.contains("指南领取每日奖励") && source.contains("双箭头是二倍速"));
        ai.state
            .checkpoint_game(&ai.state.conversations.record("long-game").unwrap(), true)
            .await
            .unwrap();
        assert_eq!(ai.state.memory.pending_imports("default").unwrap().len(), 1);
        ai.state.stop_all().await;
    }
    #[tokio::test]
    async fn periodic_input_checkpoint_is_incremental_and_keeps_one_pending_draft() {
        let (_root, ai, _extensions) = fixture().await;
        freeze(&ai.state).await;
        game(&ai, "incremental");
        game_event(
            &ai,
            "incremental",
            "assistant_final",
            "最初先整理背包",
            json!({"text":"最初先整理背包"}),
        )
        .unwrap();
        input(&ai, "incremental");
        ai.state
            .checkpoint_game(&ai.state.conversations.record("incremental").unwrap(), true)
            .await
            .unwrap();
        let first = ai
            .state
            .conversations
            .memory_checkpoint("incremental")
            .unwrap()
            .seq;
        for _ in 0..9 {
            input(&ai, "incremental");
        }
        ai.state
            .checkpoint_game(
                &ai.state.conversations.record("incremental").unwrap(),
                false,
            )
            .await
            .unwrap();
        assert_eq!(
            ai.state
                .conversations
                .memory_checkpoint("incremental")
                .unwrap()
                .seq,
            first
        );
        game_event(
            &ai,
            "incremental",
            "assistant_final",
            "本阶段清理了多余材料",
            json!({"text":"本阶段清理了多余材料"}),
        )
        .unwrap();
        input(&ai, "incremental");
        ai.state
            .checkpoint_game(
                &ai.state.conversations.record("incremental").unwrap(),
                false,
            )
            .await
            .unwrap();
        let saved = draft(&ai, "incremental").await;
        assert!(saved["memory"]["revision"].as_u64().unwrap() >= 2);
        let body = saved["memory"]["body"].as_str().unwrap();
        assert!(body.contains("最初先整理背包") && body.contains("本阶段清理了多余材料"));
        let jobs = ai.state.memory.pending_imports("default").unwrap();
        assert_eq!(jobs.len(), 2);
        let newest = jobs
            .last()
            .unwrap()
            .chunks
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(newest.contains("本阶段清理了多余材料") && !newest.contains("最初先整理背包"));
        assert_eq!(
            ai.state
                .memory
                .call("memory_list", "default", json!({}), None, false)
                .await
                .unwrap()["total"],
            1
        );
        ai.state.stop_all().await;
    }
    #[tokio::test]
    async fn paused_state_flushes_receipts_without_waiting_for_a_model_or_finishing_game() {
        let (_root, ai, _extensions) = fixture().await;
        freeze(&ai.state).await;
        game(&ai, "paused-game");
        input(&ai, "paused-game");
        game_event(
            &ai,
            "paused-game",
            "state",
            "用户暂停",
            json!({"state":"paused"}),
        )
        .unwrap();
        ai.state
            .checkpoint_game(
                &ai.state.conversations.record("paused-game").unwrap(),
                false,
            )
            .await
            .unwrap();
        assert_eq!(
            ai.state.conversations.record("paused-game").unwrap().state,
            "paused"
        );
        assert_eq!(
            draft(&ai, "paused-game").await["memory"]["validation"],
            "pending"
        );
        assert_eq!(ai.state.memory.pending_imports("default").unwrap().len(), 1);
        assert!(ai.state.settings.connection().is_err());
        ai.state.stop_all().await;
    }
    #[tokio::test]
    async fn restart_recovers_unarchived_game_history_locally_and_preserves_pending_jobs() {
        let (root, ai, _extensions) = fixture().await;
        freeze(&ai.state).await;
        game(&ai, "recovered");
        input(&ai, "recovered");
        game_event(
            &ai,
            "recovered",
            "user",
            "退出副本要点后面的按钮",
            json!({"message_id":"new-info"}),
        )
        .unwrap();
        let restored = AiService::new(
            Runtime {
                devices: ai.state.runtime.devices.clone(),
                packages: ai.state.runtime.packages.clone(),
                runs: ai.state.runtime.runs.clone(),
                scheduler: ai.state.runtime.scheduler.clone(),
                capabilities: ai.state.runtime.capabilities.clone(),
            },
            root.path(),
        )
        .unwrap();
        // Disabled network/extension state does not discard safe local history.
        assert!(!restored.state.enabled.load(Ordering::Acquire));
        restored.state.checkpoint_games().await;
        assert!(draft(&restored, "recovered").await["memory"]["body"]
            .as_str()
            .unwrap()
            .contains("退出副本要点后面的按钮"));
        assert_eq!(
            restored
                .state
                .memory
                .pending_imports("default")
                .unwrap()
                .len(),
            1
        );
        let cursor = restored
            .state
            .conversations
            .memory_checkpoint("recovered")
            .unwrap()
            .seq;
        let again = super::super::conversation::Conversations::new(root.path()).unwrap();
        assert_eq!(again.memory_checkpoint("recovered").unwrap().seq, cursor);
        restored.state.checkpoint_games().await;
        assert_eq!(
            restored
                .state
                .memory
                .pending_imports("default")
                .unwrap()
                .len(),
            1
        );
        ai.state.stop_all().await;
    }
    #[tokio::test]
    async fn deletion_cancels_linked_jobs_and_never_recreates_the_checkpoint_draft() {
        for permanent in [false, true] {
            let (_root, ai, _extensions) = fixture().await;
            freeze(&ai.state).await;
            game(&ai, "deleted-draft");
            input(&ai, "deleted-draft");
            ai.state
                .checkpoint_game(
                    &ai.state.conversations.record("deleted-draft").unwrap(),
                    true,
                )
                .await
                .unwrap();
            let saved = draft(&ai, "deleted-draft").await;
            ai.state.memory.call("memory_delete","default",json!({"id":saved["memory"]["id"],"expected_version":saved["version"],"operation_id":"delete-draft","reason":"用户删除本次经历","permanent":permanent}),None,true).await.unwrap();
            let job = ai
                .state
                .memory
                .pending_imports("default")
                .unwrap()
                .remove(0);
            assert!(!ai
                .state
                .memory_job_allowed("default", &job.id)
                .await
                .unwrap());
            game_event(
                &ai,
                "deleted-draft",
                "user",
                "不要恢复已删除的经历",
                json!({"message_id":"later"}),
            )
            .unwrap();
            ai.state
                .checkpoint_game(
                    &ai.state.conversations.record("deleted-draft").unwrap(),
                    true,
                )
                .await
                .unwrap();
            assert!(
                ai.state
                    .conversations
                    .memory_checkpoint("deleted-draft")
                    .unwrap()
                    .suppressed
            );
            assert!(ai
                .state
                .memory
                .pending_imports("default")
                .unwrap()
                .is_empty());
            if permanent {
                assert!(ai
                    .state
                    .memory
                    .call(
                        "memory_get",
                        "default",
                        json!({"id":draft_id("deleted-draft")}),
                        None,
                        false
                    )
                    .await
                    .is_err());
            } else {
                assert_eq!(
                    draft(&ai, "deleted-draft").await["memory"]["status"],
                    "deleted"
                );
            }
            ai.state.stop_all().await;
        }
    }
    #[tokio::test]
    async fn background_job_progress_and_failure_are_durable_conversation_events() {
        let (_root, ai, _extensions) = fixture().await;
        freeze(&ai.state).await;
        game(&ai, "job-events");
        input(&ai, "job-events");
        ai.state
            .checkpoint_game(&ai.state.conversations.record("job-events").unwrap(), true)
            .await
            .unwrap();
        let job = ai
            .state
            .memory
            .pending_imports("default")
            .unwrap()
            .remove(0);
        let chunk = ai
            .state
            .memory
            .claim_import_chunk("default", &job.id, "fixture-claim")
            .unwrap()
            .unwrap();
        ai.state.sync_memory_job_events().unwrap();
        ai.state
            .memory
            .fail_import_chunk(
                "default",
                &job.id,
                chunk["chunk"]["id"].as_str().unwrap(),
                "fixture-claim",
                "fixture model unavailable",
            )
            .unwrap();
        ai.state.sync_memory_job_events().unwrap();
        let view = ai
            .state
            .conversations
            .get("job-events", &json!({"after_seq":0,"limit":200}))
            .unwrap();
        let statuses = view["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["kind"] == "memory_job")
            .map(|event| event["data"]["state"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(statuses, vec!["pending", "running", "failed"]);
        let seq = ai
            .state
            .conversations
            .record("job-events")
            .unwrap()
            .latest_seq;
        ai.state.sync_memory_job_events().unwrap();
        assert_eq!(
            ai.state
                .conversations
                .record("job-events")
                .unwrap()
                .latest_seq,
            seq
        );
        ai.state.stop_all().await;
    }
    #[tokio::test]
    async fn recovery_query_does_not_starve_old_history_behind_100_archived_games() {
        let (_root, ai, _extensions) = fixture().await;
        freeze(&ai.state).await;
        game(&ai, "old-unarchived");
        input(&ai, "old-unarchived");
        for n in 0..105 {
            let id = format!("archived-{n}");
            game(&ai, &id);
            input(&ai, &id);
            ai.state
                .conversations
                .save_memory_checkpoint(
                    &id,
                    &Checkpoint {
                        seq: 1,
                        saved_at: None,
                        suppressed: false,
                    },
                )
                .unwrap();
        }
        let records = ai.state.conversations.game_records().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].conversation_id, "old-unarchived");
        ai.state.stop_all().await;
    }
    #[tokio::test]
    async fn game_journal_preserves_authoritative_usage_and_budget_without_changing_other_chats() {
        let (_root, ai, _extensions) = fixture().await;
        freeze(&ai.state).await;
        let mut r = record();
        r.session_id = "usage-game".into();
        ai.state.conversations.register_game(&r).unwrap();
        r.limits.max_turns = 0;
        r.limits.max_tokens = 0;
        r.usage.turns = 17;
        r.usage.actions = 42;
        r.usage.known_tokens = 100001;
        r.usage.total_tokens = Some(100001);
        let mut session = Arc::try_unwrap(super::super::tests::session_record(r, None))
            .unwrap_or_else(|_| panic!("fixture owns Session"));
        session.journal = Some(ai.state.conversations.clone());
        session.event("progress", "done", json!({}));
        let actual = ai.state.conversations.record("usage-game").unwrap();
        assert_eq!(actual.game_usage.as_ref().unwrap().actions, 42);
        assert_eq!(actual.game_usage.as_ref().unwrap().turns, 17);
        assert_eq!(
            actual.game_usage.as_ref().unwrap().total_tokens,
            Some(100001)
        );
        assert_eq!(actual.game_limits.as_ref().unwrap().max_tokens, 0);
        assert_eq!(actual.usage.actions, 0);
        assert_eq!(
            actual.limits.max_turns,
            super::super::Limits::default().max_turns
        );
        session.event(
            "tool",
            "",
            json!({"phase":"start","tool":"input_tap","call_id":"old-tool","generation":1}),
        );
        session.record.lock().generation = 2;
        session.event("tool","",json!({"phase":"result","tool":"input_tap","call_id":"old-tool","generation":1,"ok":true}));
        let journal = ai
            .state
            .conversations
            .get("usage-game", &json!({"after_seq":0,"limit":20}))
            .unwrap();
        let pair = journal["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["data"]["call_id"] == "old-tool")
            .collect::<Vec<_>>();
        assert_eq!(pair.len(), 2);
        assert!(pair.iter().all(|event| event["data"]["turn_id"]
            .as_str()
            .unwrap()
            .starts_with("game:usage-game:1:")));
        ai.state
            .checkpoint_game(&ai.state.conversations.record("usage-game").unwrap(), true)
            .await
            .unwrap();
        let job = ai
            .state
            .memory
            .pending_imports("default")
            .unwrap()
            .remove(0);
        assert_eq!(job.limits.max_turns, 0);
        assert_eq!(job.limits.max_tokens, 0);
        let ordinary = ai
            .state
            .conversations
            .create("default", "ordinary")
            .unwrap();
        let id = ordinary["conversation"]["conversation_id"]
            .as_str()
            .unwrap();
        game_event(
            &ai,
            id,
            "progress",
            "linked diagnostic",
            json!({"game_usage":actual.game_usage,"game_limits":actual.game_limits}),
        )
        .unwrap();
        assert_eq!(ai.state.conversations.record(id).unwrap().usage.actions, 0);
        ai.state.stop_all().await;
    }
    #[tokio::test]
    async fn natural_user_definition_is_protected_and_its_deleted_source_is_not_recreated() {
        let (_root, ai, _extensions) = fixture().await;
        freeze(&ai.state).await;
        game(&ai, "human-correction");
        let literal = "双箭头是二倍速，后面那个才是自动战斗";
        game_event(
            &ai,
            "human-correction",
            "user",
            literal,
            json!({"message_id":"human-definition"}),
        )
        .unwrap();
        let saved = ai
            .state
            .protect_user_definition(
                "default",
                "human-correction",
                "human-definition",
                literal,
                &AtomicBool::new(false),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved["memory"]["kind"], "definition");
        assert_eq!(saved["memory"]["body"], literal);
        assert!(!saved["memory"]["protected_fields"]
            .as_array()
            .unwrap()
            .is_empty());
        input(&ai, "human-correction");
        ai.state
            .checkpoint_game(
                &ai.state.conversations.record("human-correction").unwrap(),
                true,
            )
            .await
            .unwrap();
        let original = ai
            .state
            .memory
            .call(
                "memory_get",
                "default",
                json!({"id":saved["id"]}),
                None,
                false,
            )
            .await
            .unwrap();
        assert!(original["memory"]["sources"].as_array().unwrap().len() > 1);
        ai.state.memory.call("memory_delete","default",json!({"id":saved["id"],"expected_version":original["version"],"operation_id":"delete-user-definition","reason":"用户删除这条定义"}),None,true).await.unwrap();
        let job = ai
            .state
            .memory
            .pending_imports("default")
            .unwrap()
            .remove(0);
        assert!(!ai
            .state
            .memory_job_allowed("default", &job.id)
            .await
            .unwrap());
        assert!(ai
            .state
            .memory
            .pending_imports("default")
            .unwrap()
            .is_empty());
        ai.state.stop_all().await;
    }
    #[test]
    fn active_checkpoint_interval_is_sixty_seconds_without_forcing_a_pause() {
        let now = Utc::now();
        let record:Record=serde_json::from_value(json!({"conversation_id":"time-game","content_package":"default","title":"game","state":"running","created_at":(now-chrono::Duration::seconds(90)).to_rfc3339(),"updated_at":now.to_rfc3339(),"latest_seq":1,"game_session_id":"time-game"})).unwrap();
        let events = vec![json!({"kind":"tool_end","data":{"name":"input_tap"}})];
        assert!(due(&record, &Checkpoint::default(), &events, false));
        assert!(!due(
            &record,
            &Checkpoint {
                seq: 0,
                saved_at: Some(now.to_rfc3339()),
                suppressed: false
            },
            &events,
            false
        ));
    }
    #[tokio::test]
    async fn ordinary_chat_after_game_only_advances_the_cursor_without_archiving_chat() {
        let (_root, ai, _extensions) = fixture().await;
        freeze(&ai.state).await;
        game(&ai, "finished-chat");
        input(&ai, "finished-chat");
        game_event(
            &ai,
            "finished-chat",
            "state",
            "用户结束游玩",
            json!({"state":"finished"}),
        )
        .unwrap();
        ai.state
            .checkpoint_game(
                &ai.state.conversations.record("finished-chat").unwrap(),
                true,
            )
            .await
            .unwrap();
        let before = draft(&ai, "finished-chat").await;
        ai.state
            .conversations
            .event(
                "finished-chat",
                "user",
                "普通聊天的私有话题不属于游玩经历",
                json!({"message_id":"ordinary-user","turn_id":"chat:ordinary"}),
            )
            .unwrap();
        ai.state
            .conversations
            .event(
                "finished-chat",
                "assistant_final",
                "普通回复",
                json!({"text":"普通回复","turn_id":"chat:ordinary"}),
            )
            .unwrap();
        ai.state
            .checkpoint_game(
                &ai.state.conversations.record("finished-chat").unwrap(),
                false,
            )
            .await
            .unwrap();
        assert_eq!(
            draft(&ai, "finished-chat").await["memory"]["body"],
            before["memory"]["body"]
        );
        assert_eq!(
            ai.state
                .conversations
                .memory_checkpoint("finished-chat")
                .unwrap()
                .seq,
            ai.state
                .conversations
                .record("finished-chat")
                .unwrap()
                .latest_seq
        );
        assert_eq!(ai.state.memory.pending_imports("default").unwrap().len(), 1);
        ai.state.stop_all().await;
    }
    #[tokio::test]
    async fn claimed_import_cannot_write_after_its_draft_is_deleted_or_human_protected() {
        for edit in [false, true] {
            let (_root, ai, _extensions) = fixture().await;
            freeze(&ai.state).await;
            game(&ai, "late-commit");
            input(&ai, "late-commit");
            ai.state
                .checkpoint_game(&ai.state.conversations.record("late-commit").unwrap(), true)
                .await
                .unwrap();
            let job = ai
                .state
                .memory
                .pending_imports("default")
                .unwrap()
                .remove(0);
            let chunk = ai
                .state
                .memory
                .claim_import_chunk("default", &job.id, "already-claimed")
                .unwrap()
                .unwrap();
            let origins = ai.state.memory_import_origins("default", &job.id).unwrap();
            let target=ai.state.memory.call("memory_create","default",json!({"title":"已有可编辑攻略","body":"旧步骤","sources":[chunk["source_reference"].clone()],"operation_id":"existing-guide"}),None,false).await.unwrap();
            let original = draft(&ai, "late-commit").await;
            if edit {
                ai.state.memory.call("memory_update","default",json!({"id":draft_id("late-commit"),"expected_version":original["version"],"patch":{"body":"用户修订后的原文"},"operation_id":"human-edit-draft","reason":"用户修改草稿"}),None,true).await.unwrap();
            } else {
                ai.state.memory.call("memory_delete","default",json!({"id":draft_id("late-commit"),"expected_version":original["version"],"operation_id":"human-delete-draft","reason":"用户删除草稿"}),None,true).await.unwrap();
            }
            let result=ai.state.memory.call_import_for_origins_cancellable("memory_update","default",json!({"id":target["id"],"expected_version":target["version"],"patch":{"body":"晚到的覆盖步骤"},"operation_id":"late-update","reason":"自动整理"}),&job.id,&origins,&AtomicBool::new(false)).await;
            assert!(result.unwrap_err().to_string().contains("memory.import_"));
            let renamed=ai.state.memory.call_import_for_origins_cancellable("memory_create","default",json!({"title":"换名复建","body":"晚到的新攻略","operation_id":"late-create","sources":[chunk["source_reference"].clone()]}),&job.id,&origins,&AtomicBool::new(false)).await;
            assert!(renamed.is_err());
            let unchanged = ai
                .state
                .memory
                .call(
                    "memory_get",
                    "default",
                    json!({"id":target["id"]}),
                    None,
                    false,
                )
                .await
                .unwrap();
            assert_eq!(unchanged["memory"]["body"], "旧步骤");
            game_event(
                &ai,
                "late-commit",
                "user",
                "后续新增用户引导",
                json!({"message_id":"later-message"}),
            )
            .unwrap();
            ai.state
                .checkpoint_game(&ai.state.conversations.record("late-commit").unwrap(), true)
                .await
                .unwrap();
            assert!(
                ai.state
                    .conversations
                    .memory_checkpoint("late-commit")
                    .unwrap()
                    .suppressed
            );
            if edit {
                assert_eq!(
                    draft(&ai, "late-commit").await["memory"]["body"],
                    "用户修订后的原文"
                );
            }
            ai.state.stop_all().await;
        }
    }
    #[tokio::test]
    async fn restart_honors_deletion_of_a_legacy_terminal_source_using_its_exact_job_link() {
        let (root, ai, _extensions) = fixture().await;
        freeze(&ai.state).await;
        game(&ai, "legacy-game");
        input(&ai, "legacy-game");
        let imported=ai.state.memory.call("memory_import","default",json!({"operation_id":"experience:legacy-game","filename":"experience.md","title":"自动记录的游玩经历","text":"旧版自动原稿：点击入口未确认成功"}),None,false).await.unwrap();
        ai.state
            .conversations
            .event(
                "legacy-game",
                "memory_job",
                "旧版终态入队",
                json!({"result":imported}),
            )
            .unwrap();
        let source = ai
            .state
            .memory
            .call(
                "memory_source_get",
                "default",
                json!({"id":imported["source_id"]}),
                None,
                false,
            )
            .await
            .unwrap();
        ai.state.memory.call("memory_source_delete","default",json!({"id":imported["source_id"],"expected_version":source["version"],"operation_id":"delete-legacy-source","reason":"用户删除旧游玩原稿"}),None,true).await.unwrap();
        let restored = AiService::new(
            Runtime {
                devices: ai.state.runtime.devices.clone(),
                packages: ai.state.runtime.packages.clone(),
                runs: ai.state.runtime.runs.clone(),
                scheduler: ai.state.runtime.scheduler.clone(),
                capabilities: ai.state.runtime.capabilities.clone(),
            },
            root.path(),
        )
        .unwrap();
        restored.state.checkpoint_games().await;
        assert!(
            restored
                .state
                .conversations
                .memory_checkpoint("legacy-game")
                .unwrap()
                .suppressed
        );
        assert_eq!(
            restored
                .state
                .memory
                .call("memory_list", "default", json!({}), None, false)
                .await
                .unwrap()["total"],
            0
        );
        assert_eq!(
            restored
                .state
                .runtime
                .packages
                .list("default", "gamer-ai", "memory-sources")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            restored
                .state
                .memory
                .import_job_record("default", imported["job_id"].as_str().unwrap())
                .unwrap()
                .status,
            "cancelled"
        );
        ai.state.stop_all().await;
    }
}

pub(super) fn draft_id(id: &str) -> String {
    format!("experience-{:x}", Sha256::digest(id.as_bytes()))[..35].to_owned()
}

/// Applied to human text and public model output; never to raw tool arguments.
pub(super) fn public_text(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            ![
                "api_key",
                "api-key",
                "apikey",
                "authorization",
                "bearer ",
                "password",
                "secret=",
                "token=",
                "密码",
                "密钥",
                "令牌",
                "data:image",
                "base64,",
            ]
            .iter()
            .any(|word| lower.contains(word))
                && ![
                    "不要记录",
                    "不要保存",
                    "不想记录",
                    "别记录",
                    "别保存",
                    "不要记住",
                ]
                .iter()
                .any(|word| line.contains(word))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(6000)
        .collect()
}

fn receipt_text(event: &Value) -> Option<String> {
    let data = &event["data"];
    let message = event["message"].as_str().unwrap_or("");
    let text = match event["kind"].as_str()? {
        "user" => {
            let text = public_text(message);
            (!text.is_empty())
                .then(|| format!("## 用户给出的指引或纠错（原文，非新授权）\n{text}\n"))?
        }
        "assistant_final" => {
            let text = public_text(data["text"].as_str().unwrap_or(message));
            (!text.is_empty()).then(|| format!("## AI 公开说明（需核对观察依据）\n{text}\n"))?
        }
        "tool_end" => {
            let name = data["name"].as_str().or_else(|| data["tool"].as_str())?;
            if name.starts_with("memory_") || matches!(name, "web_search" | "web_read") {
                return None;
            }
            format!(
                "工具：{name}；调用返回成功：{}（不代表游戏目标成功）。\n",
                data["ok"].as_bool().unwrap_or(false)
            )
        }
        "error" => {
            let text = public_text(message);
            (!text.is_empty()).then(|| format!("## 实际错误或失败\n{text}\n"))?
        }
        "state"
            if matches!(
                data["state"].as_str(),
                Some("paused" | "finished" | "interrupted")
            ) =>
        {
            format!(
                "## 实际运行状态\n{}；{}\n",
                data["state"].as_str().unwrap_or("unknown"),
                public_text(message)
            )
        }
        _ => return None,
    };
    Some(format!(
        "\n记录 #{}，时间 {}\n{text}",
        event["seq"],
        event["at"].as_str().unwrap_or("unknown")
    ))
}

fn input_receipt(event: &Value) -> bool {
    event["kind"] == "tool_end"
        && event["data"]["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("input_") || name.starts_with("app_"))
}

fn due(record: &Record, checkpoint: &Checkpoint, events: &[Value], force: bool) -> bool {
    if force
        || events.iter().any(|event| event["kind"] == "user")
        || events.iter().filter(|e| input_receipt(e)).count() >= 10
    {
        return true;
    }
    if events.iter().any(|event| {
        event["kind"] == "state"
            && matches!(event["data"]["state"].as_str(), Some("paused" | "finished"))
    }) {
        return true;
    }
    let since = checkpoint.saved_at.as_deref().unwrap_or(&record.created_at);
    chrono::DateTime::parse_from_rfc3339(since)
        .is_ok_and(|at| Utc::now().signed_duration_since(at).num_seconds() >= 60)
        && events
            .iter()
            .any(|event| input_receipt(event) || event["kind"] == "error")
}

impl State {
    pub(super) async fn checkpoint_games(self: &Arc<Self>) {
        let records = match self.conversations.game_records() {
            Ok(records) => records,
            Err(error) => {
                tracing::warn!(%error,"读取游玩记忆补偿记录失败");
                return;
            }
        };
        for record in records {
            let present = self.sessions.lock().contains_key(&record.conversation_id);
            // A journal from a previous process never restarts device control.
            let force =
                !present || matches!(record.state.as_str(), "paused" | "finished" | "interrupted");
            if let Err(error) = self.checkpoint_game(&record, force).await {
                tracing::warn!(%error,conversation=%record.conversation_id,"保存游玩记忆检查点失败");
                let checkpoint = self
                    .conversations
                    .memory_checkpoint(&record.conversation_id)
                    .unwrap_or_default();
                let signature = format!(
                    "checkpoint-error:{}:{}",
                    record.conversation_id, checkpoint.seq
                );
                if self.checkpoint_errors.lock().insert(signature) {
                    let _=self.conversations.event(&record.conversation_id,"memory_job","游玩记忆保存失败；原始对话仍保留，下次会重试",json!({"state":"failed","error":public_text(&error.to_string()),"validation":"pending"}));
                }
            }
        }
    }

    pub(super) async fn checkpoint_game(&self, record: &Record, force: bool) -> Result<()> {
        // This lock serializes local archives only, never input admission/pause.
        let _guard = self.memory_checkpoint_gate.lock().await;
        ensure!(
            record.game_session_id.as_deref() == Some(&record.conversation_id),
            "memory.checkpoint_not_game"
        );
        if record.state == "package_deleted" {
            return Ok(());
        }
        let _package = self
            .runtime
            .packages
            .acquire_activity(&record.content_package)?;
        let mut checkpoint = self
            .conversations
            .memory_checkpoint(&record.conversation_id)?;
        if checkpoint.suppressed || record.latest_seq <= checkpoint.seq {
            return Ok(());
        }
        // Older releases had a single terminal source and no checkpoint row.
        // Honor its explicit deletion using the exact durable job link before
        // rebuilding a differently chunked source from the same private log.
        if checkpoint.seq == 0 {
            for (id, package, job, _) in self.conversations.memory_job_links()? {
                if id != record.conversation_id || package != record.content_package {
                    continue;
                }
                let Ok(previous) = self.memory.import_job_record(&package, &job) else {
                    continue;
                };
                let source = self
                    .memory
                    .call_cancellable(
                        "memory_source_get",
                        &package,
                        json!({"id":previous.source_id}),
                        None,
                        false,
                        &AtomicBool::new(false),
                    )
                    .await?;
                if source["current_deleted"] == true {
                    checkpoint.suppressed = true;
                    self.conversations
                        .save_memory_checkpoint(&record.conversation_id, &checkpoint)?;
                    let _=self.memory.call_cancellable("memory_import_cancel",&package,json!({"job_id":job,"operation_id":format!("experience-deleted-source:{job}")}),None,false,&AtomicBool::new(false)).await;
                    self.conversations.event(
                        &record.conversation_id,
                        "memory_job",
                        "旧游玩原稿已被删除，已停止从该会话自动补归档",
                        json!({"state":"cancelled","job_id":job,"validation":"pending"}),
                    )?;
                    return Ok(());
                }
            }
        }
        let events = self.conversations.memory_events(
            &record.conversation_id,
            checkpoint.seq,
            record.latest_seq,
        )?;
        if !events.iter().any(|event| receipt_text(event).is_some()) {
            checkpoint.seq = record.latest_seq;
            self.conversations
                .save_memory_checkpoint(&record.conversation_id, &checkpoint)?;
            return Ok(());
        }
        if !due(record, &checkpoint, &events, force) {
            return Ok(());
        }
        let mut text = format!("{HEADER}\n会话：{}\n", record.conversation_id);
        let mut through = checkpoint.seq;
        let mut relevant = false;
        for event in &events {
            if let Some(receipt) = receipt_text(event) {
                if text.len() + receipt.len() > SOURCE_BYTES && relevant {
                    break;
                }
                text.push_str(&receipt);
                relevant = true;
            }
            through = event["seq"].as_u64().unwrap_or(through);
        }
        if !relevant {
            checkpoint.seq = record.latest_seq;
            self.conversations
                .save_memory_checkpoint(&record.conversation_id, &checkpoint)?;
            return Ok(());
        }
        let cancel = AtomicBool::new(false);
        let memory_id = draft_id(&record.conversation_id);
        let existing = self
            .memory
            .call_cancellable(
                "memory_get",
                &record.content_package,
                json!({"id":memory_id}),
                None,
                false,
                &cancel,
            )
            .await;
        let existing = match existing {
            Ok(value) => Some(value),
            Err(error) if error.to_string().contains("memory.not_found") => None,
            Err(error) if error.to_string().contains("memory.permanently_deleted") => {
                checkpoint.suppressed = true;
                self.conversations
                    .save_memory_checkpoint(&record.conversation_id, &checkpoint)?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if existing.as_ref().is_some_and(|value| {
            value["memory"]["status"] != "active"
                || value["memory"]["validation"] != "pending"
                || !value["memory"]["tags"]
                    .as_array()
                    .is_some_and(|tags| tags.iter().any(|tag| tag == DRAFT_TAG))
                || value["memory"]["protected_fields"]
                    .as_array()
                    .is_some_and(|fields| {
                        fields
                            .iter()
                            .any(|field| matches!(field.as_str(), Some("body" | "sources")))
                    })
        }) {
            checkpoint.suppressed = true;
            self.conversations
                .save_memory_checkpoint(&record.conversation_id, &checkpoint)?;
            return Ok(());
        }
        let all = self
            .conversations
            .memory_events(&record.conversation_id, 0, through)?;
        let mut body = format!("{HEADER}\n会话：{}\n", record.conversation_id);
        for event in &all {
            if let Some(receipt) = receipt_text(event) {
                if body.len() + receipt.len() > DRAFT_BYTES {
                    body.push_str(
                        "\n草稿达到展示上限；后续原稿仍按增量保留，完整记录可在本会话查询。\n",
                    );
                    break;
                }
                body.push_str(&receipt);
            }
        }
        let operation = format!("experience-draft:{}:{through}", record.conversation_id);
        let mut draft = if let Some(existing) = existing {
            if existing["memory"]["body"] == body {
                json!({"id":memory_id,"revision":existing["revision"],"validation":"pending"})
            } else {
                self.memory.call_cancellable("memory_update",&record.content_package,json!({"id":memory_id,"expected_version":existing["version"],"patch":{"body":body},"operation_id":operation,"reason":"更新本会话真实公开回执草稿；待复核，非攻略结论"}),None,false,&cancel).await?
            }
        } else {
            let result=self.memory.call_cancellable("memory_create",&record.content_package,json!({"id":memory_id,"title":format!("游玩经历 {}（待复核）",&record.conversation_id[..record.conversation_id.len().min(8)]),"body":body,"kind":"procedure","tags":[DRAFT_TAG,"自动记录","待复核"],"validation":"pending","game_version":"unknown","sources":[{"type":"conversation","id":format!("session-{}",record.conversation_id),"conversation_id":record.conversation_id,"excerpt":"本会话的真实用户指引和公开调用回执"}],"operation_id":operation,"reason":"直接保存真实公开过程；后台空闲后再提炼，可随时查询"}),None,false,&cancel).await;
            match result {
                Ok(result) => result,
                Err(error) if error.to_string().contains("suppressed") => {
                    checkpoint.suppressed = true;
                    self.conversations
                        .save_memory_checkpoint(&record.conversation_id, &checkpoint)?;
                    return Ok(());
                }
                Err(error) => return Err(error),
            }
        };
        let game_limits = record.game_limits.as_ref().unwrap_or(&record.limits);
        let import_identity = json!({"text":text,"limits":game_limits});
        let result=self.memory.call_cancellable("memory_import",&record.content_package,json!({"operation_id":format!("experience:{}:{}:{through}:{:x}",record.conversation_id,checkpoint.seq,Sha256::digest(import_identity.to_string().as_bytes())),"filename":"experience.md","title":"自动记录的游玩经历","text":text,"game_version":"unknown","limits":game_limits}),None,false,&cancel).await?;
        let job = result["job_id"].as_str().unwrap_or("");
        self.conversations.link_memory_job(
            &record.conversation_id,
            &record.content_package,
            job,
        )?;
        let references = self.memory.source_references(
            &record.content_package,
            result["source_id"].as_str().unwrap_or(""),
            result["source_revision"].as_u64().unwrap_or(1),
        )?;
        let original_draft = self
            .memory
            .call_cancellable(
                "memory_get",
                &record.content_package,
                json!({"id":memory_id}),
                None,
                false,
                &cancel,
            )
            .await?;
        let mut draft_sources = original_draft["memory"]["sources"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let before = draft_sources.len();
        for reference in &references {
            if draft_sources.len() < 100 && !draft_sources.contains(reference) {
                draft_sources.push(reference.clone());
            }
        }
        if draft_sources.len() != before {
            draft=self.memory.call_cancellable("memory_update",&record.content_package,json!({"id":memory_id,"expected_version":original_draft["version"],"patch":{"sources":draft_sources},"operation_id":format!("experience-draft-sources:{}:{through}",record.conversation_id),"reason":"关联本次增量原稿的真实来源，删除草稿后防止同源自动复建"}),None,false,&cancel).await?;
        }
        for (definition_id, literal) in self
            .conversations
            .memory_definition_links(&record.conversation_id)?
        {
            let literal = public_text(&literal);
            if literal.is_empty() || !text.contains(&literal) {
                continue;
            }
            let original = self
                .memory
                .call_cancellable(
                    "memory_get",
                    &record.content_package,
                    json!({"id":definition_id}),
                    None,
                    false,
                    &cancel,
                )
                .await;
            if let Ok(original) = original {
                if original["memory"]["status"] == "active" {
                    let mut sources = original["memory"]["sources"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    let before = sources.len();
                    for reference in &references {
                        if sources.len() < 100 && !sources.contains(reference) {
                            sources.push(reference.clone());
                        }
                    }
                    if sources.len() != before {
                        self.memory.call_cancellable("memory_update",&record.content_package,json!({"id":definition_id,"expected_version":original["version"],"patch":{"sources":sources},"reason":"宿主关联真实用户原稿来源，防止用户删除后同源自动复建","operation_id":format!("experience-definition-link:{}:{through}:{definition_id}",record.conversation_id)}),None,true,&cancel).await?;
                    }
                }
            }
        }
        let _ = self
            .memory_job_allowed(&record.content_package, job)
            .await?;
        let from = checkpoint.seq;
        checkpoint.seq = through;
        checkpoint.saved_at = Some(Utc::now().to_rfc3339());
        self.conversations
            .save_memory_checkpoint(&record.conversation_id, &checkpoint)?;
        self.conversations.event(&record.conversation_id,"memory_staged","游玩记忆草稿已保存；攻略将在空闲时自动整理",json!({"memory":{"id":memory_id,"revision":draft["revision"],"validation":"pending"},"checkpoint":{"from_seq":from,"to_seq":through},"automatic":true,"job_id":job}))?;
        self.sync_memory_job_events()?;
        Ok(())
    }

    pub(super) fn sync_memory_job_events(&self) -> Result<()> {
        for (id, package, job, last) in self.conversations.memory_job_links()? {
            let Ok(record) = self.memory.import_job_record(&package, &job) else {
                continue;
            };
            let summary = json!({"job_id":job,"status":record.status,"total":record.total,"processed":record.processed,"counts":record.counts,"error":record.error.as_deref().map(public_text)});
            let fingerprint = summary.to_string();
            if fingerprint == last {
                continue;
            }
            let message = match record.status.as_str() {
                "running" => "后台正在整理攻略",
                "completed" => "攻略整理完成",
                "failed" => "攻略整理失败，原始资料已保留",
                "paused" => "攻略整理已暂停",
                "cancelled" => "攻略整理已取消",
                _ => "游玩经历已入队，等待空闲自动整理",
            };
            self.conversations.event(&id,"memory_job",message,json!({"state":record.status,"memory_id":draft_id(&id),"job_id":job,"result":summary,"validation":"pending","budget_scope":"independent_import_job"}))?;
            self.conversations
                .save_memory_job_summary(&id, &job, &fingerprint)?;
        }
        Ok(())
    }

    pub(super) async fn memory_job_allowed(&self, package: &str, job: &str) -> Result<bool> {
        for (id, linked_package, linked_job, _) in self.conversations.memory_job_links()? {
            if linked_package != package || linked_job != job {
                continue;
            }
            let checkpoint = self.conversations.memory_checkpoint(&id)?;
            let draft = self
                .memory
                .call_cancellable(
                    "memory_get",
                    package,
                    json!({"id":draft_id(&id)}),
                    None,
                    false,
                    &AtomicBool::new(false),
                )
                .await;
            let inactive = draft.as_ref().is_ok_and(|value| {
                value["memory"]["status"] != "active"
                    || value["memory"]["validation"] != "pending"
                    || value["memory"]["protected_fields"]
                        .as_array()
                        .is_some_and(|fields| {
                            fields
                                .iter()
                                .any(|field| matches!(field.as_str(), Some("body" | "sources")))
                        })
            }) || draft
                .as_ref()
                .is_err_and(|error| error.to_string().contains("permanently_deleted"));
            let mut definition_deleted = false;
            let source = self.memory.import_job_record(package, job)?;
            for (memory, literal) in self.conversations.memory_definition_links(&id)? {
                let safe = public_text(&literal);
                if safe.is_empty() || !source.chunks.iter().any(|chunk| chunk.text.contains(&safe))
                {
                    continue;
                }
                let original = self
                    .memory
                    .call_cancellable(
                        "memory_get",
                        package,
                        json!({"id":memory}),
                        None,
                        false,
                        &AtomicBool::new(false),
                    )
                    .await;
                if original.as_ref().is_ok_and(|value| {
                    value["memory"]["status"] != "active" || value["memory"]["body"] != literal
                }) || original
                    .as_ref()
                    .is_err_and(|error| error.to_string().contains("permanently_deleted"))
                {
                    definition_deleted = true;
                    break;
                }
            }
            if checkpoint.suppressed || inactive || definition_deleted {
                let _ = self
                    .memory
                    .call_cancellable(
                        "memory_import_cancel",
                        package,
                        json!({"job_id":job,"operation_id":format!("experience-suppress:{job}")}),
                        None,
                        false,
                        &AtomicBool::new(false),
                    )
                    .await;
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub(super) fn memory_import_origins(
        &self,
        package: &str,
        job: &str,
    ) -> Result<Vec<super::memory::ImportOrigin>> {
        let mut origins = Vec::new();
        let source = self.memory.import_job_record(package, job)?;
        for (id, linked_package, linked_job, _) in self.conversations.memory_job_links()? {
            if linked_package != package || linked_job != job {
                continue;
            }
            if self.conversations.record(&id)?.game_session_id.as_deref() == Some(&id) {
                origins.push(super::memory::ImportOrigin::Draft(draft_id(&id)));
            }
            for (memory, literal) in self.conversations.memory_definition_links(&id)? {
                let safe = public_text(&literal);
                if !safe.is_empty() && source.chunks.iter().any(|chunk| chunk.text.contains(&safe))
                {
                    origins.push(super::memory::ImportOrigin::Definition {
                        id: memory,
                        literal,
                    });
                }
            }
        }
        Ok(origins)
    }
}
