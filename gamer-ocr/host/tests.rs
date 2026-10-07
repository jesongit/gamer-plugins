use super::*;

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
