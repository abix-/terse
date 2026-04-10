# benchmarks

all numbers from benchmark run 2026-04-09 (updated with deep string compression fix).

## test corpus

114 real Claude Code conversations from 9 projects (datacenter ops, ansible, C#, kubernetes, storage monitoring, MCP servers, infrastructure-as-code).

note: segment count increased from 249 to 252 after re-parsing (minor JSONL boundary fix).

### compaction-aware segmentation

Claude Code compacts context at `compact_boundary` markers. each segment between compactions is what Claude actually sees in one API request. cross-target dedup/diff across compaction boundaries is unrealistic -- Claude never sees that data together.

- 114 JSONL files -> 249 segments (6 files had no valid segments)
- segments range from 3.8KB to 1.5MB
- each segment is an independent unit for compression benchmarking

**why this matters**: naively treating each JSONL file as one conversation inflates dedup/diff results by comparing targets that were never in the same API request.

## terse results (252 segments)

terse always compresses all messages deterministically (no cacheSafe flag).

### summary

| metric     | terse (v0.1 + deep string compression) |
|------------|----------------------------------------|
| segments   | 252                                    |
| original   | 103.5MB                                |
| compressed | 99.7MB                                 |
| savings    | **3.6%**                               |
| time       | 17.4s                                  |

### what the 3.6% means

the 3.6% is measured on the **full API request body** (103.5MB), which includes model params, system prompt, and assistant messages that can't be compressed. the actual compressible content (tool_result text blocks) is 47.3MB.

| scope | original | compressed | savings |
|-------|----------|------------|---------|
| full request body | 103.5MB | 99.7MB | 3.6% |
| tool_result content only | 47.3MB | 43.6MB | 7.8% |

most of the request body (~55%) is non-compressible overhead.

### comparison with tamp

previous head-to-head run (pre-deep-string-compression, 249 segments):

| metric  | terse  | tamp             |
|---------|--------|------------------|
| savings | 3.4%   | 3.4%             |
| time    | 4.2s   | 907.2s           |

terse now saves 3.6% (up from 3.4%) due to deep string compression cracking open JSON-wrapped CSV (InfluxDB MCP responses). tamp does not do this. fresh head-to-head not yet re-run.

### terse stage attribution (full pipeline, 252 segments)

| stage          | targets | bytes saved | % of total savings |
|----------------|---------|-------------|-------------------|
| strip-lines    | 2160    | 1.7MB       | 44.9%             |
| dedup          | 313     | 651.0KB     | 17.4%             |
| diff           | 254     | 612.5KB     | 16.3%             |
| toon           | 182     | 466.9KB     | 12.5%             |
| json-minify    | 57      | 23.2KB      | 0.6%              |
| tabular        | 23      | 15.9KB      | 0.4%              |
| whitespace     | 1       | 125B        | 0.0%              |
| **TOTAL**      |         | **3.7MB**   | **100%**          |

**strip-lines is the biggest saver** (1.7MB) -- removing line number prefixes from Claude's Read tool output. dedup and diff together save 1.2MB on repeated/similar content.

**deep string compression impact**: TOON now hits 182 targets (was 135) and tabular hits 23 (was 9). the new targets are JSON string values containing embedded CSV (InfluxDB MCP responses) and nested JSON that were previously opaque to the compressor.

### per-target comparison by content type (from previous head-to-head run)

note: tamp numbers are from the pre-deep-string-compression run. terse numbers below reflect the current version.

| content type | targets | original | terse  | terse % |
|-------------|---------|----------|--------|---------|
| Text         | 12087   | 44.4MB   | 41.5MB | 6.5%    |
| Json         | 331     | 1.4MB    | 1.2MB  | 13.5%   |
| Unknown      | 14882   | 1.2MB    | 1.2MB  | 0.0%    |
| JsonLined    | 38      | 147.5KB  | 85.1KB | 42.3%   |
| Tabular      | 83      | 97.1KB   | 84.6KB | 12.9%   |

### content type distribution

| content type             | count  | % count | size    | % size |
|--------------------------|--------|---------|---------|--------|
| plain text               | 12087  | 44.1%   | 44.4MB  | 93.9%  |
| json                     | 331    | 1.2%    | 1.4MB   | 3.0%   |
| skipped (<200 chars)     | 14882  | 54.3%   | 1.2MB   | 2.6%   |
| json (with line numbers) | 38     | 0.1%    | 147.5KB | 0.3%   |
| tabular (CSV/TSV)        | 83     | 0.3%    | 97.1KB  | 0.2%   |
| **TOTAL**                | **27421** | **100%** | **47.3MB** | **100%** |

93.9% of compressible content is plain text. this is the ceiling -- if you can't compress plain text better, you can't move the overall number much.

### per-stage effectiveness (isolation)

each stage tested independently against all eligible targets:

| stage       | targets hit   | savings on affected | overall savings |
|-------------|--------------|---------------------|----------------|
| dedup       | 12539/12539  | 98.3%               | 98.3%          |
| strip-lines | 5529/12539   | 10.8%               | 7.1%           |
| whitespace  | 2395/12539   | 15.1%               | 4.0%           |
| diff        | 263/12539    | 72.7%               | 1.4%           |
| toon        | 249/12539    | 44.9%               | 1.1%           |
| tabular     | 29/12539     | 37.1%               | 0.0%           |

**important**: dedup in isolation shows 98.3% because it compares ALL targets across ALL 252 segments. in the real pipeline, dedup only runs within each segment (realistic API request scope), dropping to 313 hits / 651KB saved.

**deep string compression note**: toon now hits 249 targets in isolation (was 153) because it can now crack open JSON string values containing embedded structured data (CSV, nested JSON) before TOON encoding.

### per-segment savings distribution

| segment size | count | original | compressed | savings |
|-------------|-------|----------|------------|---------|
| <10 msgs    | 2     | 9.4KB    | 9.4KB      | 0.0%    |
| 10-30 msgs  | 18    | 440.4KB  | 431.9KB    | 1.9%    |
| 30-80 msgs  | 23    | 2.2MB    | 2.1MB      | 5.4%    |
| 80-200 msgs | 41    | 11.1MB   | 10.7MB     | 3.5%    |
| 200+ msgs   | 168   | 89.7MB   | 86.5MB     | 3.6%    |
| **TOTAL**   | **252** | **103.5MB** | **99.7MB** | **3.6%** |

savings increase with conversation length (more tool results = more opportunities for dedup/diff/strip-lines).

### per-project savings

| project                                     | segments | original | compressed | savings |
|---------------------------------------------|----------|----------|------------|---------|
| C--Code-DC-Automation                       | 5        | 1.5MB    | 1.3MB      | 12.6%   |
| C--code-claude-blueprints-dc                | 45       | 15.8MB   | 14.6MB     | **7.5%** |
| C--Code-blackdiamond-infrastructure-ansible | 42       | 14.3MB   | 13.4MB     | 6.0%    |
| C--Code-k3sc                                | 15       | 5.3MB    | 5.2MB      | 3.2%    |
| C--code-awx-ui                              | 2        | 469.8KB  | 456.0KB    | 2.9%    |
| C--Code-ssnc-purestorage                    | 38       | 16.3MB   | 15.9MB     | 2.2%    |
| C--Code                                     | 45       | 21.5MB   | 21.0MB     | 2.2%    |
| C--Code-bdw-infra-console                   | 57       | 27.8MB   | 27.3MB     | 1.8%    |
| C--Code-bdw-infra-console-csharp            | 3        | 441.6KB  | 435.5KB    | 1.4%    |

DC-Automation has the highest savings (12.6%) -- heavy InfluxDB query output with repeated column structure. claude-blueprints-dc jumped from 6.0% to **7.5%** after deep string compression (InfluxDB MCP returns JSON-wrapped CSV).

### top 10 per-segment results

| #  | msgs | targets | original | terse   | terse % | tamp    | tamp % | winner | terse ms | tamp ms |
|----|------|---------|----------|---------|---------|---------|--------|--------|----------|---------|
| 1  | 1410 | 552     | 1.5MB    | 1.4MB   | 0.9%    | 1.4MB   | 1.0%   | tamp   | 145      | 19053   |
| 2  | 836  | 329     | 708.6KB  | 661.0KB | 6.7%    | 659.2KB | 7.0%   | tamp   | 43       | 8825    |
| 3  | 348  | 129     | 695.7KB  | 692.3KB | 0.5%    | 693.8KB | 0.3%   | terse  | 24       | 1304    |
| 4  | 860  | 339     | 679.8KB  | 657.2KB | 3.3%    | 659.9KB | 2.9%   | terse  | 21       | 11532   |
| 5  | 501  | 206     | 665.7KB  | 633.5KB | 4.8%    | 633.3KB | 4.9%   | tie    | 35       | 4348    |
| 6  | 315  | 108     | 665.1KB  | 638.6KB | 4.0%    | 637.6KB | 4.1%   | tamp   | 24       | 2430    |
| 7  | 442  | 170     | 653.8KB  | 628.6KB | 3.9%    | 628.3KB | 3.9%   | tie    | 25       | 4083    |
| 8  | 502  | 189     | 650.5KB  | 578.1KB | 11.1%   | 579.3KB | 11.0%  | terse  | 39       | 4696    |
| 9  | 421  | 171     | 634.5KB  | 630.0KB | 0.7%    | 626.0KB | 1.3%   | tamp   | 22       | 7135    |
| 10 | 640  | 246     | 634.4KB  | 619.8KB | 2.3%    | 620.6KB | 2.2%   | terse  | 46       | 6681    |

## tamp failure behavior (pre-segmentation reference)

these numbers are from the original 108-file benchmark (pre-segmentation, larger payloads). the segmented benchmark above has zero failures because all segments are under 1.5MB.

11 of 108 conversations failed:
- 7x 413 Payload Too Large (>2.5MB -- node http.createServer body limit)
- 4x 502 Bad Gateway (tokenizer took so long the connection dropped)

these are the LARGEST conversations -- the ones where compression matters most.

tamp processing time by payload size:

| payload size | time per conversation |
|-------------|----------------------|
| <100KB | <1s |
| 100-500KB | 1-6s |
| 500KB-1MB | 5-10s |
| 1-2MB | 15-27s |
| 2-5MB | 34-72s |
| 5-10MB | 59-117s |

root cause: `@anthropic-ai/tokenizer countTokens()` is O(n) at ~8s per 100KB, called twice per compressed block.

## cacheSafe is counterproductive

tamp defaults `cacheSafe=true`, which only compresses the newest user message (to "preserve prompt caching"). this is wrong:

1. **breaks caching**: cacheSafe=true changes whether a message is compressed between requests (compressed when newest, uncompressed when it becomes older). the prefix changes every turn, invalidating the cache.
2. **deterministic compression preserves caching**: cacheSafe=false compresses every message the same way every time. same input = same output. the compressed prefix is identical across requests, so the cache actually works.
3. **minimal cost**: one-time cache miss when first enabling compression. after that, caching works normally.

terse has no cacheSafe flag. it always compresses everything deterministically.

## methodology

- **date**: 2026-04-09
- **segmentation**: JSONL files split at `compact_boundary` markers. each segment treated as independent conversation.
- **minimum segment size**: 6 messages (smaller segments skipped as too trivial)
- **original bytes**: `serde_json::to_string` of full Anthropic API request body
- **compressed bytes**: `serde_json::to_string` after compression pipeline
- **tamp version**: v0.4.8, `TAMP_CACHE_SAFE=false`, `TAMP_UPSTREAM` pointed at Rust mock server on :7779
- **terse version**: v0.1.0, release build
- **token estimate**: bytes / 4 (standard approximation)
- **cost estimate**: tokens saved * $3/MTok (Sonnet input pricing)
- **estimated savings**: ~937K tokens, ~$2.81 at Sonnet $3/MTok across all 252 segments

### how to reproduce

```bash
# terse-only benchmark (all segments)
cargo run --release

# terse-only (N largest segments)
cargo run --release -- --limit 10

# head-to-head terse vs tamp (all segments)
# terminal 1: start tamp
TAMP_UPSTREAM=http://127.0.0.1:7779 TAMP_CACHE_SAFE=false \
  node tamp-bench/node_modules/@sliday/tamp/bin/tamp.js -y

# terminal 2: run benchmark
cargo run --release -- --bench
```
