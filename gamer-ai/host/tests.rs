use super::*;
use crate::core::{AndroidPackageName, AppContext, AppPackageId, DeviceId, RunId};
use crate::timer_core::Clock;
use provider::{Decision, ModelInput, Profile, Provider, Reply};
use runtime::{Backend, Capture, Execution, Search};
use std::{collections::VecDeque, sync::atomic::AtomicI64};
struct MockClock(AtomicI64);
impl crate::timer_core::Clock for MockClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp_millis(self.0.load(Ordering::SeqCst)).unwrap()
    }
}
#[derive(Default)]
struct MockBackend {
    inputs: Mutex<Vec<Value>>,
    epoch: Mutex<String>,
    keys: Mutex<Vec<String>>,
}
#[async_trait]
impl Backend for MockBackend {
    async fn capture(&self, _: &AppContext) -> Result<Capture> {
        let state = self.inputs.lock().len() as u8;
        let mut bytes = Vec::new();
        image::RgbImage::from_pixel(100, 200, image::Rgb([state, 10, 20])).write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )?;
        Ok(Capture {
            original: bytes.clone(),
            model: bytes,
            size: [100, 200],
            model_size: [100, 200],
            epoch: self.epoch.lock().clone(),
        })
    }
    async fn inject(&self, _: &AppContext, action: &Value, _: [u32; 2]) -> Result<()> {
        self.inputs.lock().push(action.clone());
        Ok(())
    }
    async fn release(&self, _: &AppContext, keys: &[String]) -> Result<()> {
        self.keys.lock().extend_from_slice(keys);
        Ok(())
    }
    async fn automation(
        &self,
        context: &RunContext,
        _: &str,
        _: serde_json::Map<String, Value>,
        _: Arc<AtomicBool>,
        scope: crate::core::side_effect::Scope,
    ) -> Result<()> {
        scope
            .policy
            .before(&context.app, json!({"kind":"tap","x":50,"y":100}))
            .await?;
        crate::core::input_ownership::scope(
            scope.input,
            self.inject(&context.app, &json!({"kind":"tap"}), [100, 200]),
        )
        .await?;
        scope.policy.after(&context.app, true).await
    }
}
struct MockProvider {
    replies: Mutex<VecDeque<Decision>>,
    delay: bool,
}
#[async_trait]
impl Provider for MockProvider {
    async fn infer(&self, _: &Profile, input: ModelInput) -> Result<Reply> {
        if self.delay {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
        let prompt: Value = serde_json::from_str(&input.prompt).unwrap_or(Value::Null);
        let mut decision = self
            .replies
            .lock()
            .pop_front()
            .context("mock replies exhausted")?;
        if decision.arguments["observation_id"] == "CURRENT" {
            decision.arguments["observation_id"] = prompt["observation_id"].clone();
            if decision.arguments["observation_id"].is_null() {
                decision.arguments["observation_id"] = prompt["current_observation_id"].clone();
            }
        }
        if let Some(goals) = decision.arguments["subgoals"].as_array_mut() {
            for goal in goals {
                if goal["evidence"] == "CURRENT" {
                    goal["evidence"] = prompt["observation_id"].clone();
                }
            }
        }
        if decision.arguments["validated_guides"] == "CURRENT_GUIDES" {
            decision.arguments["validated_guides"] = json!(prompt["guide_candidates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|g| g["path"].clone())
                .collect::<Vec<_>>());
        }
        Ok(Reply {
            decision,
            usage: json!({"total_tokens":100}),
            cost: Some(10),
            source: "estimated".into(),
            sources: Value::Null,
        })
    }
}
struct MockSearch;
#[async_trait]
impl Search for MockSearch {
    async fn query(&self, _: &runtime::SearchSettings, query: &str) -> Result<Value> {
        Ok(
            json!({"results":[{"url":"https://example.test/guide","title":query,"content":"untrusted guide"}]}),
        )
    }
}
fn profile() -> Profile {
    Profile {
        id: "mock".into(),
        protocol: "responses".into(),
        endpoint: "https://mock.invalid/v1".into(),
        model: "mock-multimodal".into(),
        key: String::new(),
        timeout_secs: 10,
        max_output_tokens: 500,
        price_version: "test-price-1".into(),
        input_micros_per_million: 1,
        output_micros_per_million: 1,
        cached_micros_per_million: None,
        cache_creation_micros_per_million: None,
        vision: "available".into(),
        native_search: "untested".into(),
        native_search_enabled: false,
        native_search_reserve_micros: 0,
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    _lease: crate::core::input_ownership::Lease,
    e: Execution,
    backend: Arc<MockBackend>,
    clock: Arc<MockClock>,
}
fn fixture(replies: Vec<Decision>, delay: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let cfg = crate::config::Config {
        data_dir: dir.path().into(),
        ..Default::default()
    };
    let packages = Arc::new(crate::resources::PackageStore::open(&cfg).unwrap());
    packages.ensure_default_package().unwrap();
    let repository =
        Arc::new(store::Repository::open(&dir.path().join("extension-data/gamer-ai")).unwrap());
    let backend = Arc::new(MockBackend::default());
    *backend.epoch.lock() = "epoch-1".into();
    let clock = Arc::new(MockClock(AtomicI64::new(1_800_000_000_000)));
    let runtime = Arc::new(Runtime {
        repository,
        packages,
        provider: Arc::new(MockProvider {
            replies: Mutex::new(replies.into()),
            delay,
        }),
        backend: backend.clone(),
        search: Arc::new(MockSearch),
        clock: clock.clone(),
    });
    let context = RunContext::new(
        RunId::generate(),
        AppContext::new(
            DeviceId::new(store::id()).unwrap(),
            AndroidPackageName::new("com.sample.game").unwrap(),
            Some(AppPackageId::new("default").unwrap()),
        ),
    );
    let p = profile();
    let settings = Settings {
        profiles: vec![p.clone()],
        ..Default::default()
    };
    let session = runtime
        .begin(&context, "完成日常", "plan-1", &settings, &p, None)
        .unwrap();
    // Ordinary tool fixtures model a session whose research/plan is already complete.
    // Planning tests explicitly clear this field to exercise the admission gate.
    runtime
        .repository
        .transaction(|data| {
            data.sessions.get_mut(&session).unwrap().execution_plan =
                json!({"steps":[{"description":"mock 计划","expected":"新画面"}]});
            Ok(())
        })
        .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let lease =
        crate::core::input_ownership::acquire(context.device_id().as_str(), stop.clone()).unwrap();
    let e = Execution {
        runtime,
        context,
        session,
        profile: p,
        settings,
        stop,
        permit: lease.0.clone(),
        observation: Arc::new(Mutex::new(None)),
        keys: Arc::new(Mutex::new(vec![])),
    };
    Fixture {
        _dir: dir,
        _lease: lease,
        e,
        backend,
        clock,
    }
}
fn decision(tool: &str, args: Value) -> Decision {
    Decision {
        tool: tool.into(),
        arguments: args,
        summary: "mock 决策".into(),
    }
}
fn consumption(category: &str) -> Value {
    json!({"category":category,"resource":"ticket","quantity":1,"purpose":"challenge","evidence":"按钮显示消耗一张门票"})
}
fn act(id: &str, observation: &str, category: &str) -> Value {
    json!({"operation_id":id,"observation_id":observation,"action":{"kind":"tap","position":[0.5,0.5]},"consumption":consumption(category),"expected":"进入挑战"})
}
#[tokio::test]
async fn multiround_completion_requires_another_image_and_verification_request() {
    let f = fixture(
        vec![
            decision("act", act("op-1", "CURRENT", "regenerative_resource")),
            decision(
                "finish",
                json!({"subgoals":[{"name":"日常","state":"completed","evidence":"CURRENT","result":"日常列表全部完成"}],"summary":"完成"}),
            ),
            decision(
                "finish",
                json!({"verified":true,"observation_id":"CURRENT","account_consistent":true,"result":"可见全部完成"}),
            ),
        ],
        false,
    );
    f.e.run().await.unwrap();
    assert_eq!(f.backend.inputs.lock().len(), 1);
    let d = f.e.runtime.repository.data.lock();
    assert_eq!(d.sessions[&f.e.session].state, "completed");
    assert_eq!(d.requests.len(), 3);
    assert!(d.sessions[&f.e.session]
        .events
        .iter()
        .any(|e| e.kind == "completion_verified"));
}
#[tokio::test]
async fn items_block_but_independent_free_actions_continue() {
    let f = fixture(vec![], false);
    let o = f.e.observe().await.unwrap();
    assert_eq!(
        tools::execute(&f.e, "act", act("item", &o.id, "item"))
            .await
            .unwrap()["blocked"],
        true
    );
    assert!(f.backend.inputs.lock().is_empty());
    let value = tools::execute(&f.e, "act", act("free", &o.id, "navigation"))
        .await
        .unwrap();
    assert_eq!(value["status"], "injected");
    assert_eq!(f.e.runtime.repository.data.lock().approvals.len(), 1);
}
#[tokio::test]
async fn duplicate_operation_never_reinjects_and_changed_payload_is_rejected() {
    let f = fixture(vec![], false);
    let o = f.e.observe().await.unwrap();
    let args = act("op", &o.id, "navigation");
    tools::execute(&f.e, "act", args.clone()).await.unwrap();
    tools::execute(&f.e, "act", args.clone()).await.unwrap();
    assert_eq!(f.backend.inputs.lock().len(), 1);
    let mut changed = args;
    changed["action"]["position"] = json!([0.1, 0.1]);
    assert!(tools::execute(&f.e, "act", changed).await.is_err());
}
#[tokio::test]
async fn unknown_consumption_cannot_be_retried_under_another_operation_id() {
    let f = fixture(vec![], false);
    let o = f.e.observe().await.unwrap();
    tools::execute(&f.e, "act", act("first", &o.id, "item"))
        .await
        .unwrap();
    let approval =
        f.e.runtime
            .repository
            .data
            .lock()
            .approvals
            .values()
            .next()
            .unwrap()
            .id
            .clone();
    f.e.runtime
        .repository
        .transaction(|data| {
            data.sessions.get_mut(&f.e.session).unwrap().state = "partial".into();
            Ok(())
        })
        .unwrap();
    mock_service(&f).dispatch("approvals.resolve", json!({"approval_id":approval,"decision":"approve","scope":"session","limit":5,"expires_at":f.clock.now().timestamp()+1000})).await.unwrap();
    tools::execute(&f.e, "act", act("first", &o.id, "item"))
        .await
        .unwrap();
    f.e.runtime
        .repository
        .transaction(|data| {
            data.sessions
                .get_mut(&f.e.session)
                .unwrap()
                .operations
                .get_mut("first")
                .unwrap()["status"] = json!("outcome_unknown");
            data.spends[0].status = "outcome_unknown".into();
            Ok(())
        })
        .unwrap();
    let fresh = f.e.observe().await.unwrap();
    let error = tools::execute(&f.e, "act", act("second", &fresh.id, "item"))
        .await
        .unwrap_err();
    assert!(error.to_string().starts_with("outcome_unknown"));
    assert_eq!(f.backend.inputs.lock().len(), 1);
    tools::execute(&f.e, "act", act("free", &fresh.id, "navigation"))
        .await
        .unwrap();
    assert_eq!(f.backend.inputs.lock().len(), 2);
    assert_eq!(f.e.runtime.repository.data.lock().spends.len(), 1);
}
#[tokio::test]
async fn pending_approvals_keep_different_quantities_distinct() {
    let f = fixture(vec![], false);
    let o = f.e.observe().await.unwrap();
    let mut second = act("two", &o.id, "item");
    second["consumption"]["quantity"] = json!(2);
    for args in [act("one", &o.id, "item"), second] {
        assert_eq!(
            tools::execute(&f.e, "act", args).await.unwrap()["blocked"],
            true
        );
    }
    assert_eq!(f.e.runtime.repository.data.lock().approvals.len(), 2);
    assert!(f.backend.inputs.lock().is_empty());
}
#[test]
fn rotating_credentials_changes_private_profile_identity_without_exposing_key() {
    let mut p = profile();
    p.key = "first-private-key".into();
    let first = p.version();
    p.key = "second-private-key".into();
    assert_ne!(first, p.version());
    assert!(p.public().get("key").is_none());
}
#[tokio::test]
async fn stale_and_reconnected_observations_cannot_act() {
    let f = fixture(vec![], false);
    let o = f.e.observe().await.unwrap();
    f.clock.0.fetch_add(16_000, Ordering::SeqCst);
    assert!(tools::execute(&f.e, "act", act("op", &o.id, "navigation"))
        .await
        .is_err());
    f.clock.0.fetch_sub(16_000, Ordering::SeqCst);
    *f.backend.epoch.lock() = "epoch-2".into();
    assert!(tools::execute(&f.e, "act", act("op", &o.id, "navigation"))
        .await
        .is_err());
    assert!(f.backend.inputs.lock().is_empty());
}
#[tokio::test]
async fn cancelled_model_response_keeps_reservation_and_cannot_touch() {
    let f = fixture(
        vec![decision("act", act("op", "CURRENT", "navigation"))],
        true,
    );
    let o = f.e.observe().await.unwrap();
    let stop = f.e.stop.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        stop.store(true, Ordering::Release);
    });
    assert!(f.e.infer("{}".into(), &o, "decision").await.is_err());
    assert!(f.backend.inputs.lock().is_empty());
    let d = f.e.runtime.repository.data.lock();
    let r = d.requests.values().next().unwrap();
    assert_eq!(r.status, "pending_reconciliation");
    assert!(r.actual.is_none());
    assert!(r.reserved > 0);
}
#[tokio::test]
async fn budget_and_unknown_cost_survive_restart_and_resume() {
    let f = fixture(vec![], false);
    let repo = &f.e.runtime.repository;
    let now = f.clock.now().timestamp();
    let r = store::RequestRecord {
        id: "request-1".into(),
        session: f.e.session.clone(),
        run_id: f.e.context.run_id.to_string(),
        profile: "mock".into(),
        model: "mock".into(),
        price_version: "v1".into(),
        kind: "decision".into(),
        day: "2027-01-15".into(),
        reserved: 500_000,
        actual: None,
        source: "unknown".into(),
        status: "reserved".into(),
        usage: Value::Null,
        at: now,
        duration_ms: 0,
    };
    repo.reserve(r.clone(), now).unwrap();
    repo.transaction(|d| {
        d.sessions.get_mut(&f.e.session).unwrap().state = "partial".into();
        Ok(())
    })
    .unwrap();
    let reopened = store::Repository::open(&repo.root).unwrap();
    assert_eq!(
        reopened.data.lock().requests["request-1"].status,
        "pending_reconciliation"
    );
    let context = RunContext::new(RunId::generate(), f.e.context.app.clone());
    f.e.runtime
        .begin(
            &context,
            "完成日常",
            "plan-1",
            &f.e.settings,
            &f.e.profile,
            Some(&f.e.session),
        )
        .unwrap();
    let mut r2 = r;
    r2.id = "request-2".into();
    r2.reserved = 600_000;
    assert!(repo.reserve(r2, now).is_err());
    assert_eq!(repo.data.lock().sessions[&f.e.session].rounds, 1);
}
#[tokio::test]
async fn yaml_each_side_effect_uses_same_model_ledger_and_gate() {
    let f = fixture(
        vec![decision(
            "act",
            json!({"safe_coordinates":true,"consumption":consumption("navigation")}),
        )],
        false,
    );
    tools::execute(
        &f.e,
        "call_automation",
        json!({"entrypoint":"default/test.yaml","args":{}}),
    )
    .await
    .unwrap();
    assert_eq!(f.backend.inputs.lock().len(), 1);
    let d = f.e.runtime.repository.data.lock();
    assert_eq!(d.requests.values().next().unwrap().kind, "yaml_gate");
    assert!(d.sessions[&f.e.session]
        .events
        .iter()
        .any(|e| e.kind == "yaml_side_effect"));
}
#[tokio::test]
async fn yaml_item_gate_blocks_before_injection() {
    let f = fixture(
        vec![decision(
            "act",
            json!({"safe_coordinates":true,"consumption":consumption("item")}),
        )],
        false,
    );
    assert!(tools::execute(
        &f.e,
        "call_automation",
        json!({"entrypoint":"default/test.yaml","args":{}})
    )
    .await
    .is_err());
    assert!(f.backend.inputs.lock().is_empty());
    assert_eq!(f.e.runtime.repository.data.lock().approvals.len(), 1);
}
#[tokio::test]
async fn one_operation_yaml_approval_survives_fresh_observation_on_resume() {
    let f = fixture(
        (0..2)
            .map(|_| {
                decision(
                    "act",
                    json!({"safe_coordinates":true,"consumption":consumption("item")}),
                )
            })
            .collect(),
        false,
    );
    let args = json!({"entrypoint":"default/test.yaml","args":{}});
    assert!(tools::execute(&f.e, "call_automation", args.clone())
        .await
        .is_err());
    let approval =
        f.e.runtime
            .repository
            .data
            .lock()
            .approvals
            .values()
            .next()
            .unwrap()
            .clone();
    f.e.runtime
        .repository
        .transaction(|data| {
            data.sessions.get_mut(&f.e.session).unwrap().state = "partial".into();
            Ok(())
        })
        .unwrap();
    let service = mock_service(&f);
    service.dispatch("approvals.resolve",json!({"approval_id":approval.id,"decision":"approve","scope":"operation","limit":1,"expires_at":f.clock.now().timestamp()+1000})).await.unwrap();
    tools::execute(&f.e, "call_automation", args).await.unwrap();
    assert_eq!(f.backend.inputs.lock().len(), 1);
    assert_eq!(
        f.e.runtime.repository.data.lock().spends[0].operation,
        approval.operation
    );
    assert_eq!(f.e.runtime.repository.data.lock().requests.len(), 2);
}
#[tokio::test]
async fn guide_trust_is_local_and_lost_on_copy_override_rebuild_or_missing_evidence() {
    let f = fixture(vec![], false);
    let o = f.e.observe().await.unwrap();
    let package = "default";
    let path = "guides/test.json";
    let bytes = json!({"app":"com.sample.game","content":"guide","verified":true}).to_string();
    f.e.runtime
        .packages
        .write_text(package, ID, path, &bytes, None, false)
        .unwrap();
    let result = tools::execute(&f.e, "memory.read", json!({"path":path}))
        .await
        .unwrap();
    assert_eq!(result["effective_status"], "candidate");
    let generation = f.e.runtime.packages.instance_generation(package).unwrap();
    f.e.runtime
        .repository
        .transaction(|d| {
            d.verifications.push(store::Verification {
                resource_instance: {
                    let m = std::fs::metadata(
                        f.e.runtime
                            .packages
                            .resource_path(package, ID, path)
                            .unwrap(),
                    )
                    .unwrap();
                    format!("{:?}:{:?}", m.created().ok(), m.modified().unwrap())
                },
                package: package.into(),
                generation,
                path: path.into(),
                hash: store::hash(bytes.as_bytes()),
                session: f.e.session.clone(),
                observation: o.id.clone(),
                conditions: "test".into(),
            });
            Ok(())
        })
        .unwrap();
    assert_eq!(
        tools::execute(&f.e, "memory.read", json!({"path":path}))
            .await
            .unwrap()["effective_status"],
        "verified"
    );
    let evidence =
        f.e.runtime
            .repository
            .root
            .join("evidence")
            .join(&f.e.session)
            .join(format!("{}.png", o.id));
    let image = std::fs::read(&evidence).unwrap();
    std::fs::remove_file(&evidence).unwrap();
    assert_eq!(
        tools::read_local_memory(&f.e.runtime, package, path).unwrap()["effective_status"],
        "candidate"
    );
    std::fs::write(&evidence, image).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    f.e.runtime
        .packages
        .delete_resource(package, ID, path)
        .unwrap();
    f.e.runtime
        .packages
        .write_text(package, ID, path, &bytes, None, false)
        .unwrap();
    assert_eq!(
        tools::read_local_memory(&f.e.runtime, package, path).unwrap()["effective_status"],
        "candidate"
    );
    let original = f.e.runtime.packages.instance_generation(package).unwrap();
    f.e.runtime
        .packages
        .duplicate_package(package, "copy")
        .unwrap();
    assert_ne!(
        f.e.runtime.packages.instance_generation("copy").unwrap(),
        original
    );
    assert_eq!(
        tools::read_local_memory(&f.e.runtime, "copy", path).unwrap()["effective_status"],
        "candidate"
    );
    f.e.runtime.packages.invalidate_instance(package).unwrap();
    assert!(f.e.check().is_err());
    assert_eq!(
        tools::read_local_memory(&f.e.runtime, package, path).unwrap()["effective_status"],
        "candidate"
    );
    let generation = f.e.runtime.packages.instance_generation(package).unwrap();
    let manifest = std::fs::read(
        f.e.runtime
            .packages
            .package_dir(package)
            .unwrap()
            .join("package.toml"),
    )
    .unwrap();
    f.e.runtime.packages.delete_package(package).unwrap();
    let folder = f.e.runtime.packages.package_dir(package).unwrap();
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("package.toml"), manifest).unwrap();
    f.e.runtime
        .packages
        .write_text(package, ID, path, &bytes, None, false)
        .unwrap();
    assert_ne!(
        f.e.runtime.packages.instance_generation(package).unwrap(),
        generation
    );
    assert_eq!(f.e.runtime.repository.data.lock().requests.len(), 0);
}
#[test]
fn corrupt_ledger_never_silently_resets() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("state.json"), b"broken").unwrap();
    assert!(store::Repository::open(dir.path()).is_err());
}
#[test]
fn adapters_send_images_and_normalize_usage_without_double_counting_cache() {
    let mut p = profile();
    let input = ModelInput {
        prompt: "test".into(),
        image: vec![1, 2, 3],
    };
    for protocol in ["responses", "chat", "ollama", "claude", "gemini"] {
        p.protocol = protocol.into();
        let (_, body) = provider::request(&p, &input).unwrap();
        assert!(body.to_string().contains(if protocol == "ollama" {
            "images"
        } else if protocol == "gemini" {
            "inlineData"
        } else {
            "image"
        }));
    }
    p.protocol = "responses".into();
    p.input_micros_per_million = 1_000_000;
    p.output_micros_per_million = 2_000_000;
    p.cached_micros_per_million = Some(500_000);
    let reply=provider::normalize(&p,json!({"output":[{"type":"function_call","name":"gamer_tool","arguments":serde_json::to_string(&decision("observe",json!({}))).unwrap()}],"usage":{"input_tokens":100,"output_tokens":10,"input_tokens_details":{"cached_tokens":40},"output_tokens_details":{"reasoning_tokens":7}}})).unwrap();
    assert_eq!(reply.cost, Some(100));
}
#[test]
fn model_and_mcp_tools_never_include_management_or_self_approval() {
    assert!(!tools::NAMES.contains(&"approvals.resolve"));
    assert!(!tools::NAMES.contains(&"settings.save"));
    assert!(!mcp::CONTROL.contains(&"/api/devices/control"));
    assert!(serde_json::from_value::<Goal>(json!({"goal":"test","prepared_id":"forged"})).is_err());
}
#[test]
fn cache_write_pricing_and_overflow_keep_unknown_costs_out_of_zero() {
    let mut p = profile();
    p.protocol = "claude".into();
    p.input_micros_per_million = 1_000_000;
    p.output_micros_per_million = 1_000_000;
    p.cached_micros_per_million = Some(500_000);
    let response = json!({"content":[{"type":"tool_use","name":"gamer_tool","input":decision("observe",json!({}))}],"usage":{"input_tokens":60,"cache_read_input_tokens":40,"cache_creation_input_tokens":20,"output_tokens":10}});
    assert_eq!(
        provider::normalize(&p, response.clone()).unwrap().cost,
        None
    );
    p.cache_creation_micros_per_million = Some(2_000_000);
    let r = provider::normalize(&p, response).unwrap();
    assert_eq!(r.cost, Some(130));
    assert_eq!(r.usage["input_tokens"], 120);
    assert_eq!(r.usage["total_tokens"], 130);
    p.input_micros_per_million = u64::MAX;
    p.output_micros_per_million = u64::MAX;
    p.max_output_tokens = u64::MAX;
    assert_eq!(provider::reserve_price(&p, 64000), u64::MAX);
}
#[tokio::test]
async fn unreadable_imported_guide_does_not_block_independent_execution() {
    let f = fixture(vec![], false);
    f.e.runtime
        .packages
        .write_text(
            "default",
            ID,
            "guides/broken.json",
            "invalid JSON",
            None,
            false,
        )
        .unwrap();
    let result = tools::memory_search(&f.e, "", 5).unwrap();
    assert_eq!(result["guides"].as_array().unwrap().len(), 0);
    tools::memory_search(&f.e, "", 5).unwrap();
    assert_eq!(
        f.e.runtime.repository.data.lock().sessions[&f.e.session]
            .events
            .iter()
            .filter(|event| event.kind == "memory_unreadable")
            .count(),
        1
    );
}

#[tokio::test]
async fn mcp_returns_image_and_cannot_declare_a_ticket_as_navigation() {
    let f = fixture(
        vec![decision(
            "act",
            json!({"safe_coordinates":true,"consumption":consumption("item")}),
        )],
        false,
    );
    let image = tools::execute_external(&f.e, "observe", json!({}))
        .await
        .unwrap();
    assert_eq!(image["_mcp_image"]["mimeType"], "image/png");
    assert!(!image["_mcp_image"]["data"].as_str().unwrap().is_empty());
    let outcome = tools::execute_external(
        &f.e,
        "act",
        act(
            "external-1",
            image["observation_id"].as_str().unwrap(),
            "navigation",
        ),
    )
    .await
    .unwrap();
    assert_eq!(outcome["blocked"], true);
    assert!(f.backend.inputs.lock().is_empty());
    assert_eq!(f.e.runtime.repository.data.lock().requests.len(), 1);
}
#[tokio::test]
async fn authorized_allowance_is_atomic_and_persists_across_runs() {
    let f = fixture(vec![], false);
    let repo = &f.e.runtime.repository;
    let o = f.e.observe().await.unwrap();
    let now = f.clock.now().timestamp();
    let c: store::Consumption = serde_json::from_value(consumption("item")).unwrap();
    assert!(!repo
        .gate(&f.e.session, "pending", &o.id, c.clone(), now)
        .unwrap());
    repo.transaction(|d| {
        d.rules.insert(
            "rule".into(),
            store::Rule {
                id: "rule".into(),
                version: "v1".into(),
                app: "com.sample.game".into(),
                account: None,
                purpose: c.purpose.clone(),
                resource: c.resource.clone(),
                category: c.category.clone(),
                scope: "session".into(),
                session: Some(f.e.session.clone()),
                operation: None,
                cycle: None,
                limit: 2,
                expires_at: now + 3600,
                revoked: false,
            },
        );
        Ok(())
    })
    .unwrap();
    assert!(repo
        .gate(&f.e.session, "first", &o.id, c.clone(), now)
        .unwrap());
    assert!(repo
        .gate(&f.e.session, "first", &o.id, c.clone(), now)
        .unwrap());
    let reopened = store::Repository::open(&repo.root).unwrap();
    assert!(reopened
        .gate(&f.e.session, "second", &o.id, c.clone(), now)
        .unwrap());
    assert!(!reopened
        .gate(&f.e.session, "third", &o.id, c.clone(), now)
        .unwrap());
    assert_eq!(reopened.data.lock().spends.len(), 2);
    assert!(!reopened
        .gate(&f.e.session, "expired", &o.id, c, now + 4000)
        .unwrap());
}
#[tokio::test]
async fn no_progress_records_conditions_and_stops_without_game_specific_branches() {
    let f = fixture(
        (0..4)
            .map(|_| decision("wait", json!({"duration_ms":0})))
            .collect(),
        false,
    );
    let error = f.e.run().await.unwrap_err();
    assert!(error.to_string().starts_with("no_progress"));
    let data = f.e.runtime.repository.data.lock();
    assert_eq!(data.failures.len(), 1);
    assert_eq!(data.requests.len(), 4);
    assert!(f.backend.inputs.lock().is_empty());
}
#[tokio::test]
async fn elapsed_time_checkpoint_is_not_reset_by_resume() {
    let f = fixture(vec![], false);
    f.clock.0.fetch_add(1500, Ordering::SeqCst);
    f.e.check().unwrap();
    assert_eq!(
        f.e.runtime.repository.data.lock().sessions[&f.e.session].active_ms,
        1500
    );
    f.e.runtime
        .repository
        .transaction(|d| {
            d.sessions.get_mut(&f.e.session).unwrap().state = "partial".into();
            Ok(())
        })
        .unwrap();
    let next = RunContext::new(RunId::generate(), f.e.context.app.clone());
    f.e.runtime
        .begin(
            &next,
            "完成日常",
            "plan-1",
            &f.e.settings,
            &f.e.profile,
            Some(&f.e.session),
        )
        .unwrap();
    assert_eq!(
        f.e.runtime.repository.data.lock().sessions[&f.e.session].active_ms,
        1500
    );
}

#[test]
fn billing_is_scoped_redacted_and_never_adds_organization_totals_to_requests() {
    let mut billing = billing::Billing {
        protocol: "openai".into(),
        endpoint: "https://billing.invalid/v1".into(),
        key: "separate-admin-key".into(),
        scope: "project-1".into(),
    };
    assert!(!billing.public().to_string().contains("separate-admin-key"));
    assert!(billing::query(&billing, 100, 200)
        .unwrap()
        .as_str()
        .contains("project_ids=project-1"));
    assert_eq!(
        billing::normalize(
            &billing,
            &json!({"data":[{"results":[{"amount":{"value":0.5,"currency":"usd"}}]}]})
        )
        .unwrap()["confirmed_micros"],
        500000
    );
    billing.protocol = "claude".into();
    assert_eq!(
        billing::normalize(
            &billing,
            &json!({"data":[{"results":[{"amount":"50","currency":"USD"}]}]})
        )
        .unwrap()["confirmed_micros"],
        500000
    );
    billing.protocol = "openrouter".into();
    let record = billing::normalize(
        &billing,
        &json!({"data":{"usage":0.5,"limit_remaining":2.0}}),
    )
    .unwrap();
    assert_eq!(record["not_additive"], true);
    assert_eq!(record["balance_micros"], 2000000);
}

#[tokio::test]
async fn real_http_adapters_send_images_and_parse_all_five_protocols_against_mock_server() {
    use axum::{routing::post, Json, Router};
    let calls = Arc::new(Mutex::new(vec![]));
    let captured = calls.clone();
    let app=Router::new().route("/*path",post(move |axum::extract::Path(path):axum::extract::Path<String>,Json(body):Json<Value>|{let captured=captured.clone();async move{
        captured.lock().push(body);let d=decision("observe",json!({}));let encoded=serde_json::to_string(&d).unwrap();
        Json(if path=="responses"{json!({"output":[{"type":"function_call","name":"gamer_tool","arguments":encoded}],"usage":{"input_tokens":100,"output_tokens":10}})}else if path=="messages"{json!({"content":[{"type":"tool_use","name":"gamer_tool","input":d}],"usage":{"input_tokens":60,"cache_read_input_tokens":40,"output_tokens":10}})}else if path.starts_with("models/"){json!({"candidates":[{"content":{"parts":[{"functionCall":{"name":"gamer_tool","args":d}}]}}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":10}})}else if path=="api/chat"{json!({"message":{"content":encoded},"prompt_eval_count":100,"eval_count":10})}else{json!({"choices":[{"message":{"tool_calls":[{"function":{"name":"gamer_tool","arguments":encoded}}]}}],"usage":{"prompt_tokens":100,"completion_tokens":10}})})
    }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let provider = provider::HttpProvider::new().unwrap();
    let f = fixture(vec![], false);
    let image = f.e.observe().await.unwrap().image;
    for protocol in ["responses", "claude", "gemini", "chat", "ollama"] {
        let mut p = profile();
        p.protocol = protocol.into();
        p.endpoint = format!("http://{address}");
        let reply = provider
            .infer(
                &p,
                ModelInput {
                    image: image.clone(),
                    prompt: "mock endpoint only".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(reply.decision.tool, "observe");
        assert_eq!(reply.usage["input_tokens"], 100);
        assert!(reply.cost.is_some());
    }
    assert_eq!(calls.lock().len(), 5);
    for body in calls.lock().iter() {
        assert!(body.to_string().contains("image") || body.to_string().contains("inlineData"));
    }
    server.abort();
}

struct UnusedExecutor;
struct UnusedLease;
impl crate::core::ActivityLease for UnusedLease {}
impl RunExecutor for UnusedExecutor {
    fn prepare<'a>(&'a self, _: &'a RunContext, _: &'a RunRequest) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn acquire(&self, _: &RunContext) -> Result<Box<dyn crate::core::ActivityLease>> {
        Ok(Box::new(UnusedLease))
    }
    fn execute<'a>(
        &'a self,
        _: &'a RunContext,
        _: &'a RunRequest,
        _: bool,
        _: Arc<AtomicBool>,
    ) -> BoxFuture<'a, Result<Vec<(String, String)>>> {
        Box::pin(async { Ok(vec![]) })
    }
}
fn mock_service(f: &Fixture) -> AiService {
    let cfg = crate::config::Config {
        data_dir: f._dir.path().to_path_buf(),
        ..Default::default()
    };
    let db = Arc::new(crate::store::Store::open(&cfg).unwrap());
    let devices = Arc::new(crate::device::DeviceManager::new(db.clone(), cfg));
    AiService {
        runtime: f.e.runtime.clone(),
        runs: Arc::new(RunManager::new(Arc::new(UnusedExecutor))),
        db,
        devices,
        settings: Mutex::new(f.e.settings.clone()),
        extensions: std::sync::RwLock::new(Weak::new()),
        prepared: Arc::new(Mutex::new(HashMap::new())),
        external: Arc::new(Mutex::new(HashMap::new())),
        wake_task: Mutex::new(None),
    }
}

struct MockAiExecutor(Arc<AiService>);
impl RunExecutor for MockAiExecutor {
    fn prepare<'a>(&'a self, _: &'a RunContext, _: &'a RunRequest) -> BoxFuture<'a, Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn acquire(&self, _: &RunContext) -> Result<Box<dyn crate::core::ActivityLease>> {
        Ok(Box::new(UnusedLease))
    }
    fn execute<'a>(
        &'a self,
        c: &'a RunContext,
        r: &'a RunRequest,
        logs: bool,
        stop: Arc<AtomicBool>,
    ) -> BoxFuture<'a, Result<Vec<(String, String)>>> {
        self.0.execute(c, r, logs, stop)
    }
}
fn ai_archive() -> Vec<u8> {
    ai_archive_with_manifest(include_str!("../manifest.toml"))
}
fn ai_archive_with_manifest(manifest: &str) -> Vec<u8> {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(vec![]));
    for (path, bytes) in [
        ("manifest.toml", manifest.as_bytes()),
        (
            "ui/plugin.js",
            b"export const sdkVersion=1;export const panels={}".as_slice(),
        ),
    ] {
        writer
            .start_file(path, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}
#[tokio::test]
async fn builtin_lifecycle_runs_the_real_ai_executor_and_preserves_timer_completion() {
    use crate::{
        capabilities::CapabilityRegistry, extensions::ExtensionService, scheduler::Scheduler,
    };
    let f = fixture(
        vec![
            decision("set_plan", plan_args("CURRENT")),
            decision("act", act("op-1", "CURRENT", "regenerative_resource")),
            decision(
                "finish",
                json!({"subgoals":[{"name":"日常","state":"completed","evidence":"CURRENT","result":"可见完成"}],"summary":"完成"}),
            ),
            decision(
                "finish",
                json!({"verified":true,"observation_id":"CURRENT","account_consistent":true,"result":"可见全部完成"}),
            ),
            decision(
                "ask_user",
                json!({"question":"攻略缺少入口，如何进入？","options":[],"reason":"当前入口未知","kind":"knowledge","observation_id":"CURRENT"}),
            ),
            decision("set_plan", plan_args("CURRENT")),
            decision("act", act("bounded-navigation", "CURRENT", "navigation")),
            decision(
                "act",
                json!({"reversible":true,"consumption":consumption("navigation")}),
            ),
            decision(
                "finish",
                json!({"subgoals":[{"name":"日常","state":"completed","evidence":"CURRENT","result":"可见完成"}],"summary":"完成"}),
            ),
            decision(
                "finish",
                json!({"verified":true,"observation_id":"CURRENT","account_consistent":true,"result":"可见全部完成"}),
            ),
        ],
        false,
    );
    let service = Arc::new(mock_service(&f));
    let scheduler = Arc::new(Scheduler::new(service.db.clone()));
    let extensions = Arc::new(
        ExtensionService::for_data_root(f._dir.path(), CapabilityRegistry::default())
            .with_builtin_service(service.clone())
            .with_runner_registrar(Arc::new(Registrar {
                service: service.clone(),
                scheduler: scheduler.clone(),
            })),
    );
    service.attach(&extensions);
    service
        .runs
        .register_executor(ID, Arc::new(MockAiExecutor(service.clone())));
    let id = ExtensionId::parse(ID).unwrap();
    assert!(service.live().await.is_err());
    extensions.install(&ai_archive()).await.unwrap();
    extensions.enable(&id).await.unwrap();
    extensions.start(&id).await.unwrap();
    assert!(scheduler
        .runners()
        .iter()
        .any(|r| r.runner_id == ID && r.owner_extension_id == ID));
    let app = f.e.context.app.clone();
    drop(f._lease);
    let request = RunRequest::for_app(
        app.clone(),
        ID,
        "default#goal",
        RunPayload::new(json!({"goal":"完成日常","model_profile_id":"mock"})),
    )
    .unwrap();
    let completions = Arc::new(Mutex::new(vec![]));
    let captured = completions.clone();
    let submitted = service
        .submit(
            request,
            "timer-ai",
            Some(123),
            Arc::new(move |result| captured.lock().push(result)),
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while completions.lock().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    {
        let completed = completions.lock();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].task_id, "timer-ai");
        assert_eq!(completed[0].scheduled_at, Some(123));
        assert!(matches!(
            completed[0].outcome,
            crate::timer_core::TimerOutcome::Success
        ));
    }
    assert!(service
        .runtime
        .repository
        .data
        .lock()
        .sessions
        .values()
        .any(|s| s.runs.contains(&submitted.run_id) && s.state == "completed"));
    assert_eq!(f.backend.inputs.lock().len(), 1);
    assert!(crate::core::input_ownership::admit(app.device_id.as_str()).is_ok());
    // The real timeout worker resumes through the same runner and device slot.
    service
        .db
        .upsert_device(&crate::store::Device {
            id: app.device_id.to_string(),
            name: "mock".into(),
            addr: "MOCK-NOT-ADB".into(),
            screen_mode: crate::store::ScreenMode::Virtual,
            vd_res: None,
            vd_dpi: None,
            pkg: Some("com.sample.game".into()),
            fps: None,
            created_at: String::new(),
        })
        .unwrap();
    let next = service
        .submit(
            RunRequest::for_app(
                app.clone(),
                ID,
                "default#goal",
                RunPayload::new(json!({"goal":"完成日常","model_profile_id":"mock"})),
            )
            .unwrap(),
            "",
            None,
            Arc::new(|_| {}),
        )
        .await
        .unwrap();
    let logical = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(s) = service
                .runtime
                .repository
                .data
                .lock()
                .sessions
                .values()
                .find(|s| {
                    s.runs.contains(&next.run_id)
                        && s.state == "waiting_user"
                        && service
                            .runs
                            .active_for_device(app.device_id.as_str())
                            .is_none()
                })
                .map(|s| s.id.clone())
            {
                break s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    f.clock.0.fetch_add(121_000, Ordering::SeqCst);
    service.wake_expired_questions().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while service.runtime.repository.data.lock().sessions[&logical].state != "completed" {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let saved = service.runtime.repository.data.lock().sessions[&logical].clone();
    assert_eq!(saved.runs.len(), 2);
    assert_eq!(saved.auto_resumes, 1);
    assert_eq!(saved.trial_operations, 1);
    assert_eq!(saved.rounds, 6);
    assert!(service.runtime.repository.data.lock().rules.is_empty());
    assert_eq!(f.backend.inputs.lock().len(), 2);
    extensions.disable(&id).await.unwrap();
    assert!(service.wake_task.lock().is_none());
    assert!(!scheduler.runners().iter().any(|r| r.runner_id == ID));
    assert!(service.live().await.is_err());
}
#[tokio::test]
async fn mcp_expiry_cancels_inflight_inference_and_releases_without_late_input() {
    let f = fixture(
        vec![decision(
            "act",
            json!({"safe_coordinates":true,"consumption":consumption("navigation")}),
        )],
        true,
    );
    let service = mock_service(&f);
    let now = f.clock.now().timestamp();
    let issued=mcp::manage(&service,"credentials.issue",&json!({"package_id":"default","android_package":"com.sample.game","device_id":f.e.context.device_id(),"tools":["act"],"expires_at":now+1})).unwrap();
    let credential = mcp::authenticate(&service, issued["token"].as_str().unwrap()).unwrap();
    let o = f.e.observe().await.unwrap();
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    let (tx, _rx) = tokio::sync::oneshot::channel();
    sender
        .try_send(mcp::Call {
            name: "act".into(),
            args: act("expiry-action", &o.id, "navigation"),
            result: tx,
        })
        .unwrap();
    service.external.lock().insert(
        "external".into(),
        mcp::External {
            run_id: f.e.context.run_id.to_string(),
            credential_id: credential.id,
            expires_at: now + 1,
            sender,
            receiver: Some(receiver),
            logical_session: None,
        },
    );
    let clock = f.clock.clone();
    let advance = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        clock.0.fetch_add(2000, Ordering::SeqCst);
    });
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        mcp::run_external(&service, &f.e, "external"),
    )
    .await
    .unwrap();
    advance.await.unwrap();
    assert!(result.is_err());
    assert!(f.e.stop.load(Ordering::Acquire));
    assert!(f.backend.inputs.lock().is_empty());
    assert!(service.external.lock().is_empty());
    let data = f.e.runtime.repository.data.lock();
    assert_eq!(data.requests.len(), 1);
    assert!(data
        .requests
        .values()
        .all(|r| r.status == "pending_reconciliation" && r.actual.is_none()));
}
#[tokio::test]
async fn truncated_builtin_manifest_cannot_infer_or_execute_with_undeclared_permissions() {
    let f = fixture(vec![], false);
    let service = Arc::new(mock_service(&f));
    let extensions = Arc::new(
        ExtensionService::for_data_root(
            f._dir.path(),
            crate::capabilities::CapabilityRegistry::default(),
        )
        .with_builtin_service(service.clone()),
    );
    service.attach(&extensions);
    let manifest = include_str!("../manifest.toml").replace("\"ai.infer\", ", "");
    let id = ExtensionId::parse(ID).unwrap();
    extensions
        .install(&ai_archive_with_manifest(&manifest))
        .await
        .unwrap();
    extensions.enable(&id).await.unwrap();
    extensions.start(&id).await.unwrap();
    assert!(service.live().await.is_err());
    assert!(extensions
        .call_extension(&id, "settings.test", json!({"profile_id":"mock"}))
        .await
        .is_err());
    assert!(f.backend.inputs.lock().is_empty());
    assert!(service.runtime.repository.data.lock().requests.is_empty());
}
#[tokio::test]
async fn independent_credentials_expire_revoke_and_cannot_borrow_runs_or_approve() {
    let f = fixture(vec![], false);
    let service = mock_service(&f);
    let expiry = f.clock.now().timestamp() + 100;
    let issued=mcp::manage(&service,"credentials.issue",&json!({"package_id":"default","android_package":"com.sample.game","device_id":f.e.context.device_id(),"tools":["observe","gamer.goal.status"],"expires_at":expiry})).unwrap();
    let token = issued["token"].as_str().unwrap();
    let credential = mcp::authenticate(&service, token).unwrap();
    assert!(!serde_json::to_string(&*f.e.runtime.repository.data.lock())
        .unwrap()
        .contains(token));
    assert!(mcp::authenticate(&service, "administrator-token").is_err());
    assert!(mcp::call(
        &service,
        &credential,
        &json!({"name":"approvals.resolve","arguments":{}})
    )
    .await
    .is_err());
    assert!(mcp::call(
        &service,
        &credential,
        &json!({"name":"gamer.goal.status","arguments":{"run_id":"foreign-run"}})
    )
    .await
    .is_err());
    assert!(mcp::manage(&service,"credentials.issue",&json!({"package_id":"default","android_package":"com.sample.game","device_id":"device","tools":["approvals.resolve"],"expires_at":expiry})).is_err());
    mcp::manage(
        &service,
        "credentials.revoke",
        &json!({"credential_id":credential.id}),
    )
    .unwrap();
    assert!(mcp::authenticate(&service, token).is_err());
    f.clock.0.fetch_add(200000, Ordering::SeqCst);
    assert!(mcp::authenticate(&service, token).is_err());
}
#[tokio::test]
async fn user_budget_adjustment_preserves_counters_and_pending_approval() {
    let f = fixture(vec![], false);
    let service = mock_service(&f);
    let o = f.e.observe().await.unwrap();
    let c: store::Consumption = serde_json::from_value(consumption("item")).unwrap();
    service
        .runtime
        .repository
        .gate(
            &f.e.session,
            "unknown-spend",
            &o.id,
            c,
            f.clock.now().timestamp(),
        )
        .unwrap();
    service
        .runtime
        .repository
        .transaction(|d| {
            let s = d.sessions.get_mut(&f.e.session).unwrap();
            s.state = "partial".into();
            s.rounds = 9;
            s.tokens = 900;
            s.active_ms = 1234;
            Ok(())
        })
        .unwrap();
    service.settings.lock().budget.max_rounds = 80;
    service
        .dispatch("sessions.budget", json!({"session_id":f.e.session}))
        .await
        .unwrap();
    let d = service.runtime.repository.data.lock();
    let s = &d.sessions[&f.e.session];
    assert_eq!(
        (s.rounds, s.tokens, s.active_ms, s.budget.max_rounds),
        (9, 900, 1234, 80)
    );
    assert_eq!(d.approvals.len(), 1);
    assert!(!tools::NAMES.contains(&"sessions.budget"));
}
#[tokio::test]
async fn expired_mcp_goal_binding_cancels_before_any_new_host_action() {
    let f = fixture(vec![], false);
    let service = mock_service(&f);
    let now = f.clock.now().timestamp();
    let issued=mcp::manage(&service,"credentials.issue",&json!({"package_id":"default","android_package":"com.sample.game","device_id":f.e.context.device_id(),"tools":["gamer.goal.submit"],"expires_at":now+1})).unwrap();
    service
        .runtime
        .repository
        .transaction(|data| {
            data.mcp_sessions.insert(
                f.e.session.clone(),
                issued["credential_id"].as_str().unwrap().into(),
            );
            Ok(())
        })
        .unwrap();
    f.clock.0.fetch_add(2000, Ordering::SeqCst);
    assert!(f.e.check().is_err());
    assert!(f.e.stop.load(Ordering::Acquire));
    assert!(f.backend.inputs.lock().is_empty());
}
#[test]
fn concurrent_devices_cannot_both_reserve_the_same_global_balance() {
    let f = fixture(vec![], false);
    let repo = f.e.runtime.repository.clone();
    let first = f.e.session.clone();
    let second = store::id();
    let now = f.clock.now().timestamp();
    repo.transaction(|data| {
        let s = data.sessions.get_mut(&first).unwrap();
        s.budget.session_micros = 600_000;
        s.budget.daily_micros = 600_000;
        s.budget.global_micros = 600_000;
        let mut other = s.clone();
        other.id = second.clone();
        data.sessions.insert(second.clone(), other);
        Ok(())
    })
    .unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let mut workers = vec![];
    for session in [first, second] {
        let repo = repo.clone();
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            repo.reserve(
                store::RequestRecord {
                    id: store::id(),
                    session,
                    run_id: store::id(),
                    profile: "mock".into(),
                    model: "mock".into(),
                    price_version: "price-1".into(),
                    kind: "decision".into(),
                    day: "2030-01-01".into(),
                    reserved: 350_000,
                    actual: None,
                    source: "unknown".into(),
                    status: "reserved".into(),
                    usage: Value::Null,
                    at: now,
                    duration_ms: 0,
                },
                now,
            )
            .is_ok()
        }));
    }
    barrier.wait();
    assert_eq!(
        workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .filter(|passed| *passed)
            .count(),
        1
    );
    assert_eq!(repo.data.lock().requests.len(), 1);
}

#[test]
fn simple_model_connection_normalizes_addresses_without_embedded_secrets() {
    for (input, expected, kind) in [
        (
            "https://proxy.example/v1",
            "https://proxy.example/v1",
            "chat",
        ),
        (
            "https://proxy.example/v1/responses",
            "https://proxy.example/v1",
            "responses",
        ),
        (
            "https://proxy.example/v1/chat/completions/",
            "https://proxy.example/v1",
            "chat",
        ),
        (
            "https://api.anthropic.com",
            "https://api.anthropic.com/v1",
            "claude",
        ),
        (
            "https://generativelanguage.googleapis.com",
            "https://generativelanguage.googleapis.com/v1beta",
            "gemini",
        ),
        ("http://localhost:11434", "http://localhost:11434", "ollama"),
        (
            "http://localhost:11434/api/chat",
            "http://localhost:11434",
            "ollama",
        ),
    ] {
        assert_eq!(
            provider::connection(input, "auto").unwrap(),
            (expected.into(), kind.into())
        );
    }
    for input in [
        "file:///models",
        "https://user:secret@host/v1",
        "https://host/v1?key=secret",
        "https://host/v1#secret",
    ] {
        assert!(provider::connection(input, "auto").is_err());
    }
}

#[test]
fn optional_prices_never_make_unknown_api_costs_zero() {
    let mut p = profile();
    p.price_version.clear();
    p.input_micros_per_million = 0;
    p.output_micros_per_million = 0;
    p.validate().unwrap();
    let mut response = json!({"output":[{"type":"function_call","name":"gamer_tool","arguments":serde_json::to_string(&decision("observe",json!({}))).unwrap()}],"usage":{"input_tokens":100,"output_tokens":10}});
    let reply = provider::normalize(&p, response.clone()).unwrap();
    assert_eq!(reply.cost, None);
    assert_eq!(reply.source, "unknown");
    response["usage"]["cost"] = json!(0.025);
    assert_eq!(
        provider::normalize(&p, response).unwrap().cost,
        Some(25_000)
    );
    p.input_micros_per_million = 1;
    assert!(p.validate().is_err());
}

#[tokio::test]
async fn unpriced_api_requires_a_positive_reserve_before_request() {
    let mut f = fixture(vec![decision("observe", json!({}))], false);
    f.e.profile.price_version.clear();
    f.e.profile.input_micros_per_million = 0;
    f.e.profile.output_micros_per_million = 0;
    f.e.settings.budget.request_micros = 0;
    let o = f.e.observe().await.unwrap();
    assert!(f
        .e
        .infer("test".into(), &o, "decision")
        .await
        .err()
        .unwrap()
        .to_string()
        .contains("预留必须大于零"));
    assert!(f.e.runtime.repository.data.lock().requests.is_empty());
}

#[tokio::test]
async fn model_discovery_is_bounded_get_and_never_infers_or_verifies_vision() {
    use axum::{http::HeaderMap, routing::get, Json, Router};
    let received = Arc::new(Mutex::new(vec![]));
    let captured = received.clone();
    let app = Router::new().route("/*path", get(move |axum::extract::Path(path):axum::extract::Path<String>, headers:HeaderMap| {
        let captured = captured.clone(); async move {
            captured.lock().push((path.clone(), headers));
            Json(if path == "api/tags" { json!({"models":[{"model":"local-vision"}]}) }
            else if path == "v1beta/models" { json!({"models":[{"name":"models/embedding","supportedGenerationMethods":["embedContent"]},{"name":"models/vision","supportedGenerationMethods":["generateContent"]}]}) }
            else { json!({"data":[{"id":"unknown-candidate"},{"id":"text-only","input_modalities":["text"]},{"id":"advertised-vision","capabilities":{"image_input":{"supported":true}}},{"id":"advertised-vision"},{"id":"unsafe?name"}]}) })
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let http = provider::HttpProvider::new().unwrap();
    for protocol in ["chat", "responses", "claude", "gemini", "ollama"] {
        let result = http
            .discover(&format!("http://{address}"), "catalog-test-key", protocol)
            .await
            .unwrap();
        assert_eq!(result["vision"], "untested");
        let expected = if protocol == "gemini" {
            "vision"
        } else if protocol == "ollama" {
            "local-vision"
        } else {
            "advertised-vision"
        };
        assert_eq!(result["models"][0]["id"], expected);
        let items = result["models"].as_array().unwrap();
        assert_eq!(
            items.len(),
            if protocol == "gemini" || protocol == "ollama" {
                1
            } else {
                2
            }
        );
        assert!(!result.to_string().contains("catalog-test-key"));
    }
    let calls = received.lock();
    assert_eq!(calls.len(), 5);
    assert_eq!(calls[0].0, "v1/models");
    assert_eq!(calls[0].1["authorization"], "Bearer catalog-test-key");
    assert_eq!(calls[2].1["x-api-key"], "catalog-test-key");
    assert_eq!(calls[3].1["x-goog-api-key"], "catalog-test-key");
    assert_eq!(calls[4].0, "api/tags");
    server.abort();
}

#[tokio::test]
async fn changing_model_endpoint_does_not_reuse_secret_or_trust() {
    let f = fixture(vec![], false);
    let service = mock_service(&f);
    service.settings.lock().profiles[0].key = "stored-private-key".into();
    let mut settings = service.settings();
    let version = settings.version.clone();
    settings.profiles[0].key.clear();
    settings.profiles[0].endpoint = "https://other.example/v1".into();
    let result = service
        .dispatch(
            "settings.save",
            json!({"expected_version":version,"settings":settings}),
        )
        .await
        .unwrap();
    assert_eq!(result["profiles"][0]["has_key"], false);
    assert_eq!(result["profiles"][0]["vision"], "untested");
    assert!(!result.to_string().contains("stored-private-key"));
}

#[tokio::test]
async fn manual_profile_accepts_full_method_url_without_network_or_reverification() {
    let f = fixture(vec![], false);
    let service = mock_service(&f);
    let mut next = service.settings();
    let version = next.version.clone();
    next.profiles[0].endpoint = "https://mock.invalid/v1/responses".into();
    let result = service
        .dispatch(
            "settings.save",
            json!({"expected_version":version,"settings":next}),
        )
        .await
        .unwrap();
    assert_eq!(result["profiles"][0]["endpoint"], "https://mock.invalid/v1");
    assert_eq!(result["profiles"][0]["vision"], "available");
    assert!(f.e.runtime.repository.data.lock().requests.is_empty());
}

fn plan_args(observation: &str) -> Value {
    json!({"observation_id":observation,"research_summary":"攻略未覆盖新活动入口，用户说明已补充；先验证入口，再核对完成状态","steps":[{"description":"按说明进入活动","expected":"活动页面可见"}],"guide_paths":[],"source_urls":[]})
}
fn waiting(f: &Fixture) {
    f.e.runtime
        .repository
        .checkpoint(&f.e.session, f.clock.now().timestamp_millis(), true)
        .unwrap();
    f.e.runtime
        .repository
        .transaction(|d| {
            d.sessions.get_mut(&f.e.session).unwrap().state = "waiting_user".into();
            Ok(())
        })
        .unwrap();
}
async fn ask(f: &Fixture, kind: &str) -> String {
    let o = f.e.observe().await.unwrap();
    let v = tools::execute(&f.e, "ask_user", json!({"question":"新的活动入口如何进入？","options":["进入侧边活动页","先完成前置"],"reason":"当前攻略和画面不足以确定路径","kind":kind,"observation_id":o.id})).await.unwrap();
    waiting(f);
    v["question_id"].as_str().unwrap().into()
}
#[tokio::test]
async fn user_answer_resumes_same_budget_and_becomes_verified_long_term_guide() {
    let f = fixture(
        vec![
            decision(
                "ask_user",
                json!({"question":"入口在哪里？","options":["侧边活动"],"reason":"攻略仍缺入口","kind":"knowledge","observation_id":"CURRENT"}),
            ),
            decision("set_plan", plan_args("CURRENT")),
            decision("act", act("learned-step", "CURRENT", "navigation")),
            decision(
                "finish",
                json!({"subgoals":[{"name":"活动","state":"completed","evidence":"CURRENT","result":"完成标记可见"}],"summary":"活动完成"}),
            ),
            decision(
                "finish",
                json!({"verified":true,"observation_id":"CURRENT","account_consistent":true,"validated_guides":"CURRENT_GUIDES","result":"全部目标完成且指南入口验证有效"}),
            ),
        ],
        false,
    );
    assert!(f
        .e
        .run()
        .await
        .unwrap_err()
        .to_string()
        .starts_with("waiting_user"));
    let q = f.e.runtime.repository.data.lock().sessions[&f.e.session].questions[0]
        .id
        .clone();
    let service = mock_service(&f);
    service.dispatch("sessions.message", json!({"session_id":f.e.session,"question_id":q,"message":"从侧边活动页进入，完成后检查活动面板"})).await.unwrap();
    let seq = f.e.runtime.repository.data.lock().sessions[&f.e.session]
        .events
        .iter()
        .find(|e| e.kind == "user_message")
        .unwrap()
        .seq;
    let o = f.e.observe().await.unwrap();
    let candidate = tools::execute(&f.e,"memory.propose",json!({"observation_id":o.id,"title":"活动入口经验","content":"从侧边活动页进入，完成后检查活动面板","conditions":"当前游戏版本、入口可见","sources":[],"user_message_refs":[seq]})).await.unwrap();
    let path = candidate["path"].as_str().unwrap();
    assert_eq!(
        tools::read_local_memory(&f.e.runtime, "default", path).unwrap()["effective_status"],
        "candidate"
    );
    let mut resumed = f.e.clone();
    resumed.context.run_id = RunId::generate();
    let same = resumed
        .runtime
        .begin(
            &resumed.context,
            "完成日常",
            "plan-1",
            &resumed.settings,
            &resumed.profile,
            Some(&resumed.session),
        )
        .unwrap();
    assert_eq!(same, f.e.session);
    resumed.run().await.unwrap();
    assert_eq!(f.backend.inputs.lock().len(), 1);
    let data = f.e.runtime.repository.data.lock();
    assert_eq!(data.sessions[&same].rounds, 5);
    assert_eq!(data.sessions[&same].runs.len(), 2);
    assert!(data.rules.is_empty());
    drop(data);
    let reopened = store::Repository::open(&f.e.runtime.repository.root).unwrap();
    assert_eq!(
        reopened.data.lock().sessions[&same].questions[0]
            .answer
            .as_deref(),
        Some("从侧边活动页进入，完成后检查活动面板")
    );
    assert_eq!(
        tools::read_local_memory(&f.e.runtime, "default", path).unwrap()["effective_status"],
        "verified"
    );
}
#[tokio::test]
async fn knowledge_timeout_is_persisted_and_bounds_actions_and_repeated_wakeups() {
    let f = fixture(vec![], false);
    for round in 0..3 {
        ask(&f, "knowledge").await;
        f.clock.0.fetch_add(121_000, Ordering::SeqCst);
        assert_eq!(
            f.e.runtime
                .repository
                .expire_questions(f.clock.now().timestamp())
                .unwrap(),
            vec![f.e.session.clone()]
        );
        let reopened = store::Repository::open(&f.e.runtime.repository.root).unwrap();
        let data = reopened.data.lock();
        let s = &data.sessions[&f.e.session];
        assert!(s.trial_mode);
        assert_eq!(s.auto_resumes, round + 1);
        assert!(data.rules.is_empty());
        assert!(!s.questions.iter().any(store::Question::pending));
    }
    ask(&f, "knowledge").await;
    f.clock.0.fetch_add(121_000, Ordering::SeqCst);
    assert!(f
        .e
        .runtime
        .repository
        .expire_questions(f.clock.now().timestamp())
        .unwrap()
        .is_empty());
    assert_eq!(
        f.e.runtime.repository.data.lock().sessions[&f.e.session].auto_resumes,
        3
    );
    let d = f.e.runtime.repository.data.lock();
    assert_eq!(d.sessions[&f.e.session].state, "partial");
    assert_eq!(
        d.sessions[&f.e.session].events.last().unwrap().kind,
        "trial_boundary"
    );
    drop(d);
    assert!(f.backend.inputs.lock().is_empty());
}
#[tokio::test]
async fn identity_preferences_secrets_and_authorization_timeout_never_choose_or_approve() {
    for kind in ["identity", "preference", "secret", "authorization"] {
        let f = fixture(vec![], false);
        ask(&f, kind).await;
        f.clock.0.fetch_add(121_000, Ordering::SeqCst);
        assert!(f
            .e
            .runtime
            .repository
            .expire_questions(f.clock.now().timestamp())
            .unwrap()
            .is_empty());
        let d = f.e.runtime.repository.data.lock();
        let s = &d.sessions[&f.e.session];
        assert!(s.questions[0].timed_out && s.questions[0].pending());
        assert_eq!(s.questions[0].answer, None);
        assert!(!s.trial_mode);
        assert!(d.rules.is_empty());
    }
}
#[tokio::test]
async fn planning_gate_requires_research_and_rejects_invented_sources() {
    let mut f = fixture(vec![], false);
    f.e.runtime
        .repository
        .transaction(|d| {
            d.sessions.get_mut(&f.e.session).unwrap().execution_plan = Value::Null;
            Ok(())
        })
        .unwrap();
    let o = f.e.observe().await.unwrap();
    assert!(
        tools::execute(&f.e, "act", act("too-early", &o.id, "navigation"))
            .await
            .unwrap_err()
            .to_string()
            .starts_with("plan_required")
    );
    assert!(tools::execute(
        &f.e,
        "call_automation",
        json!({"entrypoint":"default/test.yaml","args":{}})
    )
    .await
    .is_err());
    f.e.settings.search = Some(runtime::SearchSettings {
        endpoint: "https://search.example".into(),
        key: String::new(),
        request_micros: 1,
    });
    assert!(tools::execute(&f.e, "set_plan", plan_args(&o.id))
        .await
        .unwrap_err()
        .to_string()
        .starts_with("research_required"));
    tools::execute(&f.e, "search_guides", json!({"query":"活动入口"}))
        .await
        .unwrap();
    let mut args = plan_args(&o.id);
    args["source_urls"] = json!(["https://invented.invalid"]);
    assert!(tools::execute(&f.e, "set_plan", args).await.is_err());
    tools::execute(&f.e, "set_plan", plan_args(&o.id))
        .await
        .unwrap();
    tools::execute(&f.e, "act", act("researched", &o.id, "navigation"))
        .await
        .unwrap();
    assert_eq!(f.backend.inputs.lock().len(), 1);
}
#[tokio::test]
async fn timeout_trial_exhaustion_does_not_bill_or_create_unknown_operation() {
    let f = fixture(vec![], false);
    f.e.runtime
        .repository
        .transaction(|d| {
            let s = d.sessions.get_mut(&f.e.session).unwrap();
            s.trial_mode = true;
            s.trial_operations = 3;
            Ok(())
        })
        .unwrap();
    let o = f.e.observe().await.unwrap();
    assert!(
        tools::execute(&f.e, "act", act("beyond-limit", &o.id, "navigation"))
            .await
            .unwrap_err()
            .to_string()
            .starts_with("trial_boundary")
    );
    let d = f.e.runtime.repository.data.lock();
    assert!(d.requests.is_empty());
    assert!(d.sessions[&f.e.session].operations.is_empty());
    assert!(f.backend.inputs.lock().is_empty());
}
#[tokio::test]
async fn trial_step_requires_multimodal_reversibility_and_never_spends_extra() {
    for (reversible, category, allowed) in [
        (true, "navigation", true),
        (false, "navigation", false),
        (true, "item", false),
    ] {
        let f = fixture(
            vec![decision(
                "act",
                json!({"reversible":reversible,"consumption":consumption(category)}),
            )],
            false,
        );
        f.e.runtime
            .repository
            .transaction(|d| {
                d.sessions.get_mut(&f.e.session).unwrap().trial_mode = true;
                Ok(())
            })
            .unwrap();
        let o = f.e.observe().await.unwrap();
        assert_eq!(
            tools::execute(&f.e, "act", act("trial", &o.id, category))
                .await
                .is_ok(),
            allowed
        );
        assert_eq!(f.backend.inputs.lock().len(), usize::from(allowed));
        assert_eq!(
            f.e.runtime.repository.data.lock().sessions[&f.e.session].trial_operations,
            u32::from(allowed)
        );
    }
}
#[tokio::test]
async fn old_decision_after_user_steering_is_charged_but_cannot_execute() {
    let f = fixture(
        vec![decision("act", act("stale", "CURRENT", "navigation"))],
        false,
    );
    let o = f.e.observe().await.unwrap();
    mock_service(&f)
        .dispatch(
            "sessions.message",
            json!({"session_id":f.e.session,"message":"先看帮助页面，不进入活动"}),
        )
        .await
        .unwrap();
    let error =
        f.e.infer(
            json!({"conversation_revision":0,"observation_id":o.id}).to_string(),
            &o,
            "decision",
        )
        .await
        .err()
        .unwrap();
    assert!(error.to_string().starts_with("conversation_changed"));
    let d = f.e.runtime.repository.data.lock();
    assert_eq!(d.requests.len(), 1);
    assert_eq!(d.requests.values().next().unwrap().actual, Some(10));
    assert!(d.rules.is_empty());
    assert!(f.backend.inputs.lock().is_empty());
}
#[tokio::test]
async fn permanent_permission_is_explicit_scoped_cumulative_and_revocable() {
    let f = fixture(vec![], false);
    let repo = &f.e.runtime.repository;
    let o = f.e.observe().await.unwrap();
    let c: store::Consumption = serde_json::from_value(consumption("item")).unwrap();
    assert!(!repo
        .gate(
            &f.e.session,
            "pending-permanent",
            &o.id,
            c.clone(),
            f.clock.now().timestamp()
        )
        .unwrap());
    waiting(&f);
    let service = mock_service(&f);
    service
        .dispatch(
            "identity.confirm",
            json!({"session_id":f.e.session,"account":"verified-account"}),
        )
        .await
        .unwrap();
    let approval = repo
        .data
        .lock()
        .approvals
        .values()
        .next()
        .unwrap()
        .id
        .clone();
    assert!(service
        .dispatch(
            "approvals.resolve",
            json!({"approval_id":approval,"scope":"persistent","limit":2,"no_expiry":true})
        )
        .await
        .is_err());
    service
        .dispatch(
            "sessions.message",
            json!({"session_id":f.e.session,"message":"同意永久使用门票"}),
        )
        .await
        .unwrap();
    assert!(repo.data.lock().rules.is_empty());
    service.dispatch("approvals.resolve",json!({"approval_id":approval,"decision":"approve","scope":"persistent","limit":2,"no_expiry":true})).await.unwrap();
    let rule = repo.data.lock().rules.values().next().unwrap().clone();
    assert_eq!(rule.expires_at, i64::MAX);
    assert_eq!(rule.account.as_deref(), Some("verified-account"));
    assert!(repo
        .gate(
            &f.e.session,
            "first-permanent",
            &o.id,
            c.clone(),
            f.clock.now().timestamp()
        )
        .unwrap());
    let mut second = repo.data.lock().sessions[&f.e.session].clone();
    second.id = store::id();
    second.runs.clear();
    repo.transaction(|d| {
        d.sessions.insert(second.id.clone(), second.clone());
        Ok(())
    })
    .unwrap();
    let reopened = store::Repository::open(&repo.root).unwrap();
    assert!(reopened
        .gate(
            &second.id,
            "second-permanent",
            &o.id,
            c.clone(),
            f.clock.now().timestamp()
        )
        .unwrap());
    assert!(!reopened
        .gate(
            &second.id,
            "third-permanent",
            &o.id,
            c.clone(),
            f.clock.now().timestamp()
        )
        .unwrap());
    let mut other = second.clone();
    other.id = store::id();
    other.account = Some("another-account".into());
    repo.transaction(|d| {
        d.sessions.insert(other.id.clone(), other.clone());
        Ok(())
    })
    .unwrap();
    assert!(!repo
        .gate(
            &other.id,
            "wrong-account",
            &o.id,
            c.clone(),
            f.clock.now().timestamp()
        )
        .unwrap());
    service
        .dispatch("approvals.revoke", json!({"rule_id":rule.id}))
        .await
        .unwrap();
    assert!(!repo
        .gate(&f.e.session, "revoked", &o.id, c, f.clock.now().timestamp())
        .unwrap());
}

#[tokio::test]
async fn question_timer_never_promotes_external_or_revoked_mcp_credentials() {
    for (allowed, revoked) in [("gamer.session.open", false), ("gamer.goal.submit", true)] {
        let f = fixture(vec![], false);
        ask(&f, "knowledge").await;
        let service = Arc::new(mock_service(&f));
        let issued=mcp::manage(&service,"credentials.issue",&json!({"package_id":"default","android_package":"com.sample.game","device_id":f.e.context.device_id(),"tools":[allowed,"observe"],"expires_at":f.clock.now().timestamp()+1000})).unwrap();
        let credential = issued["credential_id"].as_str().unwrap();
        service
            .runtime
            .repository
            .transaction(|d| {
                d.credentials.get_mut(credential).unwrap().revoked = revoked;
                d.mcp_sessions
                    .insert(f.e.session.clone(), credential.into());
                Ok(())
            })
            .unwrap();
        let extensions = Arc::new(
            crate::extensions::ExtensionService::for_data_root(
                f._dir.path(),
                crate::capabilities::CapabilityRegistry::default(),
            )
            .with_builtin_service(service.clone()),
        );
        service.attach(&extensions);
        extensions.install(&ai_archive()).await.unwrap();
        let id = ExtensionId::parse(ID).unwrap();
        extensions.enable(&id).await.unwrap();
        extensions.start(&id).await.unwrap();
        f.clock.0.fetch_add(121_000, Ordering::SeqCst);
        service.wake_expired_questions().await.unwrap();
        {
            let data = service.runtime.repository.data.lock();
            let s = &data.sessions[&f.e.session];
            assert_eq!(s.runs.len(), 1);
            assert_eq!(data.mcp_sessions[&f.e.session], credential);
            assert!(data.requests.is_empty());
            assert!(data.rules.is_empty());
            assert!(s.events.iter().any(
                |e| e.kind == "tool_error" && e.data["error"].as_str().unwrap().contains("MCP")
            ));
            assert!(s.notified.contains_key("auto_resume_problem"));
        }
        extensions.disable(&id).await.unwrap();
    }
}

#[tokio::test]
async fn new_goal_reuses_permanent_rule_only_after_current_screen_identity_verification() {
    for consistent in [false, true] {
        let f = fixture(
            vec![decision(
                "act",
                json!({"account_consistent":consistent,"cycle_consistent":true,"consumption":consumption("item")}),
            )],
            false,
        );
        let o = f.e.observe().await.unwrap();
        let c: store::Consumption = serde_json::from_value(consumption("item")).unwrap();
        assert!(!f
            .e
            .runtime
            .repository
            .gate(
                &f.e.session,
                "authorize-once",
                &o.id,
                c,
                f.clock.now().timestamp()
            )
            .unwrap());
        waiting(&f);
        let service = mock_service(&f);
        service
            .dispatch(
                "identity.confirm",
                json!({"session_id":f.e.session,"account":"account-approved-once"}),
            )
            .await
            .unwrap();
        let approval =
            f.e.runtime
                .repository
                .data
                .lock()
                .approvals
                .values()
                .next()
                .unwrap()
                .id
                .clone();
        service.dispatch("approvals.resolve",json!({"approval_id":approval,"decision":"approve","scope":"persistent","limit":2,"no_expiry":true})).await.unwrap();
        let mut next = f.e.clone();
        next.context.run_id = RunId::generate();
        next.session = next
            .runtime
            .begin(
                &next.context,
                "再次完成活动",
                "next-plan",
                &next.settings,
                &next.profile,
                None,
            )
            .unwrap();
        assert_ne!(next.session, f.e.session);
        assert_eq!(
            next.runtime.repository.data.lock().sessions[&next.session]
                .account
                .as_deref(),
            Some("account-approved-once")
        );
        assert_eq!(
            next.runtime.repository.data.lock().sessions[&next.session].cycle,
            None
        );
        let listed = service.dispatch("sessions.read", json!({})).await.unwrap();
        let reference = listed["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == next.session)
            .unwrap();
        assert_eq!(reference["account_reference_only"], true);
        let fresh = next.observe().await.unwrap();
        tools::execute(&next, "set_plan", plan_args(&fresh.id))
            .await
            .unwrap();
        let outcome =
            tools::execute(&next, "act", act("permanent-next-goal", &fresh.id, "item")).await;
        if consistent {
            assert_eq!(outcome.unwrap()["status"], "injected");
        } else {
            assert!(outcome
                .unwrap_err()
                .to_string()
                .starts_with("account_unverified"));
        }
        {
            let data = next.runtime.repository.data.lock();
            assert_eq!(data.rules.len(), 1);
            assert_eq!(data.spends.len(), usize::from(consistent));
            assert_eq!(f.backend.inputs.lock().len(), usize::from(consistent));
        }
        next.runtime
            .repository
            .transaction(|d| {
                d.sessions.get_mut(&next.session).unwrap().state = "partial".into();
                Ok(())
            })
            .unwrap();
        let changed = service
            .dispatch(
                "identity.confirm",
                json!({"session_id":next.session,"account":"different-current-account"}),
            )
            .await;
        if consistent {
            assert!(changed.is_err());
        } else {
            changed.unwrap();
            assert_eq!(
                next.runtime.repository.data.lock().sessions[&next.session].rounds,
                1
            );
            let c: store::Consumption = serde_json::from_value(consumption("item")).unwrap();
            assert!(!next
                .runtime
                .repository
                .gate(
                    &next.session,
                    "different-account-spend",
                    &fresh.id,
                    c,
                    f.clock.now().timestamp()
                )
                .unwrap());
        }
        let mut other = next.context.clone();
        other.run_id = RunId::generate();
        other.app.device_id = DeviceId::new(store::id()).unwrap();
        let unconfirmed = next
            .runtime
            .begin(
                &other,
                "另一设备",
                "next-plan",
                &next.settings,
                &next.profile,
                None,
            )
            .unwrap();
        assert_eq!(
            next.runtime.repository.data.lock().sessions[&unconfirmed].account,
            None
        );
    }
}

#[test]
fn timer_reuses_pending_session_before_identity_without_crossing_device_scope() {
    let f = fixture(vec![], false);
    let service = mock_service(&f);
    let request = RunRequest::for_app(
        f.e.context.app.clone(),
        ID,
        "default#goal",
        RunPayload::new(json!({"goal":"等待活动说明","model_profile_id":"mock"})),
    )
    .unwrap();
    let first = service
        .prepare_request(request.clone(), Some("timer-ai".into()), None, None)
        .unwrap();
    let prepared = service
        .prepared
        .lock()
        .remove(first.payload.as_value()["prepared_id"].as_str().unwrap())
        .unwrap();
    let sid =
        f.e.runtime
            .begin(
                &f.e.context,
                &prepared.goal.goal,
                &prepared.plan_version,
                &prepared.settings,
                &prepared.profile,
                None,
            )
            .unwrap();
    f.e.runtime
        .repository
        .transaction(|data| {
            let s = data.sessions.get_mut(&sid).unwrap();
            s.state = "waiting_user".into();
            s.entrypoint = request.entrypoint.clone();
            s.rounds = 7;
            s.trial_operations = 2;
            Ok(())
        })
        .unwrap();
    let repeated = service
        .prepare_request(request.clone(), Some("timer-ai".into()), None, None)
        .unwrap();
    let reused = service
        .prepared
        .lock()
        .remove(repeated.payload.as_value()["prepared_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(reused.goal.resume_session_id.as_deref(), Some(sid.as_str()));
    {
        let data = f.e.runtime.repository.data.lock();
        let s = &data.sessions[&sid];
        assert!(s.account.is_none() && s.cycle.is_none());
        assert_eq!((s.rounds, s.trial_operations), (7, 2));
        assert_eq!(data.sessions.len(), 2);
    }
    let mut other = request;
    other.app.device_id = DeviceId::new(store::id()).unwrap();
    let prepared = service
        .prepare_request(other, Some("timer-other".into()), None, None)
        .unwrap();
    assert!(
        service.prepared.lock()[prepared.payload.as_value()["prepared_id"].as_str().unwrap()]
            .goal
            .resume_session_id
            .is_none()
    );
}
