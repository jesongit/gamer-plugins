// Real interpreter + component execution with synthetic capability/device services.
// These tests do not claim real-device throughput or input latency acceptance.
mod concurrency_tests {
    use super::*;
    use crate::core::{ActivityKind, ActivityLease, DeviceActivity, RunContext, RunRequest};
    use crate::run_manager::{
        CancelOutcome, RunExecutor, RunManager, RunSource, RunState, StartError, StartRequest,
    };
    use futures_util::future::BoxFuture;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};
    use tower::ServiceExt;

    struct ComponentExecutor {
        runtime: LazyYamlWasmtimeRuntime,
        wasm: Vec<u8>,
        activity: Arc<DeviceActivity>,
        events: HashMap<String, Arc<tests::EventCollect>>,
    }

    impl RunExecutor for ComponentExecutor {
        fn prepare<'a>(
            &'a self,
            _: &'a RunContext,
            _: &'a RunRequest,
        ) -> BoxFuture<'a, anyhow::Result<()>> {
            Box::pin(async { Ok(()) })
        }
        fn acquire(&self, context: &RunContext) -> anyhow::Result<Box<dyn ActivityLease>> {
            Ok(Box::new(self.activity.acquire(
                context.app.device_id.as_str(),
                ActivityKind::Run,
            )))
        }
        fn execute<'a>(
            &'a self,
            context: &'a RunContext,
            request: &'a RunRequest,
            _: bool,
            stop: Arc<AtomicBool>,
        ) -> BoxFuture<'a, anyhow::Result<Vec<(String, String)>>> {
            Box::pin(async move {
                let source = if request.entrypoint == "denied" {
                    "run: [{input_text: denied}]"
                } else if request.entrypoint == "long-sleep" {
                    "run: [{sleep: 5s}]"
                } else {
                    "run:\n  - repeat: 100000\n    do:\n      - sleep: 10ms\n"
                };
                let mut program = wire(source);
                program["trace"] = json!({"run_id": context.run_id.as_str()});
                self.runtime
                    .run(YamlWasmRunRequest {
                        notification: None,
                        wasm: self.wasm.clone(),
                        program,
                        host: host_with_permissions(
                            Arc::new(tests::Trace::default()),
                            &["device.read", "runtime.sleep"],
                        ),
                        context: context.app.clone(),
                        stop,
                        sink: self
                            .events
                            .get(context.app.device_id.as_str())
                            .cloned()
                            .map(|sink| sink as Arc<dyn EventSink>),
                    })
                    .await?;
                Ok(vec![])
            })
        }
    }

    fn start_request(device: &str, entrypoint: &str) -> StartRequest {
        StartRequest {
            request: RunRequest::for_app(
                AppContext::for_test(device, "com.example.game").unwrap(),
                "gamer-yaml",
                entrypoint,
                crate::core::RunPayload::empty(),
            )
            .unwrap(),
            source: RunSource::Manual,
            task_id: None,
            scheduled_at: None,
            realtime_logs: false,
        }
    }

    // Only one Tokio worker: before isolation even one long WASM run prevents
    // the second run, HTTP route, and timer/cancel task from being polled.
    #[tokio::test(flavor = "current_thread")]
    async fn real_yaml_concurrency_keeps_http_responsive_and_cancel_scoped() {
        let runtime = LazyYamlWasmtimeRuntime::new();
        let wasm = guest_component();
        runtime
            .run(run_request(
                wire("run: [{return: ready}]"),
                host_with_permissions(Arc::new(tests::Trace::default()), &["device.read"]),
                Arc::new(AtomicBool::new(false)),
                None,
            ))
            .await
            .unwrap();
        let a_events = tests::EventCollect::new();
        let b_events = tests::EventCollect::new();
        let activity = Arc::new(DeviceActivity::default());
        let manager = Arc::new(RunManager::new(Arc::new(ComponentExecutor {
            runtime,
            wasm,
            activity: activity.clone(),
            events: HashMap::from([
                ("device-a".into(), a_events.clone()),
                ("device-b".into(), b_events.clone()),
            ]),
        })));
        let a = manager
            .submit(start_request("device-a", "long-sleep"), None)
            .unwrap();
        let b = manager
            .submit(start_request("device-b", "loop"), None)
            .unwrap();
        assert_ne!(a.run_id, b.run_id);
        assert!(
            matches!(manager.submit(start_request("device-a", "loop"), None), Err(StartError::Conflict(record)) if record.run_id == a.run_id)
        );

        // Independent watchdog bounds a regression even when Tokio itself is
        // starved; normal cancellation still comes through the HTTP handler.
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let watchdog_manager = manager.clone();
        let watchdog_ids = [a.run_id.clone(), b.run_id.clone()];
        let watchdog = std::thread::spawn(move || {
            if finish_rx.recv_timeout(Duration::from_secs(8)).is_err() {
                for id in watchdog_ids {
                    watchdog_manager.cancel(&id);
                }
            }
        });
        let started = Instant::now();
        tokio::time::timeout(Duration::from_secs(3), async {
            while a_events.of("step_start").is_empty() || b_events.of("step_start").len() < 3 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("both real interpreters must progress on one Tokio worker");
        assert!(activity.has_active("device-a") && activity.has_active("device-b"));
        assert!(a_events
            .of("step_start")
            .iter()
            .all(|event| event["trace"]["run_id"] == a.run_id));
        assert!(b_events
            .of("step_start")
            .iter()
            .all(|event| event["trace"]["run_id"] == b.run_id));
        let cancel_manager = manager.clone();
        let cancel_id = a.run_id.clone();
        let router = axum::Router::new().route(
            "/cancel-a",
            axum::routing::post(move || async move {
                assert_eq!(cancel_manager.cancel(&cancel_id), CancelOutcome::Accepted);
                axum::http::StatusCode::ACCEPTED
            }),
        );
        let response = tokio::time::timeout(
            Duration::from_millis(500),
            router.oneshot(
                axum::http::Request::post("/cancel-a")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .expect("cancel HTTP must stay responsive")
        .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::ACCEPTED);
        let done = tokio::time::timeout(Duration::from_secs(2), manager.wait_terminal(&a.run_id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(done.state, RunState::Cancelled);
        assert!(
            !activity.has_active("device-a"),
            "terminal publication must follow lease cleanup"
        );
        assert!(manager.get_run(&b.run_id).unwrap().state.is_active());
        assert!(activity.has_active("device-b"));
        let b_steps = b_events.of("step_start").len();
        tokio::time::timeout(Duration::from_secs(1), async {
            while b_events.of("step_start").len() <= b_steps {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("cancelling A must not interrupt B's shared engine/store");
        assert_eq!(manager.cancel(&b.run_id), CancelOutcome::Accepted);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), manager.wait_terminal(&b.run_id))
                .await
                .unwrap()
                .unwrap()
                .state,
            RunState::Cancelled
        );
        assert!(!activity.has_active("device-b"));
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "watchdog cancellation is not a passing result"
        );
        finish_tx.send(()).unwrap();
        watchdog.join().unwrap();

        // A failed guest/native call also releases the slot/lease, and the same
        // device can be submitted again after both cancellation and failure.
        let failed = manager
            .submit(start_request("device-a", "denied"), None)
            .unwrap();
        let done = tokio::time::timeout(
            Duration::from_secs(2),
            manager.wait_terminal(&failed.run_id),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(done.state, RunState::Failed);
        assert!(done.error.unwrap().contains("denied"));
        assert!(!activity.has_active("device-a"));
        assert_eq!(manager.active_count(), 0);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn real_yaml_native_sleep_finishes_without_shortening_duration() {
        let runtime = LazyYamlWasmtimeRuntime::new();
        let host = host_with_permissions(
            Arc::new(tests::Trace::default()),
            &["device.read", "runtime.sleep"],
        );
        // Warm compilation before measuring the actual wait.
        runtime
            .run(run_request(
                wire("run: [{return: ready}]"),
                host.clone(),
                Arc::new(AtomicBool::new(false)),
                None,
            ))
            .await
            .unwrap();
        let started = Instant::now();
        let result = runtime
            .run(run_request(
                wire("run: [{sleep: 120ms}, {return: done}]"),
                host,
                Arc::new(AtomicBool::new(false)),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(result.value, json!("done"));
        assert!(started.elapsed() >= Duration::from_millis(120));
    }
}
