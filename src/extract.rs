use serde_json::Value;

/// a compression target -- a tool_result content string with its location in the message body
#[derive(Debug)]
pub struct Target {
    pub msg_idx: usize,
    pub block_idx: usize,
    pub path: Vec<PathSegment>,
    pub text: String,
    pub compressed: Option<String>,
}

#[derive(Debug, Clone)]
pub enum PathSegment {
    Key(String),
    Index(usize),
}

/// extract compression targets from an Anthropic API request body
pub fn extract_targets(body: &Value, cache_safe: bool) -> Vec<Target> {
    let messages = match body.get("messages").and_then(|m| m.as_array()) {
        Some(m) => m,
        None => return Vec::new(),
    };

    if cache_safe {
        extract_cache_safe(messages)
    } else {
        extract_all(messages)
    }
}

/// cacheSafe=false: extract ALL tool_result content blocks
fn extract_all(messages: &[Value]) -> Vec<Target> {
    let mut targets = Vec::new();
    for (mi, msg) in messages.iter().enumerate() {
        targets.extend(extract_from_message(msg, mi));
    }
    targets
}

/// cacheSafe=true: only extract from the LAST user message group with eligible tool_results
fn extract_cache_safe(messages: &[Value]) -> Vec<Target> {
    for mi in (0..messages.len()).rev() {
        let targets = extract_from_message(&messages[mi], mi);
        if targets.iter().any(|t| t.compressed.is_none()) {
            return targets;
        }
    }
    Vec::new()
}

fn extract_from_message(msg: &Value, mi: usize) -> Vec<Target> {
    let mut targets = Vec::new();

    if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
        return targets;
    }

    let content = match msg.get("content") {
        Some(c) => c,
        None => return targets,
    };

    // content can be a string or array
    if let Some(text) = content.as_str() {
        targets.push(Target {
            msg_idx: mi,
            block_idx: 0,
            path: vec![
                PathSegment::Key("messages".into()),
                PathSegment::Index(mi),
                PathSegment::Key("content".into()),
            ],
            text: text.to_string(),
            compressed: None,
        });
        return targets;
    }

    let blocks = match content.as_array() {
        Some(b) => b,
        None => return targets,
    };

    for (bi, block) in blocks.iter().enumerate() {
        if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
            continue;
        }
        if block.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false) {
            continue;
        }

        // content can be a string or array of content blocks
        if let Some(text) = block.get("content").and_then(|c| c.as_str()) {
            targets.push(Target {
                msg_idx: mi,
                block_idx: bi,
                path: vec![
                    PathSegment::Key("messages".into()),
                    PathSegment::Index(mi),
                    PathSegment::Key("content".into()),
                    PathSegment::Index(bi),
                    PathSegment::Key("content".into()),
                ],
                text: text.to_string(),
                compressed: None,
            });
        } else if let Some(sub_blocks) = block.get("content").and_then(|c| c.as_array()) {
            for (si, sub) in sub_blocks.iter().enumerate() {
                if sub.get("type").and_then(|t| t.as_str()) == Some("text") {
                    if let Some(text) = sub.get("text").and_then(|t| t.as_str()) {
                        targets.push(Target {
                            msg_idx: mi,
                            block_idx: bi,
                            path: vec![
                                PathSegment::Key("messages".into()),
                                PathSegment::Index(mi),
                                PathSegment::Key("content".into()),
                                PathSegment::Index(bi),
                                PathSegment::Key("content".into()),
                                PathSegment::Index(si),
                                PathSegment::Key("text".into()),
                            ],
                            text: text.to_string(),
                            compressed: None,
                        });
                    }
                }
            }
        }
    }

    targets
}

/// apply compressed targets back into the body
pub fn apply_targets(body: &mut Value, targets: &[Target]) {
    for target in targets {
        if let Some(ref compressed) = target.compressed {
            let mut node = body as &mut Value;
            for (i, seg) in target.path.iter().enumerate() {
                if i == target.path.len() - 1 {
                    // last segment -- replace the value
                    match seg {
                        PathSegment::Key(k) => {
                            node[k.as_str()] = Value::String(compressed.clone());
                        }
                        PathSegment::Index(idx) => {
                            node[*idx] = Value::String(compressed.clone());
                        }
                    }
                } else {
                    // navigate deeper
                    node = match seg {
                        PathSegment::Key(k) => &mut node[k.as_str()],
                        PathSegment::Index(idx) => &mut node[*idx],
                    };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_simple() {
        let body = json!({
            "model": "test",
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "read", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "file contents here"}]}
            ]
        });
        let targets = extract_targets(&body, false);
        assert_eq!(targets.len(), 2); // "hello" + tool_result
    }

    #[test]
    fn test_skip_errors() {
        let body = json!({
            "model": "test",
            "messages": [
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "is_error": true, "content": "error msg"}]}
            ]
        });
        let targets = extract_targets(&body, false);
        assert_eq!(targets.len(), 0);
    }

    #[test]
    fn test_apply_targets() {
        let mut body = json!({
            "model": "test",
            "messages": [
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "original text"}]}
            ]
        });
        let targets = vec![Target {
            msg_idx: 0,
            block_idx: 0,
            path: vec![
                PathSegment::Key("messages".into()),
                PathSegment::Index(0),
                PathSegment::Key("content".into()),
                PathSegment::Index(0),
                PathSegment::Key("content".into()),
            ],
            text: "original text".into(),
            compressed: Some("compressed".into()),
        }];
        apply_targets(&mut body, &targets);
        let result = body["messages"][0]["content"][0]["content"].as_str().unwrap();
        assert_eq!(result, "compressed");
    }
}
