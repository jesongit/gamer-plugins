use super::*;
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

struct Host {
    result: Value,
    calls: Mutex<Vec<(String, Value)>>,
    cancel_after_match: bool,
    cancelled: AtomicBool,
    fail: bool,
}
impl Host {
    fn new(result: Value) -> Self {
        Self {
            result,
            calls: Mutex::new(Vec::new()),
            cancel_after_match: false,
            cancelled: AtomicBool::new(false),
            fail: false,
        }
    }
}
impl HostFunctions for Host {
    fn invoke(&self, name: &str, args: Value) -> Result<Value, HostError> {
        self.calls.lock().unwrap().push((name.into(), args));
        if self.fail {
            return Err(HostError::new(HostErrorKind::Failed, "fixture failure"));
        }
        if name == "find_any" {
            self.cancelled
                .store(self.cancel_after_match, Ordering::Relaxed);
            Ok(self.result.clone())
        } else {
            Ok(Value::Null)
        }
    }
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

fn program() -> Program {
    serde_json::from_value(json!({
        "vars":{"hit":"outer"},
        "run":[{
            "op":"match_templates", "path":"run[0]", "args":{"expr":"lit","value":{"templates":["a","b"]}},
            "cases":[
                {"do":[{"op":"return","path":"run[0].cases[0].do[0]","value":{"expr":"lit","value":"first"}}]},
                {"as":"hit", "do":[{"op":"fn","fn":"log","path":"run[0].cases[1].do[0]","args":{"expr":"ref","path":"hit.template"}}]}
            ],
            "else":[{"op":"return","path":"run[0].else[0]","value":{"expr":"lit","value":"miss"}}]
        }, {"op":"return","path":"run[1]","value":{"expr":"ref","path":"hit"}}]
    })).unwrap()
}

#[test]
fn first_branch_only_local_binding_and_return_propagate() {
    let host = Host::new(json!({"index":1,"template":"b"}));
    assert_eq!(run(&program(), &host, None).unwrap(), "outer");
    assert_eq!(
        *host.calls.lock().unwrap(),
        vec![
            ("find_any".into(), json!({"templates":["a","b"]})),
            ("log".into(), json!("b"))
        ]
    );
    let host = Host::new(json!({"index":0,"template":"a"}));
    assert_eq!(run(&program(), &host, None).unwrap(), "first");
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    assert_eq!(
        run(&program(), &Host::new(Value::Null), None).unwrap(),
        "miss"
    );
    let mut scoped = program();
    let StepKind::MatchTemplates { cases, .. } = &mut scoped.run[0].kind else {
        panic!()
    };
    cases[1].save_as = Some("__return".into());
    cases[1].body = vec![Step {
        path: "run[0].cases[1].do[0]".into(),
        desc: String::new(),
        kind: StepKind::Return {
            value: Expr::Ref {
                path: "__return.template".into(),
            },
        },
    }];
    assert_eq!(
        run(&scoped, &Host::new(json!({"index":1,"template":"b"})), None).unwrap(),
        "b"
    );
}

#[test]
fn branch_errors_cancellation_and_invalid_host_results_do_not_fall_through() {
    let mut host = Host::new(json!({"index":1,"template":"b"}));
    host.cancel_after_match = true;
    assert!(run(&program(), &host, None)
        .unwrap_err()
        .starts_with("CANCELLED"));
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    let mut host = Host::new(Value::Null);
    host.fail = true;
    assert!(run(&program(), &host, None)
        .unwrap_err()
        .contains("fixture failure"));
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    for result in [json!({}), json!({"index":-1}), json!({"index":12})] {
        assert!(run(&program(), &Host::new(result), None).is_err());
    }
}

#[test]
fn nested_actions_keep_event_paths_and_step_budget() {
    #[derive(Default)]
    struct Events(Mutex<Vec<Value>>);
    impl EventSink for Events {
        fn emit(&self, e: Value) {
            self.0.lock().unwrap().push(e);
        }
    }
    let events = Events::default();
    let mut program = program();
    let StepKind::MatchTemplates { cases, .. } = &mut program.run[0].kind else {
        panic!()
    };
    cases[1].body = vec![Step {
        path: "run[0].cases[1].do[0]".into(),
        desc: "循环".into(),
        kind: StepKind::Repeat {
            times: Expr::Lit {
                value: json!(MAX_STEPS),
            },
            body: vec![],
        },
    }];
    let error = run(&program, &Host::new(json!({"index":1})), Some(&events)).unwrap_err();
    assert!(error.starts_with("STEP_BUDGET_EXCEEDED"));
    let events = events.0.lock().unwrap();
    assert!(events.iter().any(|e| e["ev"] == "step_end"
        && e["path"] == "run[0].cases[1].do[0]"
        && e["ok"] == false));
    assert!(events.iter().any(|e| e["ev"] == "budget"));
}

struct PollingHost {
    results: Mutex<std::collections::VecDeque<Value>>,
    calls: Mutex<Vec<(String, Value)>>,
    cancelled: AtomicBool,
    cancel_on_sleep: bool,
    fail_on: Option<&'static str>,
}
impl PollingHost {
    fn new(results: Vec<Value>) -> Self {
        Self {
            results: Mutex::new(results.into()),
            calls: Mutex::new(vec![]),
            cancelled: AtomicBool::new(false),
            cancel_on_sleep: false,
            fail_on: None,
        }
    }
}
impl HostFunctions for PollingHost {
    fn invoke(&self, name: &str, args: Value) -> Result<Value, HostError> {
        self.calls.lock().unwrap().push((name.into(), args));
        if self.fail_on == Some(name) {
            return Err(HostError::new(
                HostErrorKind::Failed,
                "poll fixture failure",
            ));
        }
        if name == "find_any" {
            return Ok(self
                .results
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Value::Null));
        }
        if name == "sleep" && self.cancel_on_sleep {
            self.cancelled.store(true, Ordering::Relaxed);
            return Err(HostError::new(
                HostErrorKind::Cancelled,
                "cancelled in interval",
            ));
        }
        Ok(Value::Null)
    }
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

fn polling_program(times: Value) -> Program {
    serde_json::from_value(json!({
        "vars":{"hit":"outer", "rounds":3, "gap":"400ms"},
        "run":[{
            "op":"match_templates", "path":"run[0]", "times":{"expr":"lit","value":times}, "interval":{"expr":"ref","path":"gap"},
            "args":{"expr":"lit","value":{"templates":["a","b"],"threshold":0.8}},
            "cases":[
                {"as":"hit", "do":[{"op":"fn","fn":"log","path":"run[0].cases[0].do[0]","args":{"expr":"ref","path":"hit.template"}}]},
                {"as":"hit", "do":[{"op":"fn","fn":"log","path":"run[0].cases[1].do[0]","args":{"expr":"ref","path":"hit.template"}}]}
            ],
            "else":[{"op":"fn","fn":"log","path":"run[0].else[0]","args":{"expr":"lit","value":"miss"}}]
        }, {"op":"return","path":"run[1]","value":{"expr":"ref","path":"hit"}}]
    })).unwrap()
}

#[test]
fn polling_executes_hit_and_miss_rounds_and_waits_only_between_rounds() {
    let host = PollingHost::new(vec![
        Value::Null,
        json!({"index":1,"template":"b"}),
        json!({"index":0,"template":"a"}),
    ]);
    let mut program = polling_program(json!(3));
    let StepKind::MatchTemplates { times, .. } = &mut program.run[0].kind else {
        panic!()
    };
    *times = Expr::Ref {
        path: "rounds".into(),
    };
    assert_eq!(run(&program, &host, None).unwrap(), "outer");
    let calls = host.calls.lock().unwrap();
    assert_eq!(
        calls
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["find_any", "log", "sleep", "find_any", "log", "sleep", "find_any", "log"]
    );
    assert_eq!(calls[1].1, "miss");
    assert_eq!(calls[4].1, "b");
    assert_eq!(calls[7].1, "a");
    assert_eq!(calls[2].1, json!({"duration":"400ms"}));
    assert_eq!(calls[5].1, json!({"duration":"400ms"}));
}

#[test]
fn polling_break_is_local_and_single_shot_break_still_exits_outer_repeat() {
    for count in [1, 3] {
        let mut program = polling_program(json!(count));
        let mut branch = program.run.remove(0);
        let StepKind::MatchTemplates { cases, .. } = &mut branch.kind else {
            panic!()
        };
        cases[0].body = vec![Step {
            path: "break".into(),
            desc: String::new(),
            kind: StepKind::Break,
        }];
        let after = Step {
            path: "after".into(),
            desc: String::new(),
            kind: StepKind::Fn {
                name: "log".into(),
                args: Some(Expr::Lit {
                    value: json!("after"),
                }),
                save_as: None,
            },
        };
        program.run.insert(
            0,
            Step {
                path: "outer".into(),
                desc: String::new(),
                kind: StepKind::Repeat {
                    times: Expr::Lit { value: json!(2) },
                    body: vec![branch, after],
                },
            },
        );
        let host = PollingHost::new(vec![json!({"index":0}), json!({"index":0})]);
        assert_eq!(run(&program, &host, None).unwrap(), "outer");
        let calls = host.calls.lock().unwrap();
        let names: Vec<_> = calls.iter().map(|(name, _)| name.as_str()).collect();
        if count == 1 {
            assert_eq!(names, ["find_any"]);
        } else {
            assert_eq!(names, ["find_any", "log", "find_any", "log"]);
        }
    }
}

#[test]
fn polling_return_and_errors_do_not_retry_or_wait() {
    let mut program = polling_program(json!(3));
    let StepKind::MatchTemplates { cases, .. } = &mut program.run[0].kind else {
        panic!()
    };
    cases[0].body = vec![Step {
        path: "return".into(),
        desc: String::new(),
        kind: StepKind::Return {
            value: Expr::Lit {
                value: json!("done"),
            },
        },
    }];
    let host = PollingHost::new(vec![json!({"index":0})]);
    assert_eq!(run(&program, &host, None).unwrap(), "done");
    assert_eq!(host.calls.lock().unwrap().len(), 1);
    for function in ["find_any", "log", "sleep"] {
        let mut host = PollingHost::new(vec![Value::Null]);
        host.fail_on = Some(function);
        assert!(run(&polling_program(json!(3)), &host, None)
            .unwrap_err()
            .contains("poll fixture failure"));
        assert_eq!(
            host.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(name, _)| name == "find_any")
                .count(),
            1
        );
    }
}

#[test]
fn polling_cancellation_during_interval_stops_before_next_capture() {
    let mut host = PollingHost::new(vec![Value::Null]);
    host.cancel_on_sleep = true;
    assert!(run(&polling_program(json!(999)), &host, None)
        .unwrap_err()
        .contains("cancelled"));
    assert_eq!(
        host.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == "find_any")
            .count(),
        1
    );
}

#[test]
fn polling_rejects_invalid_runtime_counts_before_matching() {
    for count in [json!(0), json!(-1), json!(1.5), json!("3"), Value::Null] {
        let host = PollingHost::new(vec![]);
        assert!(run(&polling_program(count), &host, None)
            .unwrap_err()
            .contains("匹配次数必须是正整数"));
        assert!(host.calls.lock().unwrap().is_empty());
    }
}

#[test]
fn empty_polling_rounds_are_bounded_by_step_budget() {
    struct EmptyHost;
    impl HostFunctions for EmptyHost {
        fn invoke(&self, _: &str, _: Value) -> Result<Value, HostError> {
            Ok(Value::Null)
        }
    }
    let mut program = polling_program(json!(MAX_STEPS));
    let StepKind::MatchTemplates { else_steps, .. } = &mut program.run[0].kind else {
        panic!()
    };
    else_steps.clear();
    assert!(run(&program, &EmptyHost, None)
        .unwrap_err()
        .starts_with("STEP_BUDGET_EXCEEDED"));
}
