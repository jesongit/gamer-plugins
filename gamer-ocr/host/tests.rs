use super::*;

#[test]
fn relative_data_roots_discover_bundled_dependencies_without_changing_cwd() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("data")).unwrap();
    let deps = root.path().join("deps").join("tesseract");
    std::fs::create_dir_all(deps.join("tessdata")).unwrap();
    let filename = if cfg!(windows) {
        "tesseract.exe"
    } else {
        "tesseract"
    };
    std::fs::write(deps.join(filename), b"fixture engine; discovery only").unwrap();
    std::fs::write(
        deps.join("tessdata/eng.traineddata"),
        b"fixture model; discovery only",
    )
    .unwrap();
    let anchored = settings::anchor_data_root(root.path(), Path::new("data")).unwrap();
    assert!(anchored.is_absolute());
    let engine = settings::resolve(&settings::Settings::default(), &anchored).unwrap();
    assert_eq!(
        engine.executable,
        deps.join(filename).canonicalize().unwrap()
    );
    assert_eq!(engine.languages, vec!["eng"]);
}

#[cfg(windows)]
#[test]
fn engine_arguments_keep_windows_language_file_access_compatible() {
    assert_eq!(
        settings::engine_argument_path(Path::new(r"\\?\D:\deps\tesseract\tessdata")),
        PathBuf::from(r"D:\deps\tesseract\tessdata")
    );
    assert_eq!(
        settings::engine_argument_path(Path::new(r"\\?\UNC\host\share\tessdata")),
        PathBuf::from(r"\\host\share\tessdata")
    );
    assert_eq!(
        settings::engine_argument_path(Path::new(r"D:\deps\tesseract.exe")),
        PathBuf::from(r"D:\deps\tesseract.exe")
    );
}

const HEADER:&str="level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n";

#[test]
fn tsv_keeps_chinese_lines_confidence_and_original_coordinates() {
    let tsv=format!("{HEADER}1\t1\t0\t0\t0\t0\t0\t0\t400\t200\t-1\t\n5\t1\t1\t1\t1\t1\t20\t40\t40\t20\t90\t排队\n5\t1\t1\t1\t1\t2\t60\t40\t40\t20\t80\t时间\n5\t1\t1\t1\t2\t1\t20\t100\t60\t30\t95\t10\n5\t1\t1\t1\t2\t2\t100\t100\t60\t30\t75\tminutes\n");
    let result = parser::parse_tsv(tsv.as_bytes(), 200, 100, 400, 200).unwrap();
    assert_eq!(result.text, "排队时间\n10 minutes");
    assert_eq!(result.lines.len(), 2);
    assert_eq!(
        (
            result.lines[0].x,
            result.lines[0].y,
            result.lines[0].width,
            result.lines[0].height
        ),
        (10, 20, 40, 10)
    );
    assert!((result.lines[0].confidence - 0.85).abs() < 1e-8);
    assert_eq!((result.words[2].x, result.words[2].y), (10, 50));
    assert!(result.confidence >= 0.0 && result.confidence <= 1.0);
}

#[test]
fn empty_tsv_is_success_and_malformed_output_is_rejected() {
    let empty = parser::parse_tsv(HEADER.as_bytes(), 10, 10, 10, 10).unwrap();
    assert!(empty.text.is_empty() && empty.words.is_empty() && empty.lines.is_empty());
    assert_eq!(empty.confidence, 0.0);
    assert!(parser::parse_tsv(b"not tsv", 10, 10, 10, 10).is_err());
    let infinite = format!("{HEADER}5\t1\t1\t1\t1\t1\t0\t0\t1\t1\tNaN\ttext\n");
    assert!(parser::parse_tsv(infinite.as_bytes(), 10, 10, 10, 10).is_err());
}

#[test]
fn noninteger_scale_clamps_edges_without_losing_pixels() {
    let tsv = format!("{HEADER}5\t1\t1\t1\t1\t1\t1\t1\t14\t14\t100\t角\n");
    let result = parser::parse_tsv(tsv.as_bytes(), 10, 10, 15, 15).unwrap();
    assert_eq!(
        (
            result.words[0].x,
            result.words[0].y,
            result.words[0].width,
            result.words[0].height
        ),
        (0, 0, 10, 10)
    );
}

fn png(width: u32, height: u32) -> String {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(
        width,
        height,
        image::Luma([255]),
    ))
    .write_to(&mut bytes, ImageFormat::Png)
    .unwrap();
    base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
}

#[test]
fn request_decoding_and_limits_are_checked_before_process_execution() {
    let engine = settings::Engine {
        executable: "unused".into(),
        tessdata: "unused".into(),
        languages: vec!["chi_sim".into(), "eng".into()],
    };
    let request: Request =
        serde_json::from_value(json!({"png_base64":png(40,20),"scale":2})).unwrap();
    assert_eq!(request.validate(&engine).unwrap(), 6);
    let prepared = request.prepare().unwrap();
    assert_eq!(
        (
            prepared.width,
            prepared.height,
            prepared.scaled_width,
            prepared.scaled_height
        ),
        (40, 20, 80, 40)
    );
    for extra in [
        json!({"mode":"--psm"}),
        json!({"language":"../../eng"}),
        json!({"language":"fra"}),
        json!({"timeout_ms":15001}),
        json!({"scale":0}),
        json!({"whitelist":"a\n"}),
    ] {
        let mut value = json!({"png_base64":png(2,2)});
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let bad: Request = serde_json::from_value(value).unwrap();
        assert!(bad.validate(&engine).is_err());
    }
    let huge: Request = serde_json::from_value(json!({"png_base64":png(4096,1024)})).unwrap();
    assert!(huge.prepare().is_err());
    let bad: Request = serde_json::from_value(
        json!({"png_base64":base64::engine::general_purpose::STANDARD.encode(b"not PNG")}),
    )
    .unwrap();
    assert!(bad.prepare().is_err());
}

#[tokio::test]
async fn private_settings_roundtrip_does_not_depend_on_package_resources() {
    let root = tempfile::tempdir().unwrap();
    let service = OcrService::new(root.path()).unwrap();
    let settings = service
        .call(
            "settings.save",
            json!({"engine_path":"","tessdata_path":""}),
        )
        .await
        .unwrap();
    assert!(settings["version"].is_string());
    let reopened = OcrService::new(root.path()).unwrap();
    assert_eq!(
        reopened.call("settings.read", json!({})).await.unwrap(),
        settings
    );
    assert!(reopened
        .call(
            "settings.save",
            json!({"engine_path":"","tessdata_path":"","expected_version":"old"})
        )
        .await
        .is_err());
    assert!(reopened
        .call(
            "settings.save",
            json!({"engine_path":"relative.exe","tessdata_path":""})
        )
        .await
        .is_err());
    assert!(root
        .path()
        .join("extension-data/gamer-ocr/private/settings.dat")
        .is_file());
    assert!(!root.path().join("packages").exists());
}

#[tokio::test]
async fn damaged_private_configuration_does_not_prevent_optional_service_construction() {
    let root = tempfile::tempdir().unwrap();
    let path = root
        .path()
        .join("extension-data/gamer-ocr/private/settings.dat");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let corrupt = b"damaged protected OCR settings";
    std::fs::write(&path, corrupt).unwrap();
    let service = OcrService::new(root.path())
        .expect("optional OCR configuration must not block Core startup");
    for (action, values) in [
        ("settings.read", json!({})),
        ("status", json!({})),
        ("recognize", json!({"png_base64":png(2,2),"language":"eng"})),
        (
            "settings.save",
            json!({"engine_path":"","tessdata_path":""}),
        ),
    ] {
        let error = service.call(action, values).await.unwrap_err();
        assert!(
            error.to_string().contains("配置"),
            "{action} must explicitly report private configuration failure: {error}"
        );
    }
    assert_eq!(
        std::fs::read(path).unwrap(),
        corrupt,
        "failed calls must not replace damaged or foreign-identity configuration"
    );
}

#[tokio::test]
async fn encoded_waiting_requests_have_a_bounded_shared_queue() {
    let root = tempfile::tempdir().unwrap();
    let service = OcrService::new(root.path()).unwrap();
    let admitted = (0..16)
        .map(|_| Pending::new(service.state.clone()).unwrap())
        .collect::<Vec<_>>();
    assert!(Pending::new(service.state.clone()).is_err());
    assert_eq!(service.state.pending.load(Ordering::Acquire), 16);
    drop(admitted);
    assert_eq!(service.state.pending.load(Ordering::Acquire), 0);
}

#[test]
fn downloaded_archive_and_package_executables_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    for dir in [
        "extensions/gamer-ocr/0.1.0",
        "packages/default/plugins/gamer-ocr",
    ] {
        let path = root.path().join(dir).join("tesseract.exe");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"fixture").unwrap();
        assert!(settings::checked_path(path.to_str().unwrap(), root.path(), false).is_err());
    }
}

// This launches an actual sleeping OS child only to validate ownership and
// shutdown. It does not claim to verify OCR quality or Tesseract execution.
fn sleeper() -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 30",
        ]);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("sleep");
        command.arg("30");
        command
    }
}

async fn running(state: &State) -> u32 {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pid = state.active_pid.load(Ordering::Acquire);
            if pid != 0 {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[cfg(windows)]
async fn assert_exited(pid: u32) {
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile","-NonInteractive","-Command",&format!("if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 1 }} else {{ exit 0 }}")]).creation_flags(0x08000000);
    assert!(
        command.status().await.unwrap().success(),
        "fixture child remains alive"
    );
}
#[cfg(not(windows))]
async fn assert_exited(pid: u32) {
    assert!(!Path::new(&format!("/proc/{pid}")).exists());
}

#[tokio::test]
async fn timeout_kills_and_reaps_the_real_child() {
    let root = tempfile::tempdir().unwrap();
    let service = OcrService::new(root.path()).unwrap();
    let state = service.state.clone();
    let observer = state.clone();
    let cancel = state.generation.subscribe();
    let (abort, aborted) = oneshot::channel();
    let task = tokio::spawn(run_process(
        sleeper(),
        vec![],
        Instant::now() + Duration::from_millis(800),
        cancel,
        aborted,
        state,
    ));
    let pid = running(&observer).await;
    assert!(task
        .await
        .unwrap()
        .unwrap_err()
        .to_string()
        .contains("超时"));
    drop(abort);
    assert_eq!(observer.active_pid.load(Ordering::Acquire), 0);
    assert_exited(pid).await;
}

#[tokio::test]
async fn service_stop_and_dropped_caller_wait_for_real_process_exit() {
    for via_stop in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let service = OcrService::new(root.path()).unwrap();
        let permit = service.state.workers.clone().acquire_owned().await.unwrap();
        let cancel = service.state.generation.subscribe();
        let (abort, aborted) = oneshot::channel();
        let state = service.state.clone();
        let task = tokio::spawn(async move {
            let _permit = permit;
            run_process(
                sleeper(),
                vec![],
                Instant::now() + Duration::from_secs(20),
                cancel,
                aborted,
                state,
            )
            .await
        });
        let pid = running(&service.state).await;
        if via_stop {
            service.stop().await;
        } else {
            drop(abort);
            let _ = service.state.workers.clone().acquire_owned().await;
        }
        assert!(task.await.unwrap().is_err());
        assert_eq!(service.state.active_pid.load(Ordering::Acquire), 0);
        assert_exited(pid).await;
    }
}

fn archive(manifest: &str) -> Vec<u8> {
    use std::io::Write as _;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file("manifest.toml", zip::write::SimpleFileOptions::default())
        .unwrap();
    writer.write_all(manifest.as_bytes()).unwrap();
    writer.finish().unwrap().into_inner()
}

async fn managed_fixture() -> (
    tempfile::TempDir,
    Arc<OcrService>,
    Arc<crate::extensions::ExtensionService>,
    crate::extensions::ExtensionId,
) {
    let root = tempfile::tempdir().unwrap();
    let data_root = root.path().join("data");
    std::fs::create_dir_all(&data_root).unwrap();
    let deps = root.path().join("deps/tesseract");
    std::fs::create_dir_all(deps.join("tessdata")).unwrap();
    std::fs::write(
        deps.join(if cfg!(windows) {
            "tesseract.exe"
        } else {
            "tesseract"
        }),
        b"test process factory replaces this executable",
    )
    .unwrap();
    std::fs::write(
        deps.join("tessdata/eng.traineddata"),
        b"not an OCR model; lifecycle test only",
    )
    .unwrap();
    let ocr = Arc::new(OcrService::new(&data_root).unwrap());
    *ocr.state.child_fixture.lock() = Some(sleeper);
    let manager = Arc::new(
        crate::extensions::ExtensionService::for_data_root(
            &data_root,
            crate::capabilities::CapabilityRegistry::default(),
        )
        .with_builtin_service(ocr.clone()),
    );
    let id = crate::extensions::ExtensionId::parse(ID).unwrap();
    manager
        .install(&archive(include_str!("../manifest.toml")))
        .await
        .unwrap();
    manager.enable(&id).await.unwrap();
    manager.start(&id).await.unwrap();
    (root, ocr, manager, id)
}

fn slow_request() -> Value {
    json!({"png_base64":png(2,2),"language":"eng","timeout_ms":15000})
}

#[tokio::test]
async fn management_stop_and_disable_cancel_real_children_before_waiting_for_call_gates() {
    for disable in [true, false] {
        let (_root, ocr, manager, id) = managed_fixture().await;
        let active = {
            let manager = manager.clone();
            let id = id.clone();
            tokio::spawn(async move {
                manager
                    .call_extension(&id, "recognize", slow_request())
                    .await
            })
        };
        let pid = running(&ocr.state).await;
        let queued = {
            let manager = manager.clone();
            let id = id.clone();
            tokio::spawn(async move {
                manager
                    .call_extension(&id, "recognize", slow_request())
                    .await
            })
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            while ocr.state.pending.load(Ordering::Acquire) < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // Before the pre-gate hook this waits for the full 15s call timeout.
        tokio::time::timeout(Duration::from_secs(3), async {
            if disable {
                manager.disable(&id).await
            } else {
                manager.stop(&id).await
            }
        })
        .await
        .expect("management must cancel before awaiting readers")
        .unwrap();
        assert!(active.await.unwrap().is_err());
        assert!(queued.await.unwrap().is_err());
        assert_eq!(ocr.state.active_pid.load(Ordering::Acquire), 0);
        assert_eq!(ocr.state.closing.load(Ordering::Acquire), 0);
        assert_exited(pid).await;
        assert!(manager
            .call_extension(&id, "recognize", slow_request())
            .await
            .is_err());
        if disable {
            manager.enable(&id).await.unwrap();
        }
        manager.start(&id).await.unwrap();
        let mut request = slow_request();
        request["timeout_ms"] = json!(30);
        let error = manager
            .call_extension(&id, "recognize", request)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("超时"),
            "restarting must restore admission: {error}"
        );
        manager.disable(&id).await.unwrap();
    }
}

#[tokio::test]
async fn rejected_management_preserves_provider_work_and_dropped_call_still_reaps_child() {
    let (_root, ocr, manager, id) = managed_fixture().await;
    manager.install(&archive("manifest_version=2\nid=\"gamer-notify\"\nversion=\"0.1.0\"\nname=\"Required provider fixture\"\n[execution]\nkind=\"builtin\"\nbuiltin_id=\"gamer-notify\"\n[[dependencies]]\nid=\"gamer-ocr\"\nversion=\"^0.1.0\"\nrequired=true\n")).await.unwrap();
    let dependent = crate::extensions::ExtensionId::parse("gamer-notify").unwrap();
    manager.enable(&dependent).await.unwrap();
    manager.start(&dependent).await.unwrap();
    let call = {
        let manager = manager.clone();
        let id = id.clone();
        tokio::spawn(async move {
            manager
                .call_extension(&id, "recognize", slow_request())
                .await
        })
    };
    let pid = running(&ocr.state).await;
    assert!(manager.disable(&id).await.is_err());
    assert_eq!(ocr.state.active_pid.load(Ordering::Acquire), pid);
    assert_eq!(ocr.state.closing.load(Ordering::Acquire), 0);
    call.abort();
    let _ = call.await;
    let permit = tokio::time::timeout(
        Duration::from_secs(3),
        ocr.state.workers.clone().acquire_owned(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ocr.state.active_pid.load(Ordering::Acquire), 0);
    assert_exited(pid).await;
    drop(permit);
    manager.disable(&dependent).await.unwrap();
    manager.disable(&id).await.unwrap();
}

#[tokio::test]
async fn overlapping_cancellation_guards_keep_new_calls_blocked_until_all_drop() {
    let root = tempfile::tempdir().unwrap();
    let ocr = OcrService::new(root.path()).unwrap();
    let first = ocr.cancel_inflight();
    let second = ocr.cancel_inflight();
    assert_eq!(ocr.state.closing.load(Ordering::Acquire), 2);
    drop(first);
    assert!(ocr
        .call("recognize", json!({}))
        .await
        .unwrap_err()
        .to_string()
        .contains("正在停止"));
    drop(second);
    assert_eq!(ocr.state.closing.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn shutdown_update_and_active_uninstall_cancel_real_children_before_management_waits() {
    for action in ["shutdown", "update", "uninstall"] {
        let (_root, ocr, manager, id) = managed_fixture().await;
        let call = {
            let manager = manager.clone();
            let id = id.clone();
            tokio::spawn(async move {
                manager
                    .call_extension(&id, "recognize", slow_request())
                    .await
            })
        };
        let pid = running(&ocr.state).await;
        let active = manager
            .list()
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.id() == &id)
            .unwrap()
            .active_version()
            .clone();
        tokio::time::timeout(Duration::from_secs(3), async {
            match action {
                "shutdown" => manager.shutdown_builtin_service().await,
                "update" => {
                    let manifest = include_str!("../manifest.toml")
                        .replace("version = \"0.1.0\"", "version = \"0.1.1\"");
                    manager.update(&archive(&manifest)).await.unwrap();
                }
                "uninstall" => assert!(manager.uninstall(&id, &active).await.unwrap()),
                _ => unreachable!(),
            }
        })
        .await
        .expect("management must cancel before waiting for active calls");
        assert!(call.await.unwrap().is_err());
        assert_eq!(ocr.state.active_pid.load(Ordering::Acquire), 0);
        assert_eq!(ocr.state.closing.load(Ordering::Acquire), 0);
        assert_exited(pid).await;
        if action == "shutdown" {
            assert!(manager
                .call_extension(&id, "recognize", slow_request())
                .await
                .is_err());
        } else {
            if action == "uninstall" {
                manager
                    .install(&archive(include_str!("../manifest.toml")))
                    .await
                    .unwrap();
                manager.enable(&id).await.unwrap();
                manager.start(&id).await.unwrap();
            }
            let mut request = slow_request();
            request["timeout_ms"] = json!(30);
            assert!(manager
                .call_extension(&id, "recognize", request)
                .await
                .unwrap_err()
                .to_string()
                .contains("超时"));
            manager.disable(&id).await.unwrap();
        }
    }
}

#[tokio::test]
async fn invalid_updates_and_uninstalls_reject_before_interrupting_work() {
    let (_root, ocr, manager, id) = managed_fixture().await;
    let call = {
        let manager = manager.clone();
        let id = id.clone();
        tokio::spawn(async move {
            manager
                .call_extension(&id, "recognize", slow_request())
                .await
        })
    };
    let pid = running(&ocr.state).await;
    assert!(manager.update(b"not a zip").await.is_err());
    assert!(manager
        .update(&archive(include_str!("../manifest.toml")))
        .await
        .is_err());
    let increased_permissions = include_str!("../manifest.toml")
        .replace("version = \"0.1.0\"", "version = \"0.1.1\"")
        .replace(
            "\"ui.host\", \"vision.ocr\"",
            "\"ui.host\", \"vision.ocr\", \"notify.send\"",
        );
    assert!(manager
        .update_with_context(
            &archive(&increased_permissions),
            &crate::extensions::service::ExtensionInstallContext::default()
        )
        .await
        .is_err());
    let missing = crate::extensions::ExtensionVersion::parse("9.9.9").unwrap();
    assert!(manager.uninstall(&id, &missing).await.is_err());
    assert_eq!(ocr.state.active_pid.load(Ordering::Acquire), pid);
    assert_eq!(ocr.state.closing.load(Ordering::Acquire), 0);
    call.abort();
    let _ = call.await;
    let permit = tokio::time::timeout(
        Duration::from_secs(3),
        ocr.state.workers.clone().acquire_owned(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_exited(pid).await;
    drop(permit);
    manager.disable(&id).await.unwrap();
}

#[tokio::test]
async fn incompatible_updates_and_inactive_uninstalls_do_not_cancel_active_provider() {
    let (_root, ocr, manager, id) = managed_fixture().await;
    let older =
        include_str!("../manifest.toml").replace("version = \"0.1.0\"", "version = \"0.0.9\"");
    manager.install(&archive(&older)).await.unwrap();
    manager.install(&archive("manifest_version=2\nid=\"gamer-notify\"\nversion=\"0.1.0\"\nname=\"Required provider fixture\"\n[execution]\nkind=\"builtin\"\nbuiltin_id=\"gamer-notify\"\n[[dependencies]]\nid=\"gamer-ocr\"\nversion=\"^0.1.0\"\nrequired=true\n")).await.unwrap();
    let dependent = crate::extensions::ExtensionId::parse("gamer-notify").unwrap();
    manager.enable(&dependent).await.unwrap();
    manager.start(&dependent).await.unwrap();
    let call = {
        let manager = manager.clone();
        let id = id.clone();
        tokio::spawn(async move {
            manager
                .call_extension(&id, "recognize", slow_request())
                .await
        })
    };
    let pid = running(&ocr.state).await;
    let incompatible =
        include_str!("../manifest.toml").replace("version = \"0.1.0\"", "version = \"0.2.0\"");
    assert!(manager.update(&archive(&incompatible)).await.is_err());
    let active = crate::extensions::ExtensionVersion::parse("0.1.0").unwrap();
    assert!(manager.uninstall(&id, &active).await.is_err());
    let removing = {
        let manager = manager.clone();
        let id = id.clone();
        tokio::spawn(async move {
            manager
                .uninstall(
                    &id,
                    &crate::extensions::ExtensionVersion::parse("0.0.9").unwrap(),
                )
                .await
        })
    };
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(ocr.state.active_pid.load(Ordering::Acquire), pid);
    assert_eq!(ocr.state.closing.load(Ordering::Acquire), 0);
    assert!(!removing.is_finished());
    call.abort();
    let _ = call.await;
    assert!(tokio::time::timeout(Duration::from_secs(3), removing)
        .await
        .unwrap()
        .unwrap()
        .unwrap());
    let permit = ocr.state.workers.clone().acquire_owned().await.unwrap();
    assert_exited(pid).await;
    drop(permit);
    manager.disable(&dependent).await.unwrap();
    manager.disable(&id).await.unwrap();
}
