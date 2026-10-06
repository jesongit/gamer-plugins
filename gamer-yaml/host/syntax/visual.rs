//! Version 2 is a source frontend, not another interpreter. Every visual step
//! lowers to existing function/if wire instructions; both live and replay runs
//! consume precisely the same program and native observation implementation.
use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default)]
pub(crate) struct SourceLocation {
    pub path: String,
    pub id: String,
}

fn key(name: &str) -> YamlValue {
    YamlValue::String(name.into())
}
fn get<'a>(map: &'a Mapping, name: &str) -> Option<&'a YamlValue> {
    map.get(key(name))
}
fn error(path: &str, msg: impl Into<String>) -> Vec<Diagnostic> {
    one_diagnostic("yaml.visual.invalid", path, msg)
}
fn document(source: &str) -> Result<Mapping, Vec<Diagnostic>> {
    let value: YamlValue = serde_yaml::from_str(source).map_err(yaml_to_diagnostic)?;
    let mut doc = value
        .as_mapping()
        .cloned()
        .ok_or_else(|| error("", "文档必须是映射"))?;
    if doc.remove(key("version")).and_then(|v| v.as_u64()) != Some(2) {
        return Err(one_diagnostic(
            "yaml.version.unsupported",
            "version",
            "仅支持 version: 2；旧脚本必须升级，不会按新语法静默运行",
        ));
    }
    Ok(doc)
}
fn targets(doc: &mut Mapping) -> Result<Mapping, Vec<Diagnostic>> {
    let value = doc
        .remove(key("targets"))
        .unwrap_or(YamlValue::Mapping(Mapping::new()));
    let map = value
        .as_mapping()
        .cloned()
        .ok_or_else(|| error("targets", "targets 必须是命名视觉目标映射"))?;
    for (name, spec) in &map {
        let name = name
            .as_str()
            .filter(|s| is_identifier(s))
            .ok_or_else(|| error("targets", "目标名须为小写标识符"))?;
        let spec = spec.as_mapping().ok_or_else(|| {
            error(
                &format!("targets.{name}"),
                "目标必须为 template/threshold/region 映射",
            )
        })?;
        if get(spec, "template").is_none() {
            return Err(error(&format!("targets.{name}"), "视觉目标缺少 template"));
        }
        for field in spec.keys() {
            if !matches!(field.as_str(), Some("template" | "threshold" | "region")) {
                return Err(error("targets", "目标只允许 template/threshold/region"));
            }
        }
    }
    Ok(map)
}

pub(super) fn parse_script(source: &str) -> Result<Script, Vec<Diagnostic>> {
    let original: YamlValue = serde_yaml::from_str(source).map_err(yaml_to_diagnostic)?;
    let mut doc = document(source)?;
    let targets = targets(&mut doc)?;
    if let Some(field) = doc
        .keys()
        .find(|k| !matches!(k.as_str(), Some("name" | "params" | "vars" | "run")))
    {
        return Err(one_diagnostic(
            "yaml.top.unknown",
            "",
            format!("未知顶层字段 {field:?}"),
        ));
    }
    let mut sources = BTreeMap::new();
    let mut ids = BTreeSet::new();
    let run = doc
        .remove(key("run"))
        .ok_or_else(|| error("run", "脚本必须包含 run"))?;
    let run = lower_steps(&run, &targets, "run", "run", &mut sources, &mut ids)?;
    doc.insert(key("run"), YamlValue::Sequence(run));
    let mut script =
        parse_lowered_script(&serde_yaml::to_string(&doc).unwrap()).map_err(|mut ds| {
            remap_diagnostics(&mut ds, &sources);
            ds
        })?;
    script.original = Some(original);
    script.source_map = sources;
    Ok(script)
}
pub(super) fn parse_library(source: &str) -> Result<FunctionLibrary, Vec<Diagnostic>> {
    let mut doc = document(source)?;
    let targets = targets(&mut doc)?;
    if doc.keys().any(|k| k.as_str() != Some("functions")) {
        return Err(one_diagnostic(
            if doc.contains_key(key("functions")) {
                "yaml.top.unknown"
            } else {
                "yaml.functions.missing"
            },
            "functions",
            "函数库须包含 functions 包装，仅支持 version/targets/functions",
        ));
    }
    if !doc.contains_key(key("functions")) {
        return Err(one_diagnostic(
            "yaml.functions.missing",
            "functions",
            "函数文件缺少 functions: 顶层包装",
        ));
    }
    let defs = doc
        .get_mut(key("functions"))
        .and_then(YamlValue::as_mapping_mut)
        .ok_or_else(|| error("functions", "缺少 functions 映射"))?;
    let mut originals = BTreeMap::new();
    for (name, value) in defs.iter_mut() {
        let name = name
            .as_str()
            .ok_or_else(|| error("functions", "函数名须为字符串"))?;
        let original = value.clone();
        let def = value
            .as_mapping_mut()
            .ok_or_else(|| error(name, "函数定义须为映射"))?;
        let run = def.remove(key("run")).ok_or_else(|| {
            one_diagnostic(
                "yaml.functions.shape",
                &format!("functions.{name}"),
                "函数缺少 run",
            )
        })?;
        let mut map = BTreeMap::new();
        let mut ids = BTreeSet::new();
        let run = lower_steps(
            &run,
            &targets,
            &format!("{name}.run"),
            &format!("functions.{name}.run"),
            &mut map,
            &mut ids,
        )?;
        def.insert(key("run"), YamlValue::Sequence(run));
        originals.insert(name.to_string(), (original, map));
    }
    let mut library = parse_lowered_function_library(&serde_yaml::to_string(&doc).unwrap())
        .map_err(|mut diagnostics| {
            for (_, map) in originals.values() {
                remap_diagnostics(&mut diagnostics, map);
            }
            diagnostics
        })?;
    for (name, def) in &mut library {
        let (original, map) = originals.remove(name).unwrap();
        def.original = Some(original);
        def.source_map = map;
        def.targets = targets.clone();
    }
    Ok(library)
}
pub(super) fn remap_diagnostics(ds: &mut [Diagnostic], map: &BTreeMap<String, SourceLocation>) {
    for diagnostic in ds {
        let path = diagnostic
            .path
            .strip_prefix("functions.")
            .unwrap_or(&diagnostic.path);
        if let Some((wire, location)) = map
            .iter()
            .filter(|(wire, _)| {
                path == wire.as_str()
                    || path
                        .strip_prefix(wire.as_str())
                        .is_some_and(|suffix| suffix.starts_with('.'))
            })
            .max_by_key(|(wire, _)| wire.len())
        {
            diagnostic.path = format!("{}{}", location.path, &path[wire.len()..]);
        }
    }
}
fn duration(value: &YamlValue, path: &str) -> Result<(), Vec<Diagnostic>> {
    if value
        .as_str()
        .is_some_and(|s| s.starts_with('$') && !s.starts_with("$$"))
    {
        expr_from_yaml(value, path)?;
        return Ok(());
    }

    let n = match value {
        YamlValue::Number(n) => n.as_f64(),
        YamlValue::String(s) => parse_duration_ms(s),
        _ => None,
    };
    if !n.is_some_and(|v| v.is_finite() && (0.0..=3_600_000.0).contains(&v)) {
        return Err(error(path, "等待时长须为 0..3600000ms 的有限 duration"));
    }
    Ok(())
}
fn lower_steps(
    value: &YamlValue,
    targets: &Mapping,
    wire_prefix: &str,
    source_prefix: &str,
    sources: &mut BTreeMap<String, SourceLocation>,
    ids: &mut BTreeSet<String>,
) -> Result<Vec<YamlValue>, Vec<Diagnostic>> {
    let steps = value
        .as_sequence()
        .ok_or_else(|| error(source_prefix, "步骤必须为列表"))?;
    let mut out = Vec::new();
    for (n, step) in steps.iter().enumerate() {
        let path = format!("{source_prefix}[{n}]");
        let mut map = step
            .as_mapping()
            .cloned()
            .ok_or_else(|| error(&path, "步骤须为映射"))?;
        let id = match map.remove(key("id")) {
            Some(YamlValue::String(s)) if is_identifier(&s) => s,
            None => path.clone(),
            _ => return Err(error(&path, "id 须为小写标识符")),
        };
        if !ids.insert(id.clone()) {
            return Err(error(&path, format!("重复步骤 id {id}")));
        }
        let loc = SourceLocation {
            path: path.clone(),
            id,
        };
        let at = format!("{wire_prefix}[{}]", out.len());
        sources.insert(at.clone(), loc.clone());
        let visual = ["wait", "optional", "finish"]
            .iter()
            .find(|k| get(&map, k).is_some())
            .copied();
        if let Some(kind) = visual {
            let mut target = map.remove(key(kind)).unwrap();
            if kind == "optional" {
                if let Some(nested) = target.as_mapping().filter(|m| get(m, "find").is_some()) {
                    let mut nested = nested.clone();
                    target = nested.remove(key("find")).unwrap();
                    for (k, v) in nested {
                        if map.insert(k, v).is_some() {
                            return Err(error(&path, "optional 字段重复"));
                        }
                    }
                }
            }
            let target_name = target
                .as_str()
                .filter(|s| targets.contains_key(key(s)))
                .map(str::to_owned);
            let spec = if let Some(name) = &target_name {
                targets.get(key(name)).unwrap().clone()
            } else {
                target
            };
            let mut args = spec
                .as_mapping()
                .cloned()
                .ok_or_else(|| error(&path, "视觉目标须为 targets 中的名称或 {template: ...}"))?;
            if get(&args, "template").is_none() {
                return Err(error(&path, "缺少 template"));
            }
            for field in args.keys() {
                if !matches!(field.as_str(), Some("template" | "threshold" | "region")) {
                    return Err(error(&path, "目标只允许 template/threshold/region"));
                }
            }
            let timeout = map
                .remove(key("timeout"))
                .unwrap_or(key(if kind == "optional" { "0ms" } else { "10s" }));
            duration(&timeout, &path)?;
            let interval = map.remove(key("interval")).unwrap_or(key("250ms"));
            duration(&interval, &path)?;
            let alias = match map.remove(key("as")) {
                Some(YamlValue::String(s)) => Some(s),
                None => None,
                _ => return Err(error(&path, "as 必须是变量名字符串")),
            }
            .or(target_name)
            .unwrap_or_else(|| format!("__visual_{}", sources.len()));
            if !is_identifier(&alias) {
                return Err(error(&path, "as 须为小写标识符"));
            }
            let then = map.remove(key("then"));
            if !map.is_empty() {
                return Err(error(
                    &path,
                    format!("视觉步骤含未知字段 {:?}", map.keys().collect::<Vec<_>>()),
                ));
            }
            if kind == "finish" && then.is_some() {
                return Err(error(&path, "finish 不接受 then"));
            }
            args.insert(key("timeout"), timeout);
            args.insert(key("interval"), interval);
            args.insert(key("required"), YamlValue::Bool(kind != "optional"));
            let mut call = Mapping::new();
            call.insert(
                key(if kind == "finish" {
                    "finish"
                } else {
                    "observe"
                }),
                YamlValue::Mapping(args),
            );
            call.insert(key("as"), key(&alias));
            out.push(YamlValue::Mapping(call));
            if let Some(then) = then {
                let guard_path = format!("{wire_prefix}[{}]", out.len());
                sources.insert(guard_path.clone(), loc);
                let lowered = lower_steps(
                    &then,
                    targets,
                    &format!("{guard_path}.then"),
                    &format!("{path}.then"),
                    sources,
                    ids,
                )?;
                let mut guard = Mapping::new();
                guard.insert(key("if"), key(&format!("${alias}")));
                guard.insert(key("then"), YamlValue::Sequence(lowered));
                out.push(YamlValue::Mapping(guard));
            }
            continue;
        }
        if let Some(YamlValue::String(name)) = map.get_mut(key("tap")) {
            if targets.contains_key(key(name)) {
                *name = format!("${name}");
            }
        }
        // Every nested branch is lowered, preserving the single interpreter's scope/budget rules.
        for branch in ["then", "else", "do"] {
            if let Some(child) = map.remove(key(branch)) {
                map.insert(
                    key(branch),
                    YamlValue::Sequence(lower_steps(
                        &child,
                        targets,
                        &format!("{at}.{branch}"),
                        &format!("{path}.{branch}"),
                        sources,
                        ids,
                    )?),
                );
            }
        }
        if let Some(YamlValue::Mapping(match_map)) = map.get_mut(key("match_templates")) {
            if let Some(YamlValue::Sequence(cases)) = match_map.get_mut(key("cases")) {
                for (i, case) in cases.iter_mut().enumerate() {
                    if let Some(case) = case.as_mapping_mut() {
                        if let Some(child) = case.remove(key("do")) {
                            case.insert(
                                key("do"),
                                YamlValue::Sequence(lower_steps(
                                    &child,
                                    targets,
                                    &format!("{at}.cases[{i}].do"),
                                    &format!("{path}.cases[{i}].do"),
                                    sources,
                                    ids,
                                )?),
                            );
                        }
                    }
                }
            }
            if let Some(child) = match_map.remove(key("else")) {
                match_map.insert(
                    key("else"),
                    YamlValue::Sequence(lower_steps(
                        &child,
                        targets,
                        &format!("{at}.else"),
                        &format!("{path}.else"),
                        sources,
                        ids,
                    )?),
                );
            }
        }
        out.push(YamlValue::Mapping(map));
    }
    Ok(out)
}
pub(super) fn apply_source_map(value: &mut Value, map: &BTreeMap<String, SourceLocation>) {
    match value {
        Value::Array(values) => {
            for v in values {
                apply_source_map(v, map)
            }
        }
        Value::Object(o) => {
            if let Some(loc) = o
                .get("path")
                .and_then(Value::as_str)
                .and_then(|p| map.get(p))
            {
                o.insert("path".into(), json!(loc.path));
                o.insert("source_id".into(), json!(loc.id));
            }
            for v in o.values_mut() {
                if v.is_array() || v.is_object() {
                    apply_source_map(v, map)
                }
            }
        }
        _ => {}
    }
}
pub(super) fn function_wire_steps(
    name: &str,
    def: &FunctionDef,
    functions: &FunctionLibrary,
) -> Value {
    let mut v = Value::Array(wire_steps(&def.run, &format!("{name}.run"), functions));
    apply_source_map(&mut v, &def.source_map);
    v
}
pub(super) fn rename_source(
    source: &str,
    old_name: &str,
    old_short: &str,
    new_short: &str,
) -> Result<Option<String>, Vec<Diagnostic>> {
    document(source)?;
    let mut doc: YamlValue = serde_yaml::from_str(source).map_err(yaml_to_diagnostic)?;
    let mut changed = false;
    fn visit(v: &mut YamlValue, old: &str, short: &str, new: &str, changed: &mut bool) {
        match v {
            YamlValue::Mapping(m) => {
                for (k, v) in m {
                    if matches!(
                        k.as_str(),
                        Some("template" | "find" | "wait_find" | "tap_template" | "wait_disappear")
                    ) && v.as_str().is_some_and(|s| s == old || s == short)
                    {
                        *v = YamlValue::String(new.into());
                        *changed = true;
                    } else if matches!(k.as_str(), Some("templates" | "obstacles")) {
                        if let YamlValue::Sequence(items) = v {
                            for item in items {
                                if item.as_str().is_some_and(|s| s == old || s == short) {
                                    *item = YamlValue::String(new.into());
                                    *changed = true;
                                }
                            }
                        }
                    } else {
                        visit(v, old, short, new, changed)
                    }
                }
            }
            YamlValue::Sequence(s) => {
                for v in s {
                    visit(v, old, short, new, changed)
                }
            }
            _ => {}
        }
    }
    visit(&mut doc, old_name, old_short, new_short, &mut changed);
    Ok(changed.then(|| serde_yaml::to_string(&doc).unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visual_lowering_keeps_observation_and_action_separate() {
        let s=parse_script("version: 2\ntargets:\n  claim: {template: claim.png}\n  done: {template: done.png}\nrun:\n  - id: claim_button\n    wait: claim\n    then:\n      - tap: claim\n  - optional: {find: claim, then: [{tap: claim}]}\n  - finish: done\n").unwrap();
        let p = build_program(&s, &vec![], JsonMap::new(), 0);
        assert_eq!(p["run"][0]["fn"], "observe");
        assert_eq!(p["run"][0]["source_id"], "claim_button");
        assert_eq!(p["run"][1]["then"][0]["path"], "run[0].then[0]");
        assert_eq!(p["run"][2]["args"]["value"]["timeout"]["value"], "0ms");
        assert!(serialize_script(&s).contains("targets:"));
    }
    #[test]
    fn rejects_legacy_unknown_and_unbounded_waits() {
        for s in [
            "run: []",
            "version: 1\nrun: []",
            "version: 2\nrun: [{wait: missing}]",
            "version: 2\nrun: [{wait: {template: x}, timeout: 999999999s}]",
            "version: 2\nrun: [{optional: {find: {template: x}, typo: true}}]",
        ] {
            assert!(parse_script(s).is_err(), "{s}");
        }
    }
}
