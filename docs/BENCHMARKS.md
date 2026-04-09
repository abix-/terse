# benchmarks

all numbers from live head-to-head benchmark run 2026-04-09.

## test corpus

114 real Claude Code conversations from 9 projects (datacenter ops, ansible, C#, kubernetes, storage monitoring, MCP servers, infrastructure-as-code).

### compaction-aware segmentation

Claude Code compacts context at `compact_boundary` markers. each segment between compactions is what Claude actually sees in one API request. cross-target dedup/diff across compaction boundaries is unrealistic -- Claude never sees that data together.

- 114 JSONL files -> 249 segments (6 files had no valid segments)
- segments range from 3.8KB to 1.5MB
- each segment is an independent unit for compression benchmarking

**why this matters**: naively treating each JSONL file as one conversation inflates dedup/diff results by comparing targets that were never in the same API request.

## head-to-head: terse vs tamp (all 249 segments)

both tools configured identically:
- tamp: `TAMP_CACHE_SAFE=false` (compress all messages, not just the last one)
- terse: always compresses all messages (no cacheSafe flag)
- same input: reconstructed API request bodies from real conversations

### summary

| metric     | terse  | tamp             |
|------------|--------|------------------|
| segments   | 249    | 249 ok, 0 failed |
| original   | 102.4MB | 102.4MB         |
| compressed | 98.9MB | 98.9MB           |
| savings    | 3.4%   | 3.4%             |
| time       | **4.2s** | 907.2s         |

**terse is 216x faster** with identical compression on the full corpus. tamp's bottleneck is `@anthropic-ai/tokenizer countTokens()` -- a WASM tokenizer called twice per target to report "tokens saved." this is cosmetic: fewer bytes = fewer tokens, the relationship is ~linear (1 token ~= 4 bytes). terse skips tokenization entirely.

note: no tamp failures on segmented data because all segments are under 1.5MB (below tamp's ~2.5MB body limit). the original pre-segmentation benchmark had 11 failures on large files.

### what the 3.4% means

the 3.4% is measured on the **full API request body** (102.4MB), which includes model params, system prompt, and assistant messages that can't be compressed. the actual compressible content (tool_result text blocks) is 46.7MB.

| scope | original | compressed | savings |
|-------|----------|------------|---------|
| full request body | 102.4MB | 98.9MB | 3.4% |
| tool_result content only | 46.7MB | 43.3MB | 7.3% |

most of the request body (~55%) is non-compressible overhead.

### terse stage attribution (all 249 segments)

| stage          | targets | original | compressed | saved   | savings % |
|----------------|---------|----------|------------|---------|-----------|
| strip-lines    | 2146    | 11.3MB   | 9.6MB      | 1.7MB   | 14.9%     |
| dedup          | 313     | 670.2KB  | 19.2KB     | 651.0KB | 97.1%     |
| diff           | 254     | 846.2KB  | 233.7KB    | 612.5KB | 72.4%     |
| toon           | 135     | 580.3KB  | 375.0KB    | 205.3KB | 35.4%     |
| tabular        | 9       | 24.6KB   | 14.4KB     | 10.3KB  | 41.7%     |
| json-minify    | 10      | 23.9KB   | 22.4KB     | 1.5KB   | 6.3%      |
| whitespace     | 1       | 298B     | 173B       | 125B    | 41.9%     |
| unchanged      | 9565    | 32.1MB   | 32.1MB     | 0B      | 0.0%      |
| skipped (<200) | 14809   | 1.2MB    | 1.2MB      | 0B      | -         |

**strip-lines is the biggest saver** (1.7MB) -- removing line number prefixes from Claude's Read tool output. dedup and diff together save 1.2MB on repeated/similar content.

9565 eligible targets (32.1MB) are unchanged -- these are plain text (code, prose, command output) with no exploitable structure beyond what strip-lines already handles.

### per-target comparison by content type

| content type | targets | original | terse  | terse % | tamp   | tamp % | delta  |
|-------------|---------|----------|--------|---------|--------|--------|--------|
| Text         | 11981   | 43.8MB   | 40.9MB | 6.6%    | 41.0MB | 6.5%   | +0.1%  |
| Json         | 331     | 1.4MB    | 1.2MB  | 13.5%   | 1.3MB  | 7.2%   | **+6.4%** |
| Unknown      | 14809   | 1.2MB    | 1.2MB  | 0.0%    | 1.1MB  | 8.0%   | -8.0%  |
| JsonLined    | 38      | 147.5KB  | 85.1KB | 42.3%   | 71.7KB | 51.4%  | -9.1%  |
| Tabular      | 83      | 97.1KB   | 84.6KB | 12.9%   | 91.5KB | 5.7%   | **+7.2%** |

- **terse wins on Json (+6.4%)**: TOON encoding beats tamp's approach on the full corpus
- **terse wins on Tabular (+7.2%)**: columnar grouping has no equivalent in tamp
- **tamp wins on Unknown (-8.0%)**: tamp has no minimum size threshold, compresses sub-200-byte targets
- **tamp wins on JsonLined (-9.1%)**: tamp's TOON handles line-numbered JSON slightly better
- **Text is a wash (+0.1%)**: both tools do strip-lines and whitespace normalization

### content type distribution

| content type             | count  | % count | size    | % size |
|--------------------------|--------|---------|---------|--------|
| plain text               | 11971  | 44.0%   | 43.8MB  | 93.9%  |
| json                     | 331    | 1.2%    | 1.4MB   | 3.0%   |
| skipped (<200 chars)     | 14793  | 54.4%   | 1.2MB   | 2.6%   |
| json (with line numbers) | 38     | 0.1%    | 147.5KB | 0.3%   |
| tabular (CSV/TSV)        | 83     | 0.3%    | 97.1KB  | 0.2%   |
| **TOTAL**                | **27216** | **100%** | **46.7MB** | **100%** |

93.9% of compressible content is plain text. this is the ceiling -- if you can't compress plain text better, you can't move the overall number much.

### per-stage effectiveness (isolation)

each stage tested independently against all eligible targets:

| stage       | targets hit  | savings on affected | overall savings |
|-------------|-------------|---------------------|----------------|
| dedup       | 12423/12423 | 98.3%               | 98.3%          |
| strip-lines | 5471/12423  | 10.8%               | 7.1%           |
| whitespace  | 2381/12423  | 15.1%               | 4.0%           |
| diff        | 263/12423   | 72.7%               | 1.4%           |
| toon        | 153/12423   | 34.2%               | 0.5%           |
| tabular     | 9/12423     | 41.7%               | 0.0%           |

**important**: dedup in isolation shows 98.3% because it compares ALL targets across ALL 249 segments. in the real pipeline, dedup only runs within each segment (realistic API request scope), dropping to 313 hits / 651KB saved.

### per-segment savings distribution

| segment size | count | original | compressed | savings |
|-------------|-------|----------|------------|---------|
| <10 msgs    | 2     | 9.4KB    | 9.4KB      | 0.0%    |
| 10-30 msgs  | 17    | 329.0KB  | 320.6KB    | 2.6%    |
| 30-80 msgs  | 22    | 2.1MB    | 2.0MB      | 2.7%    |
| 80-200 msgs | 43    | 11.7MB   | 11.3MB     | 3.0%    |
| 200+ msgs   | 165   | 88.2MB   | 85.1MB     | 3.4%    |
| **TOTAL**   | **249** | **102.3MB** | **98.9MB** | **3.4%** |

savings increase with conversation length (more tool results = more opportunities for dedup/diff/strip-lines).

### per-project savings

| project                                     | segments | original | compressed | savings |
|---------------------------------------------|----------|----------|------------|---------|
| C--Code-DC-Automation                       | 5        | 1.5MB    | 1.3MB      | 12.6%   |
| C--code-claude-blueprints-dc                | 45       | 15.8MB   | 14.9MB     | 6.0%    |
| C--Code-blackdiamond-infrastructure-ansible | 42       | 14.3MB   | 13.4MB     | 5.9%    |
| C--Code-k3sc                                | 15       | 5.3MB    | 5.2MB      | 3.2%    |
| C--code-awx-ui                              | 2        | 469.8KB  | 456.0KB    | 2.9%    |
| C--Code-ssnc-purestorage                    | 37       | 16.0MB   | 15.6MB     | 2.2%    |
| C--Code                                     | 45       | 21.5MB   | 21.0MB     | 2.0%    |
| C--Code-bdw-infra-console                   | 55       | 27.0MB   | 26.5MB     | 1.8%    |
| C--Code-bdw-infra-console-csharp            | 3        | 441.6KB  | 435.5KB    | 1.4%    |

DC-Automation has the highest savings (12.6%) -- heavy InfluxDB query output with repeated column structure.

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
- **estimated savings**: ~858K tokens, ~$2.57 at Sonnet $3/MTok across all 249 segments

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
