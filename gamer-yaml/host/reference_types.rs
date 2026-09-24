//! Conservative save-time reference checks. Unknown/dynamic values stay runtime
//! checked; branch-local matches and declared parameter types are known here.
use std::collections::BTreeMap;

use serde_json::Value;

use super::native_funcs::native_function;
use super::syntax::{
    Diagnostic, FunctionLibrary, ParamDecl, ParamType, Script, SurfaceExpr, SurfaceStep,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Type {
    Unknown,
    Known(ParamType),
    Match,
}
type Env = BTreeMap<String, Type>;

fn initial(params: &[ParamDecl], vars: &[(String, Value)]) -> Env {
    let mut env: Env = params
        .iter()
        .map(|p| (p.name.clone(), Type::Known(p.ty)))
        .collect();
    for (name, value) in vars {
        let ty = match value {
            Value::Bool(_) => Type::Known(ParamType::Boolean),
            Value::Number(n) if n.is_i64() || n.is_u64() => Type::Known(ParamType::Integer),
            Value::Number(_) => Type::Known(ParamType::Number),
            Value::String(_) => Type::Known(ParamType::String),
            Value::Array(_) => Type::Known(ParamType::List),
            Value::Object(_) => Type::Known(ParamType::Object),
            Value::Null => Type::Unknown,
        };
        env.insert(name.clone(), ty);
    }
    env
}

fn reference(path: &str, env: &Env) -> Type {
    let mut parts = path.split('.');
    let mut ty = env
        .get(parts.next().unwrap_or(""))
        .copied()
        .unwrap_or(Type::Unknown);
    for field in parts {
        ty = match (ty, field) {
            (Type::Match, "center") => Type::Known(ParamType::Point),
            (Type::Match, "template") => Type::Known(ParamType::Template),
            (Type::Match, "score") => Type::Known(ParamType::Number),
            (Type::Match, "index" | "x" | "y" | "width" | "height") => {
                Type::Known(ParamType::Integer)
            }
            (Type::Match, "region") => Type::Known(ParamType::Object),
            (Type::Known(ParamType::Point), "x" | "y") => Type::Known(ParamType::Number),
            _ => Type::Unknown,
        };
    }
    ty
}

fn accepts(actual: Type, expected: ParamType, tap: bool) -> bool {
    use ParamType::*;
    match actual {
        Type::Unknown | Type::Known(Any) => true,
        _ if expected == Any => true,
        Type::Match => expected == Object || (tap && expected == Point),
        Type::Known(actual) => {
            actual == expected
                || matches!(
                    (actual, expected),
                    (String | Template | Key, String | Template | Key | Duration)
            | (Integer, Number | Duration) | (Number, Integer | Duration)
            | (Duration, String | Template | Key | Number | Integer)
            // Shapes and ranges of generic containers remain runtime checked.
            | (Object | List, Point) | (Point, Object | List)
                )
        }
    }
}

fn check(
    expr: &SurfaceExpr,
    expected: ParamType,
    tap: bool,
    path: &str,
    env: &Env,
    out: &mut Vec<Diagnostic>,
) {
    let SurfaceExpr::Ref(name) = expr else { return };
    let actual = reference(name, env);
    if accepts(actual, expected, tap) {
        return;
    }
    let label = match actual {
        Type::Match => "匹配结果",
        Type::Known(ty) => ty.canonical(),
        Type::Unknown => return,
    };
    let hint = if actual == Type::Match && expected == ParamType::Template {
        format!("；模板名称请用 ${name}.template，点击该结果请使用 tap")
    } else {
        String::new()
    };
    out.push(Diagnostic::new(
        "yaml.args.ref_type",
        path,
        format!(
            "引用 ${name} 的类型为 {label}，此处需要 {}{hint}",
            expected.canonical()
        ),
    ));
}

// Branches/loops can assign different types. Forget these assignments rather
// than treating the last visited branch as the one that will execute.
fn forget_assignments(steps: &[SurfaceStep], env: &mut Env) {
    for step in steps {
        match step {
            SurfaceStep::Call {
                save_as: Some(name),
                ..
            } => {
                env.insert(name.clone(), Type::Unknown);
            }
            SurfaceStep::If {
                then_steps,
                else_steps,
                ..
            } => {
                forget_assignments(then_steps, env);
                forget_assignments(else_steps, env);
            }
            SurfaceStep::Repeat { body, .. } => forget_assignments(body, env),
            SurfaceStep::MatchTemplates {
                cases, else_steps, ..
            } => {
                for case in cases {
                    let previous = case
                        .save_as
                        .as_ref()
                        .map(|name| (name.clone(), env.get(name).copied()));
                    forget_assignments(&case.body, env);
                    if let Some((name, previous)) = previous {
                        if let Some(ty) = previous {
                            env.insert(name, ty);
                        } else {
                            env.remove(&name);
                        }
                    }
                }
                forget_assignments(else_steps, env);
            }
            _ => {}
        }
    }
}

fn walk(
    steps: &[SurfaceStep],
    base: &str,
    env: &mut Env,
    library: &FunctionLibrary,
    out: &mut Vec<Diagnostic>,
) {
    for (i, step) in steps.iter().enumerate() {
        let path = format!("{base}[{i}]");
        match step {
            SurfaceStep::Call {
                name,
                args,
                save_as,
            } => {
                let native = native_function(name);
                let params: Vec<_> = if let Some(native) = native {
                    native
                        .params
                        .iter()
                        .map(|p| (p.name.to_string(), p.ty, p.item_type))
                        .collect()
                } else {
                    library
                        .iter()
                        .find(|(n, _)| n == name)
                        .map(|(_, f)| {
                            f.call_params(name)
                                .iter()
                                .map(|p| (p.name.clone(), p.ty, None))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                for (index, (param, ty, item_type)) in params.iter().enumerate() {
                    let arg = match args {
                        SurfaceExpr::Map(entries) => entries
                            .iter()
                            .find(|(key, _)| key == param)
                            .map(|(_, value)| value),
                        _ if index == 0 => Some(args),
                        _ => None,
                    };
                    if let Some(arg) = arg {
                        let field = format!("{path}.{param}");
                        check(
                            arg,
                            *ty,
                            name == "tap" && param == "position",
                            &field,
                            env,
                            out,
                        );
                        if let (Some(item_type), SurfaceExpr::List(items)) = (item_type, arg) {
                            for (i, item) in items.iter().enumerate() {
                                check(item, *item_type, false, &format!("{field}[{i}]"), env, out);
                            }
                        }
                    }
                }
                if let Some(name) = save_as {
                    let ty = match native.map(|n| n.returns) {
                        Some("match?") => Type::Match,
                        Some("boolean") => Type::Known(ParamType::Boolean),
                        _ => Type::Unknown,
                    };
                    env.insert(name.clone(), ty);
                }
            }
            SurfaceStep::MatchTemplates {
                cases,
                threshold,
                else_steps,
            } => {
                check(
                    threshold,
                    ParamType::Number,
                    false,
                    &format!("{path}.threshold"),
                    env,
                    out,
                );
                for (i, case) in cases.iter().enumerate() {
                    check(
                        &case.template,
                        ParamType::Template,
                        false,
                        &format!("{path}.cases[{i}].template"),
                        env,
                        out,
                    );
                    let mut local = env.clone();
                    if let Some(name) = &case.save_as {
                        local.insert(name.clone(), Type::Match);
                    }
                    walk(
                        &case.body,
                        &format!("{path}.cases[{i}].do"),
                        &mut local,
                        library,
                        out,
                    );
                }
                walk(
                    else_steps,
                    &format!("{path}.else"),
                    &mut env.clone(),
                    library,
                    out,
                );
                forget_assignments(std::slice::from_ref(step), env);
            }
            SurfaceStep::If {
                then_steps,
                else_steps,
                ..
            } => {
                walk(
                    then_steps,
                    &format!("{path}.then"),
                    &mut env.clone(),
                    library,
                    out,
                );
                walk(
                    else_steps,
                    &format!("{path}.else"),
                    &mut env.clone(),
                    library,
                    out,
                );
                forget_assignments(std::slice::from_ref(step), env);
            }
            SurfaceStep::Repeat { times, body } => {
                check(
                    times,
                    ParamType::Integer,
                    false,
                    &format!("{path}.repeat"),
                    env,
                    out,
                );
                forget_assignments(body, env);
                walk(body, &format!("{path}.do"), &mut env.clone(), library, out);
            }
            _ => {}
        }
    }
}

pub(super) fn script(script: &Script, library: &FunctionLibrary) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    walk(
        &script.run,
        "run",
        &mut initial(&script.params, &script.vars),
        library,
        &mut out,
    );
    out
}

pub(super) fn functions(functions: &FunctionLibrary, library: &FunctionLibrary) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (name, function) in functions {
        walk(
            &function.run,
            &format!("functions.{name}.run"),
            &mut initial(&function.params, &function.vars),
            library,
            &mut out,
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reference_types_shared_editor_fixtures() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../tests/reference-types.json")).unwrap();
        for fixture in fixtures.as_array().unwrap() {
            let source = fixture["source"].as_str().unwrap();
            let diagnostics = if fixture["kind"] == "function_library" {
                let library = super::super::syntax::parse_function_library(source).unwrap();
                functions(&library, &library)
            } else {
                script(
                    &super::super::syntax::parse_script(source).unwrap(),
                    &vec![],
                )
            };
            let paths: Vec<_> = diagnostics.iter().map(|d| d.path.as_str()).collect();
            assert_eq!(
                serde_json::json!(paths),
                fixture["errors"],
                "{}: {diagnostics:?}",
                fixture["name"]
            );
        }
    }
}
