//! Defensive accessors over `serde_json::Value`.
//!
//! Firstmate's snapshot is read as untyped JSON on purpose: any field can be
//! missing, `null`, or of an unexpected type after a Firstmate update, and the
//! board must degrade to "unknown" for that field instead of failing the whole
//! read (Captain's Deck crashed on a `null` worktrees field; Kanbr must not).

use serde_json::Value;

static NULL: Value = Value::Null;

/// Walks a dotted path (`"current_state.state"`), returning `Null` for any
/// missing or non-object step.
pub fn get<'a>(v: &'a Value, path: &str) -> &'a Value {
    let mut cur = v;
    for key in path.split('.') {
        match cur.get(key) {
            Some(next) => cur = next,
            None => return &NULL,
        }
    }
    cur
}

/// A non-blank string at `path`, trimmed.
pub fn str_at<'a>(v: &'a Value, path: &str) -> Option<&'a str> {
    get(v, path)
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// An owned non-blank string at `path`.
pub fn string_at(v: &Value, path: &str) -> Option<String> {
    str_at(v, path).map(str::to_owned)
}

/// True only when the value at `path` is the boolean `true`.
pub fn bool_at(v: &Value, path: &str) -> bool {
    get(v, path).as_bool() == Some(true)
}

/// The array at `path`, or an empty slice when it is missing, `null`, or not an array.
pub fn arr_at<'a>(v: &'a Value, path: &str) -> &'a [Value] {
    get(v, path).as_array().map(Vec::as_slice).unwrap_or(&[])
}

/// The non-blank strings of the array at `path`; other element types are skipped.
pub fn strings_at(v: &Value, path: &str) -> Vec<String> {
    arr_at(v, path)
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// An integer at `path`, accepting JSON numbers only.
pub fn i64_at(v: &Value, path: &str) -> Option<i64> {
    get(v, path).as_i64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_and_null_fields_degrade() {
        let v = json!({"a": {"b": null, "c": "  x  ", "d": ""}, "list": null, "n": 3});
        assert!(get(&v, "a.b").is_null());
        assert!(get(&v, "a.zz.yy").is_null());
        assert_eq!(str_at(&v, "a.c"), Some("x"));
        assert_eq!(str_at(&v, "a.d"), None);
        assert_eq!(str_at(&v, "n"), None);
        assert!(arr_at(&v, "list").is_empty());
        assert!(arr_at(&v, "missing").is_empty());
        assert!(!bool_at(&v, "a.b"));
        assert_eq!(i64_at(&v, "n"), Some(3));
    }

    #[test]
    fn strings_skip_non_strings() {
        let v = json!({"ids": ["a", null, 3, " ", "b"]});
        assert_eq!(strings_at(&v, "ids"), vec!["a", "b"]);
    }
}
