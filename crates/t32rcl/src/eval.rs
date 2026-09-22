// Function results ported from lauterbach-trace32-rcl 1.1.5 (MIT, Copyright (c) 2020
// Lauterbach GmbH): `Debugger._decode_eval_result` in rcl.py and the result layout
// of `Library.t32_executefunction` in _rc/_library.py.

use crate::error::{Error, Operation, Result, Trace32Error};

/// A decoded PRACTICE function result.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i128),
    Float(f64),
    /// Strings, addresses, ranges and other kinds the Python library returns as `str`.
    Text(String),
    TimeRange(Vec<f64>),
    Empty,
}

/// Split the answer payload of `t32_executefunction` into (type, text).
pub(crate) fn split_result(result: &[u8]) -> Result<(u32, String)> {
    let malformed = || Error::Protocol("malformed function result".into());
    if result.len() < 8 {
        return Err(malformed());
    }
    let result_type = u32::from_le_bytes(result[0..4].try_into().unwrap());
    let size = u32::from_le_bytes(result[4..8].try_into().unwrap()) as usize;
    let raw = result.get(8..8 + size).ok_or_else(malformed)?;
    let text = std::str::from_utf8(raw)
        .map_err(|_| Error::Protocol("function result is not UTF-8".into()))?;
    Ok((result_type, text.to_string()))
}

/// `_decode_eval_result`.
pub(crate) fn decode(result_type: u32, value: &str, expression: &str) -> Result<Value> {
    let bad = || {
        Error::Protocol(format!(
            "cannot decode result {value:?} (type 0x{result_type:04x}) of {expression}"
        ))
    };
    Ok(match result_type {
        0x0001 => match value {
            "FALSE()" => Value::Bool(false),
            "TRUE()" => Value::Bool(true),
            _ => {
                return Err(
                    Trace32Error::new(Operation::Function(expression.into()), value).into(),
                );
            }
        },
        0x0002 => Value::Int(parse_int(value.get(2..).ok_or_else(bad)?, 2).ok_or_else(bad)?),
        0x0004 => Value::Int(parse_int(value, 16).ok_or_else(bad)?),
        0x0008 => Value::Int(parse_int(strip_last(value), 10).ok_or_else(bad)?),
        0x0010 => Value::Float(parse_float(value).ok_or_else(bad)?),
        0x0400 => Value::Float(parse_float(strip_last(value)).ok_or_else(bad)?),
        0x0800 => Value::TimeRange(
            value
                .replace("--", "..")
                .split("..")
                .map(|part| parse_float(strip_last(part)))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(bad)?,
        ),
        0x8000 => Value::Empty,
        0x0000 | 0x0020 | 0x0040 | 0x0080 | 0x0100 | 0x0200 | 0x4000 => {
            Value::Text(value.to_string())
        }
        _ => Value::Empty,
    })
}

/// `value[:-1]` (drops the unit suffix: `.` for decimals, `s` for times).
fn strip_last(value: &str) -> &str {
    let mut chars = value.chars();
    chars.next_back();
    chars.as_str()
}

/// Python `int(text, base)` for bases 2, 10 and 16: surrounding whitespace, a
/// sign, the matching `0b`/`0x` prefix and single underscores between digits.
pub fn parse_int(text: &str, base: u32) -> Option<i128> {
    let text = text.trim();
    let (negative, digits) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let prefix = match base {
        2 => ["0b", "0B"],
        16 => ["0x", "0X"],
        _ => ["", ""],
    };
    let digits = if base != 10 {
        prefix
            .iter()
            .find_map(|p| {
                digits
                    .strip_prefix(p)
                    .map(|rest| rest.strip_prefix('_').unwrap_or(rest))
            })
            .unwrap_or(digits)
    } else {
        digits
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let cleaned: String = digits.chars().filter(|&c| c != '_').collect();
    let value = i128::from_str_radix(&cleaned, base).ok()?;
    if cleaned.starts_with(['+', '-']) {
        return None;
    }
    Some(if negative { -value } else { value })
}

fn parse_float(text: &str) -> Option<f64> {
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_result_payload() {
        let mut payload = 0x0001u32.to_le_bytes().to_vec();
        payload.extend_from_slice(&6u32.to_le_bytes());
        payload.extend_from_slice(b"TRUE()");
        assert_eq!(split_result(&payload).unwrap(), (1, "TRUE()".to_string()));
        assert!(split_result(&payload[..10]).is_err());
    }

    #[test]
    fn decodes_each_result_type() {
        assert_eq!(decode(0x0001, "TRUE()", "x").unwrap(), Value::Bool(true));
        assert_eq!(decode(0x0001, "FALSE()", "x").unwrap(), Value::Bool(false));
        assert!(decode(0x0001, "MAYBE", "x").is_err());
        assert_eq!(decode(0x0002, "0y0101", "x").unwrap(), Value::Int(5));
        assert_eq!(decode(0x0004, "0x1F", "x").unwrap(), Value::Int(31));
        assert_eq!(decode(0x0004, "1F", "x").unwrap(), Value::Int(31));
        assert_eq!(decode(0x0008, "187884.", "x").unwrap(), Value::Int(187884));
        assert_eq!(decode(0x0008, "-3.", "x").unwrap(), Value::Int(-3));
        assert_eq!(decode(0x0010, "1.5", "x").unwrap(), Value::Float(1.5));
        assert_eq!(
            decode(0x0040, "abc", "x").unwrap(),
            Value::Text("abc".into())
        );
        assert_eq!(decode(0x0400, "2.5s", "x").unwrap(), Value::Float(2.5));
        assert_eq!(
            decode(0x0800, "1.0s--2.0s", "x").unwrap(),
            Value::TimeRange(vec![1.0, 2.0])
        );
        assert_eq!(decode(0x8000, "", "x").unwrap(), Value::Empty);
    }

    #[test]
    fn python_int_rules() {
        assert_eq!(parse_int(" 42 ", 10), Some(42));
        assert_eq!(parse_int("1_000", 10), Some(1000));
        assert_eq!(parse_int("+7", 10), Some(7));
        assert_eq!(parse_int("0x_ff", 16), Some(255));
        assert_eq!(parse_int("1__0", 10), None);
        assert_eq!(parse_int("_1", 10), None);
        assert_eq!(parse_int("", 10), None);
        assert_eq!(parse_int("--1", 10), None);
        assert_eq!(parse_int("0x", 16), None);
    }
}
