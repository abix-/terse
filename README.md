# terse

Lossless context compression for LLM API requests. Reduces token count in tool_result content without losing any data.

## problem

LLM API calls include the full conversation history. Tool results (file reads, query outputs, API responses) are the bulk of that payload -- full of redundancy that inflates token count without adding information.

## what it does

Content-aware text-to-text re-encoding. Detects structure in tool_result content, re-encodes it into a denser representation. All information preserved.

| Content Type | Strategy | Savings |
|---|---|---|
| Tabular (CSV/TSV) | Columnar grouping, factor out low-cardinality columns, shorten timestamps | 50% |
| JSON | Prune junk keys, TOON encoding (dense line-oriented format) | 22% |
| Cross-target identical | SHA-256 dedup, replace with back-reference | 96% |
| Cross-target similar | Jaccard similarity > 0.5, replace with unified diff | 74% |
| Line-numbered text | Strip line number prefixes (Claude Read tool output) | 21% |
| Plain text | Normalize whitespace, collapse blank lines | ~5% |

Output is always valid, human-readable text. No binary encoding.

## deployment

terse runs as an **LLM gateway** between Claude Code and the API. Supports both AWS Bedrock and Anthropic direct.

### bedrock

```bash
# start terse
cargo run --release --features proxy -- --proxy

# configure claude code (settings.json env vars)
ANTHROPIC_BEDROCK_BASE_URL=http://localhost:7778
CLAUDE_CODE_SKIP_BEDROCK_AUTH=1
```

`CLAUDE_CODE_SKIP_BEDROCK_AUTH=1` tells Claude Code not to SigV4-sign the request. This is required because SigV4 includes the request body in the signature -- if Claude Code signs first and terse then modifies the body (compression), the signature is invalidated and Bedrock rejects it. Instead, Claude Code sends unsigned HTTP to terse, and terse signs the compressed request via the AWS SDK before forwarding to Bedrock. Uses the standard AWS credential chain (AWS_PROFILE, SSO, env vars).

### anthropic direct

```bash
# start terse
cargo run --release --features proxy -- --proxy

# configure claude code
ANTHROPIC_BASE_URL=http://localhost:7778
# x-api-key header passes through automatically
```

## benchmarks

Tested on 114 real Claude Code conversations (245 segments, 7.4MB top 10):

| Metric | terse | tamp |
|---|---|---|
| Savings | 3.5% | 3.5% |
| Speed | **0.4s** | 70s |
| Failures | 0 | 11/108 (413/502 on >2.5MB) |
| Bedrock support | **yes** | no |

**terse is 170x faster** with equivalent compression. tamp's bottleneck is a WASM tokenizer called twice per target for cosmetic "tokens saved" stats. terse skips it (fewer bytes = fewer tokens, the relationship is ~linear at 1 token ~= 4 bytes).

See [docs/BENCHMARKS.md](docs/BENCHMARKS.md) for detailed per-segment, per-target, per-stage results.

## build

```bash
# benchmark only (no proxy deps)
cargo build --release

# with proxy/gateway support
cargo build --release --features proxy
```

## usage

```bash
# proxy mode (LLM gateway)
terse --proxy                    # listen on :7778 (default)
terse --proxy --port 7790        # custom port

# benchmark mode (offline, against saved conversation data)
terse                            # all segments
terse --limit 10                 # top 10 by size
terse --bench --limit 10         # head-to-head vs tamp
```

## why not hooks?

Claude Code hooks can't intercept API request bodies. There is no pre-API-request hook. Available hooks (PreToolUse, PostToolUse) operate on individual tool calls, can't see the full conversation context, and can't modify built-in tool results. The gateway model is the only way to compress at the right layer.

## why not tamp?

- Only supports Anthropic direct (no Bedrock, no Vertex)
- 170x slower (WASM tokenizer overhead)
- Fails on >2.5MB payloads (Node body limit)
- `cacheSafe=true` (default) is counterproductive -- see BENCHMARKS.md

## license

GPL-3.0. See [LICENSE](LICENSE).
