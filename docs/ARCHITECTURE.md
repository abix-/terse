# terse architecture

## what it does

terse is a lossless context compressor for LLM API requests. it reduces the size of tool_result content (file reads, query outputs, API responses) in conversation history before sending to the API. same data, fewer tokens.

## deployment

terse runs as an **LLM gateway** (HTTP proxy) between Claude Code and the API. it supports two backends:

### bedrock (AWS)

```
Claude Code --(plain HTTP)--> terse (:7778) --(SigV4)--> Bedrock Runtime
```

```bash
# claude code settings
ANTHROPIC_BEDROCK_BASE_URL=http://localhost:7778
CLAUDE_CODE_SKIP_BEDROCK_AUTH=1

# terse handles auth via standard AWS credential chain
AWS_PROFILE=your-profile
AWS_REGION=us-east-1
```

Claude Code sends unsigned requests. terse compresses the body, then forwards to Bedrock using the AWS SDK (which handles SigV4 signing, SSO token refresh, etc).

### anthropic direct

```
Claude Code --(x-api-key)--> terse (:7778) --(x-api-key)--> api.anthropic.com
```

```bash
ANTHROPIC_BASE_URL=http://localhost:7778
# x-api-key header passes through automatically
```

### why not hooks?

Claude Code hooks can't do this. there is no hook that fires before the API request is sent. available hooks (PreToolUse, PostToolUse) operate on individual tool calls, not the assembled conversation body. they can't:
- see all tool results together (needed for cross-target dedup/diff)
- modify built-in tool results (Read, Bash, Grep)
- intercept the API request before sending

the gateway model is the only way to compress the full conversation context.

### why not tamp?

tamp is the prior art (Node.js proxy). problems:
- **only supports Anthropic direct** -- no Bedrock, no Vertex. hardcoded to `api.anthropic.com` with `x-api-key` auth. no SigV4.
- **170x slower** -- 99% of runtime is `@anthropic-ai/tokenizer` (Rust->WASM BPE tokenizer) called twice per target. purely cosmetic "tokens saved" metric.
- **fails on large payloads** -- 413 on >2.5MB (Node http.createServer body limit). 502 on large payloads (tokenizer timeout).
- **cacheSafe=true is counterproductive** -- default mode only compresses the newest message, which breaks prompt caching by changing whether each message is compressed across requests. see BENCHMARKS.md.

## compression pipeline

```
request body (JSON)
    |
extract tool_result content blocks from messages[]
    |
phase 1: cross-target dedup (SHA-256 hash, identical -> back-reference)
    |
phase 2: cross-target diff (Jaccard similarity > 0.5 -> unified diff)
    |
phase 3: per-block compression:
    classify -> tabular | json | json-lined | text | unknown
    |
    tabular: factor out low-cardinality columns, shorten timestamps
    json: prune npm keys -> TOON encoding -> fallback minify
    json-lined: strip line numbers -> json path
    text: strip line numbers, normalize whitespace
    unknown: skip
    |
apply compressed content back to JSON body
    |
forward to API
```

### stages

| stage | scope | what it does | typical savings |
|-------|-------|-------------|-----------------|
| dedup | cross-target | replace identical content with back-reference | 96% on hits |
| diff | cross-target | replace similar content with unified diff | 74% on hits |
| tabular | per-block | columnar grouping for CSV/TSV, timestamp shortening | 50% on hits |
| toon | per-block | JSON -> TOON dense line-oriented encoding via serde_toon2 | 22% on hits |
| json-minify | per-block | prune npm keys + JSON minification | 26% on hits |
| strip-lines | per-block | remove line number prefixes (Claude Read tool output) | 21% on hits |
| whitespace | per-block | trailing space removal, blank line collapse | ~5% on hits |

### TOON format

JSON objects become `key: value` per line. arrays of uniform objects become tabular (header + CSV rows). nested objects use indentation. no braces, minimal quoting.

```json
{"name":"Ada","age":42,"tags":["rust","serde"]}
```
```
name: Ada
age: 42
tags[2]: rust,serde
```

implemented via `serde_toon2` crate (`serde_toon2::to_string(&serde_json::Value)`).

### content classification

order matters -- first match wins:

1. **TOON**: first line matches `[TOON]` or `\w+\[\d+\][{:]` -> skip (already compressed)
2. strip line numbers if present (handles Claude Read tool `  42\tcode`)
3. **tabular**: header row + consistent column count, rejects grep output (`path:line:code`)
4. **JSON**: valid `serde_json::from_str` parse
5. **JSON-lined**: after stripping line numbers, valid JSON parse
6. **text**: everything else
7. **unknown**: empty, too small (<200 bytes)

## project structure

```
terse/
  Cargo.toml
  src/
    main.rs        -- CLI: benchmark mode (default), proxy mode (--proxy)
    proxy.rs       -- LLM gateway: Bedrock (AWS SDK) + Anthropic (passthrough)
    compress.rs    -- pipeline orchestration, stage enum
    extract.rs     -- target extraction from Anthropic message bodies
    classify.rs    -- content type detection
    tabular.rs     -- CSV/TSV columnar compression
    json.rs        -- JSON prune + TOON encoding + minification
    text.rs        -- strip-lines, whitespace, dedup, diff
  docs/
    ARCHITECTURE.md
    BENCHMARKS.md
```

## dependencies

### core (always)

| crate | purpose |
|-------|---------|
| serde + serde_json | JSON parsing |
| serde_toon2 | TOON encoding for JSON |
| regex | line number detection, content classification |
| sha2 | dedup hashing |
| similar | unified diff generation |
| comfy-table | benchmark output formatting |
| csv | CSV parsing |
| ureq | HTTP client (benchmark mode, tamp comparison) |

### proxy mode (`--features proxy`)

| crate | purpose |
|-------|---------|
| tokio | async runtime |
| hyper + hyper-util | HTTP server |
| http-body-util + bytes | request body handling |
| aws-config | AWS credential loading (SSO, profile, env) |
| aws-sdk-bedrockruntime | Bedrock API calls with SigV4 |
| reqwest | HTTP client for Anthropic passthrough |

## CLI

```bash
# benchmark mode (default) -- run against real conversation data
cargo run --release
cargo run --release -- --limit 10
cargo run --release -- --bench --limit 10  # head-to-head vs tamp

# proxy mode -- LLM gateway
cargo run --release --features proxy -- --proxy
cargo run --release --features proxy -- --proxy --port 7790

# build optimized proxy binary
cargo build --release --features proxy
./target/release/terse --proxy
```

## request flow (proxy mode)

1. Claude Code sends POST to terse (no auth for Bedrock, API key for Anthropic)
2. terse reads full request body
3. parses JSON, extracts tool_result content blocks from messages[]
4. runs compression pipeline (dedup -> diff -> per-block)
5. applies compressed content back into JSON
6. routes based on URL path:
   - `/model/{id}/invoke*` -> Bedrock via AWS SDK (streaming supported)
   - `/v1/messages` -> Anthropic via reqwest (headers passed through)
7. streams response back to Claude Code unchanged
8. logs compression stats to stderr

## what's NOT compressed

- assistant messages (only user messages with tool_results)
- is_error: true tool results
- content < 200 bytes
- already-compressed TOON content
- non-POST requests (GET, OPTIONS, etc.)
