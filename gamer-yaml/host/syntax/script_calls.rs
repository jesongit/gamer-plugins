use super::*;

pub(crate) fn call_closure(
    mut names: BTreeSet<String>,
    library: &FunctionLibrary,
) -> BTreeSet<String> {
    let mut pending: Vec<_> = names.iter().cloned().collect();
    while let Some(name) = pending.pop() {
        if let Some((_, def)) = library.iter().find(|(target, _)| target == &name) {
            for called in def.called_functions() {
                if names.insert(called.clone()) {
                    pending.push(called);
                }
            }
        }
    }
    names
}

/// Script names are scoped to the current Package. Qualified functions never
/// collide across scripts; unqualified calls inside a script bind to its locals.
pub(crate) fn callable_source(
    path: &str,
    source: &str,
) -> Result<FunctionLibrary, Vec<Diagnostic>> {
    let path = path.strip_prefix("automations/").unwrap_or(path);
    if crate::extensions::gamer_yaml::resources::is_function_library_path(path) {
        return parse_function_library(source);
    }
    // Unrelated invalid imported scripts are unavailable, not a Package-wide
    // veto. Parsing the actual entry remains strict; calling this file then
    // fails as an unresolved callable before any input is sent.
    let script = match parse_script(source) {
        Ok(script) => script,
        Err(_) => return Ok(vec![]),
    };
    let locals: BTreeSet<_> = script
        .functions
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    fn bind(steps: &mut [SurfaceStep], path: &str, locals: &BTreeSet<String>) {
        for step in steps {
            match step {
                SurfaceStep::Call { name, .. } if locals.contains(name) => {
                    *name = format!("{path}#{name}")
                }
                SurfaceStep::If {
                    then_steps,
                    else_steps,
                    ..
                } => {
                    bind(then_steps, path, locals);
                    bind(else_steps, path, locals);
                }
                SurfaceStep::Repeat { body, .. } => bind(body, path, locals),
                SurfaceStep::MatchTemplates {
                    cases, else_steps, ..
                } => {
                    for case in cases {
                        bind(&mut case.body, path, locals);
                    }
                    bind(else_steps, path, locals);
                }
                _ => {}
            }
        }
    }
    let targets = script
        .original
        .as_ref()
        .and_then(|doc| doc.get("targets"))
        .and_then(YamlValue::as_mapping)
        .cloned()
        .unwrap_or_default();
    let mut entry = FunctionDef {
        targets,
        original: None,
        source_map: script
            .source_map
            .iter()
            .map(|(key, value)| (format!("{path}.{key}"), value.clone()))
            .collect(),
        description: script.name.clone(),
        params: script.params.clone(),
        vars: script.vars.clone(),
        returns: None,
        run: script.run.clone(),
    };
    bind(&mut entry.run, path, &locals);
    let mut library = vec![(path.to_string(), entry)];
    for (name, mut def) in script.functions {
        bind(&mut def.run, path, &locals);
        def.source_map = def
            .source_map
            .into_iter()
            .map(|(key, value)| (format!("{path}#{key}"), value))
            .collect();
        library.push((format!("{path}#{name}"), def));
    }
    Ok(library)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Host;
    impl yaml_interp::HostFunctions for Host {
        fn invoke(&self, name: &str, _: Value) -> Result<Value, yaml_interp::HostError> {
            panic!("unexpected native side effect: {name}")
        }
    }
    #[test]
    fn same_named_locals_execute_in_their_own_script_scope() {
        let mut library = callable_source("a.yaml", "functions: {value: {run: [{return: a}]}}\nrun: [{value: {}, as: result}, {return: $result}]\n").unwrap();
        library.extend(callable_source("b.yaml", "functions: {value: {run: [{return: b}]}}\nrun: [{value: {}, as: result}, {return: $result}]\n").unwrap());
        let caller = parse_script(
            "run: [{a.yaml: {}, as: a}, {b.yaml#value: {}, as: b}, {return: {a: $a, b: $b}}]\n",
        )
        .unwrap();
        let wire = build_program(&caller, &library, Default::default(), 0);
        let program: yaml_interp::Program = serde_json::from_value(wire).unwrap();
        assert_eq!(
            yaml_interp::run(&program, &Host, None).unwrap(),
            json!({"a":"a","b":"b"})
        );
    }
    #[test]
    fn embedded_functions_roundtrip_and_bind_to_their_script() {
        let source = "# preserved\nfunctions:\n  greet:\n    params:\n      text: {type: string, default: hello}\n    run: [{log: $text}]\nrun: [{greet: {}}]\n";
        let script = parse_script(source).unwrap();
        assert_eq!(script.functions.len(), 1);
        assert!(serialize_script(&script).contains("functions:"));
        assert_eq!(
            parse_script(&serialize_script(&script))
                .unwrap()
                .functions
                .len(),
            1
        );
        let library = callable_source("common.yaml", source).unwrap();
        assert_eq!(
            library[0].1.called_functions(),
            BTreeSet::from(["common.yaml#greet".into()])
        );
        let caller =
            parse_script("run: [{common.yaml: {}}, {common.yaml#greet: {text: world}}]\n").unwrap();
        let wire = build_program(&caller, &library, Default::default(), 0);
        assert!(wire["functions"]["common.yaml#greet"].is_object());
    }
}
