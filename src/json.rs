use crate::classify::{is_tabular, strip_line_numbers};
use crate::tabular::compress_tabular;
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

/// compress JSON content: prune junk keys, deep-compress string values, then TOON
pub fn compress_json(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;

    // 1. prune known junk keys
    let pruned = deep_prune(value);

    // 2. deep-compress string values (CSV inside JSON, nested JSON, etc.)
    let compressed = deep_compress_strings(pruned);

    // 3. encode as TOON (dense line-oriented format)
    if let Ok(toon) = serde_toon2::to_string(&compressed) {
        if toon.len() < (text.len() as f64 * 0.95) as usize {
            return Some(toon);
        }
    }

    // fallback: plain minify
    let minified = serde_json::to_string(&compressed).ok()?;
    if minified.len() < (text.len() as f64 * 0.95) as usize {
        Some(minified)
    } else {
        None
    }
}

/// walk JSON tree and compress string values that contain structured data
fn deep_compress_strings(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let compressed: serde_json::Map<String, Value> = map
                .into_iter()
                .map(|(k, v)| (k, deep_compress_strings(v)))
                .collect();
            Value::Object(compressed)
        }
        Value::Array(arr) => {
            Value::Array(arr.into_iter().map(deep_compress_strings).collect())
        }
        Value::String(s) => {
            if s.len() < 200 {
                return Value::String(s);
            }
            // unescape \r\n sequences (JSON-escaped CSV often has these)
            let unescaped = if s.contains("\\r\\n") || s.contains("\\n") || s.contains("\\t") {
                s.replace("\\r\\n", "\n")
                    .replace("\\n", "\n")
                    .replace("\\t", "\t")
            } else {
                s.clone()
            };

            // try tabular (CSV/TSV)
            if is_tabular(&unescaped) {
                if let Some(compressed) = compress_tabular(&unescaped) {
                    if compressed.len() < s.len() {
                        return Value::String(compressed);
                    }
                }
            }

            // try nested JSON
            if let Ok(inner) = serde_json::from_str::<Value>(&unescaped) {
                // recurse into the parsed JSON
                let inner_compressed = deep_compress_strings(inner);
                // TOON-encode the inner value
                if let Ok(toon) = serde_toon2::to_string(&inner_compressed) {
                    if toon.len() < s.len() {
                        return Value::String(toon);
                    }
                }
                // fallback: minify inner JSON
                if let Ok(minified) = serde_json::to_string(&inner_compressed) {
                    if minified.len() < s.len() {
                        return Value::String(minified);
                    }
                }
            }

            // try strip line numbers
            let stripped = strip_line_numbers(&unescaped);
            if stripped.len() < s.len() {
                return Value::String(stripped);
            }

            // if unescaping alone saved space, use that
            if unescaped.len() < s.len() {
                return Value::String(unescaped);
            }

            Value::String(s)
        }
        other => other,
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
