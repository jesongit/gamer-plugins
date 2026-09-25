use super::*;
use crate::capabilities::CapabilityRegistry;
use crate::extensions::{ExtensionId, ExtensionService};
use crate::resources::PackageInput;
use std::io::Write;

fn archive(permission: bool) -> Vec<u8> {
    let manifest = format!("manifest_version=2\nid=\"{ID}\"\nversion=\"0.1.0\"\nname=\"配置包发布\"\npermissions=[{}]\n[host_api]\nresource=\"^1.1\"\n[execution]\nkind=\"builtin\"\nbuiltin_id=\"{ID}\"\n",if permission {"\"package.publish\""}else{""});
    let mut output = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut output);
    zip.start_file("manifest.toml", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(manifest.as_bytes()).unwrap();
    zip.finish().unwrap();
    output.into_inner()
}
async fn service(data: &Path, allowed: bool) -> ExtensionService {
    let s = ExtensionService::for_data_root(data, CapabilityRegistry::default());
    let id = ExtensionId::parse(ID).unwrap();
    s.install(&archive(allowed)).await.unwrap();
    s.enable(&id).await.unwrap();
    s.start(&id).await.unwrap();
    s
}
#[tokio::test]
async fn permission_and_lifecycle_guard_precede_any_publisher_io() {
    let dir = tempfile::tempdir().unwrap();
    let id = ExtensionId::parse(ID).unwrap();
    let denied = service(dir.path(), false).await;
    assert!(denied
        .call_extension(&id, "publisher.jobs", json!({}))
        .await
        .is_err());
    assert!(!dir.path().join("package-publisher").exists());
    let dir = tempfile::tempdir().unwrap();
    let allowed = service(dir.path(), true).await;
    assert_eq!(
        allowed
            .call_extension(&id, "publisher.jobs", json!({}))
            .await
            .unwrap(),
        json!([])
    );
    assert!(allowed
        .call_extension(&id, "shell", json!({}))
        .await
        .is_err());
    allowed.disable(&id).await.unwrap();
    assert!(allowed
        .call_extension(&id, "publisher.jobs", json!({}))
        .await
        .is_err());
}

#[test]
fn remote_verification_requires_all_files_and_exact_digests() {
    let root = tempfile::tempdir().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let dir = job_root(root.path(), &id).unwrap();
    fs::create_dir_all(&dir).unwrap();
    let catalog = Catalog {
        schema_version: 1,
        packages: vec![],
    };
    fs::write(
        dir.join("packages.json"),
        serde_json::to_vec(&catalog).unwrap(),
    )
    .unwrap();
    fs::write(dir.join("SHA256SUMS.txt"), "empty fixture").unwrap();
    let job = Job {
        id,
        repository: "o/r".into(),
        tag: "catalog-1".into(),
        notes: String::new(),
        baseline: None,
        catalog,
        state: "prepared".into(),
        account: "o".into(),
        release_url: None,
    };
    let assets:Vec<_>=files(root.path(),&job).unwrap().into_iter().map(|(name,(_,hash,size))|json!({"name":name,"size":size,"digest":format!("sha256:{hash}"),"state":"uploaded"})).collect();
    let mut remote = json!({"body":marker(&job),"assets":assets});
    verify_remote(&job, &remote, root.path()).unwrap();
    remote["assets"][0]["digest"] = json!("sha256:wrong");
    assert!(verify_remote(&job, &remote, root.path()).is_err());
    remote["body"] = json!("unrelated draft");
    assert!(verify_owner(&job, &remote).is_err());
}

/// Explicit opt-in only. This test publishes to the repository named by the
/// maintainer, through the real installed plugin lifecycle and gh adapter.
#[tokio::test]
#[ignore = "requires explicit authorization and GAMER_PUBLISH_TEST_REPO; publishes a real Release"]
async fn publisher_live_release_roundtrip() {
    let repo = std::env::var("GAMER_PUBLISH_TEST_REPO")
        .expect("authorized public test repository required");
    let dir = tempfile::tempdir().unwrap();
    let cfg = crate::config::Config {
        data_dir: dir.path().into(),
        ..Default::default()
    };
    let store = PackageStore::open(&cfg).unwrap();
    store
        .create_package(PackageInput {
            id: "publisher-smoke".into(),
            name: Some("配置包发布验收示例（无脚本）".into()),
            version: Some("1.0.0".into()),
            author: Some("Gamer integration test".into()),
            android_targets: vec!["*".into()],
            ..Default::default()
        })
        .unwrap();
    let service = service(dir.path(), true).await;
    let id = ExtensionId::parse(ID).unwrap();
    let prepared=service.call_extension(&id,"publisher.prepare",json!({"repository":repo,"package_ids":["publisher-smoke"],"notes":"发布插件真实闭环验收：仅包含无脚本、无个人数据的 publisher-smoke 配置；其他已有配置保留。"})).await.unwrap();
    let args = json!({"job_id":prepared["id"]});
    let draft = service
        .call_extension(&id, "publisher.draft", args.clone())
        .await
        .unwrap();
    assert_eq!(draft["state"], "draft");
    // Retrying verified draft upload must not create a second release or overwrite assets.
    let again = service
        .call_extension(&id, "publisher.draft", args.clone())
        .await
        .unwrap();
    assert_eq!(again["tag"], draft["tag"]);
    let published = service
        .call_extension(&id, "publisher.publish", args.clone())
        .await
        .unwrap();
    assert_eq!(published["state"], "published");
    let retry = service
        .call_extension(&id, "publisher.publish", args)
        .await
        .unwrap();
    assert_eq!(retry["tag"], published["tag"]);
    println!("PUBLISHER_PUBLIC_RESULT {}", published);
    verify_live_subscription(&repo, dir.path());
}

#[test]
#[ignore = "read-only public repository acceptance; requires GAMER_PUBLISH_TEST_REPO"]
fn publisher_live_subscription() {
    let repo = std::env::var("GAMER_PUBLISH_TEST_REPO").unwrap();
    let dir = tempfile::tempdir().unwrap();
    verify_live_subscription(&repo, dir.path());
}

fn verify_live_subscription(repo: &str, dir: &Path) {
    let market = crate::package_market::PackageMarket::new(dir.join("subscriber"));
    let sources = market.save_source(repo, true).unwrap();
    let source = sources
        .iter()
        .find(|s| s.repository == repository(repo).unwrap())
        .unwrap();
    let catalog = market.catalog(&source.id, true).unwrap();
    println!("PUBLISHER_SUBSCRIPTION_RESPONSE {catalog}");
    let p: PackageEntry = serde_json::from_value(
        catalog["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "publisher-smoke")
            .unwrap()
            .clone(),
    )
    .unwrap();
    let bytes = market
        .archive(&source.id, &p.id, &p.version, &p.sha256)
        .unwrap();
    p.verify(&bytes).unwrap();
    let imported = dir.join("imported");
    fs::create_dir_all(&imported).unwrap();
    let manifest = crate::package_archive::extract_archive(&bytes, &imported).unwrap();
    assert_eq!(manifest.id, "publisher-smoke");
    println!(
        "PUBLISHER_LIVE_RESULT {}",
        json!({"repository":repo,"package_id":p.id,"version":p.version,"sha256":p.sha256,"size":p.size,"public_discovery":true,"archive_import":true})
    );
}
