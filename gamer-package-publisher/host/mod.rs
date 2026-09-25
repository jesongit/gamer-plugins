//! Configuration publishing belongs to this builtin plugin, not Package Core.
mod gh;
#[cfg(test)]
mod integration_tests;
use crate::{
    extensions::{ExtensionError, ExtensionResult, Permission},
    package_archive::{export_package, sha256_hex},
    package_market::{repository, Catalog, PackageEntry, PublicClient, MAX_CATALOG_BYTES},
    resources::{validate_scope_id, PackageStore},
};
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
};

pub(crate) const ID: &str = "gamer-package-publisher";
pub(crate) const ACTIONS: &[&str] = &[
    "publisher.status",
    "publisher.prepare",
    "publisher.draft",
    "publisher.publish",
    "publisher.cancel",
    "publisher.jobs",
];
pub(crate) fn accepts(id: &str, action: &str) -> bool {
    id == ID && ACTIONS.contains(&action)
}
pub(crate) fn permissions(id: &str, action: &str) -> Option<&'static [Permission]> {
    accepts(id, action).then_some(&[Permission::PackagePublish])
}
#[derive(Default)]
struct Runtime {
    gate: Mutex<()>,
    cancelled: AtomicBool,
}
fn runtime(root: &Path) -> Arc<Runtime> {
    static RUNTIMES: OnceLock<Mutex<BTreeMap<PathBuf, Arc<Runtime>>>> = OnceLock::new();
    RUNTIMES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(root.into())
        .or_default()
        .clone()
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Job {
    id: String,
    repository: String,
    tag: String,
    notes: String,
    baseline: Option<String>,
    catalog: Catalog,
    state: String,
    account: String,
    release_url: Option<String>,
}
fn job_root(root: &Path, id: &str) -> Result<PathBuf> {
    ensure!(uuid::Uuid::parse_str(id).is_ok(), "发布任务 ID 无效");
    Ok(root.join("jobs").join(id))
}
fn save(root: &Path, job: &Job) -> Result<()> {
    crate::core::fs::atomic_write(
        &job_root(root, &job.id)?.join("job.json"),
        &serde_json::to_vec_pretty(job)?,
    )?;
    Ok(())
}
fn load(root: &Path, id: &str) -> Result<Job> {
    Ok(serde_json::from_slice(&fs::read(
        job_root(root, id)?.join("job.json"),
    )?)?)
}
fn gh_json(args: &[String], root: &Path, rt: &Runtime) -> Result<Value> {
    Ok(serde_json::from_slice(&gh::run(
        args,
        root,
        &rt.cancelled,
    )?)?)
}
fn api(endpoint: &str, root: &Path, rt: &Runtime) -> Result<Value> {
    gh_json(
        &[
            "api".into(),
            "--hostname".into(),
            "github.com".into(),
            endpoint.into(),
        ],
        root,
        rt,
    )
}
fn login(root: &Path, rt: &Runtime) -> Result<String> {
    api("user", root, rt)?["login"]
        .as_str()
        .map(str::to_owned)
        .context("gh 尚未登录，请先执行 gh auth login --hostname github.com")
}
fn check_repo(repo: &str, root: &Path, rt: &Runtime) -> Result<()> {
    let value = api(&format!("repos/{repo}"), root, rt)?;
    ensure!(value["private"] == false, "首版仅支持公开仓库");
    ensure!(
        value["permissions"]["push"] == true || value["permissions"]["admin"] == true,
        "当前 gh 账号没有目标仓库发布权限"
    );
    Ok(())
}
fn check_baseline(job: &Job) -> Result<()> {
    let current = PublicClient::default()
        .latest(&job.repository)?
        .map(|p| p.fingerprint());
    ensure!(
        current == job.baseline,
        "仓库目录已被其他发布更新，请重新准备，避免丢失配置"
    );
    Ok(())
}
fn releases(repo: &str, root: &Path, rt: &Runtime) -> Result<Vec<Value>> {
    // A publisher-owned job has a newly generated tag; a limited list is not an
    // existence proof. Query its exact tag through GraphQL-backed gh release view
    // only after finding it; list all pages for draft retries.
    let args = vec![
        "api".into(),
        "--hostname".into(),
        "github.com".into(),
        "--paginate".into(),
        "--slurp".into(),
        format!("repos/{repo}/releases?per_page=100"),
    ];
    let pages = gh_json(&args, root, rt)?;
    Ok(pages
        .as_array()
        .context("Release 列表无效")?
        .iter()
        .flat_map(|p| p.as_array().into_iter().flatten().cloned())
        .collect())
}
fn release(job: &Job, root: &Path, rt: &Runtime) -> Result<Option<Value>> {
    Ok(releases(&job.repository, root, rt)?
        .into_iter()
        .find(|r| r["tag_name"] == job.tag))
}
fn marker(job: &Job) -> String {
    format!("<!-- gamer-package-publisher:{} -->", job.id)
}
fn verify_owner(job: &Job, r: &Value) -> Result<()> {
    ensure!(
        r["body"]
            .as_str()
            .unwrap_or_default()
            .contains(&marker(job)),
        "同名 Release 不属于该发布任务，拒绝修改"
    );
    Ok(())
}
fn files(root: &Path, job: &Job) -> Result<BTreeMap<String, (PathBuf, String, u64)>> {
    let dir = job_root(root, &job.id)?;
    let mut result = BTreeMap::new();
    for name in job
        .catalog
        .packages
        .iter()
        .map(|p| p.asset_name.clone())
        .chain(["packages.json".into(), "SHA256SUMS.txt".into()])
    {
        let path = dir.join(&name);
        let bytes = fs::read(&path)?;
        if let Some(p) = job.catalog.packages.iter().find(|p| p.asset_name == name) {
            p.verify(&bytes)?;
        }
        if name == "packages.json" {
            ensure!(
                serde_json::from_slice::<Catalog>(&bytes)? == job.catalog,
                "本地目录被修改，请重新准备"
            );
        }
        result.insert(name, (path, sha256_hex(&bytes), bytes.len() as u64));
    }
    Ok(result)
}
fn verify_remote(job: &Job, r: &Value, root: &Path) -> Result<()> {
    verify_owner(job, r)?;
    let expected = files(root, job)?;
    let assets = r["assets"].as_array().context("Release 缺少资产")?;
    ensure!(assets.len() == expected.len(), "草稿资产数量不一致");
    for (name, (_, hash, size)) in expected {
        ensure!(
            assets
                .iter()
                .filter(|a| a["name"] == name
                    && a["size"] == size
                    && a["digest"] == format!("sha256:{hash}")
                    && a["state"] == "uploaded")
                .count()
                == 1,
            "草稿资产校验失败：{name}"
        );
    }
    Ok(())
}
pub(crate) fn call(
    id: &str,
    action: &str,
    values: &Value,
    data: &Path,
) -> Option<ExtensionResult<Value>> {
    if !accepts(id, action) {
        return None;
    }
    Some(dispatch(action, values, data).map_err(|e| ExtensionError::CallRejected(e.to_string())))
}
fn dispatch(action: &str, values: &Value, data: &Path) -> Result<Value> {
    let root = data.join("package-publisher");
    let rt = runtime(&root);
    if action == "publisher.cancel" {
        rt.cancelled.store(true, Ordering::SeqCst);
        return Ok(json!({"cancelled":true}));
    }
    let _guard = rt
        .gate
        .try_lock()
        .map_err(|_| anyhow!("已有发布操作正在执行"))?;
    rt.cancelled.store(false, Ordering::SeqCst);
    fs::create_dir_all(&root)?;
    match action {
        "publisher.status" => Ok(json!({"account":login(&root,&rt)?,"host":"github.com"})),
        "publisher.jobs" => {
            let mut jobs = Vec::new();
            if let Ok(entries) = fs::read_dir(root.join("jobs")) {
                for entry in entries.flatten() {
                    if let Ok(job) = load(&root, &entry.file_name().to_string_lossy()) {
                        jobs.push(job);
                    }
                }
            }
            jobs.sort_by(|a, b| b.tag.cmp(&a.tag));
            jobs.truncate(100);
            Ok(serde_json::to_value(jobs)?)
        }
        "publisher.prepare" => prepare(values, data, &root, &rt),
        "publisher.draft" | "publisher.publish" => {
            let id = values["job_id"].as_str().context("缺少发布任务 ID")?;
            let mut job = load(&root, id)?;
            repository(&job.repository)?;
            job.catalog.validate()?;
            ensure!(
                login(&root, &rt)? == job.account,
                "gh 账号发生变化，请重新准备发布"
            );
            check_repo(&job.repository, &root, &rt)?;
            if let Some(r) = release(&job, &root, &rt)? {
                verify_owner(&job, &r)?;
                if r["draft"] == false {
                    verify_remote(&job, &r, &root)?;
                    job.state = "published".into();
                    job.release_url = r["html_url"].as_str().map(str::to_owned);
                    save(&root, &job)?;
                    return Ok(serde_json::to_value(job)?);
                }
            }
            check_baseline(&job)?;
            if action == "publisher.draft" {
                make_draft(&mut job, &root, &rt)?;
            } else {
                ensure!(job.state == "draft", "请先创建并校验草稿");
                let r = release(&job, &root, &rt)?.context("草稿不存在")?;
                verify_remote(&job, &r, &root)?;
                ensure!(r["draft"] == true, "Release 状态已变化");
                check_baseline(&job)?;
                gh::run(
                    &[
                        "release".into(),
                        "edit".into(),
                        job.tag.clone(),
                        "--repo".into(),
                        format!("github.com/{}", job.repository),
                        "--draft=false".into(),
                        "--prerelease=false".into(),
                        "--latest".into(),
                    ],
                    &root,
                    &rt.cancelled,
                )?;
                let published =
                    release(&job, &root, &rt)?.context("公开状态暂不可确认，可重试查询")?;
                ensure!(
                    published["draft"] == false && published["prerelease"] == false,
                    "公开状态暂不可确认，可重试查询"
                );
                verify_remote(&job, &published, &root)?;
                job.state = "published".into();
                job.release_url = published["html_url"].as_str().map(str::to_owned);
                save(&root, &job)?;
            }
            Ok(serde_json::to_value(job)?)
        }
        _ => bail!("未知发布操作"),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    repository: String,
    package_ids: Vec<String>,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    include_media: bool,
}
fn prepare(values: &Value, data: &Path, root: &Path, rt: &Runtime) -> Result<Value> {
    let input: Prepare = serde_json::from_value(values.clone())?;
    ensure!(
        !input.package_ids.is_empty()
            && input.package_ids.len() <= 128
            && input.notes.len() <= 16000,
        "请选择配置包且发布说明不超过 16000 字节"
    );
    let repo = repository(&input.repository)?;
    let account = login(root, rt)?;
    check_repo(&repo, root, rt)?;
    let client = PublicClient::default();
    let previous = client.latest(&repo)?;
    let baseline = previous.as_ref().map(|p| p.fingerprint());
    let cfg = crate::config::Config {
        data_dir: data.into(),
        ..Default::default()
    };
    let store = PackageStore::open(&cfg)?;
    let media = crate::media::MediaService::open(data.join("media"), cfg.ffmpeg_path)?;
    let mut updates = BTreeMap::new();
    for id in input.package_ids {
        ensure!(!rt.cancelled.load(Ordering::SeqCst), "操作已取消");
        validate_scope_id("package", &id)?;
        ensure!(!updates.contains_key(&id), "选择了重复的配置包");
        let built = export_package(&store, &id, Some(&media), input.include_media)?;
        updates.insert(
            id,
            (PackageEntry::from_archive(&built.archive)?, built.archive),
        );
        ensure!(
            updates
                .values()
                .map(|(_, bytes)| bytes.len() as u64)
                .sum::<u64>()
                <= MAX_CATALOG_BYTES,
            "所选配置包总量超过 512 MiB"
        );
    }
    let merged = merge_snapshot(
        previous.as_ref().map(|p| &p.catalog),
        updates.keys().cloned().collect(),
        &updates,
    )?;
    let id = uuid::Uuid::new_v4().to_string();
    let dir = job_root(root, &id)?;
    fs::create_dir_all(&dir)?;
    // Freeze all bytes locally before any remote write. Partial prepare is not a resumable job.
    let result = (|| -> Result<Job> {
        for p in &merged.packages {
            ensure!(!rt.cancelled.load(Ordering::SeqCst), "操作已取消");
            let bytes = if let Some((_, bytes)) = updates.remove(&p.id) {
                bytes
            } else {
                client.archive(&repo, &previous.as_ref().context("缺少上一份目录")?.tag, p)?
            };
            p.verify(&bytes)?;
            fs::write(dir.join(&p.asset_name), bytes)?;
        }
        let catalog_bytes = serde_json::to_vec_pretty(&merged)?;
        fs::write(dir.join("packages.json"), &catalog_bytes)?;
        let mut sums = merged
            .packages
            .iter()
            .map(|p| format!("{}  {}\n", p.sha256, p.asset_name))
            .collect::<String>();
        sums.push_str(&format!("{}  packages.json\n", sha256_hex(&catalog_bytes)));
        fs::write(dir.join("SHA256SUMS.txt"), sums)?;
        let job = Job {
            tag: format!(
                "catalog-{}-{}",
                chrono::Utc::now().format("%Y%m%d%H%M%S"),
                &id[..8]
            ),
            id: id.clone(),
            repository: repo,
            account,
            notes: input.notes,
            baseline,
            catalog: merged,
            state: "prepared".into(),
            release_url: None,
        };
        save(root, &job)?;
        Ok(job)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    Ok(serde_json::to_value(result?)?)
}
fn merge_snapshot(
    previous: Option<&Catalog>,
    ids: Vec<String>,
    updates: &BTreeMap<String, (PackageEntry, Vec<u8>)>,
) -> Result<Catalog> {
    let mut entries: BTreeMap<String, PackageEntry> = previous
        .into_iter()
        .flat_map(|c| c.packages.iter())
        .map(|p| (p.id.clone(), p.clone()))
        .collect();
    for id in ids {
        let entry = &updates[&id].0;
        if let Some(old) = entries.get(&id) {
            ensure!(
                semver::Version::parse(&entry.version)? >= semver::Version::parse(&old.version)?,
                "{} 不能降级发布",
                id
            );
            ensure!(
                old.version != entry.version || old.sha256 == entry.sha256,
                "{}@{} 已发布且内容不同，请提升配置包版本",
                id,
                entry.version
            );
        }
        entries.insert(id, entry.clone());
    }
    let catalog = Catalog {
        schema_version: 1,
        packages: entries.into_values().collect(),
    };
    catalog.validate()?;
    ensure!(
        catalog.packages.iter().map(|p| p.size).sum::<u64>() <= MAX_CATALOG_BYTES,
        "目录总量超限"
    );
    Ok(catalog)
}
fn make_draft(job: &mut Job, root: &Path, rt: &Runtime) -> Result<()> {
    if release(job, root, rt)?.is_none() {
        let notes = job_root(root, &job.id)?.join("notes.md");
        fs::write(&notes, format!("{}\n\n{}\n", job.notes, marker(job)))?;
        gh::run(
            &[
                "release".into(),
                "create".into(),
                job.tag.clone(),
                "--repo".into(),
                format!("github.com/{}", job.repository),
                "--draft".into(),
                "--title".into(),
                format!("配置目录 {}", job.tag),
                "--notes-file".into(),
                notes.to_string_lossy().into(),
            ],
            root,
            &rt.cancelled,
        )?;
    }
    let remote = release(job, root, rt)?.context("草稿创建结果未知，可重试")?;
    verify_owner(job, &remote)?;
    ensure!(remote["draft"] == true, "不能修改已公开 Release");
    let assets = remote["assets"].as_array().context("草稿资产无效")?;
    for (name, (path, hash, size)) in files(root, job)? {
        if let Some(asset) = assets.iter().find(|a| a["name"] == name) {
            ensure!(
                asset["size"] == size && asset["digest"] == format!("sha256:{hash}"),
                "草稿同名资产内容不同，拒绝覆盖：{name}"
            );
        } else {
            gh::run(
                &[
                    "release".into(),
                    "upload".into(),
                    job.tag.clone(),
                    "--repo".into(),
                    format!("github.com/{}", job.repository),
                    path.to_string_lossy().into(),
                ],
                root,
                &rt.cancelled,
            )?;
        }
    }
    let verified = release(job, root, rt)?.context("草稿查询失败")?;
    verify_remote(job, &verified, root)?;
    job.release_url = verified["html_url"].as_str().map(str::to_owned);
    job.state = "draft".into();
    save(root, job)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(id: &str, version: &str) -> PackageEntry {
        PackageEntry {
            id: id.into(),
            name: id.into(),
            version: version.into(),
            author: String::new(),
            android_targets: vec![],
            required_plugins: vec![],
            optional_plugins: vec![],
            asset_name: format!("{id}-{version}.gamerpkg"),
            size: 1,
            sha256: "a".repeat(64),
        }
    }
    #[test]
    fn snapshot_preserves_other_packages_and_rejects_version_rewrite() {
        let old = Catalog {
            schema_version: 1,
            packages: vec![entry("a", "1.0.0"), entry("b", "1.0.0")],
        };
        let update = BTreeMap::from([("a".into(), (entry("a", "1.1.0"), vec![]))]);
        let result = merge_snapshot(Some(&old), vec!["a".into()], &update).unwrap();
        assert_eq!(result.packages.len(), 2);
        assert_eq!(result.packages[1], old.packages[1]);
        let mut changed = entry("a", "1.0.0");
        changed.sha256 = "b".repeat(64);
        assert!(merge_snapshot(
            Some(&old),
            vec!["a".into()],
            &BTreeMap::from([("a".into(), (changed, vec![]))])
        )
        .is_err());
    }
    #[test]
    fn unknown_actions_and_paths_are_rejected() {
        assert!(!accepts(ID, "shell"));
        assert!(job_root(Path::new("test"), "../../config").is_err());
        assert!(serde_json::from_value::<Prepare>(
            json!({"repository":"o/r","package_ids":["a"],"command":"gh secret"})
        )
        .is_err());
    }
}
