//! Pure numeric helpers shared by live execution and recorded replay.
use anyhow::{bail, ensure, Result};
use serde_json::{json, Value};

pub(super) fn number(text: &str, index: u64, format: &str) -> Result<Value> {
    ensure!(text.len() <= 4096 && index <= 32, "number 参数超出范围");
    let part = match format {
        "tokens" => text
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .nth(index as usize)
            .ok_or_else(|| anyhow::anyhow!("NUMBER_NOT_FOUND: OCR 未包含所需数字"))?,
        "fraction" => {
            ensure!(index <= 1, "NUMBER_INVALID: fraction index 只能为 0 或 1");
            let (numerator, denominator) = text
                .trim()
                .split_once('/')
                .ok_or_else(|| anyhow::anyhow!("NUMBER_INVALID: 需要完整的整数/整数"))?;
            let numerator = numerator.trim();
            let denominator = denominator.trim();
            for field in [numerator, denominator] {
                ensure!(
                    !field.is_empty() && field.bytes().all(|c| c.is_ascii_digit()),
                    "NUMBER_INVALID: 需要完整的整数/整数"
                );
                let value: u64 = field
                    .parse()
                    .map_err(|_| anyhow::anyhow!("NUMBER_INVALID: 数字溢出"))?;
                ensure!(value <= 1_000_000_000, "NUMBER_INVALID: 数字超出范围");
            }
            ensure!(
                denominator.bytes().any(|c| c != b'0'),
                "NUMBER_INVALID: 分母不能为零"
            );
            if index == 0 {
                numerator
            } else {
                denominator
            }
        }
        _ => bail!("NUMBER_INVALID: 未知 number format {format}"),
    };
    let value: u64 = part
        .parse()
        .map_err(|_| anyhow::anyhow!("NUMBER_INVALID: 数字溢出"))?;
    ensure!(value <= 1_000_000_000, "NUMBER_INVALID: 数字超出范围");
    Ok(json!(value))
}
pub(super) fn calculate(op: &str, a: f64, b: f64) -> Result<Value> {
    ensure!(a.is_finite() && b.is_finite(), "calculate 需要有限数字");
    let result = match op {
        "add" => a + b,
        "sub" => a - b,
        "mul" => a * b,
        "div" | "floor_div" => {
            ensure!(b != 0.0, "calculate 不允许除以零");
            if op == "floor_div" {
                (a / b).floor()
            } else {
                a / b
            }
        }
        "min" => a.min(b),
        "max" => a.max(b),
        _ => bail!("calculate 未知运算 {op}"),
    };
    ensure!(result.is_finite(), "calculate 结果溢出");
    // Integer results can directly drive the interpreter's bounded repeat.
    Ok(
        if result.fract() == 0.0 && result.abs() <= (1u64 << 53) as f64 {
            json!(result as i64)
        } else {
            json!(result)
        },
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stamina_fields_and_batch_count_are_unambiguous() {
        assert_eq!(number("60 / 300", 0, "tokens").unwrap(), json!(60));
        assert_eq!(number("60 / 300", 1, "tokens").unwrap(), json!(300));
        assert_eq!(calculate("floor_div", 60.0, 10.0).unwrap(), json!(6));
        assert_eq!(calculate("min", 24.0, 6.0).unwrap(), json!(6));
        assert!(number("未识别", 0, "tokens").is_err());
        assert!(number("99999999999999999999999", 0, "tokens").is_err());
        assert!(calculate("div", 40.0, 0.0).is_err());
    }
    #[test]
    fn fraction_requires_both_fields_and_never_substitutes_the_denominator() {
        for value in [0, 8, 20, 27, 60, 251, 257, 267, 300] {
            let text = format!(" {value} / 300\n");
            assert_eq!(number(&text, 0, "fraction").unwrap(), json!(value));
            assert_eq!(number(&text, 1, "fraction").unwrap(), json!(300));
        }
        for text in [
            "/300",
            "300/",
            "300",
            "8/300/300",
            "8x/300",
            "-8/300",
            "8/0",
            "8/000",
            "8/99999999999999999999999",
        ] {
            assert!(number(text, 0, "fraction").is_err(), "{text}");
        }
        assert!(number("8/300", 2, "fraction").is_err());
        assert!(number("8/300", 0, "unknown").is_err());
    }
}
