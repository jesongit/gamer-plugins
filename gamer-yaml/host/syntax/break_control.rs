use super::*;

/// 每个脚本/函数单独检查，调用方的循环不向被调用函数开放。
pub(super) fn validate(
    steps: &[SurfaceStep],
    prefix: &str,
    in_loop: bool,
) -> Result<(), Vec<Diagnostic>> {
    for (i, step) in steps.iter().enumerate() {
        let path = format!("{prefix}[{i}]");
        match step {
            SurfaceStep::Break if !in_loop => {
                return Err(one_diagnostic(
                    "yaml.break.outside_loop",
                    &path,
                    "break 只能在当前脚本或函数的 repeat 或多轮模板匹配内使用",
                ))
            }
            SurfaceStep::Repeat { body, .. } => validate(body, &format!("{path}.do"), true)?,
            SurfaceStep::If {
                then_steps,
                else_steps,
                ..
            } => {
                validate(then_steps, &format!("{path}.then"), in_loop)?;
                validate(else_steps, &format!("{path}.else"), in_loop)?;
            }
            SurfaceStep::MatchTemplates {
                cases,
                times,
                else_steps,
                ..
            } => {
                let in_loop = in_loop || match_templates::repeats(times);
                for (n, case) in cases.iter().enumerate() {
                    validate(&case.body, &format!("{path}.cases[{n}].do"), in_loop)?;
                }
                validate(else_steps, &format!("{path}.else"), in_loop)?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_script(source: &str) -> Result<Script, Vec<Diagnostic>> {
        let source = if source.trim_start().starts_with("version:") {
            source.to_string()
        } else {
            format!("version: 2\n{source}")
        };
        super::super::parse_script(&source)
    }
    fn parse_function_library(source: &str) -> Result<FunctionLibrary, Vec<Diagnostic>> {
        let source = if source.trim_start().starts_with("version:") {
            source.to_string()
        } else {
            format!("version: 2\n{source}")
        };
        super::super::parse_function_library(&source)
    }

    #[test]
    fn break_roundtrip_and_scope() {
        let source = "run:\n  - repeat: 3\n    do:\n      - if: true\n        then:\n          - break: {}\n";
        let parsed = parse_script(source).unwrap();
        let text = serialize_script(&parsed);
        assert_eq!(parse_script(&text).unwrap().run, parsed.run);
        assert!(parsed.called_functions().is_empty());
        assert_eq!(
            wire_steps(&parsed.run, "run", &vec![])[0]["do"][0]["then"][0]["op"],
            "break"
        );
        for source in [
            "run: [{break: {}}]",
            "run: [{if: false, then: [{break: {}}]}]",
            "functions: {f: {run: [{break: {}}]}}",
        ] {
            let errors = if source.starts_with("functions") {
                parse_function_library(source).unwrap_err()
            } else {
                parse_script(source).unwrap_err()
            };
            assert_eq!(errors[0].code, "yaml.break.outside_loop");
        }
        for value in ["true", "1", "{value: 1}"] {
            assert_eq!(
                parse_script(&format!("run: [{{repeat: 1, do: [{{break: {value}}}]}}]"))
                    .unwrap_err()[0]
                    .code,
                "yaml.break.shape"
            );
        }
        assert!(parse_script("run: [{repeat: 1, do: [{break: {}, as: x}]}]").is_err());
        assert!(parse_function_library("functions: {break: {run: []}}").is_err());
    }
}
