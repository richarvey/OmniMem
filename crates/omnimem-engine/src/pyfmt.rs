//! Formatting that has to match what 6.x Python wrote.
//!
//! Stored fields were written with `str(float)` and `json.dumps` defaults,
//! and records move between versions through backups, so the Rust side
//! writes the same text.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

/// Python's `str(float)`: shortest round-trip digits, and always a decimal
/// point for whole numbers (`1.0`, not `1`).
pub fn py_float(x: f64) -> String {
    if x.is_finite() && x.fract() == 0.0 && x.abs() < 1e16 {
        format!("{x:.1}")
    } else {
        format!("{x}")
    }
}

pub fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `str(time.time())`
pub fn now_str() -> String {
    py_float(now_secs())
}

/// `s[:n]` on a Python string: by character, not byte.
pub fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Python's `round(x, digits)` for the non-tie cases that matter here.
pub fn round_to(x: f64, digits: i32) -> f64 {
    let m = 10f64.powi(digits);
    (x * m).round() / m
}

/// `json.dumps(value)` with its defaults: `", "` and `": "` separators and
/// non-ASCII escaped as `\uXXXX`.
pub fn py_json(value: &Value) -> String {
    let mut out = String::new();
    write_py_json(value, &mut out);
    out
}

fn write_py_json(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                out.push_str(&i.to_string());
            } else if let Some(u) = n.as_u64() {
                out.push_str(&u.to_string());
            } else {
                out.push_str(&py_float(n.as_f64().unwrap_or(0.0)));
            }
        }
        Value::String(s) => write_py_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_py_json(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (i, (k, v)) in map.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_py_string(k, out);
                out.push_str(": ");
                write_py_json(v, out);
            }
            out.push('}');
        }
    }
}

fn write_py_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// `tools._compact`: drop keys whose value is null, `""`, `[]` or `{}`.
pub fn compact(map: Map<String, Value>) -> Value {
    Value::Object(
        map.into_iter()
            .filter(|(_, v)| match v {
                Value::Null => false,
                Value::String(s) => !s.is_empty(),
                Value::Array(a) => !a.is_empty(),
                Value::Object(o) => !o.is_empty(),
                _ => true,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn floats_print_like_python() {
        assert_eq!(py_float(1.0), "1.0");
        assert_eq!(py_float(0.2), "0.2");
        assert_eq!(py_float(0.7700000000000001), "0.7700000000000001");
        assert_eq!(py_float(1788802536.1234567), "1788802536.1234567");
    }

    #[test]
    fn json_dumps_defaults() {
        assert_eq!(
            py_json(&json!(["python", "docker"])),
            r#"["python", "docker"]"#
        );
        assert_eq!(py_json(&json!([])), "[]");
        assert_eq!(
            py_json(&json!({"key": "a", "n": 1})),
            r#"{"key": "a", "n": 1}"#
        );
        // Built from pieces so no tool or editor can decode the escapes.
        let expected = format!("[\"caf{b}u00e9\", \"{b}ud83d{b}ude80\"]", b = '\\');
        assert_eq!(py_json(&json!(["caf\u{e9}", "\u{1F680}"])), expected);
        assert_eq!(py_json(&json!("line\n\"q\"")), r#""line\n\"q\"""#);
    }

    #[test]
    fn compact_drops_empties_only() {
        let mut m = Map::new();
        m.insert("a".into(), json!(null));
        m.insert("b".into(), json!(""));
        m.insert("c".into(), json!([]));
        m.insert("d".into(), json!(0));
        m.insert("e".into(), json!(false));
        assert_eq!(compact(m), json!({"d": 0, "e": false}));
    }
}
