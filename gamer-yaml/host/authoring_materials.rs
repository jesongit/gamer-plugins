//! Verified, self-contained teaching material. Never reads original videos.
use super::authoring_support::{decode_files, required, Sample};
use crate::resources::PackageStore;
use anyhow::{ensure, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(super) fn read_reference(
    packages: &PackageStore,
    package: &str,
    reference: &Value,
) -> Result<Sample> {
    packages.manifest(package)?;
    let id = required(reference, "sample_id")?;
    crate::resources::validate_scope_id("sample", id)?;
    let value = match required(reference, "plugin_id")? {
        "gamer-video" => {
            crate::extensions::video::sample::read_stored_bundle(packages.data_root(), id)?
        }
        "gamer-yaml" => {
            if let Some(candidate) = reference["candidate_id"].as_str() {
                ensure!(
                    uuid::Uuid::parse_str(candidate).is_ok(),
                    "invalid candidate id"
                );
                let root = packages
                    .data_root()
                    .join("extension-data/gamer-yaml/candidates")
                    .join(package);
                let envelope: Value =
                    read_bounded_json(&root.join(format!("{candidate}.json")), 8 * 1024 * 1024)?;
                ensure!(
                    envelope["candidate"]["package_id"] == package
                        && envelope["candidate"]["id"] == candidate,
                    "candidate scope mismatch"
                );
                let snapshot = read_bounded_json(
                    &root.join(format!("{candidate}.snapshot")),
                    280 * 1024 * 1024,
                )?;
                snapshot["samples"]
                    .as_array()
                    .context("candidate samples missing")?
                    .iter()
                    .find(|sample| sample["manifest"]["id"] == id)
                    .cloned()
                    .context("frozen sample missing")?
            } else {
                crate::extensions::video::sample::read_bundle(
                    &packages
                        .read_binary(package, "gamer-yaml", &format!("samples/{id}.gamersample"))?
                        .context("selected material missing")?,
                )?
            }
        }
        _ => anyhow::bail!("unsupported material owner"),
    };
    let sample: Sample = serde_json::from_value(value)?;
    ensure!(
        sample.manifest["id"] == id
            && sample.manifest["content_sha256"] == reference["content_sha256"],
        "selected material identity or content hash changed"
    );
    let files = decode_files(&sample)?;
    crate::extensions::video::sample::validate_bundle(&sample.manifest, &files)?;
    Ok(sample)
}
pub(super) fn frame_png(sample: &Sample, id: &str) -> Result<Vec<u8>> {
    let frame = sample.manifest["frames"]
        .as_array()
        .context("frames missing")?
        .iter()
        .find(|frame| frame["id"] == id)
        .context("frame missing")?;
    let files = decode_files(sample)?;
    let bytes = files
        .get(required(frame, "path")?)
        .context("frame PNG missing")?;
    ensure!(
        format!("{:x}", Sha256::digest(bytes)) == required(frame, "sha256")?,
        "frame content hash changed"
    );
    Ok(bytes.clone())
}
pub(super) fn read_bounded_json(path: &std::path::Path, limit: u64) -> Result<Value> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= limit,
        "invalid or oversized frozen data file"
    );
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}
