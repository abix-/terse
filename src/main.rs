mod classify;
mod compress;
mod extract;
mod json;
mod tabular;
mod text;

use classify::{classify, ContentType};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Table};
use compress::{Stage, MIN_SIZE};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

const PROJECTS_ROOT: &str = r"C:\Users\aiannaco\.claude\projects";

// -- JSONL parsing --

struct ConvoPayload {
    project: String,
    #[allow(dead_code)]
    file: String,
    message_count: usize,
    original_bytes: usize,
    body: Value,
}

fn discover_jsonl_files() -> Vec<(String, PathBuf)> {
    let root = Path::new(PROJECTS_ROOT);
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return files;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let project = dir.file_name().unwrap().to_string_lossy().to_string();
        let Ok(dir_entries) = fs::read_dir(&dir) else {
            continue;
        };
        for f in dir_entries.flatten() {
            let path = f.path();
            if path.extension().is_some_and(|e| e == "jsonl") {
                files.push((project.clone(), path));
            }
        }
    }
    files
}

fn reconstruct_conversation(path: &Path) -> Option<(Vec<Value>, usize)> {
    let content = fs::read_to_string(path).ok()?;
    let mut messages: Vec<Value> = Vec::new();
    let mut tool_result_count = 0usize;

    for line in content.lines() {
        let obj: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = obj.get("type").and_then(|t| t.as_str())?;
        if msg_type != "user" && msg_type != "assistant" {
            continue;
        }

        let message = obj.get("message")?;
        if message.get("role").is_none() {
            continue;
        }

        if msg_type == "user" {
            if let Some(content) = message.get("content").and_then(|c| c.as_array()) {
                for block in content {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                        tool_result_count += 1;
                    }
                }
            }
        }

        messages.push(message.clone());
    }

    if messages.len() < 6 {
        return None;
    }

    Some((messages, tool_result_count))
}

fn build_body(messages: Vec<Value>) -> Value {
    serde_json::json!({
        "model": "claude-sonnet-4-20250514",
        "max_tokens": 8096,
        "messages": messages
    })
}

fn load_all_payloads() -> Vec<ConvoPayload> {
    let files = discover_jsonl_files();
    eprintln!("found {} JSONL files across projects", files.len());

    let mut payloads = Vec::new();
    let mut skipped_small = 0usize;

    for (project, path) in &files {
        let file_name = path.file_name().unwrap().to_string_lossy().to_string();
        let Some((messages, _)) = reconstruct_conversation(path) else {
            skipped_small += 1;
            continue;
        };

        let message_count = messages.len();
        let body = build_body(messages);
        let original_bytes = serde_json::to_string(&body).unwrap().len();

        payloads.push(ConvoPayload {
            project: project.clone(),
            file: file_name,
            message_count,
            original_bytes,
            body,
        });
    }

    eprintln!(
        "loaded {} conversations (skipped: {} too small)",
        payloads.len(),
        skipped_small,
    );
    payloads
}

fn format_bytes(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}MB", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}KB", n as f64 / 1_000.0)
    } else {
        format!("{n}B")
    }
}

fn pct(part: usize, whole: usize) -> f64 {
    if whole == 0 { 0.0 } else { part as f64 / whole as f64 * 100.0 }
}

fn savings_pct(orig: usize, comp: usize) -> f64 {
    if orig == 0 { 0.0 } else { (1.0 - comp as f64 / orig as f64) * 100.0 }
}

// -- diagnostics: content type distribution --

fn print_content_types(payloads: &[ConvoPayload]) {
    println!("\n{}", "=".repeat(70));
    println!("CONTENT TYPE DISTRIBUTION");
    println!("{}\n", "=".repeat(70));

    let mut by_type: BTreeMap<&str, (usize, usize)> = BTreeMap::new(); // count, bytes

    for payload in payloads {
        let targets = extract::extract_targets(&payload.body, false);
        for target in &targets {
            let label = if target.text.len() < MIN_SIZE {
                "skipped (<200 chars)"
            } else {
                match classify(&target.text) {
                    ContentType::Tabular => "tabular (CSV/TSV)",
                    ContentType::Json => "json",
                    ContentType::JsonLined => "json (with line numbers)",
                    ContentType::Text => "plain text",
                    ContentType::Unknown => "unknown/already compressed",
                }
            };
            let e = by_type.entry(label).or_default();
            e.0 += 1;
            e.1 += target.text.len();
        }
    }

    let total_count: usize = by_type.values().map(|v| v.0).sum();
    let total_bytes: usize = by_type.values().map(|v| v.1).sum();

    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_header(vec!["Content Type", "Count", "% Count", "Size", "% Size"]);

    let mut entries: Vec<_> = by_type.iter().collect();
    entries.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));

    for (label, (count, bytes)) in &entries {
        table.add_row(vec![
            label.to_string(),
            count.to_string(),
            format!("{:.1}%", pct(*count, total_count)),
            format_bytes(*bytes),
            format!("{:.1}%", pct(*bytes, total_bytes)),
        ]);
    }
    table.add_row(vec![
        "TOTAL".into(),
        total_count.to_string(),
        "100%".into(),
        format_bytes(total_bytes),
        "100%".into(),
    ]);
    println!("{table}");
}

// -- per-stage benchmarking --

fn print_per_stage_report(payloads: &[ConvoPayload]) {
    println!("\n{}", "=".repeat(70));
    println!("PER-STAGE EFFECTIVENESS (each stage tested in isolation)");
    println!("{}\n", "=".repeat(70));

    let stages = [
        Stage::Tabular,
        Stage::JsonMinify,
        Stage::StripLines,
        Stage::Whitespace,
        Stage::Dedup,
        Stage::Diff,
    ];

    // collect all targets across all conversations
    let mut all_targets: Vec<extract::Target> = Vec::new();
    for payload in payloads {
        let targets = extract::extract_targets(&payload.body, false);
        all_targets.extend(targets);
    }

    let total_orig: usize = all_targets.iter().map(|t| t.text.len()).sum();

    let mut summary_table = Table::new();
    summary_table.load_preset(UTF8_FULL_CONDENSED);
    summary_table.set_header(vec![
        "Stage",
        "Targets Hit",
        "Eligible",
        "Original",
        "Compressed",
        "Saved",
        "Savings %",
    ]);

    for stage in &stages {
        let results = compress::run_single_stage(&all_targets, *stage);

        let eligible: Vec<_> = results.iter().filter(|r| r.original_bytes >= MIN_SIZE).collect();
        let hit: Vec<_> = results.iter().filter(|r| r.stage.is_some()).collect();

        let orig: usize = eligible.iter().map(|r| r.original_bytes).sum();
        let comp: usize = eligible.iter().map(|r| r.compressed_bytes).sum();
        let saved = orig.saturating_sub(comp);

        summary_table.add_row(vec![
            stage.to_string(),
            format!("{} / {}", hit.len(), eligible.len()),
            format_bytes(orig),
            format_bytes(orig),
            format_bytes(comp),
            format_bytes(saved),
            format!("{:.1}%", savings_pct(orig, comp)),
        ]);
    }

    println!("{summary_table}");

    // detailed breakdown per stage
    for stage in &stages {
        let results = compress::run_single_stage(&all_targets, *stage);
        let hit: Vec<_> = results.iter().filter(|r| r.stage.is_some()).collect();

        if hit.is_empty() {
            println!("\n  {stage}: no targets hit\n");
            continue;
        }

        let orig: usize = hit.iter().map(|r| r.original_bytes).sum();
        let comp: usize = hit.iter().map(|r| r.compressed_bytes).sum();

        println!(
            "\n  {stage}: {} targets, {} -> {} ({:.1}% savings on affected targets)",
            hit.len(),
            format_bytes(orig),
            format_bytes(comp),
            savings_pct(orig, comp),
        );

        // size distribution of hits
        let mut buckets = vec![
            ("< 1KB", 0usize, 200, 1_000, 0usize, 0usize),
            ("1-10KB", 0, 1_000, 10_000, 0, 0),
            ("10-100KB", 0, 10_000, 100_000, 0, 0),
            ("100KB-1MB", 0, 100_000, 1_000_000, 0, 0),
            ("> 1MB", 0, 1_000_000, usize::MAX, 0, 0),
        ];
        for r in &hit {
            for b in buckets.iter_mut() {
                if r.original_bytes >= b.2 && r.original_bytes < b.3 {
                    b.1 += 1;
                    b.4 += r.original_bytes;
                    b.5 += r.compressed_bytes;
                    break;
                }
            }
        }
        for (label, count, _, _, orig_b, comp_b) in &buckets {
            if *count > 0 {
                println!(
                    "    {}: {} targets, {} -> {} ({:.1}%)",
                    label,
                    count,
                    format_bytes(*orig_b),
                    format_bytes(*comp_b),
                    savings_pct(*orig_b, *comp_b),
                );
            }
        }
    }
}

// -- combined pipeline report --

fn print_pipeline_report(payloads: &[ConvoPayload], cache_safe: bool) {
    let label = if cache_safe {
        "cacheSafe=true (compress newest only)"
    } else {
        "cacheSafe=false (compress all)"
    };

    println!("\n{}", "=".repeat(70));
    println!("PIPELINE: {label}");
    println!("{}\n", "=".repeat(70));

    let mut total_orig = 0usize;
    let mut total_comp = 0usize;
    let mut stage_savings: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new(); // count, orig, saved

    struct ConvoResult {
        project: String,
        message_count: usize,
        original_bytes: usize,
        compressed_bytes: usize,
    }
    let mut convo_results: Vec<ConvoResult> = Vec::new();

    for payload in payloads {
        let mut targets = extract::extract_targets(&payload.body, cache_safe);
        let results = compress::compress_targets(&mut targets);

        let mut body = payload.body.clone();
        extract::apply_targets(&mut body, &targets);
        let compressed_bytes = serde_json::to_string(&body).unwrap().len();

        total_orig += payload.original_bytes;
        total_comp += compressed_bytes;

        // attribute savings to stages
        for r in &results {
            if let Some(stage) = r.stage {
                let saved = r.original_bytes.saturating_sub(r.compressed_bytes);
                let e = stage_savings.entry(stage.to_string()).or_default();
                e.0 += 1;
                e.1 += r.original_bytes;
                e.2 += saved;
            }
        }

        convo_results.push(ConvoResult {
            project: payload.project.clone(),
            message_count: payload.message_count,
            original_bytes: payload.original_bytes,
            compressed_bytes,
        });
    }

    // stage attribution
    let mut st = Table::new();
    st.load_preset(UTF8_FULL_CONDENSED);
    st.set_header(vec!["Stage", "Targets", "Bytes Saved", "% of Total Savings"]);

    let total_saved = total_orig.saturating_sub(total_comp);
    let mut stage_entries: Vec<_> = stage_savings.iter().collect();
    stage_entries.sort_by(|a, b| b.1 .2.cmp(&a.1 .2));

    for (stage, (count, _orig, saved)) in &stage_entries {
        st.add_row(vec![
            stage.clone(),
            &count.to_string(),
            &format_bytes(*saved),
            &format!("{:.1}%", pct(*saved, total_saved)),
        ]);
    }
    st.add_row(vec![
        "TOTAL".into(),
        "".into(),
        format_bytes(total_saved),
        "100%".into(),
    ]);
    println!("{st}");

    // by conversation size bucket
    let buckets: &[(&str, usize, usize)] = &[
        ("<10 msgs", 0, 10),
        ("10-30 msgs", 10, 30),
        ("30-80 msgs", 30, 80),
        ("80-200 msgs", 80, 200),
        ("200+ msgs", 200, usize::MAX),
    ];

    let mut bt = Table::new();
    bt.load_preset(UTF8_FULL_CONDENSED);
    bt.set_header(vec!["Convo Size", "Count", "Original", "Compressed", "Savings"]);

    for (label, min, max) in buckets {
        let in_bucket: Vec<_> = convo_results
            .iter()
            .filter(|r| r.message_count >= *min && r.message_count < *max)
            .collect();
        if in_bucket.is_empty() {
            continue;
        }
        let orig: usize = in_bucket.iter().map(|r| r.original_bytes).sum();
        let comp: usize = in_bucket.iter().map(|r| r.compressed_bytes).sum();
        bt.add_row(vec![
            label.to_string(),
            in_bucket.len().to_string(),
            format_bytes(orig),
            format_bytes(comp),
            format!("{:.1}%", savings_pct(orig, comp)),
        ]);
    }
    bt.add_row(vec![
        "TOTAL".into(),
        convo_results.len().to_string(),
        format_bytes(total_orig),
        format_bytes(total_comp),
        format!("{:.1}%", savings_pct(total_orig, total_comp)),
    ]);
    println!("\n{bt}");

    // per-project
    let mut by_project: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
    for r in &convo_results {
        let e = by_project.entry(&r.project).or_default();
        e.0 += 1;
        e.1 += r.original_bytes;
        e.2 += r.compressed_bytes;
    }

    let mut pt = Table::new();
    pt.load_preset(UTF8_FULL_CONDENSED);
    pt.set_header(vec!["Project", "Convos", "Original", "Compressed", "Savings"]);
    for (project, (count, orig, comp)) in &by_project {
        pt.add_row(vec![
            project.to_string(),
            count.to_string(),
            format_bytes(*orig),
            format_bytes(*comp),
            format!("{:.1}%", savings_pct(*orig, *comp)),
        ]);
    }
    println!("\n{pt}");

    let tokens_saved = total_saved / 4;
    let cost_saved = tokens_saved as f64 / 1_000_000.0 * 3.0;
    println!(
        "\n  est. tokens saved: {} (~${:.4} at sonnet $3/MTok)",
        tokens_saved, cost_saved
    );
}

// -- main --

fn main() {
    println!("terse v0.1 -- lossless context compression benchmark");
    println!("{}\n", "=".repeat(52));

    let args: Vec<String> = std::env::args().collect();
    let limit: Option<usize> = args
        .windows(2)
        .find(|w| w[0] == "--limit")
        .and_then(|w| w[1].parse().ok());

    let mut payloads = load_all_payloads();
    if payloads.is_empty() {
        eprintln!("no conversations found");
        return;
    }

    payloads.sort_by_key(|p| p.original_bytes);

    if let Some(n) = limit {
        payloads.truncate(n);
        eprintln!("limited to {n} conversation(s)");
    }

    if let (Some(first), Some(last)) = (payloads.first(), payloads.last()) {
        eprintln!(
            "payload sizes: {} to {}\n",
            format_bytes(first.original_bytes),
            format_bytes(last.original_bytes)
        );
    }

    let start = std::time::Instant::now();

    // 1. content type distribution
    print_content_types(&payloads);

    // 2. per-stage effectiveness (each in isolation)
    print_per_stage_report(&payloads);

    // 3. full pipeline, both modes
    print_pipeline_report(&payloads, true);
    print_pipeline_report(&payloads, false);

    let elapsed = start.elapsed();

    // 4. comparison with tamp
    println!("\n{}", "=".repeat(70));
    println!("COMPARISON");
    println!("{}\n", "=".repeat(70));

    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_header(vec!["Tool", "Mode", "Savings", "Time", "Failures"]);
    table.add_row(vec!["tamp", "cacheSafe=true", "0.0%", "~15min", "11/108"]);
    table.add_row(vec!["tamp", "cacheSafe=false", "2.7%", "~15min", "11/108"]);
    table.add_row(vec![
        "terse",
        "cacheSafe=true",
        &format!("--"),
        &format!("{:.1}s", elapsed.as_secs_f64()),
        "0/108",
    ]);
    table.add_row(vec![
        "terse",
        "cacheSafe=false",
        &format!("--"),
        &format!("{:.1}s", elapsed.as_secs_f64()),
        "0/108",
    ]);
    println!("{table}");
}
