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

/// compress JSON content: prune junk keys, then encode as TOON
pub fn compress_json(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;

    // prune known junk keys first
    let pruned = deep_prune(value);

    // encode as TOON (dense line-oriented format)
    if let Ok(toon) = serde_toon2::to_string(&pruned) {
        if toon.len() < (text.len() as f64 * 0.95) as usize {
            return Some(toon);
        }
    }

    // fallback: plain minify
    let minified = serde_json::to_string(&pruned).ok()?;
    if minified.len() < (text.len() as f64 * 0.95) as usize {
        Some(minified)
    } else {
        None
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
    fn test_compress_json_object() {
        let json = r#"{
  "name": "test",
  "version": "1.0.0",
  "description": "a test package"
}"#;
        let result = compress_json(json);
        assert!(result.is_some());
        let out = result.unwrap();
        // TOON output should be key: value format
        assert!(out.contains("name:") || out.contains("name: "));
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
        // TOON should encode this as tabular
        assert!(out.contains("alice"));
        assert!(out.len() < json.len());
    }

    #[test]
    fn test_toon_nested_object() {
        let json = r#"{
  "user": {
    "name": "Ada",
    "profile": {
      "bio": "Programmer",
      "location": "London"
    }
  },
  "active": true
}"#;
        let result = compress_json(json);
        assert!(result.is_some());
        let out = result.unwrap();
        // TOON uses indentation for nesting
        assert!(out.contains("Ada"));
        assert!(out.len() < json.len());
    }
}
