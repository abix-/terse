use crate::classify::{classify, strip_line_numbers, ContentType};
use crate::extract::Target;
use crate::json::compress_json;
use crate::tabular::compress_tabular;
use crate::text::{compress_text, DedupState, DiffState};

pub const MIN_SIZE: usize = 200;

/// which stage compressed a target
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    Tabular,
    JsonMinify,
    JsonFlatten,
    StripLines,
    Whitespace,
    Dedup,
    Diff,
}

impl std::fmt::Display for Stage {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Stage::Tabular => write!(f, "tabular"),
            Stage::JsonMinify => write!(f, "json-minify"),
            Stage::JsonFlatten => write!(f, "json-flatten"),
            Stage::StripLines => write!(f, "strip-lines"),
            Stage::Whitespace => write!(f, "whitespace"),
            Stage::Dedup => write!(f, "dedup"),
            Stage::Diff => write!(f, "diff"),
        }
    }
}

/// per-target result with stage attribution
#[derive(Debug)]
pub struct TargetResult {
    pub content_type: ContentType,
    pub original_bytes: usize,
    pub compressed_bytes: usize,
    pub stage: Option<Stage>,
}

/// run the full pipeline, return per-target results
pub fn compress_targets(targets: &mut [Target]) -> Vec<TargetResult> {
    let mut results = Vec::with_capacity(targets.len());

    // phase 1: per-block compression
    for target in targets.iter_mut() {
        let ct = if target.text.len() < MIN_SIZE {
            ContentType::Unknown
        } else {
            classify(&target.text)
        };

        if target.text.len() < MIN_SIZE {
            results.push(TargetResult {
                content_type: ct,
                original_bytes: target.text.len(),
                compressed_bytes: target.text.len(),
                stage: None,
            });
            continue;
        }

        let (compressed, stage) = match ct {
            ContentType::Tabular => {
                let r = compress_tabular(&target.text);
                (r, Stage::Tabular)
            }
            ContentType::Json => {
                // try flatten first, then minify
                let r = compress_json(&target.text);
                let st = if r.as_ref().is_some_and(|s| s.contains('\n')) {
                    Stage::JsonFlatten
                } else {
                    Stage::JsonMinify
                };
                (r, st)
            }
            ContentType::JsonLined => {
                let stripped = strip_line_numbers(&target.text);
                let r = compress_json(&stripped);
                let st = if r.is_some() {
                    Stage::JsonMinify
                } else {
                    Stage::StripLines
                };
                let r = r.or_else(|| compress_text(&target.text));
                (r, st)
            }
            ContentType::Text => {
                let r = compress_text(&target.text);
                // figure out which sub-stage fired
                let st = if r.is_some() {
                    // check if it was strip-lines or whitespace
                    let stripped = strip_line_numbers(&target.text);
                    if stripped.len() < target.text.len() {
                        Stage::StripLines
                    } else {
                        Stage::Whitespace
                    }
                } else {
                    Stage::Whitespace
                };
                (r, st)
            }
            ContentType::Unknown => (None, Stage::Whitespace),
        };

        let comp_len = compressed.as_ref().map(|c| c.len()).unwrap_or(target.text.len());
        let fired = compressed.is_some();

        if let Some(c) = compressed {
            target.compressed = Some(c);
        }

        results.push(TargetResult {
            content_type: ct,
            original_bytes: target.text.len(),
            compressed_bytes: comp_len,
            stage: if fired { Some(stage) } else { None },
        });
    }

    // phase 2: cross-target dedup
    let mut dedup = DedupState::new();
    for (i, target) in targets.iter_mut().enumerate() {
        if target.compressed.is_some() || target.text.len() < MIN_SIZE {
            continue;
        }
        if let Some(ref_text) = dedup.check(&target.text, target.msg_idx, target.block_idx) {
            let comp_len = ref_text.len();
            target.compressed = Some(ref_text);
            results[i].compressed_bytes = comp_len;
            results[i].stage = Some(Stage::Dedup);
        }
    }

    // phase 3: cross-target diff
    let mut diff = DiffState::new();
    for (i, target) in targets.iter_mut().enumerate() {
        if target.compressed.is_some() || target.text.len() < MIN_SIZE {
            continue;
        }
        if let Some(diff_text) = diff.check(&target.text, target.msg_idx, target.block_idx) {
            let comp_len = diff_text.len();
            target.compressed = Some(diff_text);
            results[i].compressed_bytes = comp_len;
            results[i].stage = Some(Stage::Diff);
        }
    }

    results
}

/// run a SINGLE stage in isolation on a set of targets (for per-stage benchmarking)
pub fn run_single_stage(targets: &[Target], stage: Stage) -> Vec<TargetResult> {
    let mut results = Vec::with_capacity(targets.len());

    match stage {
        Stage::Dedup => {
            let mut dedup = DedupState::new();
            for target in targets {
                if target.text.len() < MIN_SIZE {
                    results.push(TargetResult {
                        content_type: ContentType::Unknown,
                        original_bytes: target.text.len(),
                        compressed_bytes: target.text.len(),
                        stage: None,
                    });
                    continue;
                }
                // clone the dedup state check -- we can't mutate targets here
                let mut dedup_copy = DedupState::new();
                std::mem::swap(&mut dedup_copy, &mut dedup);
                let result = dedup_copy.check(&target.text, target.msg_idx, target.block_idx);
                std::mem::swap(&mut dedup_copy, &mut dedup);
                // re-do properly
                let comp = dedup.check(&target.text, target.msg_idx, target.block_idx);
                let comp_len = comp.as_ref().map(|c| c.len()).unwrap_or(target.text.len());
                results.push(TargetResult {
                    content_type: classify(&target.text),
                    original_bytes: target.text.len(),
                    compressed_bytes: comp_len,
                    stage: if comp.is_some() { Some(Stage::Dedup) } else { None },
                });
            }
        }
        Stage::Diff => {
            let mut diff = DiffState::new();
            for target in targets {
                if target.text.len() < MIN_SIZE {
                    results.push(TargetResult {
                        content_type: ContentType::Unknown,
                        original_bytes: target.text.len(),
                        compressed_bytes: target.text.len(),
                        stage: None,
                    });
                    continue;
                }
                let comp = diff.check(&target.text, target.msg_idx, target.block_idx);
                let comp_len = comp.as_ref().map(|c| c.len()).unwrap_or(target.text.len());
                results.push(TargetResult {
                    content_type: classify(&target.text),
                    original_bytes: target.text.len(),
                    compressed_bytes: comp_len,
                    stage: if comp.is_some() { Some(Stage::Diff) } else { None },
                });
            }
        }
        _ => {
            for target in targets {
                let ct = if target.text.len() < MIN_SIZE {
                    ContentType::Unknown
                } else {
                    classify(&target.text)
                };

                if target.text.len() < MIN_SIZE {
                    results.push(TargetResult {
                        content_type: ct,
                        original_bytes: target.text.len(),
                        compressed_bytes: target.text.len(),
                        stage: None,
                    });
                    continue;
                }

                let compressed = match stage {
                    Stage::Tabular => {
                        if ct == ContentType::Tabular { compress_tabular(&target.text) } else { None }
                    }
                    Stage::JsonMinify | Stage::JsonFlatten => {
                        if ct == ContentType::Json || ct == ContentType::JsonLined {
                            let text = if ct == ContentType::JsonLined {
                                strip_line_numbers(&target.text)
                            } else {
                                target.text.clone()
                            };
                            compress_json(&text)
                        } else {
                            None
                        }
                    }
                    Stage::StripLines => {
                        let stripped = strip_line_numbers(&target.text);
                        if stripped.len() < target.text.len() { Some(stripped) } else { None }
                    }
                    Stage::Whitespace => compress_text(&target.text),
                    _ => None,
                };

                let comp_len = compressed.as_ref().map(|c| c.len()).unwrap_or(target.text.len());
                results.push(TargetResult {
                    content_type: ct,
                    original_bytes: target.text.len(),
                    compressed_bytes: comp_len,
                    stage: if compressed.is_some() { Some(stage) } else { None },
                });
            }
        }
    }

    results
}
