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
            decision("act", act("op-1", "CURRENT", "regenerative_resource")),
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
    extensions.disable(&id).await.unwrap();
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
