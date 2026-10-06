//! Atomic script/template transactions with private, artifact-scoped undo history.
//! All publication shares PackageStore's snapshot barrier; no model-facing write API.
use crate::resources::PackageStore;
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
const MAX_SNAPSHOT: usize = 512 * 1024 * 1024;
const MAX_FILES: usize = 8192;
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Revision {
    pub id: String,
    pub at: String,
    pub candidate_id: String,
    pub previous_version: String,
    pub version: String,
    pub kind: String,
    pub changed_paths: Vec<String>,
    pub before_versions: BTreeMap<String, Option<String>>,
    pub after_versions: BTreeMap<String, Option<String>>,
    #[serde(default)]
    pub metadata: Value,
}
fn read_tree(root: &Path, prefix: &str, files: &mut BTreeMap<String, Vec<u8>>) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("non-UTF8 resource path"))?;
        let kind = entry.file_type()?;
        ensure!(!kind.is_symlink(), "symlink resource rejected");
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if kind.is_dir() {
            read_tree(&entry.path(), &path, files)?
        } else if kind.is_file() {
            ensure!(files.len() < MAX_FILES, "snapshot file limit exceeded");
            ensure!(
                entry.metadata()?.len() <= MAX_SNAPSHOT as u64,
                "resource snapshot too large"
            );
            files.insert(path, std::fs::read(entry.path())?);
            ensure!(
                files.values().map(Vec::len).sum::<usize>() <= MAX_SNAPSHOT,
                "snapshot size limit exceeded"
            );
        }
    }
    Ok(())
}
pub(crate) fn snapshot(store: &PackageStore, package: &str) -> Result<BTreeMap<String, Vec<u8>>> {
    store.with_package_read(package, || {
        let mut files = BTreeMap::new();
        read_tree(
            &store.plugin_dir(package, super::YAML_EXTENSION_ID)?,
            "",
            &mut files,
        )?;
        Ok(files)
    })
}
pub(crate) fn hash(files: &BTreeMap<String, Vec<u8>>) -> String {
    let mut h = Sha256::new();
    for (path, bytes) in files {
        h.update((path.len() as u64).to_le_bytes());
        h.update(path);
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes)
    }
    format!("{:x}", h.finalize())
}
fn file_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(crate) fn version(store: &PackageStore, package: &str) -> Result<String> {
    Ok(hash(&snapshot(store, package)?))
}
fn write_tree(root: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    std::fs::create_dir_all(root)?;
    for (path, bytes) in files {
        crate::resources::sanitize_rel_path(path)?;
        let target = root.join(path);
        std::fs::create_dir_all(target.parent().context("path parent")?)?;
        crate::core::fs::atomic_write(&target, bytes)?;
    }
    Ok(())
}
fn private_root(store: &PackageStore, package: &str) -> Result<PathBuf> {
    store.manifest(package)?;
    Ok(store
        .data_root()
        .join("extension-data/gamer-yaml/revisions")
        .join(package))
}
fn paths(store: &PackageStore, package: &str) -> Result<(PathBuf, PathBuf, PathBuf, PathBuf)> {
    let live = store.plugin_dir(package, super::YAML_EXTENSION_ID)?;
    let parent = live.parent().context("plugin parent")?;
    Ok((
        live.clone(),
        parent.join(".gamer-yaml-transaction"),
        parent.join(".gamer-yaml-backup"),
        private_root(store, package)?.join("transaction.json"),
    ))
}
fn read_revision(path: &Path) -> Result<Revision> {
    ensure!(
        std::fs::metadata(path)?.len() <= 1024 * 1024,
        "revision metadata too large"
    );
    let revision: Revision = serde_json::from_slice(&std::fs::read(path)?)?;
    ensure!(
        uuid::Uuid::parse_str(&revision.id).is_ok(),
        "invalid revision id"
    );
    for p in &revision.changed_paths {
        scoped_path(p)?;
    }
    Ok(revision)
}
fn scoped_path(path: &str) -> Result<()> {
    crate::resources::sanitize_rel_path(path)?;
    ensure!(
        path.starts_with("automations/") || path.starts_with("templates/"),
        "unscoped revision artifact"
    );
    Ok(())
}
/// Completes publication after a process interruption, or restores the old root.
/// A backup is never discarded if live bytes do not match either recorded version.
pub(crate) fn recover(store: &PackageStore, package: &str) -> Result<()> {
    store.with_package_write(package, || {
        let (live, stage, backup, journal) = paths(store, package)?;
        if !journal.exists() {
            ensure!(
                !backup.exists(),
                "transaction_recovery_required: unjournaled backup preserved"
            );
            if stage.exists() {
                std::fs::remove_dir_all(stage)?;
            }
            return Ok(());
        }
        let revision = read_revision(&journal)?;
        let root = private_root(store, package)?;
        let pending = root.join(format!(".pending-{}", revision.id));
        let finished = root.join(&revision.id);
        let current = if live.exists() {
            Some(version(store, package)?)
        } else {
            None
        };
        if current.as_deref() == Some(&revision.version) {
            if !finished.exists() {
                ensure!(
                    pending.exists(),
                    "transaction history missing; backup preserved"
                );
                std::fs::rename(&pending, &finished)?;
            }
        } else if current.as_deref() == Some(&revision.previous_version) {
            if pending.exists() {
                std::fs::remove_dir_all(pending)?;
            }
        } else if current.is_none() && backup.exists() {
            std::fs::rename(&backup, &live)?;
            if pending.exists() {
                std::fs::remove_dir_all(pending)?;
            }
        } else if current.is_none() && revision.previous_version == hash(&BTreeMap::new()) {
            if pending.exists() {
                std::fs::remove_dir_all(pending)?;
            }
        } else {
            anyhow::bail!(
                "transaction_recovery_conflict: preserve backup/history for manual review"
            )
        }
        if stage.exists() {
            std::fs::remove_dir_all(stage)?;
        }
        if backup.exists() {
            std::fs::remove_dir_all(backup)?;
        }
        std::fs::remove_file(journal)?;
        Ok(())
    })
}
/// Expected version protects publication against concurrent edits. History saves
/// only changed artifacts; unrelated scripts, samples and private data are never copied.
pub(crate) fn commit(
    store: &PackageStore,
    package: &str,
    expected: &str,
    changes: &BTreeMap<String, Vec<u8>>,
    candidate: &str,
    kind: &str,
    _replace: bool,
) -> Result<Revision> {
    apply(
        store,
        package,
        expected,
        &changes
            .iter()
            .map(|(p, b)| (p.clone(), Some(b.clone())))
            .collect(),
        candidate,
        kind,
        Value::Null,
    )
}
pub(crate) fn commit_with_metadata(
    store: &PackageStore,
    package: &str,
    expected: &str,
    changes: &BTreeMap<String, Vec<u8>>,
    candidate: &str,
    metadata: Value,
) -> Result<Revision> {
    apply(
        store,
        package,
        expected,
        &changes
            .iter()
            .map(|(p, b)| (p.clone(), Some(b.clone())))
            .collect(),
        candidate,
        "validated",
        metadata,
    )
}
fn apply(
    store: &PackageStore,
    package: &str,
    expected: &str,
    changes: &BTreeMap<String, Option<Vec<u8>>>,
    candidate: &str,
    kind: &str,
    metadata: Value,
) -> Result<Revision> {
    store.with_package_write(package, || {
        recover(store, package)?;
        let original = snapshot(store, package)?;
        let previous_version = hash(&original);
        ensure!(
            previous_version == expected,
            "version_conflict: 自动化资源已被修改，请重新读取并验证"
        );
        let mut updated = original.clone();
        let mut before = BTreeMap::new();
        let mut before_versions = BTreeMap::new();
        let mut after_versions = BTreeMap::new();
        for (path, value) in changes {
            scoped_path(path)?;
            if original.get(path) == value.as_ref() {
                continue;
            }
            before_versions.insert(path.clone(), original.get(path).map(|b| file_hash(b)));
            after_versions.insert(path.clone(), value.as_ref().map(|b| file_hash(b)));
            if let Some(bytes) = original.get(path) {
                before.insert(path.clone(), bytes.clone());
            }
            if let Some(bytes) = value {
                updated.insert(path.clone(), bytes.clone());
            } else {
                updated.remove(path);
            }
        }
        let revision = Revision {
            id: uuid::Uuid::new_v4().to_string(),
            at: chrono::Utc::now().to_rfc3339(),
            candidate_id: candidate.into(),
            previous_version,
            version: hash(&updated),
            kind: kind.into(),
            changed_paths: after_versions.keys().cloned().collect(),
            before_versions,
            after_versions,
            metadata,
        };
        let (live, stage, backup, journal) = paths(store, package)?;
        let root = private_root(store, package)?;
        std::fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        }
        ensure!(
            std::fs::read_dir(&root)?
                .filter_map(std::result::Result::ok)
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .count()
                < 100,
            "revision history storage limit reached; preserve/export history before cleanup"
        );
        let pending = root.join(format!(".pending-{}", revision.id));
        let finished = root.join(&revision.id);
        let result = (|| -> Result<()> {
            write_tree(&stage, &updated)?;
            write_tree(&pending.join("before"), &before)?;
            crate::core::fs::atomic_write(
                &pending.join("revision.json"),
                &serde_json::to_vec(&revision)?,
            )?;
            crate::core::fs::atomic_write(&journal, &serde_json::to_vec(&revision)?)?;
            if live.exists() {
                std::fs::rename(&live, &backup)?;
            }
            if let Err(error) = std::fs::rename(&stage, &live) {
                if backup.exists() {
                    std::fs::rename(&backup, &live).context("rollback failed; backup preserved")?;
                }
                return Err(error.into());
            }
            if let Err(error) = std::fs::rename(&pending, &finished) {
                // Roll back the entire publication while the shared barrier is held.
                std::fs::rename(&live, &stage)?;
                if backup.exists() {
                    std::fs::rename(&backup, &live)?;
                }
                return Err(error.into());
            }
            // Both live resources and private history are published. Cleanup
            // cannot turn a completed commit into an ambiguous failure response.
            let cleaned = !backup.exists() || std::fs::remove_dir_all(&backup).is_ok();
            if cleaned {
                let _ = std::fs::remove_file(&journal);
            }
            Ok(())
        })();
        if let Err(error) = result {
            if journal.exists() {
                recover(store, package)
                    .with_context(|| format!("{error}; transaction recovery failed"))?;
            } else {
                if stage.exists() {
                    let _ = std::fs::remove_dir_all(&stage);
                }
                if pending.exists() {
                    let _ = std::fs::remove_dir_all(&pending);
                }
            }
            return Err(error);
        }
        Ok(revision)
    })
}
pub(crate) fn history(store: &PackageStore, package: &str) -> Result<Value> {
    store.with_package_read(package, || {
        recover(store, package)?;
        let root = private_root(store, package)?;
        let mut revisions = vec![];
        if root.exists() {
            for entry in std::fs::read_dir(root)? {
                let entry = entry?;
                if entry.file_type()?.is_dir()
                    && uuid::Uuid::parse_str(&entry.file_name().to_string_lossy()).is_ok()
                {
                    revisions.push(read_revision(&entry.path().join("revision.json"))?);
                }
            }
        }
        revisions.sort_by(|a, b| b.at.cmp(&a.at));
        Ok(json!({"version":version(store,package)?,"revisions":revisions}))
    })
}
pub(crate) fn rollback(
    store: &PackageStore,
    package: &str,
    id: &str,
    expected: &str,
) -> Result<Revision> {
    ensure!(uuid::Uuid::parse_str(id).is_ok(), "invalid revision id");
    store.with_package_write(package, || {
        recover(store, package)?;
        let root = private_root(store, package)?.join(id);
        let revision = read_revision(&root.join("revision.json"))?;
        let current = snapshot(store, package)?;
        let mut inverse = BTreeMap::new();
        for path in &revision.changed_paths {
            scoped_path(path)?;
            ensure!(
                current.get(path).map(|b| file_hash(b))
                    == revision.after_versions.get(path).cloned().flatten(),
                "revision_conflict: {path} changed after the selected revision"
            );
            let before = revision
                .before_versions
                .get(path)
                .context("revision before fingerprint missing")?;
            let bytes = if let Some(expected) = before {
                let bytes = std::fs::read(root.join("before").join(path))?;
                ensure!(
                    &file_hash(&bytes) == expected,
                    "revision before bytes corrupt"
                );
                Some(bytes)
            } else {
                None
            };
            inverse.insert(path.clone(), bytes);
        }
        apply(
            store,
            package,
            expected,
            &inverse,
            id,
            "rollback",
            json!({"reverted_revision":id,"validation_status":"not_revalidated"}),
        )
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, PackageStore) {
        let root = tempfile::tempdir().unwrap();
        let store = PackageStore::open(&crate::config::Config {
            data_dir: root.path().into(),
            ..Default::default()
        })
        .unwrap();
        store.ensure_default_package().unwrap();
        (root, store)
    }
    #[test]
    fn hash_covers_names_and_bytes() {
        assert_ne!(
            hash(&BTreeMap::from([("a".into(), b"bc".to_vec())])),
            hash(&BTreeMap::from([("ab".into(), b"c".to_vec())]))
        );
    }
    #[test]
    fn rollback_only_reverts_owned_artifacts_and_history_stays_private() {
        let (_root, store) = fixture();
        let old = version(&store, "default").unwrap();
        let r = commit(
            &store,
            "default",
            &old,
            &BTreeMap::from([
                ("automations/selected.yaml".into(), b"selected".to_vec()),
                ("templates/selected.png".into(), vec![1, 2, 3]),
            ]),
            "c",
            "validated",
            false,
        )
        .unwrap();
        store
            .write_text(
                "default",
                super::super::YAML_EXTENSION_ID,
                "automations/unrelated.yaml",
                "keep me",
                None,
                false,
            )
            .unwrap();
        let now = version(&store, "default").unwrap();
        rollback(&store, "default", &r.id, &now).unwrap();
        assert_eq!(
            store
                .read_text(
                    "default",
                    super::super::YAML_EXTENSION_ID,
                    "automations/unrelated.yaml"
                )
                .unwrap()
                .unwrap()
                .content,
            "keep me"
        );
        assert!(store
            .read_text(
                "default",
                super::super::YAML_EXTENSION_ID,
                "automations/selected.yaml"
            )
            .unwrap()
            .is_none());
        assert!(!store
            .plugin_dir("default", super::super::YAML_EXTENSION_ID)
            .unwrap()
            .join(".revisions")
            .exists());
    }
    #[test]
    fn stale_expected_and_changed_owned_artifacts_reject_without_partial_write() {
        let (_root, store) = fixture();
        let empty = version(&store, "default").unwrap();
        let r = commit(
            &store,
            "default",
            &empty,
            &BTreeMap::from([("automations/a.yaml".into(), b"first".to_vec())]),
            "c",
            "validated",
            false,
        )
        .unwrap();
        assert!(commit(
            &store,
            "default",
            &empty,
            &BTreeMap::from([("templates/new.png".into(), vec![0])]),
            "c",
            "validated",
            false
        )
        .is_err());
        store
            .write_text(
                "default",
                super::super::YAML_EXTENSION_ID,
                "automations/a.yaml",
                "user changed",
                None,
                true,
            )
            .unwrap();
        assert!(rollback(
            &store,
            "default",
            &r.id,
            &version(&store, "default").unwrap()
        )
        .is_err());
        assert!(store
            .read_binary(
                "default",
                super::super::YAML_EXTENSION_ID,
                "templates/new.png"
            )
            .unwrap()
            .is_none());
    }
}
