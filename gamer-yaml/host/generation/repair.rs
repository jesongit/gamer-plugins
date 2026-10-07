//! Failure repair starts from the selected run's immutable source, never the
//! current editor or a model-supplied run identity.
use super::*;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct RepairContext {
    pub run_id: String,
    pub entrypoint: String,
    pub source_sha256: String,
    pub minimum_threshold: f64,
    pub failure: Value,
    pub observations: Vec<Value>,
    pub template_versions: BTreeMap<String, String>,
    pub function_sources: BTreeMap<String, String>,
    pub baseline_report: Option<Value>,
}

pub(super) fn from_run(
    package: &str,
    script: &str,
    run_id: &str,
    record: &Value,
    trace: &Value,
) -> Result<(String, RepairContext)> {
    ensure!(record["run_id"] == run_id
        && record["runner_id"] == super::super::YAML_EXTENSION_ID
        && record["entrypoint"] == format!("{package}/{script}"),
        "repair_run_scope: select a failed run belonging to this script/package");
    ensure!(record["state"] == "failed", "repair_run_not_failed");
    let sources = trace["snapshot"]["_source_files"].as_object()
        .context("repair_source_unavailable: run source snapshot is missing or expired")?;
    let path = format!("automations/{script}");
    let yaml = sources.get(&path).and_then(Value::as_str)
        .context("repair_source_unavailable: selected script is absent from run snapshot")?;
    ensure!(yaml.len() <= 512 * 1024, "repair source exceeds512KiB");
    super::super::syntax::parse_script(yaml)
        .map_err(|e| anyhow::anyhow!("repair source invalid: {e:?}"))?;
    let template_versions = serde_json::from_value(trace["snapshot"]["_template_versions"].clone())
        .context("repair template versions missing")?;
    let observations = trace["images"].as_array().into_iter().flatten()
        .rev().take(12).map(|image| json!({"image_id":image["image_id"],
            "kind":image["kind"],"metadata":image["metadata"]})).collect();
    let function_sources = sources.iter().filter(|(name,_)| *name != &path)
        .map(|(name, value)| Ok((name.clone(), value.as_str()
            .context("invalid run function source")?.to_string())))
        .collect::<Result<BTreeMap<_,_>>>()?;
    Ok((yaml.to_string(), RepairContext {
        run_id: run_id.into(), entrypoint: format!("{package}/{script}"),
        source_sha256: format!("{:x}",Sha256::digest(yaml.as_bytes())),
        minimum_threshold: minimum_threshold(yaml)?,
        failure: json!({"error":record["error"],"state":record["state"]}),
        observations, template_versions, function_sources, baseline_report: None,
    }))
}

fn minimum_threshold(yaml: &str) -> Result<f64> {
    let doc: serde_yaml::Value = serde_yaml::from_str(yaml)?;
    Ok(doc["targets"].as_mapping().into_iter().flat_map(|m| m.values())
        .map(|v| v["threshold"].as_f64().unwrap_or(0.8)).reduce(f64::min).unwrap_or(0.8))
}

pub(super) fn check_proposal(context: &RepairContext, yaml: &str) -> Result<()> {
    ensure!(minimum_threshold(yaml)? + f64::EPSILON >= context.minimum_threshold,
        "repair_threshold_lowered: preserve matching thresholds from the failed source (minimum {})",
        context.minimum_threshold);
    fn check(value: &serde_yaml::Value, floor: f64) -> Result<()> {
        if let Some(map) = value.as_mapping() {
            for (key, value) in map {
                if key.as_str() == Some("threshold") {
                    ensure!(value.as_f64().is_some_and(|v| v + f64::EPSILON >= floor),
                        "repair_threshold_lowered: threshold must preserve the original confidence floor");
                }
                check(value,floor)?;
            }
        } else if let Some(list) = value.as_sequence() {
            for value in list { check(value,floor)?; }
        }
        Ok(())
    }
    check(&serde_yaml::from_str(yaml)?,context.minimum_threshold)?;
    Ok(())
}

pub(super) fn check_base(context: &RepairContext, base: &BTreeMap<String,Vec<u8>>) -> Result<()> {
    for (path, expected) in &context.template_versions {
        crate::resources::sanitize_rel_path(path)?;
        ensure!(path.starts_with("templates/"), "invalid repair template scope");
        let bytes = base.get(path).context("repair_template_missing")?;
        ensure!(&format!("{:x}",Sha256::digest(bytes)) == expected,
            "repair_template_changed: selected run's template bytes are no longer available: {path}");
    }
    for (path, source) in &context.function_sources {
        ensure!(base.get(path).map(Vec::as_slice) == Some(source.as_bytes()),
            "repair_function_changed: selected run's function source changed: {path}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repair_uses_failed_snapshot_and_rejects_other_runs_or_missing_evidence() {
        let yaml = "version: 2\nrun: []\n";
        let record = json!({"run_id":"r","runner_id":"gamer-yaml","entrypoint":"p/a.yaml","state":"failed"});
        let trace = json!({"snapshot":{"_source_files":{"automations/a.yaml":yaml},"_template_versions":{}},"images":[]});
        let (source, context) = from_run("p","a.yaml","r",&record,&trace).unwrap();
        assert_eq!(source,yaml);
        assert_eq!(context.source_sha256,format!("{:x}",Sha256::digest(yaml.as_bytes())));
        for (key,value) in [("run_id","other"),("entrypoint","q/a.yaml"),("state","success"),("runner_id","gamer-ai")] {
            let mut wrong = record.clone(); wrong[key]=json!(value);
            assert!(from_run("p","a.yaml","r",&wrong,&trace).is_err(),"{key}");
        }
        assert!(from_run("p","a.yaml","r",&record,&json!({})).is_err());
        let mut context=context;
        context.minimum_threshold=0.9;
        assert!(check_proposal(&context,"version: 2\ntargets:\n  done:\n    template: done.png\n    threshold: 0.8\nrun: []\n").is_err());
        context.template_versions.insert("templates/button.png".into(),format!("{:x}",Sha256::digest(b"original")));
        assert!(check_base(&context,&BTreeMap::from([("templates/button.png".into(),b"changed".to_vec())])).is_err());
        assert!(check_base(&context,&BTreeMap::from([("templates/button.png".into(),b"original".to_vec())])).is_ok());
    }
}
