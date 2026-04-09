# terse

Lossless context compression for LLM API requests. Reduces token count in tool_result content without losing any data.

## status

**Working but not yet tested end-to-end with Claude Code.**

What works:
- Compression pipeline: dedup, diff, tabular, TOON, strip-lines, whitespace -- all stages functional
- Proxy compiles and starts, routes requests correctly
- Bedrock path: sends requests via AWS SDK, gets valid responses
- Anthropic path: forwards with header passthrough, gets valid responses
- Benchmarked against 114 real conversations (245 segments, 7.4MB top 10)

What hasn't been tested:
- **Live Claude Code session through the proxy** -- tested with curl, not with real Claude Code
- **Streaming responses** -- Bedrock streaming currently buffers all events before returning (no true passthrough yet)
- **Long-running sessions** -- SSO token refresh during multi-hour sessions untested
- **Error recovery** -- proxy crashes = Claude Code loses connection, no reconnect logic

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

## how it works

terse runs as an **LLM gateway** (HTTP proxy) between Claude Code and the API.

```
Claude Code --> terse (:7778) --> Bedrock or Anthropic API
```

1. Claude Code sends API request to terse (localhost:7778) instead of directly to the API
2. terse reads the full request body (JSON with messages array)
3. extracts tool_result content blocks from all user messages
4. runs the compression pipeline (dedup -> diff -> per-block stages)
5. applies compressed content back into the JSON body
6. forwards the smaller request to the actual API (Bedrock or Anthropic)
7. streams the response back unchanged

terse only modifies the request body (compressing tool results). Responses pass through untouched.

### bedrock

```bash
# start terse
cargo run --release --features proxy -- --proxy

# configure claude code (settings.json env vars)
ANTHROPIC_BEDROCK_BASE_URL=http://localhost:7778
CLAUDE_CODE_SKIP_BEDROCK_AUTH=1
```

**Why `CLAUDE_CODE_SKIP_BEDROCK_AUTH=1`?**

AWS Bedrock uses SigV4 request signing. SigV4 includes a SHA-256 hash of the request body in the Authorization header. Normally Claude Code signs the request, but terse modifies the body after receiving it (compression), which would invalidate that signature. So we tell Claude Code to skip signing and send plain HTTP to terse on localhost. terse then compresses the body and signs the modified request itself using the AWS SDK before forwarding to Bedrock.

The unsigned hop is localhost only (127.0.0.1). terse signs with real AWS credentials via the standard credential chain (AWS_PROFILE, SSO, env vars, IMDS).

### anthropic direct

```bash
# start terse
cargo run --release --features proxy -- --proxy

# configure claude code
ANTHROPIC_BASE_URL=http://localhost:7778
# x-api-key header passes through automatically
```

No signing needed -- Anthropic uses a static API key in the `x-api-key` header. terse forwards all headers (except hop-by-hop) unchanged.

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

The `proxy` feature adds tokio, hyper, aws-config, aws-sdk-bedrockruntime, and reqwest as dependencies. These are all optional and don't affect the benchmark binary.

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

## prior art

`CLAUDE_CODE_SKIP_BEDROCK_AUTH` + `ANTHROPIC_BEDROCK_BASE_URL` is the documented pattern for Bedrock gateway proxies. The [official Claude Code docs](https://github.com/anthropics/claude-code/issues/44899) show:

```bash
export ANTHROPIC_BEDROCK_BASE_URL='https://your-llm-gateway.com/bedrock'
export CLAUDE_CODE_SKIP_BEDROCK_AUTH=1 # If gateway handles AWS auth
```

46 GitHub issues reference this pattern (Tailscale Aperture, OpenCode, Zed, etc.). No public repo implements a **compression** gateway -- all existing gateways are auth/routing proxies.

No prior art exists for lossless text-to-text re-encoding of LLM tool results.

tamp (Node.js proxy by @sliday) is the closest prior art for compression but only supports Anthropic direct.

## why not hooks?

Claude Code hooks can't intercept API request bodies. There is no pre-API-request hook. Available hooks (PreToolUse, PostToolUse) operate on individual tool calls, can't see the full conversation context, and can't modify built-in tool results. The gateway model is the only way to compress at the right layer.

## why not tamp?

- Only supports Anthropic direct (no Bedrock, no Vertex)
- 170x slower (WASM tokenizer overhead)
- Fails on >2.5MB payloads (Node body limit)
- `cacheSafe=true` (default) is counterproductive -- see BENCHMARKS.md

## license

GPL-3.0. See [LICENSE](LICENSE).
