use serde_json::Value;
use std::collections::HashSet;

/// keys to prune from JSON (npm metadata, registry junk)
static PRUNE_KEYS: &[&str] = &[
    "integrity",
    "shasum",
    "_id",
    "_from",
    "_resolved",
    "_integrity",
    "_nodeVersion",
    "_npmVersion",
    "_phantomChildren",
    "_requiredBy",
];

/// compress JSON content: minify + prune + flatten uniform arrays
pub fn compress_json(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;

    // try uniform array -> CSV-style first (biggest win)
    if let Some(csv) = try_flatten_uniform_array(&value) {
        if csv.len() < text.len() {
            return Some(csv);
        }
    }

    // prune + minify
    let pruned = deep_prune(value);
    let minified = serde_json::to_string(&pruned).ok()?;

    if minified.len() < (text.len() as f64 * 0.95) as usize {
        Some(minified)
    } else {
        None
    }
}

/// if the value is an array of objects with identical keys, flatten to CSV
fn try_flatten_uniform_array(value: &Value) -> Option<String> {
    let arr = value.as_array()?;
    if arr.len() < 3 {
        return None;
    }

    // check all elements are objects with same keys
    let first_keys: Vec<&str> = arr[0].as_object()?.keys().map(|k| k.as_str()).collect();
    if first_keys.is_empty() {
        return None;
    }

    for item in &arr[1..] {
        let obj = item.as_object()?;
        if obj.len() != first_keys.len() {
            return None;
        }
        for key in &first_keys {
            if !obj.contains_key(*key) {
                return None;
            }
        }
    }

    // build CSV-style output: header + rows
    let mut out = String::new();
    out.push_str(&first_keys.join(","));
    out.push('\n');

    for item in arr {
        let obj = item.as_object().unwrap();
        let vals: Vec<String> = first_keys
            .iter()
            .map(|k| format_csv_value(&obj[*k]))
            .collect();
        out.push_str(&vals.join(","));
        out.push('\n');
    }

    Some(out)
}

fn format_csv_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => {
            if s.contains(',') || s.contains('"') || s.contains('\n') {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.clone()
            }
        }
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn deep_prune(value: Value) -> Value {
    let prune_set: HashSet<&str> = PRUNE_KEYS.iter().copied().collect();

    match value {
        Value::Object(map) => {
            let filtered: serde_json::Map<String, Value> = map
                .into_iter()
                .filter(|(k, v)| !should_prune(k, v, &prune_set))
                .map(|(k, v)| (k, deep_prune(v)))
                .collect();
            Value::Object(filtered)
        }
        Value::Array(arr) => Value::Array(arr.into_iter().map(deep_prune).collect()),
        other => other,
    }
}

fn should_prune(key: &str, val: &Value, prune_set: &HashSet<&str>) -> bool {
    if prune_set.contains(key) {
        return true;
    }
    if key == "resolved" {
        if let Some(s) = val.as_str() {
            return s.starts_with("https://registry.");
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minify_json() {
        let json = r#"{
  "name": "test",
  "version": "1.0.0",
  "description": "a test package"
}"#;
        let result = compress_json(json);
        assert!(result.is_some());
        assert!(!result.unwrap().contains('\n'));
    }

    #[test]
    fn test_prune_npm_keys() {
        let json = r#"{"name":"pkg","integrity":"sha512-abc","_resolved":"https://registry.npmjs.org/foo","version":"1.0"}"#;
        let result = compress_json(json);
        assert!(result.is_some());
        let out = result.unwrap();
        assert!(!out.contains("integrity"));
        assert!(!out.contains("_resolved"));
    }

    #[test]
    fn test_flatten_uniform_array() {
        let json = r#"[
            {"name": "alice", "age": 30, "city": "nyc"},
            {"name": "bob", "age": 25, "city": "sf"},
            {"name": "charlie", "age": 35, "city": "la"}
        ]"#;
        let result = compress_json(json);
        assert!(result.is_some());
        let out = result.unwrap();
        // should be CSV-style
        assert!(out.contains("name,age,city"));
        assert!(out.contains("alice,30,nyc"));
    }
}
