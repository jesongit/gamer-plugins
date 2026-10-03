//! Package-owned guide memories. JSON is authoritative; SQLite is disposable.
//! Network calls never hold the package snapshot/commit barrier.
use super::services::{EmbeddingPurpose, ServiceConnection};
use crate::resources::{validate_scope_id, PackageStore, ResourceEntry};
use anyhow::{bail, ensure, Context, Result};
use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

mod imports;
mod index;
#[cfg(test)]
mod tests;
const PLUGIN: &str = "gamer-ai";
const FORMAT: u32 = 1;
const MAX_TEXT: usize = 1024 * 1024;
const MAX_BODY: usize = 512 * 1024;
type Snapshot = (Vec<(Memory, String)>, Vec<Value>);
const PROTECTED: &[&str] = &[
    "title",
    "body",
    "tags",
    "applicability",
    "game_version",
    "status",
    "validation",
];

pub struct MemoryStore {
    packages: Arc<PackageStore>,
    root: PathBuf,
    gate: Mutex<()>,
    index_building: AtomicBool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Memory {
    pub format_version: u32,
    pub id: String,
    pub title: String,
    pub body: String,
    pub kind: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub applicability: String,
    #[serde(default = "unknown")]
    pub game_version: String,
    pub validation: String,
    pub status: String,
    pub revision: u64,
    #[serde(default)]
    pub protected_fields: BTreeSet<String>,
    #[serde(default)]
    pub sources: Vec<Value>,
    pub created_at: String,
    pub updated_at: String,
    pub reason: String,
    pub actor: String,
    pub operation_id: String,
    pub operation_fingerprint: String,
    /// Repair receipts after a crash between the JSON commit and receipt write.
    #[serde(default)]
    applied_operations: BTreeMap<String, Applied>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Applied {
    input: String,
    revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Tombstone {
    format_version: u32,
    id: String,
    title_hash: String,
    body_hash: String,
    permanent: bool,
    updated_at: String,
    operation_id: String,
    operation_fingerprint: String,
    #[serde(default)]
    source_fingerprints: BTreeSet<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dictionary {
    #[serde(default)]
    pub terms: Vec<String>,
    #[serde(default)]
    pub aliases: BTreeMap<String, Vec<String>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    pub format_version: u32,
    pub id: String,
    pub title: String,
    pub filename: String,
    pub format: String,
    /// Raw Markdown/TXT is stored separately so a 1MiB UTF-8 guide does not
    /// become an oversized escaped JSON resource invisible to PackageStore.
    #[serde(skip_serializing, default)]
    pub text: String,
    pub game_version: String,
    pub source_url: Option<String>,
    pub revision: u64,
    pub created_at: String,
    pub updated_at: String,
    pub deleted: bool,
    pub content_hash: String,
    #[serde(default)]
    applied_operations: BTreeMap<String, Applied>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct ImportJob {
    pub id: String,
    pub package: String,
    pub source_id: String,
    pub source_revision: u64,
    pub title: String,
    pub status: String,
    pub total: usize,
    pub processed: usize,
    pub counts: BTreeMap<String, usize>,
    pub created_at: String,
    pub updated_at: String,
    pub error: Option<String>,
    pub chunks: Vec<ImportChunk>,
    #[serde(default)]
    pub limits: super::Limits,
    #[serde(default)]
    pub usage: super::Usage,
    #[serde(default)]
    open_requests: BTreeSet<String>,
    #[serde(default)]
    usage_receipts: BTreeMap<String, String>,
    #[serde(default)]
    unknown_usage: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportChunk {
    pub id: String,
    pub section: String,
    pub text: String,
    pub state: String,
    pub claim_id: Option<String>,
    pub claimed_at: Option<i64>,
    pub outcome: Option<Value>,
}
fn unknown() -> String {
    "unknown".into()
}
fn now() -> String {
    Utc::now().to_rfc3339()
}
fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn id(args: &Value) -> Result<&str> {
    let value = args
        .get("id")
        .and_then(Value::as_str)
        .context("memory.id_required")?;
    validate_scope_id("memory id", value)?;
    Ok(value)
}
fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("memory.{key}_required"))
}
fn bounded_limit(args: &Value, default: usize, max: usize) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(default as u64)
        .clamp(1, max as u64) as usize
}
fn path(id: &str) -> String {
    format!("memories/{id}.json")
}
fn operation(args: &Value, name: &str, allow_protected: bool) -> Result<(String, String)> {
    let op = required(args, "operation_id")?;
    ensure!(
        op.len() <= 160 && !op.chars().any(char::is_control),
        "memory.operation_id_invalid"
    );
    let fp = hash(&serde_json::to_string(
        &json!({"name":name,"args":args,"delegated":allow_protected}),
    )?);
    Ok((op.to_owned(), fp))
}

impl MemoryStore {
    pub fn new(packages: Arc<PackageStore>, root: &Path) -> Self {
        Self {
            packages,
            root: root.join("extension-data/gamer-ai"),
            gate: Mutex::new(()),
            index_building: AtomicBool::new(false),
        }
    }
    pub async fn call(
        &self,
        name: &str,
        package: &str,
        args: Value,
        embedding: Option<&ServiceConnection>,
        allow_protected: bool,
    ) -> Result<Value> {
        self.call_cancellable(
            name,
            package,
            args,
            embedding,
            allow_protected,
            &AtomicBool::new(false),
        )
        .await
    }
    /// Import writes recheck pause/cancel/source changes in the same short commit
    /// barrier as the mutation; a check before a model request is insufficient.
    pub async fn call_import_cancellable(
        &self,
        name: &str,
        package: &str,
        args: Value,
        job_id: &str,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        ensure!(
            ["memory_create", "memory_update", "memory_set_status"].contains(&name),
            "memory.import_tool_invalid"
        );
        ensure!(args.is_object(), "memory.arguments_object_required");
        for key in [
            "package",
            "package_id",
            "content_package",
            "human",
            "user_requested",
            "actor",
            "protected_fields",
        ] {
            ensure!(
                args.get(key).is_none(),
                "memory.untrusted_identity_field: {key}"
            );
        }
        let mut result = {
            let _guard = self.gate.lock();
            self.packages.with_package_write(package, || {
                ensure!(!cancel.load(Ordering::Acquire), "memory.cancelled");
                ensure!(
                    self.import_job_active_unlocked(package, job_id)?,
                    "memory.import_not_active"
                );
                self.mutate(name, package, &args, false)
            })?
        };
        if let Some(out) = result.as_object_mut() {
            out.insert(
                "index".into(),
                self.sync_index(package).unwrap_or_else(
                    |error| json!({"pending_repair":true,"error":error.to_string()}),
                ),
            );
        }
        Ok(result)
    }
    pub async fn call_cancellable(
        &self,
        name: &str,
        package: &str,
        args: Value,
        embedding: Option<&ServiceConnection>,
        allow_protected: bool,
        cancel: &AtomicBool,
    ) -> Result<Value> {
        validate_scope_id("package id", package)?;
        self.packages.manifest(package)?;
        ensure!(args.is_object(), "memory.arguments_object_required");
        // Identity and package scope only come from the caller's authenticated context.
        for key in [
            "package",
            "package_id",
            "content_package",
            "human",
            "user_requested",
            "actor",
            "protected_fields",
        ] {
            ensure!(
                args.get(key).is_none(),
                "memory.untrusted_identity_field: {key}"
            );
        }
        match name {
            "memory_search" => self.search(package, &args, embedding, cancel).await,
            "memory_index_rebuild" => self.rebuild(package, embedding, cancel).await,
            "memory_index_status" => self.index_status_connection(package, embedding),
            "memory_list" => self.list(package, &args),
            "memory_get" => self.get(package, &args),
            "memory_history" => self.history(package, &args),
            "memory_dictionary_get" => self.dictionary_get(package),
            "memory_import_jobs" => self.jobs(package, &args),
            "memory_source_get" => self.source_get(package, &args),
            _ => {
                ensure!(!cancel.load(Ordering::Acquire), "memory.cancelled");
                let result = {
                    let _guard = self.gate.lock();
                    self.packages.with_package_write(package, || {
                        self.mutate(name, package, &args, allow_protected)
                    })?
                };
                // A cache failure must never turn a committed JSON mutation into a failed save.
                let sync = self.sync_index(package);
                let mut result = result;
                if let Value::Object(ref mut out) = result {
                    out.insert("index".into(), match sync {
                        Ok(status) => status,
                        Err(error) => json!({"keyword_ready":false,"semantic_ready":false,"pending_repair":true,"error":error.to_string()})
                    });
                }
                Ok(result)
            }
        }
    }
    fn read_memory(&self, pkg: &str, memory_id: &str) -> Result<(Memory, ResourceEntry)> {
        validate_scope_id("memory id", memory_id)?;
        if let Some(t) = self.tombstone(pkg, memory_id)? {
            ensure!(!t.permanent, "memory.permanently_deleted: {memory_id}");
        }
        let entry = self
            .packages
            .read_text(pkg, PLUGIN, &path(memory_id))?
            .context("memory.not_found")?;
        ensure!(
            entry.content.len() <= MAX_BODY + 256 * 1024,
            "memory.document_too_large"
        );
        let memory: Memory =
            serde_json::from_str(&entry.content).context("memory.invalid_document")?;
        validate_memory(&memory)?;
        ensure!(memory.id == memory_id, "memory.id_mismatch");
        Ok((memory, entry))
    }
    fn snapshot(&self, pkg: &str) -> Result<Snapshot> {
        self.packages.with_package_read(pkg, || {
            let mut memories = Vec::new(); let mut diagnostics = Vec::new();
            for entry in self.packages.list(pkg, PLUGIN, "memories")? {
                if !entry.path.ends_with(".json") { continue; }
                let Some(memory_id) = entry.path.strip_prefix("memories/").and_then(|s| s.strip_suffix(".json")) else { continue; };
                match self.read_memory(pkg, memory_id) {
                    Ok((m,e)) => memories.push((m,e.version())),
                    Err(error) => diagnostics.push(json!({"path":entry.path,"error":error.to_string()})),
                }
            }
            for entry in self.packages.list(pkg,PLUGIN,"memory-unrecognized")? {diagnostics.push(json!({"path":entry.path,"error":"memory.imported_unknown_format_preserved","size":entry.size}));}
            Ok((memories, diagnostics))
        })
    }
    fn list(&self, pkg: &str, args: &Value) -> Result<Value> {
        let (mut memories, diagnostics) = self.snapshot(pkg)?;
        memories.retain(|(m, _)| {
            args.get("status")
                .and_then(Value::as_str)
                .is_none_or(|s| s == "any" || m.status == s)
                && args
                    .get("validation")
                    .and_then(Value::as_str)
                    .is_none_or(|s| s == "any" || m.validation == s)
                && args
                    .get("kind")
                    .and_then(Value::as_str)
                    .is_none_or(|s| s == "any" || m.kind == s)
                && (!args
                    .get("protected_only")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || !m.protected_fields.is_empty())
        });
        memories.sort_by(|a, b| {
            b.0.updated_at
                .cmp(&a.0.updated_at)
                .then(a.0.id.cmp(&b.0.id))
        });
        let total = memories.len();
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let items: Vec<Value> = memories
            .into_iter()
            .skip(offset)
            .take(bounded_limit(args, 30, 100))
            .map(|(m, v)| {
                let mut value = summary(&m, &v);
                value["source_conflicts"] =
                    json!(self.source_conflicts(pkg, &m).unwrap_or_default());
                value["effective_validation"] =
                    json!(if self.source_review_required(pkg, &m).unwrap_or(true) {
                        "pending"
                    } else {
                        &m.validation
                    });
                value
            })
            .collect();
        Ok(json!({"items":items,"total":total,"diagnostics":diagnostics}))
    }
    fn get(&self, pkg: &str, args: &Value) -> Result<Value> {
        self.packages.with_package_read(pkg, || {
            let memory_id = id(args)?;
            let (current, current_entry) = self.read_memory(pkg, memory_id)?;
            let (mut memory, version) = match args.get("revision").and_then(Value::as_u64) {
                Some(rev) if rev != current.revision => {
                    ensure!(rev > 0 && current.applied_operations.values().any(|a|a.revision==rev), "memory.revision_not_committed");
                    let e = self.packages.read_text(pkg,PLUGIN,&format!("memory-revisions/{memory_id}/{rev}.json"))?.context("memory.revision_not_found")?;
                    let m:Memory=serde_json::from_str(&e.content)?; validate_memory(&m)?;
                    (m,e.version())
                },
                _ => (current,current_entry.version()),
            };
            let mut selected: Option<Value> = None;
            if args.get("section").is_some() || args.get("chunk_id").is_some() {
                let chunks = index::chunks(&memory, 24 * 1024);
                let matching: Vec<_> = chunks.into_iter().filter(|c| {
                    args.get("section").and_then(Value::as_str).is_none_or(|s|c.section==s)
                    && args.get("chunk_id").and_then(Value::as_str).is_none_or(|s|c.id==s)
                }).collect();
                ensure!(!matching.is_empty(), "memory.chunk_not_found");
                memory.body = matching.iter().map(|c|c.text.as_str()).collect::<Vec<_>>().join("\n\n");
                selected = Some(serde_json::to_value(matching)?);
            }
            let mut public=public_memory(&memory);public["effective_validation"]=json!(if self.source_review_required(pkg,&memory)?{"pending"}else{&memory.validation});
            Ok(json!({"memory":public,"version":version,"revision":memory.revision,"selected_chunks":selected,"source_conflicts":self.source_conflicts(pkg,&memory)?,
                "reference":{"id":memory.id,"revision":memory.revision,"version":version}}))
        })
    }
    fn history(&self, pkg: &str, args: &Value) -> Result<Value> {
        self.packages.with_package_read(pkg, || {
            let memory_id=id(args)?; let (m,_)=self.read_memory(pkg,memory_id)?;
            let offset=args.get("offset").and_then(Value::as_u64).unwrap_or(0);
            let mut items=Vec::new();
            let committed:BTreeSet<u64>=m.applied_operations.values().map(|a|a.revision).chain(std::iter::once(m.revision)).collect();
            let total=committed.len();
            for rev in committed.into_iter().rev().skip(offset as usize).take(bounded_limit(args,20,100)) {
                if let Some(e)=self.packages.read_text(pkg,PLUGIN,&format!("memory-revisions/{memory_id}/{rev}.json"))? {
                    match serde_json::from_str::<Memory>(&e.content) {
                        Ok(old) => items.push(json!({"memory":public_memory(&old),"revision":rev,"version":e.version()})),
                        Err(error)=>items.push(json!({"revision":rev,"error":error.to_string()})),
                    }
                }
            }
            Ok(json!({"items":items,"total":total}))
        })
    }
    fn tombstone(&self, pkg: &str, memory_id: &str) -> Result<Option<Tombstone>> {
        self.packages
            .read_text(pkg, PLUGIN, &format!("memory-tombstones/{memory_id}.json"))?
            .map(|e| serde_json::from_str(&e.content).map_err(Into::into))
            .transpose()
    }
    fn receipt(&self, pkg: &str, op: &str, fp: &str) -> Result<Option<Value>> {
        if let Some(e) = self.packages.read_text(
            pkg,
            PLUGIN,
            &format!("memory-operation-intents/{}.json", hash(op)),
        )? {
            let v: Value = serde_json::from_str(&e.content)?;
            ensure!(
                v["fingerprint"].as_str() == Some(fp),
                "memory.operation_id_reused"
            );
        }
        if let Some(e) =
            self.packages
                .read_text(pkg, PLUGIN, &format!("memory-operations/{}.json", hash(op)))?
        {
            let v: Value = serde_json::from_str(&e.content)?;
            ensure!(
                v["fingerprint"].as_str() == Some(fp),
                "memory.operation_id_reused"
            );
            return Ok(Some(v["result"].clone()));
        }
        // The current document is the authoritative commit marker, not a prepared revision.
        for e in self.packages.list(pkg, PLUGIN, "memories")? {
            let Some(text) = e.content else { continue };
            let Ok(m) = serde_json::from_str::<Memory>(&text) else {
                continue;
            };
            if let Some(applied) = m.applied_operations.get(op) {
                ensure!(applied.input == fp, "memory.operation_id_reused");
                let revision = self
                    .packages
                    .read_text(
                        pkg,
                        PLUGIN,
                        &format!("memory-revisions/{}/{}.json", m.id, applied.revision),
                    )?
                    .context("memory.committed_revision_missing")?;
                let result = json!({"saved":true,"id":m.id,"revision":applied.revision,"version":revision.version(),"recovered":true});
                self.save_receipt(pkg, op, fp, &result)?;
                return Ok(Some(result));
            }
        }
        for e in self.packages.list(pkg, PLUGIN, "memory-tombstones")? {
            let Some(text) = e.content else { continue };
            let Ok(t) = serde_json::from_str::<Tombstone>(&text) else {
                continue;
            };
            if t.operation_id == op {
                ensure!(t.operation_fingerprint == fp, "memory.operation_id_reused");
                let result =
                    json!({"saved":true,"id":t.id,"permanent":t.permanent,"recovered":true});
                self.save_receipt(pkg, op, fp, &result)?;
                return Ok(Some(result));
            }
        }
        for e in self.packages.list(pkg, PLUGIN, "memory-sources")? {
            let Some(text) = e.content else { continue };
            let Ok(s) = serde_json::from_str::<Source>(&text) else {
                continue;
            };
            if let Some(applied) = s.applied_operations.get(op) {
                ensure!(applied.input == fp, "memory.operation_id_reused");
                let result = json!({"saved":true,"source_id":s.id,"source_revision":applied.revision,"version":e.version,"recovered":true});
                self.save_receipt(pkg, op, fp, &result)?;
                return Ok(Some(result));
            }
        }
        Ok(None)
    }
    fn save_receipt(&self, pkg: &str, op: &str, fp: &str, result: &Value) -> Result<()> {
        self.write_json_new(
            pkg,
            &format!("memory-operations/{}.json", hash(op)),
            &json!({"format_version":FORMAT,"operation_id":op,"fingerprint":fp,"result":result}),
        )
    }
    fn write_json_new<T: Serialize>(&self, pkg: &str, resource: &str, value: &T) -> Result<()> {
        let text = serde_json::to_string_pretty(value)?;
        if let Some(old) = self.packages.read_text(pkg, PLUGIN, resource)? {
            ensure!(
                old.content == text,
                "memory.immutable_revision_conflict: {resource}"
            );
        } else {
            self.packages
                .write_text(pkg, PLUGIN, resource, &text, None, false)?;
        }
        Ok(())
    }
    fn commit(
        &self,
        pkg: &str,
        mut m: Memory,
        expected: Option<&str>,
        op: &str,
        fp: &str,
    ) -> Result<Value> {
        // Prepared revisions survive failed current-document writes. Reuse an
        // identical operation, or allocate a later immutable revision; never
        // expose an unrelated prepared snapshot as a committed history entry.
        loop {
            let resource = format!("memory-revisions/{}/{}.json", m.id, m.revision);
            let Some(prepared) = self.packages.read_text(pkg, PLUGIN, &resource)? else {
                break;
            };
            let old: Memory = serde_json::from_str(&prepared.content)
                .context("memory.invalid_prepared_revision")?;
            if old.operation_id == op && old.operation_fingerprint == fp {
                m = old;
                break;
            }
            m.revision = m
                .revision
                .checked_add(1)
                .context("memory.revision_overflow")?;
        }
        m.operation_id = op.into();
        m.operation_fingerprint = fp.into();
        m.applied_operations.insert(
            op.into(),
            Applied {
                input: fp.into(),
                revision: m.revision,
            },
        );
        validate_memory(&m)?;
        let text = serde_json::to_string_pretty(&m)?;
        ensure!(
            text.len() <= MAX_BODY + 256 * 1024,
            "memory.document_too_large_keep_body_and_history"
        );
        self.write_json_new(
            pkg,
            &format!("memory-revisions/{}/{}.json", m.id, m.revision),
            &m,
        )?;
        let entry = self
            .packages
            .write_text(pkg, PLUGIN, &path(&m.id), &text, expected, false)?;
        let result = json!({"saved":true,"id":m.id,"revision":m.revision,"version":entry.version(),"memory":public_memory(&m)});
        // Receipt recovery is possible from applied_operations if this final write fails.
        if let Err(error) = self.save_receipt(pkg, op, fp, &result) {
            return Ok(
                json!({"saved":true,"id":m.id,"revision":m.revision,"version":entry.version(),"receipt_pending":true,"diagnostic":error.to_string()}),
            );
        }
        Ok(result)
    }
    fn mutate(&self, name: &str, pkg: &str, args: &Value, delegated: bool) -> Result<Value> {
        let (op, fp) = operation(args, name, delegated)?;
        if let Some(result) = self.receipt(pkg, &op, &fp)? {
            return Ok(result);
        }
        self.write_json_new(
            pkg,
            &format!("memory-operation-intents/{}.json", hash(&op)),
            &json!({"format_version":FORMAT,"operation_id":op,"fingerprint":fp}),
        )?;
        match name {
            "memory_import" => return self.import(pkg, args, &op, &fp),
            "memory_source_update" | "memory_source_delete" => {
                return self.source_mutate(pkg, args, name, &op, &fp, delegated)
            }
            "memory_import_pause" | "memory_import_resume" | "memory_import_cancel" => {
                return self.job_control(pkg, args, name, &op, &fp)
            }
            "memory_dictionary_update" => {
                ensure!(delegated, "memory.user_delegation_required");
                let d: Dictionary = serde_json::from_value(
                    json!({"terms":args.get("terms").cloned().unwrap_or(json!([])),"aliases":args.get("aliases").cloned().unwrap_or(json!({}))}),
                )?;
                validate_dictionary(&d)?;
                let e = self.packages.write_text(
                    pkg,
                    PLUGIN,
                    "memory-dictionary.json",
                    &serde_json::to_string_pretty(&d)?,
                    args.get("expected_version").and_then(Value::as_str),
                    false,
                )?;
                let result = json!({"saved":true,"dictionary":d,"version":e.version()});
                self.save_receipt(pkg, &op, &fp, &result)?;
                return Ok(result);
            }
            "memory_create" => {
                let memory_id = args
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("memory-{}", &hash(&op)[..24]));
                validate_scope_id("memory id", &memory_id)?;
                ensure!(
                    self.packages
                        .read_text(pkg, PLUGIN, &path(&memory_id))?
                        .is_none(),
                    "memory.already_exists"
                );
                ensure!(
                    self.tombstone(pkg, &memory_id)?.is_none(),
                    "memory.id_tombstoned_use_restore"
                );
                let title = required(args, "title")?.to_owned();
                let body = required(args, "body")?.to_owned();
                let sources = parse_sources(args.get("sources"))?;
                if !delegated {
                    self.check_suppression(pkg, &title, &body, &sources)?;
                }
                let timestamp = now();
                let m = Memory {
                    format_version: FORMAT,
                    id: memory_id,
                    title,
                    body,
                    kind: args
                        .get("kind")
                        .and_then(Value::as_str)
                        .unwrap_or("procedure")
                        .into(),
                    tags: parse_tags(args.get("tags"))?,
                    applicability: args
                        .get("applicability")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .into(),
                    game_version: args
                        .get("game_version")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                        .into(),
                    validation: args
                        .get("validation")
                        .and_then(Value::as_str)
                        .unwrap_or("pending")
                        .into(),
                    status: "active".into(),
                    revision: 1,
                    protected_fields: if delegated {
                        PROTECTED
                            .iter()
                            .filter(|f| args.get(**f).is_some())
                            .map(|f| (*f).to_string())
                            .collect()
                    } else {
                        BTreeSet::new()
                    },
                    sources,
                    created_at: timestamp.clone(),
                    updated_at: timestamp,
                    reason: args
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("created")
                        .into(),
                    actor: if delegated { "user_delegated" } else { "ai" }.into(),
                    operation_id: op.clone(),
                    operation_fingerprint: fp.clone(),
                    applied_operations: BTreeMap::new(),
                };
                return self.commit(pkg, m, None, &op, &fp);
            }
            "memory_update" | "memory_set_status" | "memory_restore" | "memory_delete" => {}
            _ => bail!("memory.unknown_tool: {name}"),
        }
        let memory_id = id(args)?;
        let (mut m, e) = self.read_memory(pkg, memory_id)?;
        let expected = required(args, "expected_version")?;
        ensure!(e.version() == expected, "version_conflict");
        if name == "memory_delete"
            && args
                .get("permanent")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            ensure!(delegated, "memory.user_delegation_required");
            let t = Tombstone {
                format_version: FORMAT,
                id: m.id.clone(),
                title_hash: hash(&m.title.trim().to_lowercase()),
                body_hash: hash(m.body.trim()),
                permanent: true,
                updated_at: now(),
                operation_id: op.clone(),
                operation_fingerprint: fp.clone(),
                source_fingerprints: source_fingerprints(&m.sources),
            };
            self.put_tombstone(pkg, &t)?;
            let cleanup = self.remove_memory_files(pkg, &m.id);
            let mut result = json!({"saved":true,"id":m.id,"permanent":true});
            if let Err(error) = cleanup {
                result["cleanup_pending"] = json!(true);
                result["diagnostic"] = json!(error.to_string());
            }
            if let Err(error) = self.save_receipt(pkg, &op, &fp, &result) {
                result["receipt_pending"] = json!(true);
                result["diagnostic"] = json!(error.to_string());
            }
            return Ok(result);
        }
        let mut fields = BTreeSet::new();
        if name == "memory_restore" {
            ensure!(delegated, "memory.user_delegation_required");
            if let Some(rev) = args.get("revision").and_then(Value::as_u64) {
                ensure!(
                    rev == m.revision || m.applied_operations.values().any(|a| a.revision == rev),
                    "memory.revision_not_committed"
                );
                let old = self
                    .packages
                    .read_text(
                        pkg,
                        PLUGIN,
                        &format!("memory-revisions/{memory_id}/{rev}.json"),
                    )?
                    .context("memory.revision_not_found")?;
                let old: Memory = serde_json::from_str(&old.content)?;
                for f in PROTECTED {
                    fields.insert((*f).into());
                }
                m.title = old.title;
                m.body = old.body;
                m.tags = old.tags;
                m.applicability = old.applicability;
                m.game_version = old.game_version;
                m.validation = old.validation;
                m.sources = old.sources;
                m.kind = old.kind;
            }
            m.status = "active".into();
            fields.insert("status".into());
        } else if name == "memory_delete" {
            ensure!(delegated, "memory.user_delegation_required");
            m.status = "deleted".into();
            fields.insert("status".into());
        } else {
            let patch = if name == "memory_update" {
                args.get("patch").context("memory.patch_required")?
            } else {
                args
            };
            ensure!(patch.is_object(), "memory.patch_object_required");
            let allowed = [
                "title",
                "body",
                "kind",
                "tags",
                "applicability",
                "game_version",
                "validation",
                "status",
                "sources",
            ];
            if name == "memory_update" {
                for key in patch.as_object().unwrap().keys() {
                    ensure!(
                        allowed.contains(&key.as_str()),
                        "memory.patch_field_invalid: {key}"
                    );
                }
            }
            for field in allowed {
                if patch.get(field).is_some() {
                    fields.insert(field.to_string());
                }
            }
            ensure!(!fields.is_empty(), "memory.empty_patch");
            if !delegated {
                ensure!(
                    m.status == "active",
                    "memory.inactive_user_restore_required"
                );
                for f in &fields {
                    ensure!(
                        !m.protected_fields.contains(f),
                        "memory.user_field_protected: {f}"
                    );
                }
                ensure!(
                    patch.get("status").is_none_or(
                        |v| v == "active" || (v == "merged" && m.protected_fields.is_empty())
                    ),
                    "memory.user_delegation_required"
                );
                // Additional conditions cannot broaden a protected user instruction through a new kind/source.
                ensure!(
                    !(fields.contains("kind") || fields.contains("sources"))
                        || m.protected_fields.is_empty(),
                    "memory.user_content_protected"
                );
            }
            for f in [
                "title",
                "body",
                "kind",
                "applicability",
                "game_version",
                "validation",
                "status",
            ] {
                if let Some(v) = patch.get(f) {
                    let value = v
                        .as_str()
                        .context("memory.string_field_required")?
                        .to_owned();
                    match f {
                        "title" => m.title = value,
                        "body" => m.body = value,
                        "kind" => m.kind = value,
                        "applicability" => m.applicability = value,
                        "game_version" => m.game_version = value,
                        "validation" => m.validation = value,
                        "status" => m.status = value,
                        _ => unreachable!(),
                    }
                }
            }
            if let Some(v) = patch.get("tags") {
                m.tags = parse_tags(Some(v))?;
            }
            if let Some(v) = patch.get("sources") {
                m.sources = parse_sources(Some(v))?;
            }
        }
        if delegated {
            m.protected_fields.extend(
                fields
                    .into_iter()
                    .filter(|f| PROTECTED.contains(&f.as_str())),
            );
        }
        if !delegated {
            self.check_suppression(pkg, &m.title, &m.body, &m.sources)?;
        }
        m.revision = m
            .revision
            .checked_add(1)
            .context("memory.revision_overflow")?;
        m.updated_at = now();
        m.reason = required(args, "reason")?.into();
        m.actor = if delegated { "user_delegated" } else { "ai" }.into();
        let mut result = self.commit(pkg, m.clone(), Some(expected), &op, &fp)?;
        if matches!(m.status.as_str(), "disabled" | "deleted") {
            if let Err(error) = self.put_tombstone(
                pkg,
                &Tombstone {
                    format_version: FORMAT,
                    id: m.id.clone(),
                    title_hash: hash(&m.title.trim().to_lowercase()),
                    body_hash: hash(m.body.trim()),
                    permanent: false,
                    updated_at: now(),
                    operation_id: op,
                    operation_fingerprint: fp,
                    source_fingerprints: source_fingerprints(&m.sources),
                },
            ) {
                result["suppression_pending"] = json!(true);
                result["diagnostic"] = json!(error.to_string());
            }
        }
        Ok(result)
    }
    fn put_tombstone(&self, pkg: &str, t: &Tombstone) -> Result<()> {
        let p = format!("memory-tombstones/{}.json", t.id);
        let old = self.packages.read_text(pkg, PLUGIN, &p)?;
        self.packages.write_text(
            pkg,
            PLUGIN,
            &p,
            &serde_json::to_string_pretty(t)?,
            old.as_ref().map(|e| e.version()).as_deref(),
            false,
        )?;
        Ok(())
    }
    fn check_suppression(
        &self,
        pkg: &str,
        title: &str,
        body: &str,
        sources: &[Value],
    ) -> Result<()> {
        let th = hash(&title.trim().to_lowercase());
        let bh = hash(body.trim());
        let source_hashes = source_fingerprints(sources);
        for e in self.packages.list(pkg, PLUGIN, "memory-tombstones")? {
            if let Some(text) = e.content {
                let t: Tombstone = serde_json::from_str(&text)?;
                if !t.permanent
                    && self
                        .read_memory(pkg, &t.id)
                        .is_ok_and(|(m, _)| m.status == "active")
                {
                    continue;
                }
                ensure!(
                    t.title_hash != th
                        && t.body_hash != bh
                        && t.source_fingerprints.is_disjoint(&source_hashes),
                    "memory.auto_recreation_suppressed"
                );
            }
        }
        // The canonical status also prevents recreation after a crash before
        // the redundant minimal tombstone was written.
        for (m, _) in self.snapshot(pkg)?.0 {
            if matches!(m.status.as_str(), "disabled" | "deleted") {
                ensure!(
                    hash(&m.title.trim().to_lowercase()) != th
                        && hash(m.body.trim()) != bh
                        && source_fingerprints(&m.sources).is_disjoint(&source_hashes),
                    "memory.auto_recreation_suppressed"
                );
            }
        }
        Ok(())
    }
    fn remove_memory_files(&self, pkg: &str, memory_id: &str) -> Result<()> {
        if self
            .packages
            .read_text(pkg, PLUGIN, &path(memory_id))?
            .is_some()
        {
            self.packages
                .delete_resource(pkg, PLUGIN, &path(memory_id))?;
        }
        for e in self
            .packages
            .list(pkg, PLUGIN, &format!("memory-revisions/{memory_id}"))?
        {
            self.packages.delete_resource(pkg, PLUGIN, &e.path)?;
        }
        // Receipts containing full memory copies would retain permanently deleted prose.
        for e in self.packages.list(pkg, PLUGIN, "memory-operations")? {
            if let Some(text) = e.content {
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    if v["result"]["id"] == memory_id {
                        let minimal = json!({"format_version":FORMAT,"operation_id":v["operation_id"],"fingerprint":v["fingerprint"],"result":{"saved":true,"id":memory_id,"permanently_deleted":true}});
                        let replacement = serde_json::to_string_pretty(&minimal)?;
                        if replacement != text {
                            self.packages.write_text(
                                pkg,
                                PLUGIN,
                                &e.path,
                                &replacement,
                                e.version.as_deref(),
                                false,
                            )?;
                        }
                    }
                }
            }
        }
        // A quarantined derived database can retain the old plaintext too.
        // It is disposable and must not outlive a permanent memory deletion.
        self.purge_corrupt_cache(pkg)?;
        Ok(())
    }
    fn dictionary(&self, pkg: &str) -> Result<(Dictionary, Option<String>)> {
        match self
            .packages
            .read_text(pkg, PLUGIN, "memory-dictionary.json")?
        {
            Some(e) => {
                let d: Dictionary = serde_json::from_str(&e.content)?;
                validate_dictionary(&d)?;
                Ok((d, Some(e.version())))
            }
            None => Ok((Dictionary::default(), None)),
        }
    }
    fn dictionary_get(&self, pkg: &str) -> Result<Value> {
        let (d, v) = self.dictionary(pkg)?;
        Ok(json!({"dictionary":d,"version":v}))
    }
    fn reconcile_permanent_deletions(&self, pkg: &str) -> Result<()> {
        for entry in self.packages.list(pkg, PLUGIN, "memory-tombstones")? {
            if let Some(text) = entry.content {
                let tombstone: Tombstone = serde_json::from_str(&text)?;
                if tombstone.permanent {
                    validate_scope_id("memory id", &tombstone.id)?;
                    self.remove_memory_files(pkg, &tombstone.id)?;
                }
            }
        }
        Ok(())
    }
}

fn parse_tags(v: Option<&Value>) -> Result<Vec<String>> {
    let tags: Vec<String> = serde_json::from_value(v.cloned().unwrap_or(json!([])))?;
    ensure!(
        tags.len() <= 100 && tags.iter().all(|t| !t.trim().is_empty() && t.len() <= 120),
        "memory.tags_invalid"
    );
    Ok(tags)
}
fn parse_sources(v: Option<&Value>) -> Result<Vec<Value>> {
    let sources: Vec<Value> = serde_json::from_value(v.cloned().unwrap_or(json!([])))?;
    ensure!(
        sources.len() <= 100 && serde_json::to_vec(&sources)?.len() <= 128 * 1024,
        "memory.sources_too_large"
    );
    for s in &sources {
        ensure!(s.is_object(), "memory.source_object_required");
    }
    Ok(sources)
}
fn source_fingerprints(sources: &[Value]) -> BTreeSet<String> {
    sources.iter().filter_map(|s|{
        let source_id=s.get("source_id").or_else(||s.get("id")).and_then(Value::as_str);
        let url=s.get("url").or_else(||s.get("source_url")).and_then(Value::as_str);
        let excerpt=s.get("excerpt").and_then(Value::as_str);
        if source_id.is_none()&&url.is_none()&&excerpt.is_none(){return None}
        Some(hash(&json!({"id":source_id,"url":url,"revision":s.get("revision"),"section":s.get("section"),"excerpt_hash":excerpt.map(hash)}).to_string()))
    }).collect()
}
fn validate_memory(m: &Memory) -> Result<()> {
    ensure!(
        m.format_version == FORMAT,
        "memory.unsupported_format: {}",
        m.format_version
    );
    validate_scope_id("memory id", &m.id)?;
    ensure!(
        !m.title.trim().is_empty() && m.title.len() <= 600,
        "memory.title_invalid"
    );
    ensure!(
        !m.body.trim().is_empty() && m.body.len() <= MAX_BODY,
        "memory.body_invalid"
    );
    ensure!(
        ["definition", "pitfall", "procedure"].contains(&m.kind.as_str()),
        "memory.kind_invalid"
    );
    ensure!(
        ["pending", "verified", "invalid"].contains(&m.validation.as_str()),
        "memory.validation_invalid"
    );
    ensure!(
        ["active", "disabled", "deleted", "merged"].contains(&m.status.as_str()),
        "memory.status_invalid"
    );
    ensure!(
        m.revision > 0
            && m.applicability.len() <= 16 * 1024
            && !m.game_version.is_empty()
            && m.game_version.len() <= 120,
        "memory.metadata_invalid"
    );
    parse_tags(Some(&json!(m.tags)))?;
    parse_sources(Some(&json!(m.sources)))?;
    ensure!(
        m.protected_fields
            .iter()
            .all(|f| PROTECTED.contains(&f.as_str())),
        "memory.protection_invalid"
    );
    ensure!(
        m.reason.len() <= 4000 && m.applied_operations.len() <= 10000,
        "memory.history_limit"
    );
    Ok(())
}
fn validate_dictionary(d: &Dictionary) -> Result<()> {
    ensure!(
        d.terms.len() <= 5000 && d.aliases.len() <= 2000,
        "memory.dictionary_too_large"
    );
    for t in d
        .terms
        .iter()
        .chain(d.aliases.keys())
        .chain(d.aliases.values().flatten())
    {
        ensure!(
            !t.trim().is_empty() && t.len() <= 120 && !t.chars().any(char::is_control),
            "memory.dictionary_term_invalid"
        );
    }
    Ok(())
}
fn public_memory(m: &Memory) -> Value {
    let mut v = serde_json::to_value(m).expect("memory serializes");
    v.as_object_mut().unwrap().remove("applied_operations");
    v.as_object_mut().unwrap().remove("operation_fingerprint");
    v
}
fn summary(m: &Memory, version: &str) -> Value {
    json!({"id":m.id,"title":m.title,"summary":m.body.chars().take(220).collect::<String>(),"kind":m.kind,"tags":m.tags,"applicability":m.applicability,"game_version":m.game_version,"validation":m.validation,"status":m.status,"revision":m.revision,"version":version,"sources":m.sources,"protected_fields":m.protected_fields,"updated_at":m.updated_at})
}
