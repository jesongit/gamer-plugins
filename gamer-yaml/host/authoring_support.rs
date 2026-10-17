//! Shared authoring/candidate data encoding. No model or device dependency.
use anyhow::{ensure, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{atomic::AtomicBool, Arc},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileInput {
    pub path: String,
    pub base64: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Sample {
    pub manifest: Value,
    pub files: Vec<FileInput>,
}
pub(super) mod base64_map {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        map: &BTreeMap<String, Vec<u8>>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        map.iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    base64::engine::general_purpose::STANDARD.encode(v),
                )
            })
            .collect::<BTreeMap<_, _>>()
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<BTreeMap<String, Vec<u8>>, D::Error> {
        BTreeMap::<String, String>::deserialize(deserializer)?
            .into_iter()
            .map(|(k, v)| {
                base64::engine::general_purpose::STANDARD
                    .decode(v)
                    .map(|v| (k, v))
                    .map_err(serde::de::Error::custom)
            })
            .collect()
    }
}
pub(super) fn required<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("missing {k}"))
}
pub(super) fn script_name(name: &str) -> Result<String> {
    let name = name.trim();
    ensure!(!name.is_empty() && name.len() <= 200, "invalid script name");
    let name = if name.ends_with(".yaml") {
        name.to_owned()
    } else {
        format!("{name}.yaml")
    };
    crate::resources::sanitize_rel_path(&name)?;
    ensure!(
        !name
            .split('/')
            .next_back()
            .unwrap_or("")
            .starts_with("_function"),
        "script target cannot be a function library"
    );
    Ok(name)
}
pub(super) fn decode_files(sample: &Sample) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    for file in &sample.files {
        crate::resources::sanitize_rel_path(&file.path)?;
        ensure!(!files.contains_key(&file.path), "duplicate sample file");
        files.insert(
            file.path.clone(),
            base64::engine::general_purpose::STANDARD.decode(&file.base64)?,
        );
    }
    Ok(files)
}
pub(super) struct Job {
    pub(super) cancel: Arc<AtomicBool>,
}
