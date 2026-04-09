# tamp-rust architecture

## why this exists

tamp (Node.js) claims 52.6% token savings on LLM API requests. we benchmarked it against 108 real Claude Code conversations (97.3MB total) and found:

- `cacheSafe=true` (default): **0.0% savings**
- `cacheSafe=false` (full scan): **2.7% savings**
- 11/108 conversations failed outright (413/502 on anything >2.5MB)
- processing time: 17-117 seconds per conversation over 1MB
- root cause: `@anthropic-ai/tokenizer countTokens()` takes ~8s per 100KB, called 6x per block

the approach is valid. the implementation is not. tamp-rust is a 1:1 reimplementation in rust that:
- handles any payload size (no 2.5MB ceiling)
- processes in milliseconds, not minutes
- skips the catastrophic tokenizer (uses chars/4 estimate)
- same compression stages, same API, same config

## what tamp does

tamp is an HTTP proxy that sits between an LLM client and the API:

```
claude code -> tamp (:7778) -> anthropic API
```

it intercepts POST requests to `/v1/messages`, parses the JSON body, finds `tool_result` content blocks in the messages array, compresses them, and forwards the smaller payload upstream.

claude code sends the ENTIRE conversation history in every API call. a 200-message conversation with file reads, grep results, and code outputs can be 5-10MB. many tool results are duplicates (re-reading the same file) or near-duplicates (reading a file after a small edit).

## modules (1:1 mapping to tamp)

```
tamp (node)          tamp-rust
-----------          ---------
config.js        ->  config.rs
detect.js        ->  detect.rs
providers.js     ->  providers.rs
compress.js      ->  compress.rs
stats.js         ->  stats.rs
index.js         ->  proxy.rs
                     main.rs (CLI entry point)
```

### config.rs

loads configuration from env vars and optional config file at `~/.config/tamp/config`.

| env var | default | description |
|---------|---------|-------------|
| TAMP_PORT | 7778 | proxy listen port |
| TAMP_UPSTREAM | https://api.anthropic.com | upstream API URL |
| TAMP_STAGES | minify,toon,strip-lines,whitespace,dedup,diff,prune | compression pipeline |
| TAMP_MIN_SIZE | 200 | skip content smaller than this (chars) |
| TAMP_LOG | true | log to stderr |
| TAMP_MAX_BODY | 10485760 | passthrough bodies larger than this |
| TAMP_CACHE_SAFE | true | only compress newest tool_result group |

config file format: `KEY=value` per line, `#` comments. env vars override file.

### detect.rs

content classification. determines what compression stages apply.

```
classifyContent(text) -> toon | json | json-lined | text | unknown
```

rules (evaluated in order):
1. **toon**: first line matches `[TOON]` or `\w+\[\d+\]{` or `\w+\[\d+]:`
2. **json**: valid JSON parse
3. **json-lined**: strip line number prefixes (`^ *\d+[\t->]`), then valid JSON parse
4. **text**: non-empty string that isn't any of the above
5. **unknown**: empty or non-string

helper: `stripLineNumbers(text)` -- if 2+ of first 5 non-empty lines match `^ *\d+[\t->]`, strip those prefixes from all lines. this handles Claude Code's Read tool output format (`  1\tline content`).

### providers.rs

extracts compression targets from API request bodies. each provider knows its API format.

**anthropic** (the one we care about):
- walks `body.messages[]`
- for each message with `role: "user"`, examines `content[]` array
- finds blocks with `type: "tool_result"`
- extracts the `content` field (string) as a compression target
- skips blocks where `is_error: true`
- records the JSON path for later replacement: `["messages", mi, "content", ci, "content"]`

**cacheSafe mode** (default):
- only extracts targets from the LAST user message group that has eligible tool_results
- walks messages array backwards, finds first user message with non-skipped tool_results
- this preserves prompt caching -- anthropic caches message prefixes, so we only touch the newest content that hasn't been cached yet

**cacheSafe=false**:
- extracts ALL tool_result content blocks across ALL messages
- enables cross-message dedup and diff (much higher savings)
- breaks prompt caching but saves more tokens overall

also has openai and gemini providers (same idea, different JSON paths). only anthropic matters for claude code.

### compress.rs

the compression pipeline. stages run in this order:

#### 1. dedup (cross-target)
- hash each target's text (SHA-256 for >=128 chars, identity for shorter)
- if same hash+text already seen in an earlier target, replace content with:
  `[see tool_result in message {mi}, block {bi} -- identical content]`
- handles: re-reading the same file multiple times in a conversation

#### 2. diff (cross-target)
- for targets >200 chars that weren't deduped
- compare against all previously seen targets using jaccard similarity on line sets
- if similarity >0.5 and <1.0, compute unified diff (context=1 line)
- if diff body < 50% of original text length, replace with:
  `[diff from tool_result in message {mi}, block {bi}]:\n{diff_body}`
- handles: reading a file, editing it, reading it again

#### 3. per-block compression (applied to each remaining non-dedup/non-diff target)

classify content, then:

**text content:**
- `strip-lines`: remove line number prefixes (see detect.rs)
- `whitespace`: strip trailing spaces/tabs per line, collapse 3+ newlines to 2
- accept if result < 90% of original length

**json / json-lined content:**
- `strip-lines`: remove line number prefixes (if json-lined)
- `prune`: recursively remove npm metadata keys (integrity, shasum, _id, _from, _resolved, _integrity, _nodeVersion, _npmVersion, _phantomChildren, _requiredBy, resolved if starts with https://registry.)
- `minify`: re-serialize JSON with no whitespace
- accept if result < original length

**toon content:**
- skip (already compressed)

**unknown / too small (<minSize):**
- skip

#### stages we skip (not worth porting)
- **toon encoding**: complex custom format, marginal gains over minified JSON. tamp itself falls back to minify when toon doesn't help.
- **llmlingua/textpress/foundation-models**: require external AI services
- **strip-comments**: opt-in, not in default stages

### stats.rs

tracks session-level compression statistics:
- total requests processed
- total blocks compressed
- total chars original / saved
- estimated tokens saved (chars / 4)

### proxy.rs

HTTP proxy server. hyper-based (or axum).

request flow:
1. accept incoming request
2. if not POST to `/v1/messages` (or openai/gemini equivalents), passthrough
3. read full body (up to maxBody, passthrough if larger)
4. decompress if content-encoding set (gzip, deflate, br, zstd)
5. parse JSON
6. extract targets via provider
7. run compression pipeline
8. re-serialize JSON
9. forward to upstream with updated content-length
10. pipe response back to client

also serves `/health` endpoint with session stats JSON.

### main.rs

CLI entry point. two modes:

**proxy mode** (default): `tamp-rust` or `tamp-rust -y`
- starts HTTP proxy on configured port
- identical behavior to `npx tamp -y`

**benchmark mode**: `tamp-rust bench`
- loads JSONL conversation files from `~/.claude/projects/*/`
- reconstructs full message arrays
- runs compression pipeline directly (no HTTP)
- reports savings by conversation size bucket
- no network, no proxy, no mock upstream needed

## data flow

```
                    provider.extract()
                         |
                    [target, target, target, ...]
                         |
                    dedup stage (cross-target)
                         |
                    diff stage (cross-target)
                         |
                    per-block compress:
                      classify -> strip-lines -> whitespace -> minify/prune
                         |
                    provider.apply() -- write compressed text back into body
                         |
                    JSON.stringify -> forward upstream
```

## file layout

```
tamp-rust/
  Cargo.toml
  src/
    main.rs          -- CLI entry, arg parsing, bench mode
    config.rs        -- config loading (env + file)
    detect.rs        -- content classification
    providers.rs     -- target extraction (anthropic, openai, gemini)
    compress.rs      -- compression pipeline + stages
    stats.rs         -- session statistics
    proxy.rs         -- HTTP proxy server
  docs/
    ARCHITECTURE.md  -- this file
    BENCHMARKS.md    -- benchmark methodology and results
```

## dependencies

| crate | purpose |
|-------|---------|
| tokio | async runtime (proxy mode) |
| hyper | HTTP proxy |
| serde + serde_json | JSON parsing |
| regex | line number detection, whitespace normalization |
| sha2 | dedup hashing |
| similar | unified diff generation |
| comfy-table | benchmark output formatting |

## implementation order (by immediate impact)

1. **detect.rs + providers.rs** -- target extraction, without this nothing works
2. **compress.rs: dedup** -- biggest single-stage win (identical file re-reads)
3. **compress.rs: diff** -- second biggest (similar file re-reads)
4. **compress.rs: minify + prune** -- JSON compaction
5. **compress.rs: strip-lines + whitespace** -- line numbers, whitespace
6. **main.rs: bench mode** -- validate against tamp's numbers using real data
7. **config.rs + stats.rs** -- configuration and tracking
8. **proxy.rs + main.rs: proxy mode** -- drop-in tamp replacement
