use crate::classify::strip_line_numbers;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

/// compress plain text: strip line numbers + normalize whitespace
pub fn compress_text(text: &str) -> Option<String> {
    let mut result = strip_line_numbers(text);

    // normalize whitespace
    result = normalize_whitespace(&result);

    // only return if we saved at least 10%
    if result.len() < (text.len() as f64 * 0.9) as usize {
        Some(result)
    } else {
        None
    }
}

fn normalize_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut consecutive_empty = 0;

    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            consecutive_empty += 1;
            if consecutive_empty <= 2 {
                out.push('\n');
            }
        } else {
            consecutive_empty = 0;
            out.push_str(trimmed);
            out.push('\n');
        }
    }

    out
}

/// dedup target: if text matches a previously seen target, return a back-reference
pub struct DedupState {
    seen: HashMap<String, (usize, usize)>, // hash -> (message_index, block_index)
}

impl DedupState {
    pub fn new() -> Self {
        Self {
            seen: HashMap::new(),
        }
    }

    /// check if this text has been seen before. if so, return a back-reference string.
    /// if not, record it and return None.
    pub fn check(&mut self, text: &str, msg_idx: usize, block_idx: usize) -> Option<String> {
        let key = hash_text(text);
        if let Some((prev_msg, prev_block)) = self.seen.get(&key) {
            Some(format!(
                "[see tool_result in message {}, block {} -- identical content]",
                prev_msg, prev_block
            ))
        } else {
            self.seen.insert(key, (msg_idx, block_idx));
            None
        }
    }
}

fn hash_text(text: &str) -> String {
    if text.len() < 128 {
        text.to_string()
    } else {
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        format!("{:x}", hasher.finalize())
    }
}

/// diff target: if text is similar to a previously seen target, return a diff
pub struct DiffState {
    seen: Vec<(usize, usize, String)>, // (message_index, block_index, text)
}

impl DiffState {
    pub fn new() -> Self {
        Self { seen: Vec::new() }
    }

    /// check if this text is similar to a previously seen target.
    /// returns a diff string if similar, None otherwise. always records the text.
    pub fn check(&mut self, text: &str, msg_idx: usize, block_idx: usize) -> Option<String> {
        if text.len() < 200 {
            self.seen.push((msg_idx, block_idx, text.to_string()));
            return None;
        }

        let mut found = None;
        // compare against all previous targets (matches tamp's behavior)
        for (prev_msg, prev_block, prev_text) in &self.seen[..] {
            let sim = jaccard_similarity(prev_text, text);
            if sim > 0.5 && sim < 1.0 {
                let diff = compute_diff(prev_text, text);
                if diff.len() < text.len() / 2 {
                    found = Some(format!(
                        "[diff from tool_result in message {}, block {}]:\n{}",
                        prev_msg, prev_block, diff
                    ));
                    break;
                }
            }
        }

        self.seen.push((msg_idx, block_idx, text.to_string()));
        found
    }
}

fn jaccard_similarity(a: &str, b: &str) -> f64 {
    if (a.len() as isize - b.len() as isize).unsigned_abs() > a.len().max(b.len()) / 2 {
        return 0.0;
    }

    let set_a: HashSet<&str> = a.lines().collect();
    let set_b: HashSet<&str> = b.lines().collect();

    let intersection = set_a.intersection(&set_b).count();
    let union = set_a.union(&set_b).count();

    if union == 0 {
        0.0
    } else {
        intersection as f64 / union as f64
    }
}

fn compute_diff(old: &str, new: &str) -> String {
    use similar::{ChangeTag, TextDiff};

    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();

    for hunk in diff.unified_diff().context_radius(1).iter_hunks() {
        out.push_str(&format!("{}\n", hunk.header()));
        for change in hunk.iter_changes() {
            let sign = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };
            out.push_str(sign);
            out.push_str(change.value());
            if !change.value().ends_with('\n') {
                out.push('\n');
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_whitespace() {
        let text = "line1   \nline2\t  \n\n\n\n\nline3\n";
        let result = normalize_whitespace(text);
        assert_eq!(result, "line1\nline2\n\n\nline3\n");
    }

    #[test]
    fn test_dedup() {
        let mut state = DedupState::new();
        assert!(state.check("hello world", 0, 0).is_none());
        assert!(state.check("different text", 1, 0).is_none());
        assert!(state.check("hello world", 2, 0).is_some());
    }

    #[test]
    fn test_jaccard() {
        let a = "line1\nline2\nline3\nline4\n";
        let b = "line1\nline2\nline3\nline5\n";
        let sim = jaccard_similarity(a, b);
        assert!(sim > 0.5);
        assert!(sim < 1.0);
    }
}
