//! Isolated browser/native-host integration for the installed cleanup script.
//! The game, account and queue are represented by a loopback HTML fixture.
//! The parser, interpreter, native functions, adapters, matcher and CDP input
//! path are production implementations; no production service is contacted.
use super::*;
use crate::browser::{BrowserTarget, CdpSession};
use crate::capabilities::adapters::{
    DeviceAdapter, FrameAdapter, FrameStore, InputAdapter, LogAdapter, ResourceAdapter,
    TouchAdapter, VisionAdapter,
};
use crate::core::control::{ControlLease, ControlRegistry};
use crate::extensions::HostApiCatalog;
use base64::Engine as _;
use std::sync::atomic::AtomicUsize;
use tokio::sync::Semaphore;

fn pattern(seed: u32) -> Vec<u8> {
    let mut state = seed;
    let mut image = image::GrayImage::new(64, 40);
    // Block texture gives the production coarse matcher a correlated neighbour
    // on either pixel parity, while retaining a unique exact-match target.
    for by in (0..40).step_by(8) {
        for bx in (0..64).step_by(8) {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let value = if state & 0x8000_0000 == 0 { 24 } else { 232 };
            for y in by..by + 8 {
                for x in bx..bx + 8 {
                    image.put_pixel(x, y, image::Luma([value]));
                }
            }
        }
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

fn fixture_html(images: &std::collections::BTreeMap<&str, Vec<u8>>) -> String {
    let urls: std::collections::BTreeMap<_, _> = images
        .iter()
        .map(|(key, bytes)| {
            (
                *key,
                format!(
                    "data:image/png;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                ),
            )
        })
        .collect();
    format!(
        r#"<!doctype html><meta charset="utf-8"><style>
html,body{{margin:0;background:#234;width:640px;height:480px;overflow:hidden}}
button{{position:absolute;border:0;padding:0;background:transparent;width:64px;height:40px}}
img{{display:block;width:64px;height:40px}}[hidden]{{display:none!important}}
#enter{{left:230px;top:220px}}#more{{left:540px;top:24px}}
#logout{{left:510px;top:90px}}#confirm{{left:280px;top:300px}}
</style><button id="enter"></button><button id="more"></button>
<button id="logout" hidden></button><button id="confirm" hidden></button>
<script>
const urls={};
window.fixtureErrors=[];addEventListener('error',e=>fixtureErrors.push(e.message));
for(const id of ['enter','more','logout','confirm'])document.getElementById(id).innerHTML='<img src="'+urls[id]+'">';
window.loads=Number(sessionStorage.loads||0)+1;sessionStorage.loads=loads;
function event(name){{const events=JSON.parse(sessionStorage.events||'[]');events.push(name);sessionStorage.events=JSON.stringify(events)}}
event('load:'+loads);
function menu(){{document.getElementById('logout').hidden=false}}
document.getElementById('more').onmouseenter=()=>{{event('more-hover');menu()}};
document.getElementById('more').onclick=()=>{{event('more-click');menu()}};
document.getElementById('enter').onclick=()=>event('unexpected-enter-click');
document.getElementById('logout').onclick=()=>{{event('logout');document.getElementById('logout').hidden=true;document.getElementById('confirm').hidden=false}};
document.getElementById('confirm').onclick=()=>{{event('confirm');sessionStorage.signedOut='yes';for(const id of ['enter','more','logout','confirm'])document.getElementById(id).hidden=true;document.body.append('signed out')}};
</script>"#,
        serde_json::to_string(&urls).unwrap()
    )
}

struct BrowserNativeHost {
    native: Arc<NativeYamlHost>,
    runtime: tokio::runtime::Handle,
    controls: Arc<ControlRegistry>,
    lease: ControlLease,
    session: Arc<CdpSession>,
    inject_refresh_race: AtomicBool,
    reload_seen: Arc<Semaphore>,
    allow_reload: Arc<Semaphore>,
    calls: std::sync::Mutex<Vec<String>>,
}
impl yaml_interp::HostFunctions for BrowserNativeHost {
    fn invoke(&self, name: &str, args: Value) -> Result<Value, yaml_interp::HostError> {
        self.calls.lock().unwrap().push(name.into());
        self.runtime
            .block_on(async {
                if name == "wait_find" && self.inject_refresh_race.swap(false, Ordering::SeqCst) {
                    // The HTTP response is deliberately held until reset_input
                    // has returned and the unchanged YAML begins its wait.
                    tokio::time::timeout(Duration::from_secs(5), self.reload_seen.acquire())
                        .await?
                        .map_err(|error| anyhow!(error))?
                        .forget();
                    self.session.invalidate_next_captures_for_test(2);
                    self.allow_reload.add_permits(1);
                }
                self.controls
                    .execute(&self.lease, self.native.call_function(name, args))
                    .await
            })
            .map_err(|error| {
                yaml_interp::HostError::new(
                    yaml_interp::HostErrorKind::Failed,
                    format!("{error:#}"),
                )
            })
    }
}

fn program(source: &str) -> yaml_interp::Program {
    let script = crate::extensions::gamer_yaml::syntax::parse_script(source).unwrap();
    let mut vars: JsonMap<String, Value> = script
        .params
        .iter()
        .filter_map(|param| {
            param
                .default
                .clone()
                .map(|value| (param.name.clone(), value))
        })
        .collect();
    vars.extend(script.vars.iter().cloned());
    serde_json::from_value(crate::extensions::gamer_yaml::syntax::build_program(
        &script,
        &vec![],
        vars,
        0,
    ))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires installed browser and local cleanup YAML; isolated real native/CDP logout after main failure"]
async fn failed_main_run_still_resets_and_logs_out_during_refresh() {
    use axum::{response::Html, routing::get, Router};
    let templates = std::collections::BTreeMap::from([
        ("enter", pattern(11)),
        ("more", pattern(23)),
        ("logout", pattern(37)),
        ("confirm", pattern(49)),
        ("later", pattern(61)),
    ]);
    let html = Arc::new(fixture_html(&templates));
    let reload_seen = Arc::new(Semaphore::new(0));
    let allow_reload = Arc::new(Semaphore::new(0));
    let requests = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let app = Router::new().route(
        "/",
        get({
            let reload_seen = reload_seen.clone();
            let allow_reload = allow_reload.clone();
            move || {
                let html = html.clone();
                let reload_seen = reload_seen.clone();
                let allow_reload = allow_reload.clone();
                let requests = requests.clone();
                async move {
                    if requests.fetch_add(1, Ordering::SeqCst) > 0 {
                        reload_seen.add_permits(1);
                        allow_reload.acquire().await.unwrap().forget();
                    }
                    Html((*html).clone())
                }
            }
        }),
    );
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let cfg = crate::config::Config {
        data_dir: dir.path().into(),
        ..Default::default()
    };
    let db = Arc::new(crate::store::Store::open(&cfg).unwrap());
    let resources = Arc::new(crate::resources::PackageStore::open(&cfg).unwrap());
    resources
        .create_package(crate::resources::PackageInput {
            id: "cleanup-fixture".into(),
            web_url_prefixes: vec![url.clone()],
            ..Default::default()
        })
        .unwrap();
    for (id, name) in [
        (
            "enter",
            "live_cloud_enter_game_20261015#340_430_480_565.png",
        ),
        ("more", "live_cloud_more_20261015#825_25_965_158.png"),
        ("logout", "live_cloud_logout_20261015#780_165_920_295.png"),
        (
            "confirm",
            "live_cloud_logout_confirm_20261015#420_600_560_735.png",
        ),
        (
            "later",
            "live_cloud_add_desktop_later_20261015#30_820_170_955.png",
        ),
    ] {
        resources
            .write_binary(
                "cleanup-fixture",
                YAML_EXTENSION_ID,
                &format!("templates/{name}"),
                &templates[id],
                None,
                false,
            )
            .unwrap();
    }
    let devices = Arc::new(crate::device::DeviceManager::new(db.clone(), cfg));
    let target = BrowserTarget {
        id: "browser-refresh-cleanup-fixture".into(),
        name: "isolated cleanup".into(),
        url,
        profile_id: "refresh-cleanup-fixture".into(),
        width: 640,
        height: 480,
    };
    db.save_browser_target(target.clone()).unwrap();
    let session = devices.browsers.prepare(&target.id).await.unwrap();
    session.capture_for_prepare().await.unwrap();
    let before = session.stamp();
    let device = Arc::new(DeviceAdapter::new(devices.clone()));
    let touch = Arc::new(TouchAdapter::new(device.clone()));
    let frames = Arc::new(FrameStore::new());
    let resource = Arc::new(ResourceAdapter::new(resources));
    let registry = CapabilityRegistry::builder()
        .with_device_service(device.clone())
        .with_input_service(Arc::new(InputAdapter::new(device, touch.clone())))
        .with_touch_service(touch)
        .with_frame_service(Arc::new(FrameAdapter::new(devices.clone(), frames.clone())))
        .with_vision_service(Arc::new(VisionAdapter::new(frames, resource.clone())))
        .with_resource_service(resource)
        .with_log(LogAdapter::new(db))
        .build();
    let manifest =
        crate::extensions::parse_manifest(YAML_EXTENSION_MANIFEST_TOML.as_bytes()).unwrap();
    let api = HostApi::for_manifest(registry, HostApiCatalog::default(), &manifest).unwrap();
    let context = AppContext {
        device_id: crate::core::DeviceId::new(target.id.clone()).unwrap(),
        android_package: None,
        content_package: Some(crate::core::AppPackageId::new("cleanup-fixture").unwrap()),
    };
    let native = Arc::new(
        NativeYamlHost::new(api, context, Arc::new(AtomicBool::new(false)), None)
            .await
            .unwrap(),
    );
    let controls = devices.controls.clone();
    let main_lease = controls
        .claim(&target.id, "fixture-failed-main")
        .await
        .unwrap();
    let main_bridge = BrowserNativeHost {
        native: native.clone(),
        runtime: tokio::runtime::Handle::current(),
        controls: controls.clone(),
        lease: main_lease.clone(),
        session: session.clone(),
        inject_refresh_race: AtomicBool::new(false),
        reload_seen: reload_seen.clone(),
        allow_reload: allow_reload.clone(),
        calls: Default::default(),
    };
    let main_program =
        program("run:\n  - fail: {code: PROBE_MAIN_FAILURE, message: intentional main failure}\n");
    let failed =
        tokio::task::spawn_blocking(move || yaml_interp::run(&main_program, &main_bridge, None))
            .await
            .unwrap();
    assert!(failed.as_ref().unwrap_err().contains("PROBE_MAIN_FAILURE"));
    controls
        .release(&main_lease, async {
            session.release_inputs().await;
            Ok(())
        })
        .await
        .unwrap();
    let cleanup_lease = controls
        .claim(&target.id, "fixture-failure-cleanup")
        .await
        .unwrap();
    let check = async {
        let source = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("data/packages/hsr/plugins/gamer-yaml/automations/直播云游戏收尾.yaml"),
        )
        .unwrap();
        let cleanup_program = program(&source);
        let bridge = BrowserNativeHost {
            native,
            runtime: tokio::runtime::Handle::current(),
            controls: controls.clone(),
            lease: cleanup_lease.clone(),
            session: session.clone(),
            inject_refresh_race: AtomicBool::new(true),
            reload_seen,
            allow_reload,
            calls: Default::default(),
        };
        let attempts = session.capture_attempts_for_test();
        let completed = tokio::time::timeout(
            Duration::from_secs(30),
            tokio::task::spawn_blocking(move || {
                let result = yaml_interp::run(&cleanup_program, &bridge, None);
                (result, bridge.calls.into_inner().unwrap())
            }),
        )
        .await
        .unwrap()
        .unwrap();
        let dom = session.evaluate_for_test("({events:sessionStorage.events,signedOut:sessionStorage.signedOut,errors:fixtureErrors,targets:Object.fromEntries(['enter','more','logout','confirm'].map(id=>{const el=document.getElementById(id);return [id,{hidden:el.hidden,box:el.getBoundingClientRect().toJSON()}]}))})").await;
        assert!(
            completed.0.is_ok(),
            "cleanup must finish after refresh races: {:?}; calls={:?}; dom={dom}",
            completed.0,
            completed.1
        );
        assert_eq!(
            dom["errors"],
            json!([]),
            "fixture JavaScript must remain error-free: {dom}"
        );
        assert_eq!(
            completed
                .1
                .iter()
                .filter(|name| *name == "reset_input")
                .count(),
            1
        );
        assert!(completed.1.iter().any(|name| name == "hover"));
        assert_eq!(session.evaluate_for_test("loads").await, json!(2));
        assert_eq!(
            session.evaluate_for_test("sessionStorage.signedOut").await,
            json!("yes")
        );
        let events = session
            .evaluate_for_test("JSON.parse(sessionStorage.events)")
            .await;
        assert_eq!(
            events,
            json!(["load:1", "load:2", "more-hover", "logout", "confirm"])
        );
        assert!(session.capture_attempts_for_test() >= attempts + 6);
        assert_eq!(session.stamp().target, before.target);
        assert_eq!(session.stamp().epoch, before.epoch);
        assert!(controls
            .execute(
                &cleanup_lease,
                session.input(&json!({"type":"tap","x":20,"y":30}), Some(&before))
            )
            .await
            .is_err());
        assert!(
            failed.is_err(),
            "cleanup success must not rewrite the main failure result"
        );
    };
    let result = futures_util::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(check)).await;
    controls
        .release(&cleanup_lease, async {
            session.release_inputs().await;
            Ok(())
        })
        .await
        .unwrap();
    session.close().await;
    server.abort();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
