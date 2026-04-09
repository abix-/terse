mod classify;
mod compress;
mod extract;
mod json;
#[cfg(feature = "proxy")]
mod proxy;
mod tabular;
mod text;

use classify::{classify, ContentType};
use comfy_table::{presets::UTF8_FULL_CONDENSED, Table};
use compress::{Stage, MIN_SIZE};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read as IoRead, Write as IoWrite},
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Instant,
};

const PROJECTS_ROOT: &str = r"C:\Users\aiannaco\.claude\projects";

// -- JSONL parsing --

struct ConvoPayload {
    project: String,
    #[allow(dead_code)]
    file: String,
    segment: usize,
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

/// parse a JSONL file into segments split at compact_boundary lines.
/// each segment is a realistic API request (what claude actually sees).
fn reconstruct_segments(path: &Path) -> Vec<Vec<Value>> {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut segments: Vec<Vec<Value>> = Vec::new();
    let mut current: Vec<Value> = Vec::new();

    for line in content.lines() {
        let obj: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // detect compaction boundary
        if obj.get("type").and_then(|t| t.as_str()) == Some("system")
            && obj.get("subtype").and_then(|t| t.as_str()) == Some("compact_boundary")
        {
            if current.len() >= 6 {
                segments.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
            continue;
        }

        let msg_type = match obj.get("type").and_then(|t| t.as_str()) {
            Some(t) if t == "user" || t == "assistant" => t,
            _ => continue,
        };

        let message = match obj.get("message") {
            Some(m) if m.get("role").is_some() => m,
            _ => continue,
        };

        // skip sidechain messages (agent subconversations)
        if obj.get("isSidechain").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }

        let _ = msg_type;
        current.push(message.clone());
    }

    // last segment
    if current.len() >= 6 {
        segments.push(current);
    }

    segments
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
    let mut total_segments = 0usize;

    for (project, path) in &files {
        let file_name = path.file_name().unwrap().to_string_lossy().to_string();
        let segments = reconstruct_segments(path);

        if segments.is_empty() {
            skipped_small += 1;
            continue;
        }

        for (seg_idx, messages) in segments.into_iter().enumerate() {
            let message_count = messages.len();
            let body = build_body(messages);
            let original_bytes = serde_json::to_string(&body).unwrap().len();

            payloads.push(ConvoPayload {
                project: project.clone(),
                file: file_name.clone(),
                segment: seg_idx,
                message_count,
                original_bytes,
                body,
            });
            total_segments += 1;
        }
    }

    eprintln!(
        "loaded {} segments from {} files (skipped: {} too small)",
        total_segments,
        files.len(),
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
        let targets = extract::extract_targets(&payload.body);
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
        Stage::Toon,
        Stage::JsonMinify,
        Stage::StripLines,
        Stage::Whitespace,
        Stage::Dedup,
        Stage::Diff,
    ];

    // for per-block stages (tabular, json, strip-lines, whitespace),
    // we can pool all targets since they operate independently.
    // for cross-target stages (dedup, diff), we must run per-segment.
    let mut all_targets: Vec<extract::Target> = Vec::new();
    for payload in payloads {
        let targets = extract::extract_targets(&payload.body);
        all_targets.extend(targets);
    }

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
        let results = if *stage == Stage::Dedup || *stage == Stage::Diff {
            // run per-segment to avoid cross-compaction comparisons
            let mut all_results = Vec::new();
            for payload in payloads {
                let targets = extract::extract_targets(&payload.body);
                let r = compress::run_single_stage(&targets, *stage);
                all_results.extend(r);
            }
            all_results
        } else {
            compress::run_single_stage(&all_targets, *stage)
        };

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
        let results = if *stage == Stage::Dedup || *stage == Stage::Diff {
            let mut all_results = Vec::new();
            for payload in payloads {
                let targets = extract::extract_targets(&payload.body);
                let r = compress::run_single_stage(&targets, *stage);
                all_results.extend(r);
            }
            all_results
        } else {
            compress::run_single_stage(&all_targets, *stage)
        };
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

fn print_pipeline_report(payloads: &[ConvoPayload]) -> (usize, usize) {
    println!("\n{}", "=".repeat(70));
    println!("FULL PIPELINE (dedup -> diff -> per-block)");
    println!("{}\n", "=".repeat(70));

    let mut total_orig = 0usize;
    let mut total_comp = 0usize;
    let mut stage_savings: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new(); // count, orig, saved

    struct SegResult {
        project: String,
        message_count: usize,
        original_bytes: usize,
        compressed_bytes: usize,
    }
    let mut seg_results: Vec<SegResult> = Vec::new();

    for payload in payloads {
        let mut targets = extract::extract_targets(&payload.body);
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

        seg_results.push(SegResult {
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
            stage.to_string(),
            count.to_string(),
            format_bytes(*saved),
            format!("{:.1}%", pct(*saved, total_saved)),
        ]);
    }
    st.add_row(vec![
        "TOTAL".into(),
        "".into(),
        format_bytes(total_saved),
        "100%".into(),
    ]);
    println!("{st}");

    // by segment size bucket
    let buckets: &[(&str, usize, usize)] = &[
        ("<10 msgs", 0, 10),
        ("10-30 msgs", 10, 30),
        ("30-80 msgs", 30, 80),
        ("80-200 msgs", 80, 200),
        ("200+ msgs", 200, usize::MAX),
    ];

    let mut bt = Table::new();
    bt.load_preset(UTF8_FULL_CONDENSED);
    bt.set_header(vec!["Segment Size", "Count", "Original", "Compressed", "Savings"]);

    for (label, min, max) in buckets {
        let in_bucket: Vec<_> = seg_results
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
        seg_results.len().to_string(),
        format_bytes(total_orig),
        format_bytes(total_comp),
        format!("{:.1}%", savings_pct(total_orig, total_comp)),
    ]);
    println!("\n{bt}");

    // per-project
    let mut by_project: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
    for r in &seg_results {
        let e = by_project.entry(&r.project).or_default();
        e.0 += 1;
        e.1 += r.original_bytes;
        e.2 += r.compressed_bytes;
    }

    let mut pt = Table::new();
    pt.load_preset(UTF8_FULL_CONDENSED);
    pt.set_header(vec!["Project", "Segments", "Original", "Compressed", "Savings"]);
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

    (total_orig, total_comp)
}

// -- bench mode: head-to-head terse vs tamp --

/// minimal HTTP server that captures request body sizes from tamp's forwarded requests
fn start_mock_upstream() -> Arc<Mutex<Vec<(usize, String)>>> {
    let captured: Arc<Mutex<Vec<(usize, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_clone = captured.clone();

    thread::spawn(move || {
        let listener = TcpListener::bind("127.0.0.1:7779").expect("failed to bind mock upstream on :7779");
        for stream in listener.incoming() {
            let mut stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };

            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0usize;

            // read HTTP headers (case-insensitive)
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line.trim().is_empty() {
                    break;
                }
                let lower = line.to_lowercase();
                if let Some(val) = lower.strip_prefix("content-length:") {
                    content_length = val.trim().parse().unwrap_or(0);
                }
            }

            // read body
            let mut body = vec![0u8; content_length];
            if content_length > 0 {
                let _ = reader.read_exact(&mut body);
            }
            let body_str = String::from_utf8_lossy(&body).to_string();

            // body captured silently
            captured_clone.lock().unwrap().push((content_length, body_str));

            // respond 200 with minimal anthropic response
            let reply = r#"{"id":"msg_bench","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"model":"claude-sonnet-4-20250514","stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1}}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                reply.len(),
                reply
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });

    // give it a moment to bind
    thread::sleep(std::time::Duration::from_millis(100));
    captured
}

fn check_tamp_running() -> bool {
    match ureq::get("http://127.0.0.1:7778/health").call() {
        Ok(resp) => resp.status() == 200,
        Err(_) => false,
    }
}

/// per-target detail for the detailed report
#[derive(Debug)]
struct TargetDetail {
    msg_idx: usize,
    block_idx: usize,
    content_type: ContentType,
    original_bytes: usize,
    terse_bytes: usize,
    terse_stage: Option<Stage>,
    tamp_bytes: Option<usize>,
}

struct BenchResult {
    original_bytes: usize,
    terse_bytes: usize,
    terse_ms: f64,
    tamp_bytes: Option<usize>,
    tamp_ms: Option<f64>,
    tamp_error: Option<String>,
    targets: Vec<TargetDetail>,
    message_count: usize,
}

fn bench_segment(payload: &ConvoPayload, captured: &Arc<Mutex<Vec<(usize, String)>>>) -> BenchResult {
    let original_json = serde_json::to_string(&payload.body).unwrap();
    let original_bytes = original_json.len();

    // terse -- get per-target detail
    let terse_start = Instant::now();
    let mut targets = extract::extract_targets(&payload.body);
    let results = compress::compress_targets(&mut targets);
    let mut terse_body = payload.body.clone();
    extract::apply_targets(&mut terse_body, &targets);
    let terse_json = serde_json::to_string(&terse_body).unwrap();
    let terse_ms = terse_start.elapsed().as_secs_f64() * 1000.0;
    let terse_bytes = terse_json.len();

    // build per-target detail (tamp side filled in later)
    let mut target_details: Vec<TargetDetail> = targets
        .iter()
        .zip(results.iter())
        .map(|(t, r)| TargetDetail {
            msg_idx: t.msg_idx,
            block_idx: t.block_idx,
            content_type: r.content_type,
            original_bytes: r.original_bytes,
            terse_bytes: r.compressed_bytes,
            terse_stage: r.stage,
            tamp_bytes: None,
        })
        .collect();

    // extract original targets for comparison with tamp
    let orig_targets = extract::extract_targets(&payload.body);

    // tamp: POST to proxy, capture what it forwards to mock upstream
    let pre_count = captured.lock().unwrap().len();
    let tamp_start = Instant::now();

    let tamp_result = ureq::post("http://127.0.0.1:7778/v1/messages")
        .set("Content-Type", "application/json")
        .set("x-api-key", "bench-fake-key")
        .set("anthropic-version", "2023-06-01")
        .send_string(&original_json);

    let tamp_ms = tamp_start.elapsed().as_secs_f64() * 1000.0;

    match tamp_result {
        Ok(_) => {
            thread::sleep(std::time::Duration::from_millis(50));
            let captures = captured.lock().unwrap();
            if captures.len() > pre_count {
                let (_cl, body_str) = &captures[captures.len() - 1];
                let tamp_total = body_str.len();

                // parse tamp's output and extract targets to get per-target sizes
                if let Ok(tamp_body) = serde_json::from_str::<Value>(body_str) {
                    let tamp_targets = extract::extract_targets(&tamp_body);
                    // match tamp targets to our targets by (msg_idx, block_idx)
                    for td in target_details.iter_mut() {
                        // find matching tamp target
                        for tt in &tamp_targets {
                            if tt.msg_idx == td.msg_idx && tt.block_idx == td.block_idx {
                                td.tamp_bytes = Some(tt.text.len());
                                break;
                            }
                        }
                        // if no match found, tamp didn't touch it -- same as original
                        if td.tamp_bytes.is_none() {
                            // check if original target exists in tamp output at same position
                            for ot in &orig_targets {
                                if ot.msg_idx == td.msg_idx && ot.block_idx == td.block_idx {
                                    // tamp may have compressed it to something we can't match
                                    // or it was unchanged -- find by path in tamp body
                                    td.tamp_bytes = Some(td.original_bytes);
                                    break;
                                }
                            }
                        }
                    }
                }

                BenchResult {
                    original_bytes,
                    terse_bytes,
                    terse_ms,
                    tamp_bytes: Some(tamp_total),
                    tamp_ms: Some(tamp_ms),
                    tamp_error: None,
                    targets: target_details,
                    message_count: payload.message_count,
                }
            } else {
                BenchResult {
                    original_bytes,
                    terse_bytes,
                    terse_ms,
                    tamp_bytes: None,
                    tamp_ms: Some(tamp_ms),
                    tamp_error: Some("no capture from mock".into()),
                    targets: target_details,
                    message_count: payload.message_count,
                }
            }
        }
        Err(e) => BenchResult {
            original_bytes,
            terse_bytes,
            terse_ms,
            tamp_bytes: None,
            tamp_ms: Some(tamp_ms),
            tamp_error: Some(format!("{}", e)),
            targets: target_details,
            message_count: payload.message_count,
        },
    }
}

fn run_bench(payloads: &[ConvoPayload]) {
    println!("\n{}", "=".repeat(70));
    println!("HEAD-TO-HEAD BENCHMARK: terse vs tamp");
    println!("{}\n", "=".repeat(70));

    if !check_tamp_running() {
        eprintln!("ERROR: tamp is not running on localhost:7778");
        eprintln!("start it with: TAMP_UPSTREAM=http://127.0.0.1:7779 TAMP_CACHE_SAFE=false node tamp-bench/node_modules/@sliday/tamp/bin/tamp.js -y");
        return;
    }
    eprintln!("tamp detected on :7778");

    let captured = start_mock_upstream();
    eprintln!("mock upstream listening on :7779");

    let mut results: Vec<BenchResult> = Vec::new();
    let total = payloads.len();

    for (i, payload) in payloads.iter().enumerate() {
        eprint!("\r  segment {}/{} ({})...", i + 1, total, format_bytes(payload.original_bytes));
        let r = bench_segment(payload, &captured);
        results.push(r);
    }
    eprintln!("\r  done.{}", " ".repeat(40));

    // === SUMMARY TABLE ===
    println!("\n--- SUMMARY ---\n");
    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_header(vec!["Metric", "terse", "tamp"]);

    let total_orig: usize = results.iter().map(|r| r.original_bytes).sum();
    let total_terse: usize = results.iter().map(|r| r.terse_bytes).sum();
    let total_tamp: usize = results.iter().filter_map(|r| r.tamp_bytes).sum();
    let tamp_ok: Vec<_> = results.iter().filter(|r| r.tamp_bytes.is_some()).collect();
    let tamp_fail: Vec<_> = results.iter().filter(|r| r.tamp_error.is_some()).collect();

    let terse_time: f64 = results.iter().map(|r| r.terse_ms).sum();
    let tamp_time: f64 = results.iter().filter_map(|r| r.tamp_ms).sum();

    table.add_row(vec![
        "segments".into(),
        format!("{}", results.len()),
        format!("{} ok, {} failed", tamp_ok.len(), tamp_fail.len()),
    ]);
    table.add_row(vec![
        "original".into(),
        format_bytes(total_orig),
        format_bytes(total_orig),
    ]);
    table.add_row(vec![
        "compressed".into(),
        format_bytes(total_terse),
        format_bytes(total_tamp),
    ]);
    table.add_row(vec![
        "savings".into(),
        format!("{:.1}%", savings_pct(total_orig, total_terse)),
        format!("{:.1}%", savings_pct(total_orig, total_tamp)),
    ]);
    table.add_row(vec![
        "time".into(),
        format!("{:.1}s", terse_time / 1000.0),
        format!("{:.1}s", tamp_time / 1000.0),
    ]);
    println!("{table}");

    if !tamp_fail.is_empty() {
        println!("\ntamp failures ({}):", tamp_fail.len());
        for r in &tamp_fail {
            println!(
                "  {} segment: {}",
                format_bytes(r.original_bytes),
                r.tamp_error.as_deref().unwrap_or("unknown")
            );
        }
    }

    // === PER-SEGMENT OVERVIEW ===
    println!("\n--- PER-SEGMENT OVERVIEW ---\n");
    let mut dt = Table::new();
    dt.load_preset(UTF8_FULL_CONDENSED);
    dt.set_header(vec!["#", "Msgs", "Targets", "Original", "terse", "terse %", "tamp", "tamp %", "winner", "terse ms", "tamp ms"]);

    for (i, r) in results.iter().enumerate() {
        let tamp_str = r.tamp_bytes.map(|b| format_bytes(b)).unwrap_or("FAIL".into());
        let tamp_pct = r.tamp_bytes.map(|b| format!("{:.1}%", savings_pct(r.original_bytes, b))).unwrap_or("-".into());
        let terse_sav = savings_pct(r.original_bytes, r.terse_bytes);
        let tamp_sav = r.tamp_bytes.map(|b| savings_pct(r.original_bytes, b)).unwrap_or(0.0);
        let winner = if terse_sav > tamp_sav + 0.05 {
            "terse"
        } else if tamp_sav > terse_sav + 0.05 {
            "tamp"
        } else {
            "tie"
        };
        dt.add_row(vec![
            format!("{}", i + 1),
            r.message_count.to_string(),
            r.targets.len().to_string(),
            format_bytes(r.original_bytes),
            format_bytes(r.terse_bytes),
            format!("{:.1}%", terse_sav),
            tamp_str,
            tamp_pct,
            winner.into(),
            format!("{:.0}", r.terse_ms),
            r.tamp_ms.map(|ms| format!("{:.0}", ms)).unwrap_or("-".into()),
        ]);
    }
    println!("{dt}");

    // === TERSE STAGE ATTRIBUTION (aggregate across all segments) ===
    println!("\n--- TERSE STAGE ATTRIBUTION (all segments) ---\n");
    let mut stage_totals: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new(); // count, orig, compressed
    let mut total_target_orig = 0usize;
    let mut total_target_terse = 0usize;
    let mut skipped_count = 0usize;
    let mut skipped_bytes = 0usize;
    let mut unchanged_count = 0usize;
    let mut unchanged_bytes = 0usize;

    for r in &results {
        for td in &r.targets {
            total_target_orig += td.original_bytes;
            total_target_terse += td.terse_bytes;
            if td.original_bytes < MIN_SIZE {
                skipped_count += 1;
                skipped_bytes += td.original_bytes;
            } else if let Some(stage) = td.terse_stage {
                let e = stage_totals.entry(stage.to_string()).or_default();
                e.0 += 1;
                e.1 += td.original_bytes;
                e.2 += td.terse_bytes;
            } else {
                unchanged_count += 1;
                unchanged_bytes += td.original_bytes;
            }
        }
    }

    let mut st = Table::new();
    st.load_preset(UTF8_FULL_CONDENSED);
    st.set_header(vec!["Stage", "Targets", "Original", "Compressed", "Saved", "Savings %"]);

    let mut entries: Vec<_> = stage_totals.iter().collect();
    entries.sort_by(|a, b| (b.1.1 - b.1.2).cmp(&(a.1.1 - a.1.2)));

    for (stage, (count, orig, comp)) in &entries {
        st.add_row(vec![
            stage.to_string(),
            count.to_string(),
            format_bytes(*orig),
            format_bytes(*comp),
            format_bytes(orig - comp),
            format!("{:.1}%", savings_pct(*orig, *comp)),
        ]);
    }
    st.add_row(vec![
        "unchanged".into(),
        unchanged_count.to_string(),
        format_bytes(unchanged_bytes),
        format_bytes(unchanged_bytes),
        "0B".into(),
        "0.0%".into(),
    ]);
    st.add_row(vec![
        "skipped (<200)".into(),
        skipped_count.to_string(),
        format_bytes(skipped_bytes),
        format_bytes(skipped_bytes),
        "0B".into(),
        "-".into(),
    ]);
    println!("{st}");

    // === TAMP vs TERSE PER-TARGET COMPARISON (aggregate) ===
    println!("\n--- PER-TARGET COMPARISON (terse vs tamp, by content type) ---\n");
    let mut by_ct: BTreeMap<String, (usize, usize, usize, usize, usize)> = BTreeMap::new(); // count, orig, terse, tamp, tamp_count

    for r in &results {
        for td in &r.targets {
            let ct_name = format!("{:?}", td.content_type);
            let e = by_ct.entry(ct_name).or_default();
            e.0 += 1;
            e.1 += td.original_bytes;
            e.2 += td.terse_bytes;
            if let Some(tb) = td.tamp_bytes {
                e.3 += tb;
                e.4 += 1;
            } else {
                e.3 += td.original_bytes; // assume unchanged if no tamp data
            }
        }
    }

    let mut ct = Table::new();
    ct.load_preset(UTF8_FULL_CONDENSED);
    ct.set_header(vec!["Content Type", "Targets", "Original", "terse", "terse %", "tamp", "tamp %", "delta"]);

    let mut ct_entries: Vec<_> = by_ct.iter().collect();
    ct_entries.sort_by(|a, b| b.1.1.cmp(&a.1.1));

    for (ct_name, (count, orig, terse, tamp, _)) in &ct_entries {
        let terse_sav = savings_pct(*orig, *terse);
        let tamp_sav = savings_pct(*orig, *tamp);
        let delta = terse_sav - tamp_sav;
        ct.add_row(vec![
            ct_name.to_string(),
            count.to_string(),
            format_bytes(*orig),
            format_bytes(*terse),
            format!("{:.1}%", terse_sav),
            format_bytes(*tamp),
            format!("{:.1}%", tamp_sav),
            format!("{:+.1}%", delta),
        ]);
    }
    println!("{ct}");

    // === DETAILED PER-SEGMENT TARGET BREAKDOWN ===
    println!("\n--- DETAILED PER-SEGMENT TARGET BREAKDOWN ---\n");

    for (i, r) in results.iter().enumerate() {
        let terse_sav = savings_pct(r.original_bytes, r.terse_bytes);
        let tamp_sav = r.tamp_bytes.map(|b| savings_pct(r.original_bytes, b)).unwrap_or(0.0);
        println!(
            "SEGMENT {} | {} msgs | {} | terse {:.1}% | tamp {:.1}%",
            i + 1,
            r.message_count,
            format_bytes(r.original_bytes),
            terse_sav,
            tamp_sav,
        );

        // only show targets that are >= MIN_SIZE (skip tiny ones)
        let eligible: Vec<_> = r.targets.iter().filter(|t| t.original_bytes >= MIN_SIZE).collect();
        if eligible.is_empty() {
            println!("  (no eligible targets >= 200 bytes)\n");
            continue;
        }

        let mut tt = Table::new();
        tt.load_preset(UTF8_FULL_CONDENSED);
        tt.set_header(vec![
            "msg", "blk", "type", "original", "terse", "terse %", "stage",
            "tamp", "tamp %", "winner",
        ]);

        for td in &eligible {
            let terse_sav_t = savings_pct(td.original_bytes, td.terse_bytes);
            let tamp_bytes_t = td.tamp_bytes.unwrap_or(td.original_bytes);
            let tamp_sav_t = savings_pct(td.original_bytes, tamp_bytes_t);
            let stage_str = td.terse_stage.map(|s| s.to_string()).unwrap_or("-".into());
            let winner = if terse_sav_t > tamp_sav_t + 0.5 {
                "terse"
            } else if tamp_sav_t > terse_sav_t + 0.5 {
                "tamp"
            } else {
                "="
            };
            let ct_short = match td.content_type {
                ContentType::Tabular => "tab",
                ContentType::Json => "json",
                ContentType::JsonLined => "jsonl",
                ContentType::Text => "text",
                ContentType::Unknown => "unk",
            };
            tt.add_row(vec![
                td.msg_idx.to_string(),
                td.block_idx.to_string(),
                ct_short.into(),
                format_bytes(td.original_bytes),
                format_bytes(td.terse_bytes),
                format!("{:.1}%", terse_sav_t),
                stage_str,
                format_bytes(tamp_bytes_t),
                format!("{:.1}%", tamp_sav_t),
                winner.into(),
            ]);
        }
        println!("{tt}");

        // per-segment stage summary
        let mut seg_stages: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
        for td in &eligible {
            if let Some(stage) = td.terse_stage {
                let e = seg_stages.entry(stage.to_string()).or_default();
                e.0 += 1;
                e.1 += td.original_bytes;
                e.2 += td.original_bytes.saturating_sub(td.terse_bytes);
            }
        }
        if !seg_stages.is_empty() {
            print!("  stages: ");
            let parts: Vec<_> = seg_stages.iter()
                .map(|(s, (c, _o, saved))| format!("{}({}, -{})", s, c, format_bytes(*saved)))
                .collect();
            println!("{}", parts.join(", "));
        }
        println!();
    }
}

// -- main --

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // --proxy mode: run as LLM gateway (Bedrock)
    #[cfg(feature = "proxy")]
    if args.iter().any(|a| a == "--proxy") {
        let port: u16 = args
            .windows(2)
            .find(|w| w[0] == "--port")
            .and_then(|w| w[1].parse().ok())
            .unwrap_or(7778);

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            if let Err(e) = proxy::run_proxy(port).await {
                eprintln!("proxy error: {e}");
                std::process::exit(1);
            }
        });
        return;
    }

    #[cfg(not(feature = "proxy"))]
    if args.iter().any(|a| a == "--proxy") {
        eprintln!("proxy mode requires the 'proxy' feature:");
        eprintln!("  cargo build --release --features proxy");
        std::process::exit(1);
    }

    println!("terse v0.1 -- lossless context compression benchmark");
    println!("{}\n", "=".repeat(52));

    let limit: Option<usize> = args
        .windows(2)
        .find(|w| w[0] == "--limit")
        .and_then(|w| w[1].parse().ok());
    let bench_mode = args.iter().any(|a| a == "--bench");
    let dump_mode = args.iter().any(|a| a == "--dump");

    let mut payloads = load_all_payloads();
    if payloads.is_empty() {
        eprintln!("no segments found");
        return;
    }

    payloads.sort_by_key(|p| std::cmp::Reverse(p.original_bytes));

    if let Some(n) = limit {
        payloads.truncate(n);
        eprintln!("limited to {n} segment(s)");
    }

    if let (Some(first), Some(last)) = (payloads.first(), payloads.last()) {
        eprintln!(
            "segment sizes: {} to {}\n",
            format_bytes(first.original_bytes),
            format_bytes(last.original_bytes)
        );
    }

    if bench_mode {
        run_bench(&payloads);
        return;
    }

    if dump_mode {
        // analyze redundancy in ALL unchanged targets like 7zip would
        use std::collections::HashMap;
        println!("\n=== REDUNDANCY ANALYSIS (7zip thinking) ===\n");

        let mut all_texts: Vec<(usize, usize, usize, String)> = Vec::new();
        for payload in &payloads {
            let mut targets = extract::extract_targets(&payload.body);
            let results = compress::compress_targets(&mut targets);
            for (i, target) in targets.iter().enumerate() {
                if target.text.len() < MIN_SIZE {
                    continue;
                }
                if target.compressed.is_some() {
                    continue;
                }
                all_texts.push((target.msg_idx, target.block_idx, target.text.len(),
                    target.text.clone()));
            }
        }
        all_texts.sort_by_key(|x| std::cmp::Reverse(x.2));

        let total_unchanged: usize = all_texts.iter().map(|x| x.2).sum();
        println!("{} unchanged targets, {} total\n",
            all_texts.len(), format_bytes(total_unchanged));

        // aggregate redundancy stats
        let mut total_ws = 0usize;
        let mut total_dup_lines = 0usize;
        let mut total_lines = 0usize;
        let mut total_prefix_saveable = 0usize;
        let mut total_blank_lines_bytes = 0usize;
        let mut total_trailing_ws = 0usize;

        for (rank, (msg, blk, size, text)) in all_texts.iter().enumerate() {
            let lines: Vec<&str> = text.lines().collect();
            let unique_lines: std::collections::HashSet<&str> = lines.iter().copied().collect();
            let dup_lines = lines.len() - unique_lines.len();

            let ws_bytes: usize = lines.iter().map(|l| l.len() - l.trim_start().len()).sum();
            let trailing: usize = lines.iter().map(|l| l.len() - l.trim_end().len()).sum();
            let blank_bytes: usize = lines.iter().filter(|l| l.trim().is_empty()).map(|l| l.len() + 1).sum();

            // common path prefix
            let non_empty: Vec<&str> = lines.iter().filter(|l| !l.trim().is_empty()).copied().collect();
            let path_lines: Vec<&str> = non_empty.iter()
                .filter(|l| l.contains('\\') || l.contains('/'))
                .copied().collect();
            let prefix_save = if path_lines.len() >= 2 {
                let first = path_lines[0];
                let mut plen = first.len();
                for p in &path_lines[1..] {
                    plen = plen.min(p.len());
                    for (i, (a, b)) in first.bytes().zip(p.bytes()).enumerate() {
                        if a != b { plen = plen.min(i); break; }
                    }
                }
                let prefix = &first[..plen];
                if let Some(pos) = prefix.rfind(|c: char| c == '/' || c == '\\') {
                    let p = &first[..=pos];
                    if p.len() > 5 { (path_lines.len() - 1) * p.len() } else { 0 }
                } else { 0 }
            } else { 0 };

            total_ws += ws_bytes;
            total_dup_lines += dup_lines;
            total_lines += lines.len();
            total_prefix_saveable += prefix_save;
            total_blank_lines_bytes += blank_bytes;
            total_trailing_ws += trailing;

            // show top 10 individual targets
            if rank < 10 {
                let avg_line = if non_empty.is_empty() { 0 } else {
                    non_empty.iter().map(|l| l.len()).sum::<usize>() / non_empty.len()
                };
                println!("#{} msg={} blk={} ({}) -- {} lines, avg {}B/line",
                    rank+1, msg, blk, format_bytes(*size), lines.len(), avg_line);
                println!("  leading ws: {}B ({:.0}%) | trailing ws: {}B | blank lines: {}B | dup lines: {}/{}",
                    ws_bytes, ws_bytes as f64 / *size as f64 * 100.0,
                    trailing, blank_bytes, dup_lines, lines.len());
                if prefix_save > 0 {
                    println!("  path prefix saveable: {}B", prefix_save);
                }
                for line in lines.iter().take(4) {
                    println!("  | {line}");
                }
                if lines.len() > 4 { println!("  | ... ({} more)", lines.len() - 4); }
                println!();
            }
        }

        println!("=== AGGREGATE ACROSS ALL {} UNCHANGED TARGETS ({}) ===\n",
            all_texts.len(), format_bytes(total_unchanged));
        println!("  leading whitespace:   {} ({:.1}%)", format_bytes(total_ws),
            total_ws as f64 / total_unchanged as f64 * 100.0);
        println!("  trailing whitespace:  {} ({:.1}%)", format_bytes(total_trailing_ws),
            total_trailing_ws as f64 / total_unchanged as f64 * 100.0);
        println!("  blank line bytes:     {} ({:.1}%)", format_bytes(total_blank_lines_bytes),
            total_blank_lines_bytes as f64 / total_unchanged as f64 * 100.0);
        println!("  duplicate lines:      {} of {} ({:.1}%)", total_dup_lines, total_lines,
            total_dup_lines as f64 / total_lines.max(1) as f64 * 100.0);
        println!("  path prefix saveable: {} ({:.1}%)", format_bytes(total_prefix_saveable),
            total_prefix_saveable as f64 / total_unchanged as f64 * 100.0);
        let est_savings = total_trailing_ws + total_blank_lines_bytes / 2 + total_prefix_saveable;
        println!("\n  estimated saveable:   {} ({:.1}% of unchanged)",
            format_bytes(est_savings), est_savings as f64 / total_unchanged as f64 * 100.0);

        return;
    }

    let start = Instant::now();

    // 1. content type distribution
    print_content_types(&payloads);

    // 2. per-stage effectiveness (each in isolation)
    print_per_stage_report(&payloads);

    // 3. full pipeline
    let (total_orig, total_comp) = print_pipeline_report(&payloads);

    let elapsed = start.elapsed();

    // 4. comparison with tamp
    println!("\n{}", "=".repeat(70));
    println!("COMPARISON");
    println!("{}\n", "=".repeat(70));

    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_header(vec!["Tool", "Savings", "Time", "Failures"]);
    table.add_row(vec!["tamp (cacheSafe=false)", "2.7%", "~15min", "11/108"]);
    table.add_row(vec![
        "terse",
        &format!("{:.1}%", savings_pct(total_orig, total_comp)),
        &format!("{:.1}s", elapsed.as_secs_f64()),
        "0",
    ]);
    println!("{table}");
}
