use super::super::services::{EmbeddingConfig, SearchConfig, WebReadConfig};
use super::*;
use crate::{config::Config, resources::PackageInput};

fn store() -> (MemoryStore, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let cfg = Config {
        data_dir: temp.path().to_path_buf(),
        ..Default::default()
    };
    let packages = Arc::new(PackageStore::open(&cfg).unwrap());
    for name in ["game-a", "game-b"] {
        packages
            .create_package(PackageInput {
                id: name.into(),
                android_targets: vec!["*".into()],
                ..Default::default()
            })
            .unwrap();
    }
    (MemoryStore::new(packages, temp.path()), temp)
}
async fn create(
    s: &MemoryStore,
    pkg: &str,
    id: &str,
    title: &str,
    body: &str,
    user: bool,
) -> Value {
    s.call("memory_create",pkg,json!({"id":id,"operation_id":format!("create:{id}"),"title":title,"body":body,"validation":"verified","game_version":"1.0"}),None,user).await.unwrap()
}
#[tokio::test]
async fn protects_delegated_fields_and_rejects_forged_identity() {
    let (s, _temp) = store();
    let a = create(
        &s,
        "game-a",
        "manual",
        "用户定义",
        "不要使用钻石购买体力",
        true,
    )
    .await;
    let error=s.call("memory_update","game-a",json!({"id":"manual","operation_id":"wrong","expected_version":a["version"],"reason":"模型猜测","patch":{"body":"花钻石购买"}}),None,false).await.unwrap_err();
    assert!(error.to_string().contains("protected"));
    assert!(s
        .call(
            "memory_get",
            "game-a",
            json!({"id":"manual","human":true}),
            None,
            false
        )
        .await
        .is_err());
    let b=s.call("memory_update","game-a",json!({"id":"manual","operation_id":"user-fix","expected_version":a["version"],"reason":"用户委托","patch":{"body":"只有用户指示才购买"}}),None,true).await.unwrap();
    assert_eq!(b["revision"], 2);
    assert!(b["memory"]["protected_fields"]
        .as_array()
        .unwrap()
        .contains(&json!("body")));
}
#[tokio::test]
async fn retries_are_durable_and_conflicts_never_overwrite() {
    let (s, temp) = store();
    let a = create(&s, "game-a", "first", "背包", "打开背包后整理材料", false).await;
    let args = json!({"id":"first","operation_id":"one-update","expected_version":a["version"],"reason":"修复","patch":{"body":"打开背包后先筛选材料"}});
    let b = s
        .call("memory_update", "game-a", args.clone(), None, false)
        .await
        .unwrap();
    let restarted = MemoryStore::new(s.packages.clone(), temp.path());
    let retry = restarted
        .call("memory_update", "game-a", args.clone(), None, false)
        .await
        .unwrap();
    assert_eq!(b["revision"], retry["revision"]);
    let mut different = args.clone();
    different["patch"]["body"] = json!("不同输入");
    assert!(restarted
        .call("memory_update", "game-a", different, None, false)
        .await
        .unwrap_err()
        .to_string()
        .contains("operation_id_reused"));
    let mut stale = args;
    stale["operation_id"] = json!("second-update");
    assert!(restarted
        .call("memory_update", "game-a", stale, None, false)
        .await
        .unwrap_err()
        .to_string()
        .contains("version_conflict"));
    let history = restarted
        .call(
            "memory_history",
            "game-a",
            json!({"id":"first"}),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(history["total"], 2);
}
#[tokio::test]
async fn disable_restore_and_permanent_delete_preserve_the_right_evidence() {
    let (s, _temp) = store();
    let a = create(
        &s,
        "game-a",
        "old",
        "领取奖励",
        "先打开邮箱再领取奖励",
        false,
    )
    .await;
    let b=s.call("memory_set_status","game-a",json!({"id":"old","status":"disabled","operation_id":"disable","reason":"用户停用","expected_version":a["version"]}),None,true).await.unwrap();
    assert!(s
        .call(
            "memory_create",
            "game-a",
            json!({"title":"领取奖励","body":"新说法","operation_id":"resurrect"}),
            None,
            false
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("suppressed"));
    let c=s.call("memory_restore","game-a",json!({"id":"old","revision":1,"operation_id":"restore","reason":"用户恢复","expected_version":b["version"]}),None,true).await.unwrap();
    assert_eq!(c["revision"], 3);
    let old = s
        .call(
            "memory_get",
            "game-a",
            json!({"id":"old","revision":1}),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(old["memory"]["body"], "先打开邮箱再领取奖励");
    let quarantined = s.root.join("cache/memory-index-corrupt/game-a/old.sqlite");
    let other_cache = s.root.join("cache/memory-index-corrupt/game-b/keep.sqlite");
    for file in [&quarantined, &other_cache] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, b"retained derived cache fixture").unwrap();
    }
    s.call(
        "memory_delete",
        "game-a",
        json!({"id":"old","permanent":true,"operation_id":"purge","expected_version":c["version"]}),
        None,
        true,
    )
    .await
    .unwrap();
    assert!(!quarantined.exists());
    assert!(other_cache.exists());
    assert!(s
        .call(
            "memory_get",
            "game-a",
            json!({"id":"old","revision":1}),
            None,
            false
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("permanently_deleted"));
    assert!(s
        .packages
        .list("game-a", PLUGIN, "memory-revisions/old")
        .unwrap()
        .is_empty());
    let db = s.open_index("game-a").unwrap();
    let count: i64 = db
        .query_row("SELECT count(*) FROM chunks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}
#[tokio::test]
async fn chinese_alias_queries_and_version_filters_reconcile_external_edits() {
    let (s, _temp) = store();
    create(
        &s,
        "game-a",
        "a",
        "圣遗物",
        "打开背包，筛选圣遗物后强化",
        false,
    )
    .await;
    create(&s, "game-b", "b", "圣遗物", "另一个游戏的攻略", false).await;
    s.call("memory_dictionary_update","game-a",json!({"operation_id":"dictionary","terms":["圣遗物"],"aliases":{"圣遗物":["遗物","装备词条"]}}),None,true).await.unwrap();
    let r = s
        .call(
            "memory_search",
            "game-a",
            json!({"query":"装备词条","mode":"keyword","game_version":"1.0"}),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(r["items"][0]["id"], "a");
    let no = s
        .call(
            "memory_search",
            "game-a",
            json!({"query":"圣遗物","mode":"keyword","game_version":"2.0"}),
            None,
            false,
        )
        .await
        .unwrap();
    assert!(no["items"].as_array().unwrap().is_empty());
    let two = s
        .call(
            "memory_search",
            "game-a",
            json!({"query":"背包","mode":"keyword"}),
            None,
            false,
        )
        .await
        .unwrap();
    assert!(!two["items"].as_array().unwrap().is_empty());
    let (m, e) = s.read_memory("game-a", "a").unwrap();
    let mut m = m;
    m.body = "需要先挑战地脉".into();
    m.revision += 1;
    s.packages
        .write_text(
            "game-a",
            PLUGIN,
            &path("a"),
            &serde_json::to_string_pretty(&m).unwrap(),
            Some(&e.version()),
            false,
        )
        .unwrap();
    let new = s
        .call(
            "memory_search",
            "game-a",
            json!({"query":"地脉","mode":"keyword"}),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(new["items"][0]["revision"], 2);
    let injection = s
        .call(
            "memory_search",
            "game-a",
            json!({"query":"\" OR *; DROP TABLE memories; --","mode":"keyword"}),
            None,
            false,
        )
        .await;
    assert!(injection.is_ok());
}
#[tokio::test]
async fn import_outcomes_and_writes_reject_canonical_raw_drafts() {
    let (s, _temp) = store();
    let raw=s.call("memory_create","game-a",json!({"id":"raw-evidence","title":"待复核原稿","body":"用户原始回执","validation":"pending","tags":["session_receipts_pending"],"operation_id":"raw-receipt"}),None,false).await.unwrap();
    let imported=s.call("memory_import","game-a",json!({"operation_id":"raw-target-test","filename":"guide.md","text":"# 原稿\n等待提炼"}),None,false).await.unwrap();
    let job = imported["job_id"].as_str().unwrap();
    let claim = s
        .claim_import_chunk("game-a", job, "raw-claim")
        .unwrap()
        .unwrap();
    for disposition in ["retained", "created", "updated", "merged"] {
        let mut outcome = json!({"disposition":disposition,"operation_id":"raw-receipt"});
        if disposition == "retained" {
            outcome["id"] = json!("raw-evidence");
        }
        let error = s
            .complete_import_chunk(
                "game-a",
                job,
                claim["chunk"]["id"].as_str().unwrap(),
                "raw-claim",
                outcome,
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("raw_draft_not_guide"),
            "{disposition}: {error}"
        );
    }
    let origins = [ImportOrigin::Draft("raw-evidence".into())];
    let error=s.call_import_for_origins_cancellable("memory_update","game-a",json!({"id":"raw-evidence","expected_version":raw["version"],"patch":{"body":"AI改写原稿","tags":[]},"operation_id":"overwrite-origin","reason":"提炼"}),job,&origins,&AtomicBool::new(false)).await.unwrap_err();
    assert!(error.to_string().contains("raw_draft_not_guide"));
    assert_eq!(
        s.call(
            "memory_get",
            "game-a",
            json!({"id":"raw-evidence"}),
            None,
            false
        )
        .await
        .unwrap()["memory"]["body"],
        "用户原始回执"
    );
    let guide = create(&s, "game-a", "real-guide", "正常攻略", "已提炼步骤", false).await;
    for (name, args) in [
        (
            "memory_create",
            json!({"title":"伪原稿","body":"伪原稿","tags":["session_receipts_pending"],"operation_id":"forge-raw"}),
        ),
        (
            "memory_update",
            json!({"id":"real-guide","expected_version":guide["version"],"patch":{"tags":["session_receipts_pending"]},"operation_id":"forge-raw-update","reason":"更改"}),
        ),
    ] {
        assert!(s
            .call_import_cancellable(name, "game-a", args, job, &AtomicBool::new(false))
            .await
            .unwrap_err()
            .to_string()
            .contains("raw_draft_not_editable"));
    }
    assert_eq!(s.import_job_record("game-a", job).unwrap().processed, 0);
    let finished = s
        .complete_import_chunk(
            "game-a",
            job,
            claim["chunk"]["id"].as_str().unwrap(),
            "raw-claim",
            json!({"disposition":"retained","id":"real-guide"}),
        )
        .unwrap();
    assert_eq!(finished["processed"], 1);
}
#[tokio::test]
async fn import_queue_survives_restart_deduplicates_and_requires_committed_targets() {
    let (s, temp) = store();
    let args = json!({"operation_id":"import-one","filename":"攻略.md","text":"# 奖励\n\n1. 打开邮箱。\n2. 领取奖励，直到邮箱空了。"});
    let a = s
        .call("memory_import", "game-a", args.clone(), None, true)
        .await
        .unwrap();
    let restarted = MemoryStore::new(s.packages.clone(), temp.path());
    let retry = restarted
        .call("memory_import", "game-a", args, None, true)
        .await
        .unwrap();
    assert_eq!(a["job_id"], retry["job_id"]);
    let duplicate=restarted.call("memory_import","game-a",json!({"operation_id":"import-two","filename":"相同.txt","text":"# 奖励\n\n1. 打开邮箱。\n2. 领取奖励，直到邮箱空了。"}),None,true).await.unwrap();
    assert_eq!(a["job_id"], duplicate["job_id"]);
    let job_id = a["job_id"].as_str().unwrap();
    let claim = restarted
        .claim_import_chunk("game-a", job_id, "claim-one")
        .unwrap()
        .unwrap();
    let chunk_id = claim["chunk"]["id"].as_str().unwrap();
    assert!(restarted
        .complete_import_chunk(
            "game-a",
            job_id,
            chunk_id,
            "claim-one",
            json!({"disposition":"created","operation_id":"not-saved"})
        )
        .is_err());
    let saved = create(
        &restarted,
        "game-a",
        "merged-guide",
        "邮箱",
        "打开邮箱领取奖励",
        false,
    )
    .await;
    let result = restarted
        .complete_import_chunk(
            "game-a",
            job_id,
            chunk_id,
            "claim-one",
            json!({"disposition":"created","operation_id":"create:merged-guide","id":saved["id"]}),
        )
        .unwrap();
    assert_eq!(result["processed"], 1);
}
#[tokio::test]
async fn irrelevant_experience_can_be_skipped_without_fabricating_a_memory() {
    let (s, _temp) = store();
    let imported=s.call("memory_import","game-a",json!({"operation_id":"skip-source","filename":"experience.txt","text":"用户说谢谢，没有攻略信息"}),None,false).await.unwrap();
    let job = imported["job_id"].as_str().unwrap();
    let claim = s
        .claim_import_chunk("game-a", job, "skip-claim")
        .unwrap()
        .unwrap();
    let result = s
        .complete_import_chunk(
            "game-a",
            job,
            claim["chunk"]["id"].as_str().unwrap(),
            "skip-claim",
            json!({"disposition":"skipped","reason":"无可复用经验"}),
        )
        .unwrap();
    assert_eq!(result["counts"]["skipped"], 1);
    assert_eq!(
        s.call("memory_list", "game-a", json!({}), None, false)
            .await
            .unwrap()["total"],
        0
    );
}
#[test]
fn structural_chunks_keep_table_headers_and_never_exceed_embedding_bytes() {
    let (s, _temp) = store();
    let _ = s;
    let source = Source {
        format_version: FORMAT,
        id: "long".into(),
        title: "游戏攻略".into(),
        filename: "test.md".into(),
        format: "markdown".into(),
        text: format!(
            "# 表格\n\n| 道具 | 条件 |\n| --- | --- |\n{}\n\n# 流程\n\n{}",
            (0..150)
                .map(|n| format!("| 物品{n} | 完成挑战后领取 |\n"))
                .collect::<String>(),
            (0..150)
                .map(|n| format!("{n}. 打开背包，筛选资源并观察成功提示。\n"))
                .collect::<String>()
        ),
        game_version: "unknown".into(),
        source_url: None,
        revision: 1,
        created_at: now(),
        updated_at: now(),
        deleted: false,
        content_hash: String::new(),
        applied_operations: BTreeMap::new(),
    };
    let m = imports::source_memory(&source);
    let chunks = index::chunks(&m, 1000);
    assert!(chunks.len() > 5);
    assert!(chunks.iter().all(|c| c.embedding_text.len() <= 1000));
    for c in chunks
        .iter()
        .filter(|c| c.section == "表格" && c.text.contains("| 物品"))
    {
        assert!(c.text.contains("| 道具 | 条件 |"));
    }
}
async fn local_embedding() -> (ServiceConnection, tokio::task::JoinHandle<()>) {
    use axum::{routing::post, Json, Router};
    let app = Router::new().route(
        "/embeddings",
        post(|Json(v): Json<Value>| async move {
            let vectors: Vec<Value> = v["input"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    let text = t.as_str().unwrap();
                    let vector = if text.contains("背包")
                        || text.contains("物品整理")
                        || text.contains("物资分类")
                    {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    };
                    json!({"index":i,"embedding":vector})
                })
                .collect();
            Json(json!({"data":vectors,"usage":{"total_tokens":10}}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let connection = ServiceConnection::new(
        EmbeddingConfig {
            enabled: true,
            provider: "local-test".into(),
            base_url: format!("http://{addr}"),
            model: "mock-2d".into(),
            ..Default::default()
        },
        String::new(),
        SearchConfig::default(),
        String::new(),
        WebReadConfig::default(),
    )
    .unwrap();
    (connection, task)
}
#[tokio::test]
async fn deleted_source_provenance_cannot_be_paraphrased_back_into_memory() {
    let (s, _temp) = store();
    let reference =
        json!({"id":"source-one","revision":1,"section":"奖励","excerpt":"打开邮箱领取奖励"});
    let saved=s.call("memory_create","game-a",json!({"id":"original","operation_id":"source-create","title":"原攻略","body":"开邮箱领取","sources":[reference.clone()]}),None,false).await.unwrap();
    s.call("memory_delete","game-a",json!({"id":"original","expected_version":saved["version"],"operation_id":"source-delete","reason":"用户删除"}),None,true).await.unwrap();
    let args = json!({"operation_id":"paraphrase","title":"全新名称","body":"用另外的措辞描述奖励收取","sources":[reference]});
    assert!(s
        .call("memory_create", "game-a", args.clone(), None, false)
        .await
        .unwrap_err()
        .to_string()
        .contains("suppressed"));
    let mut delegated = args;
    delegated["operation_id"] = json!("explicit-new-user-request");
    assert!(s
        .call("memory_create", "game-a", delegated, None, true)
        .await
        .is_ok());
}
#[tokio::test]
async fn protected_sources_show_conflicts_without_changing_user_fields() {
    let (s, _temp) = store();
    let imported = s
        .call(
            "memory_import",
            "game-a",
            json!({"operation_id":"source-input","filename":"原稿.md","text":"打开邮箱领取奖励"}),
            None,
            true,
        )
        .await
        .unwrap();
    let source_id = imported["source_id"].as_str().unwrap();
    let reference = json!({"id":source_id,"revision":1,"section":"","excerpt":"打开邮箱领取奖励"});
    s.call("memory_create","game-a",json!({"id":"protected","operation_id":"protected-create","title":"邮箱奖励","body":"用户指定：打开邮箱领取奖励","validation":"verified","kind":"definition","sources":[reference]}),None,true).await.unwrap();
    let source = s
        .call(
            "memory_source_get",
            "game-a",
            json!({"id":source_id}),
            None,
            false,
        )
        .await
        .unwrap();
    s.call("memory_source_update","game-a",json!({"id":source_id,"expected_version":source["version"],"operation_id":"new-source","reason":"来源修正","text":"新版改为活动界面领取"}),None,true).await.unwrap();
    let current = s
        .call(
            "memory_get",
            "game-a",
            json!({"id":"protected"}),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(current["memory"]["body"], "用户指定：打开邮箱领取奖励");
    assert_eq!(current["memory"]["validation"], "verified");
    assert_eq!(current["memory"]["effective_validation"], "pending");
    assert_eq!(current["source_conflicts"][0]["current_revision"], 2);
    let normal = s
        .call(
            "memory_search",
            "game-a",
            json!({"query":"邮箱","mode":"keyword"}),
            None,
            false,
        )
        .await
        .unwrap();
    assert!(normal["items"].as_array().unwrap().is_empty());
    let instructions = s
        .call(
            "memory_list",
            "game-a",
            json!({"validation":"any","kind":"definition","protected_only":true}),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(instructions["items"].as_array().unwrap().len(), 1);
    let old = s
        .call(
            "memory_source_get",
            "game-a",
            json!({"id":source_id,"revision":1}),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(old["source"]["text"], "打开邮箱领取奖励");
    assert_eq!(old["changed_since_reference"], true);
}
#[tokio::test]
async fn import_pause_budget_unknown_and_zero_limits_are_persistent() {
    let (s, temp) = store();
    let imported=s.call("memory_import","game-a",json!({"operation_id":"budget-import","filename":"预算.txt","text":"打开背包整理材料","limits":{"max_turns":1,"max_actions":0,"max_seconds":0,"max_tokens":0,"max_failures":0}}),None,true).await.unwrap();
    let job = imported["job_id"].as_str().unwrap();
    s.record_import_request("game-a", job, "request-one")
        .unwrap();
    s.record_import_usage("game-a", job, "request-one", None, 0.1, false)
        .unwrap();
    assert!(s
        .record_import_request("game-a", job, "request-two")
        .unwrap_err()
        .to_string()
        .contains("budget"));
    let restarted = MemoryStore::new(s.packages.clone(), temp.path());
    assert_eq!(
        restarted.import_job_record("game-a", job).unwrap().status,
        "paused"
    );
    restarted.call("memory_import_resume","game-a",json!({"job_id":job,"operation_id":"unlimited","limits":{"max_turns":0,"max_actions":0,"max_seconds":0,"max_tokens":0,"max_failures":0}}),None,true).await.unwrap();
    restarted
        .record_import_request("game-a", job, "request-two")
        .unwrap();
    restarted
        .record_import_usage("game-a", job, "request-two", Some(20), 0.2, false)
        .unwrap();
    let usage = restarted.import_job_record("game-a", job).unwrap().usage;
    assert_eq!(usage.turns, 2);
    assert!(usage.has_unknown_tokens);
    assert_eq!(usage.total_tokens, None);
    assert_eq!(usage.known_tokens, 20);
    let chunk = restarted
        .claim_import_chunk("game-a", job, "claim")
        .unwrap()
        .unwrap();
    let chunk_id = chunk["chunk"]["id"].as_str().unwrap();
    restarted
        .call(
            "memory_import_pause",
            "game-a",
            json!({"job_id":job,"operation_id":"user-pause"}),
            None,
            true,
        )
        .await
        .unwrap();
    assert!(restarted
        .complete_import_chunk(
            "game-a",
            job,
            chunk_id,
            "claim",
            json!({"disposition":"retained"})
        )
        .is_err());
    assert!(restarted
        .call_import_cancellable(
            "memory_create",
            "game-a",
            json!({"operation_id":"late","title":"过期结果","body":"旧原稿"}),
            job,
            &AtomicBool::new(false)
        )
        .await
        .is_err());
}
#[tokio::test]
async fn archive_candidates_preserve_local_memory_and_delete_clears_local_jobs() {
    use crate::resources::ResourceHandler;
    let (s, temp) = store();
    create(&s, "game-a", "local", "本地约定", "保留用户定义", true).await;
    let other = create(&s, "game-b", "incoming", "导入攻略", "新的操作步骤", false).await;
    let incoming = temp.path().join("staged-plugin");
    std::fs::create_dir_all(incoming.join("memories")).unwrap();
    let (_, entry) = s
        .read_memory("game-b", other["id"].as_str().unwrap())
        .unwrap();
    std::fs::write(incoming.join("memories/incoming.json"), entry.content).unwrap();
    std::fs::write(
        incoming.join("memories/unknown.json"),
        "{\"format_version\":999,\"body\":\"未知原文\"}",
    )
    .unwrap();
    s.prepare_package_replace(
        "game-a",
        Some(&s.packages.plugin_dir("game-a", PLUGIN).unwrap()),
        &incoming,
    )
    .unwrap();
    assert!(incoming.join("memories/local.json").exists());
    assert!(!incoming.join("memories/incoming.json").exists());
    assert_eq!(
        std::fs::read_dir(incoming.join("memory-unrecognized"))
            .unwrap()
            .count(),
        1
    );
    let source_path = std::fs::read_dir(incoming.join("memory-sources"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let source: Source =
        serde_json::from_str(&std::fs::read_to_string(source_path).unwrap()).unwrap();
    assert!(source.text.is_empty());
    let imported = s
        .call(
            "memory_import",
            "game-a",
            json!({"operation_id":"delete-job","filename":"delete.txt","text":"之后删除配置包"}),
            None,
            true,
        )
        .await
        .unwrap();
    assert!(s
        .import_job_record("game-a", imported["job_id"].as_str().unwrap())
        .is_ok());
    s.packages.delete_package("game-a").unwrap();
    s.cleanup_deleted_package("game-a").unwrap();
    s.packages
        .create_package(PackageInput {
            id: "game-a".into(),
            android_targets: vec!["*".into()],
            ..Default::default()
        })
        .unwrap();
    assert!(s.pending_imports("game-a").unwrap().is_empty());
    assert_eq!(
        s.call("memory_list", "game-a", json!({}), None, false)
            .await
            .unwrap()["total"],
        0
    );
}
#[tokio::test]
async fn prepared_orphan_revisions_are_immutable_but_not_committed_history() {
    let (s, _temp) = store();
    let first = create(&s, "game-a", "orphan", "背包", "整理材料", false).await;
    let (mut prepared, _) = s.read_memory("game-a", "orphan").unwrap();
    prepared.revision = 2;
    prepared.operation_id = "failed-other-op".into();
    prepared.operation_fingerprint = "failed-other-fingerprint".into();
    prepared.body = "未提交内容".into();
    s.write_json_new("game-a", "memory-revisions/orphan/2.json", &prepared)
        .unwrap();
    let current=s.call("memory_update","game-a",json!({"id":"orphan","operation_id":"successful-op","expected_version":first["version"],"reason":"实际修订","patch":{"body":"整理后检查容量"}}),None,false).await.unwrap();
    assert_eq!(current["revision"], 3);
    assert!(s
        .call(
            "memory_get",
            "game-a",
            json!({"id":"orphan","revision":2}),
            None,
            false
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("not_committed"));
    let history = s
        .call(
            "memory_history",
            "game-a",
            json!({"id":"orphan"}),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(history["total"], 2);
    let receipt = format!("memory-operations/{}.json", hash("successful-op"));
    s.packages
        .delete_resource("game-a", PLUGIN, &receipt)
        .unwrap();
    let recovered=s.call("memory_update","game-a",json!({"id":"orphan","operation_id":"successful-op","expected_version":first["version"],"reason":"实际修订","patch":{"body":"整理后检查容量"}}),None,false).await.unwrap();
    assert_eq!(recovered["revision"], 3);
    assert_eq!(recovered["recovered"], true);
}
#[tokio::test]
async fn actual_sqlite_vec_hybrid_search_filters_before_topk_and_updates_incrementally() {
    let (s, _temp) = store();
    create(&s, "game-a", "bag", "背包整理", "背包内筛选材料", false).await;
    create(&s, "game-a", "mail", "邮箱奖励", "邮件领取奖励", false).await;
    let (connection, task) = local_embedding().await;
    let r = s
        .call(
            "memory_search",
            "game-a",
            json!({"query":"物品整理","game_version":"1.0","mode":"hybrid"}),
            Some(&connection),
            false,
        )
        .await
        .unwrap();
    assert_eq!(r["retrieval"]["semantic"], true);
    assert_eq!(r["items"][0]["id"], "bag");
    let db = s.open_index("game-a").unwrap();
    let vec_version: String = db
        .query_row("SELECT vec_version()", [], |r| r.get(0))
        .unwrap();
    assert!(vec_version.starts_with('v') || vec_version.starts_with('0'));
    let before: Vec<u8> = db
        .query_row(
            "SELECT embedding FROM chunks WHERE memory_id='bag'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(db);
    let (_, e) = s.read_memory("game-a", "bag").unwrap();
    s.call("memory_update","game-a",json!({"id":"bag","operation_id":"tag-only","expected_version":e.version(),"reason":"标签","patch":{"tags":["材料"]}}),None,false).await.unwrap();
    let db = s.open_index("game-a").unwrap();
    let after: Vec<u8> = db
        .query_row(
            "SELECT embedding FROM chunks WHERE memory_id='bag'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(before, after);
    drop(db);
    let no = s
        .call(
            "memory_search",
            "game-a",
            json!({"query":"物品整理","game_version":"9.0"}),
            Some(&connection),
            false,
        )
        .await
        .unwrap();
    assert!(no["items"].as_array().unwrap().is_empty());
    task.abort();
    let degraded = s
        .call(
            "memory_search",
            "game-a",
            json!({"query":"背包"}),
            Some(&connection),
            false,
        )
        .await
        .unwrap();
    assert_eq!(degraded["retrieval"]["semantic"], false);
    assert!(!degraded["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn merging_shared_sources_and_user_restore_do_not_block_active_maintenance() {
    let (s, _temp) = store();
    let reference =
        json!({"id":"shared-source","revision":1,"section":"背包","excerpt":"背包内筛选材料"});
    let mut records = Vec::new();
    for id in ["merge-old", "merge-target"] {
        records.push(s.call("memory_create","game-a",json!({"id":id,"operation_id":format!("create-{id}"),"title":id,"body":format!("{id} 的背包整理流程"),"sources":[reference.clone()]}),None,false).await.unwrap());
    }
    s.call("memory_set_status","game-a",json!({"id":"merge-old","status":"merged","operation_id":"merge-mark","reason":"合入另一条记忆","expected_version":records[0]["version"]}),None,false).await.unwrap();
    let target=s.call("memory_update","game-a",json!({"id":"merge-target","operation_id":"merge-target-update","expected_version":records[1]["version"],"reason":"补充合并内容","patch":{"body":"筛选材料后检查背包容量"}}),None,false).await.unwrap();
    let disabled=s.call("memory_set_status","game-a",json!({"id":"merge-target","status":"disabled","operation_id":"target-disable","reason":"用户暂停使用","expected_version":target["version"]}),None,true).await.unwrap();
    let restored=s.call("memory_restore","game-a",json!({"id":"merge-target","operation_id":"target-restore","reason":"用户恢复使用","expected_version":disabled["version"]}),None,true).await.unwrap();
    assert!(s.call("memory_update","game-a",json!({"id":"merge-target","operation_id":"after-restore","expected_version":restored["version"],"reason":"复核可编辑流程","patch":{"body":"整理材料后检查容量和操作结果"}}),None,false).await.is_ok());
}

#[tokio::test]
async fn imported_deletion_markers_and_inactive_sources_cannot_revive_old_knowledge() {
    use crate::resources::ResourceHandler;
    let (s, temp) = store();
    create(
        &s,
        "game-a",
        "same-id",
        "本地用户定义",
        "保留此条本地约定",
        true,
    )
    .await;
    let source =
        json!({"id":"archived-source","revision":1,"section":"已删除攻略","excerpt":"过期流程"});
    let marker = Tombstone {
        format_version: FORMAT,
        id: "same-id".into(),
        title_hash: hash("已删除攻略"),
        body_hash: hash("过期流程"),
        permanent: true,
        updated_at: now(),
        operation_id: "archived-delete".into(),
        operation_fingerprint: "marker".into(),
        source_fingerprints: source_fingerprints(std::slice::from_ref(&source)),
    };
    let archived = create(
        &s,
        "game-b",
        "inactive",
        "停用的攻略",
        "这段攻略已停用",
        false,
    )
    .await;
    s.call("memory_set_status","game-b",json!({"id":"inactive","status":"disabled","operation_id":"archived-disable","reason":"旧攻略不适用","expected_version":archived["version"]}),None,true).await.unwrap();
    let incoming = temp.path().join("staged-markers");
    std::fs::create_dir_all(incoming.join("memory-tombstones")).unwrap();
    std::fs::create_dir_all(incoming.join("memories")).unwrap();
    std::fs::write(
        incoming.join("memory-tombstones/same-id.json"),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    let (_, disabled_entry) = s.read_memory("game-b", "inactive").unwrap();
    std::fs::write(
        incoming.join("memories/inactive.json"),
        disabled_entry.content,
    )
    .unwrap();
    s.prepare_package_replace(
        "game-a",
        Some(&s.packages.plugin_dir("game-a", PLUGIN).unwrap()),
        &incoming,
    )
    .unwrap();
    let mut blocked_archive_fingerprints = 0;
    for entry in std::fs::read_dir(incoming.join("memory-tombstones")).unwrap() {
        let entry = entry.unwrap();
        let text = std::fs::read_to_string(entry.path()).unwrap();
        let t: Tombstone = serde_json::from_str(&text).unwrap();
        assert_ne!(t.id, "same-id");
        if t.id.starts_with("import-inactive-") {
            blocked_archive_fingerprints = t.source_fingerprints.len();
        }
        s.packages
            .write_text(
                "game-a",
                PLUGIN,
                &format!("memory-tombstones/{}", entry.file_name().to_string_lossy()),
                &text,
                None,
                false,
            )
            .unwrap();
    }
    assert!(blocked_archive_fingerprints > 0);
    assert!(s
        .call("memory_get", "game-a", json!({"id":"same-id"}), None, false)
        .await
        .is_ok());
    assert!(s.call("memory_create","game-a",json!({"operation_id":"old-source-paraphrase","title":"改名","body":"用新措辞复建","sources":[source]}),None,false).await.unwrap_err().to_string().contains("suppressed"));
}

#[tokio::test]
async fn retrieval_fixture_reports_keyword_vector_and_hybrid_hit_counts_and_time() {
    let (s, _temp) = store();
    create(&s, "game-a", "bag", "背包整理", "背包内筛选材料", false).await;
    create(&s, "game-a", "mail", "邮箱奖励", "邮件领取奖励", false).await;
    let (connection, task) = local_embedding().await;
    s.call(
        "memory_index_rebuild",
        "game-a",
        json!({}),
        Some(&connection),
        false,
    )
    .await
    .unwrap();
    let questions = [("背包", "bag"), ("物资分类", "bag"), ("邮箱", "mail")];
    for mode in ["keyword", "vector", "hybrid"] {
        let start = std::time::Instant::now();
        let mut hits = 0;
        for (query, expected) in questions {
            let result = s
                .call(
                    "memory_search",
                    "game-a",
                    json!({"query":query,"game_version":"1.0","mode":mode}),
                    if mode == "keyword" {
                        None
                    } else {
                        Some(&connection)
                    },
                    false,
                )
                .await
                .unwrap();
            if result["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == expected)
            {
                hits += 1;
            }
        }
        eprintln!("memory retrieval fixture: mode={mode}, questions={}, hits={hits}, elapsed_ms={:.3}; deterministic local mock embeddings, pipeline regression only, not real model recall",questions.len(),start.elapsed().as_secs_f64()*1000.0);
        if mode != "keyword" {
            assert_eq!(hits, questions.len());
        }
    }
    task.abort();
}

#[tokio::test]
async fn embedding_byte_budget_rechunks_fts_and_bounds_all_context_and_prefixes() {
    let (s, _temp) = store();
    let title = "超长标题".repeat(20);
    let conditions = "游戏版本和适用条件".repeat(30);
    let body = format!(
        "# {}\n\n{}\n\n成功标记",
        "很长的章节名称".repeat(30),
        "背包筛选材料并检查操作结果。\n".repeat(50)
    );
    s.call("memory_create","game-a",json!({"id":"long-budget","operation_id":"long-budget-create","title":title,"applicability":conditions,"body":body,"validation":"verified"}),None,false).await.unwrap();
    let configured = |bytes, prefix: &str| {
        ServiceConnection::new(
            EmbeddingConfig {
                enabled: true,
                provider: "local-test".into(),
                base_url: "http://127.0.0.1:1".into(),
                model: "mock-2d".into(),
                max_input_bytes: bytes,
                document_prefix: prefix.into(),
                ..Default::default()
            },
            String::new(),
            SearchConfig::default(),
            String::new(),
            WebReadConfig::default(),
        )
        .unwrap()
    };
    let larger = configured(480, "document: ");
    let large = s
        .call(
            "memory_index_status",
            "game-a",
            json!({}),
            Some(&larger),
            false,
        )
        .await
        .unwrap();
    let smaller = configured(128, "document: ");
    let small = s
        .call(
            "memory_index_status",
            "game-a",
            json!({}),
            Some(&smaller),
            false,
        )
        .await
        .unwrap();
    assert!(small["total_chunks"].as_u64().unwrap() > large["total_chunks"].as_u64().unwrap());
    assert_eq!(small["chunk_bytes"], 118);
    let db = s.open_index("game-a").unwrap();
    let maximum: usize = db
        .query_row(
            "SELECT max(length(CAST(embedding_text AS BLOB))) FROM chunks",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(maximum <= 118);
    let omitted: usize = db
        .query_row(
            "SELECT count(*) FROM chunks WHERE embedding_text LIKE '%省略%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(omitted > 0);
    let (count, fts): (usize, usize) = db
        .query_row(
            "SELECT (SELECT count(*) FROM chunks),(SELECT count(*) FROM chunk_fts)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, fts);
    drop(db);
    let full = s
        .call(
            "memory_get",
            "game-a",
            json!({"id":"long-budget"}),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(full["memory"]["body"], body);
    assert_eq!(full["memory"]["title"], title);
    let tiny = configured(128, &"p".repeat(127));
    let degraded = s
        .call(
            "memory_index_rebuild",
            "game-a",
            json!({}),
            Some(&tiny),
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        degraded["degraded_reason"]["code"],
        "embedding_context_budget_too_small"
    );
    assert_eq!(degraded["embedding_usage"], json!([]));
    let keyword = s
        .call(
            "memory_search",
            "game-a",
            json!({"query":"成功标记","mode":"keyword"}),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(keyword["items"][0]["id"], "long-budget");
}

#[tokio::test]
async fn canonical_source_references_match_jobs_and_suppress_derived_recreation() {
    let (s, _temp) = store();
    let imported=s.call("memory_import","game-a",json!({"operation_id":"human-guide","filename":"user.md","text":"# 用户定义\n\n默认先整理背包，再领取邮箱。"}),None,false).await.unwrap();
    let refs = s
        .source_references("game-a", imported["source_id"].as_str().unwrap(), 1)
        .unwrap();
    let claimed = s
        .claim_import_chunk(
            "game-a",
            imported["job_id"].as_str().unwrap(),
            "canonical-claim",
        )
        .unwrap()
        .unwrap();
    assert!(refs.contains(&claimed["source_reference"]));
    let saved=s.call("memory_create","game-a",json!({"id":"user-definition","operation_id":"definition-create","title":"用户默认步骤","body":"先整理背包，再领取邮箱","kind":"definition","sources":refs}),None,true).await.unwrap();
    s.call("memory_delete","game-a",json!({"id":"user-definition","expected_version":saved["version"],"operation_id":"definition-delete","reason":"用户删除此定义"}),None,true).await.unwrap();
    assert!(s.call("memory_create","game-a",json!({"operation_id":"derived-paraphrase","title":"另一种名称","body":"复述旧原稿中的流程","sources":[claimed["source_reference"].clone()]}),None,false).await.unwrap_err().to_string().contains("suppressed"));
}

fn copy_fixture_directory(source: &std::path::Path, target: &std::path::Path) {
    std::fs::create_dir_all(target).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination = target.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_fixture_directory(&entry.path(), &destination);
        } else {
            std::fs::copy(entry.path(), destination).unwrap();
        }
    }
}

#[tokio::test]
async fn archive_source_branch_collision_preserves_both_raw_guides_and_reference_mapping() {
    use crate::resources::ResourceHandler;
    let (s, temp) = store();
    let mut source_id = String::new();
    for (pkg, text) in [
        ("game-a", "本机修订：先整理背包"),
        ("game-b", "传入修订：先领取邮箱"),
    ] {
        let source=s.call("memory_import",pkg,json!({"operation_id":format!("source-{pkg}"),"filename":"共同攻略.md","text":"共同原稿：领取奖励"}),None,false).await.unwrap();
        source_id = source["source_id"].as_str().unwrap().into();
        let current = s
            .call(
                "memory_source_get",
                pkg,
                json!({"id":source_id}),
                None,
                false,
            )
            .await
            .unwrap();
        s.call("memory_source_update",pkg,json!({"id":source_id,"expected_version":current["version"],"operation_id":format!("source-update-{pkg}"),"reason":"两处独立修改","text":text}),None,true).await.unwrap();
    }
    let refs = s.source_references("game-b", &source_id, 2).unwrap();
    s.call("memory_create","game-b",json!({"id":"incoming-branch","operation_id":"incoming-branch-create","title":"传入流程","body":"传入修订先领取邮箱","sources":refs,"validation":"verified"}),None,false).await.unwrap();
    let incoming = temp.path().join("incoming-source-branches");
    copy_fixture_directory(&s.packages.plugin_dir("game-b", PLUGIN).unwrap(), &incoming);
    s.prepare_package_replace(
        "game-a",
        Some(&s.packages.plugin_dir("game-a", PLUGIN).unwrap()),
        &incoming,
    )
    .unwrap();
    let mapped = std::fs::read_dir(incoming.join("memory-sources"))
        .unwrap()
        .filter_map(|entry| {
            let path = entry.unwrap().path();
            let source: Source = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            source
                .id
                .starts_with("archive-source-")
                .then_some(source.id)
        })
        .next()
        .unwrap();
    let original = std::fs::read_to_string(
        incoming
            .join("memory-source-originals")
            .join(format!("{mapped}.json")),
    )
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&original).unwrap()["id"],
        source_id
    );
    copy_fixture_directory(&incoming, &s.packages.plugin_dir("game-a", PLUGIN).unwrap());
    let local = s
        .call(
            "memory_source_get",
            "game-a",
            json!({"id":source_id,"revision":2}),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(local["source"]["text"], "本机修订：先整理背包");
    let foreign = s
        .call(
            "memory_source_get",
            "game-a",
            json!({"id":mapped,"revision":2}),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(foreign["source"]["text"], "传入修订：先领取邮箱");
    assert_eq!(
        foreign["source"]["import_provenance"]["original_id"],
        source_id
    );
    let historical = s
        .call(
            "memory_source_get",
            "game-a",
            json!({"id":mapped,"revision":1}),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(historical["source"]["text"], "共同原稿：领取奖励");
    let jobs = s.pending_imports("game-a").unwrap();
    let job = jobs.iter().find(|job| job.source_id == mapped).unwrap();
    let claim = s
        .claim_import_chunk("game-a", &job.id, "source-branch-claim")
        .unwrap()
        .unwrap();
    assert_eq!(claim["source_reference"]["id"], mapped);
    assert_eq!(claim["source_reference"]["revision"], 2);
    let guide = std::fs::read_dir(incoming.join("memory-source-origins"))
        .unwrap()
        .filter_map(|entry| {
            let provenance: Value =
                serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap();
            (provenance["original_kind"] == "memory").then_some(provenance)
        })
        .next()
        .unwrap();
    assert_eq!(guide["source_id_map"][&source_id], mapped);
    assert_eq!(guide["mapped_sources"][0]["id"], mapped);
    let original_guide: Value = serde_json::from_slice(
        &std::fs::read(incoming.join(guide["original_resource"].as_str().unwrap())).unwrap(),
    )
    .unwrap();
    assert_eq!(original_guide["sources"][0]["id"], source_id);
    let mapped_guide = s
        .call(
            "memory_source_get",
            "game-a",
            json!({"id":guide["mapped_id"]}),
            None,
            false,
        )
        .await
        .unwrap();
    assert!(mapped_guide["source"]["text"]
        .as_str()
        .unwrap()
        .contains(&mapped));
}

#[tokio::test]
async fn namespaced_archive_source_keeps_existing_deletion_suppression() {
    use crate::resources::ResourceHandler;
    let (s, temp) = store();
    let mut source_id = String::new();
    for pkg in ["game-a", "game-b"] {
        let source=s.call("memory_import",pkg,json!({"operation_id":format!("deleted-source-{pkg}"),"filename":"定义.txt","title":format!("定义-{pkg}"),"text":"用户定义：必须先整理背包"}),None,false).await.unwrap();
        source_id = source["source_id"].as_str().unwrap().into();
    }
    let refs = s.source_references("game-a", &source_id, 1).unwrap();
    let record=s.call("memory_create","game-a",json!({"id":"deleted-definition","operation_id":"deleted-definition-create","title":"默认步骤","body":"必须先整理背包","sources":refs}),None,true).await.unwrap();
    s.call("memory_delete","game-a",json!({"id":"deleted-definition","operation_id":"deleted-definition-delete","expected_version":record["version"],"permanent":true}),None,true).await.unwrap();
    let incoming = temp.path().join("incoming-deleted-source");
    copy_fixture_directory(&s.packages.plugin_dir("game-b", PLUGIN).unwrap(), &incoming);
    s.prepare_package_replace(
        "game-a",
        Some(&s.packages.plugin_dir("game-a", PLUGIN).unwrap()),
        &incoming,
    )
    .unwrap();
    copy_fixture_directory(&incoming, &s.packages.plugin_dir("game-a", PLUGIN).unwrap());
    let job = s
        .pending_imports("game-a")
        .unwrap()
        .into_iter()
        .find(|job| job.source_id.starts_with("archive-source-"))
        .unwrap();
    let claim = s
        .claim_import_chunk("game-a", &job.id, "deleted-copy-claim")
        .unwrap()
        .unwrap();
    assert!(s.call("memory_create","game-a",json!({"operation_id":"deleted-copy-recreate","title":"重新命名","body":"用另一段文字复述流程","sources":[claim["source_reference"].clone()]}),None,false).await.unwrap_err().to_string().contains("suppressed"));
}
