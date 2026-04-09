# benchmarks -- real-world measurements

## test corpus

114 real Claude Code conversations from 9 projects (datacenter ops, ansible, C#, kubernetes, storage monitoring).

### compaction-aware segmentation

Claude Code compacts context at `compact_boundary` markers. each segment between compactions is what Claude actually sees in one API request. cross-target dedup/diff across compaction boundaries is unrealistic -- Claude never sees that data together.

- 114 JSONL files -> 245 segments (6 files had no valid segments)
- segments range from 3.8KB to 1.5MB
- each segment is an independent unit for compression benchmarking

**why this matters**: naively treating each JSONL file as one conversation inflates dedup/diff results by comparing targets that were never in the same API request. compaction-aware segmentation gives realistic numbers.

## head-to-head: terse vs tamp (top 10 segments)

both tools configured identically:
- tamp: `TAMP_CACHE_SAFE=false` (compress all messages, not just the last one)
- terse: always compresses all messages (no cacheSafe flag)
- same input: reconstructed API request bodies from real conversations

### summary

| metric     | terse | tamp            |
|------------|-------|-----------------|
| segments   | 10    | 10 ok, 0 failed |
| original   | 7.4MB | 7.4MB           |
| compressed | 7.2MB | 7.2MB           |
| savings    | 3.6%  | 3.5%            |
| time       | 0.4s  | 67.9s           |

**terse is 170x faster** with equivalent compression. tamp's bottleneck is `@anthropic-ai/tokenizer countTokens()` -- a WASM tokenizer called twice per target to report "tokens saved." this is cosmetic: fewer bytes = fewer tokens, the relationship is ~linear (1 token ~= 4 bytes). terse skips tokenization entirely.

### per-segment overview

| #  | msgs | targets | original | terse   | terse % | tamp    | tamp % | winner | terse ms | tamp ms |
|----|------|---------|----------|---------|---------|---------|--------|--------|----------|---------|
| 1  | 1410 | 552     | 1.5MB    | 1.4MB   | 1.7%    | 1.4MB   | 1.0%   | terse  | 139      | 17599   |
| 2  | 836  | 329     | 708.6KB  | 662.4KB | 6.5%    | 659.2KB | 7.0%   | tamp   | 42       | 8348    |
| 3  | 348  | 129     | 695.7KB  | 692.4KB | 0.5%    | 693.8KB | 0.3%   | terse  | 24       | 1309    |
| 4  | 860  | 339     | 679.8KB  | 649.1KB | 4.5%    | 659.9KB | 2.9%   | terse  | 21       | 11276   |
| 5  | 501  | 206     | 665.7KB  | 636.4KB | 4.4%    | 633.3KB | 4.9%   | tamp   | 37       | 4142    |
| 6  | 315  | 108     | 665.1KB  | 652.6KB | 1.9%    | 637.6KB | 4.1%   | tamp   | 21       | 2495    |
| 7  | 442  | 170     | 653.8KB  | 630.6KB | 3.5%    | 628.3KB | 3.9%   | tamp   | 23       | 4072    |
| 8  | 502  | 189     | 650.5KB  | 578.1KB | 11.1%   | 579.3KB | 11.0%  | terse  | 39       | 4748    |
| 9  | 421  | 171     | 634.5KB  | 630.9KB | 0.6%    | 626.0KB | 1.3%   | tamp   | 24       | 7077    |
| 10 | 640  | 246     | 634.4KB  | 610.9KB | 3.7%    | 620.6KB | 2.2%   | terse  | 44       | 6814    |

terse wins 5/10 segments, tamp wins 5/10. compression quality is equivalent -- the difference is speed.

### terse stage attribution (all 10 segments)

| stage          | targets | original | compressed | saved   | savings % |
|----------------|---------|----------|------------|---------|-----------|
| diff           | 37      | 137.1KB  | 35.9KB     | 101.2KB | 73.8%     |
| strip-lines    | 95      | 308.9KB  | 243.1KB    | 65.8KB  | 21.3%     |
| dedup          | 34      | 53.9KB   | 2.1KB      | 51.8KB  | 96.1%     |
| tabular        | 16      | 78.5KB   | 39.3KB     | 39.1KB  | 49.9%     |
| json-minify    | 3       | 6.1KB    | 4.5KB      | 1.6KB   | 26.4%     |
| unchanged      | 900     | 2.1MB    | 2.1MB      | 0B      | 0.0%      |
| skipped (<200) | 1354    | 104.7KB  | 104.7KB    | 0B      | -         |

**key insight**: 900 eligible targets (2.1MB) are unchanged. diff and dedup are the biggest savers by absolute bytes. tabular only hits 16/361 tabular targets (4.4%) -- massive untapped potential.

### per-target comparison by content type

| content type | targets | original | terse   | terse % | tamp   | tamp % | delta  |
|-------------|---------|----------|---------|---------|--------|--------|--------|
| Tabular      | 361     | 1.4MB    | 1.3MB   | 6.5%    | 1.4MB  | 5.5%   | +1.0%  |
| Text         | 720     | 1.2MB    | 1.1MB   | 13.3%   | 1.1MB  | 13.0%  | +0.3%  |
| Unknown      | 1354    | 104.7KB  | 104.7KB | 0.0%    | 99.0KB | 5.5%   | -5.5%  |
| Json         | 4       | 12.9KB   | 11.3KB  | 12.4%   | 10.9KB | 15.0%  | -2.6%  |

terse beats tamp on Tabular (+1.0%) and Text (+0.3%). tamp beats terse on Unknown (-5.5%, tamp compresses sub-200-byte targets) and Json (-2.6%, tamp's TOON encoding).

### where tamp wins and why

1. **TOON encoding** (json): tamp converts JSON arrays to TOON format (tabular encoding for uniform arrays). terse only minifies. small advantage on the 4 JSON targets.
2. **sub-200-byte targets**: tamp has no minimum size threshold (or lower). terse skips anything <200 bytes. 1,354 targets skipped = 104.7KB untouched.
3. **whitespace on small targets**: tamp's whitespace/strip-lines runs on smaller targets that terse classifies as "too small."

### where terse wins and why

1. **tabular compression**: terse's columnar grouping (factor out low-cardinality columns) has no equivalent in tamp. 16 targets at 49.9% savings.
2. **speed**: 170x faster. no tokenizer overhead. pure rust, no WASM, no GC.
3. **reliability**: processes any payload size. tamp fails on >2.5MB (node body limit) and drops connections on large payloads (tokenizer timeout).

## tamp baseline (full corpus, pre-segmentation)

these numbers are from the original 108-file benchmarking before compaction-aware segmentation. included for reference but less realistic than the segmented results above.

### cacheSafe=true (default mode)

| convo size | count | original | compressed | savings |
|-----------|-------|----------|-----------|---------|
| <10 msgs | 2 | 9.4KB | 9.4KB | 0.0% |
| 10-30 msgs | 14 | 255.7KB | 255.7KB | 0.0% |
| 30-80 msgs | 17 | 1.1MB | 1.1MB | 0.2% |
| 80-200 msgs | 19 | 3.0MB | 3.0MB | 0.0% |
| 200+ msgs | 56 | 92.9MB | 92.9MB | 0.0% |
| **TOTAL** | **108** | **97.3MB** | **97.3MB** | **0.0%** |

effectively useless in default mode. cacheSafe=true only compresses the LAST user message with tool_results -- everything else is untouched.

### cacheSafe=false (full scan)

| convo size | count | original | compressed | savings |
|-----------|-------|----------|-----------|---------|
| <10 msgs | 2 | 9.4KB | 9.4KB | 0.0% |
| 10-30 msgs | 14 | 255.7KB | 248.2KB | 2.9% |
| 30-80 msgs | 17 | 1.1MB | 1.1MB | 3.6% |
| 80-200 msgs | 19 | 3.0MB | 2.9MB | 3.5% |
| 200+ msgs | 56 | 92.9MB | 90.4MB | 2.7% |
| **TOTAL** | **108** | **97.3MB** | **94.7MB** | **2.7%** |

### tamp failures

11 of 108 conversations failed:
- 7x 413 Payload Too Large (>2.5MB -- node http.createServer body limit)
- 4x 502 Bad Gateway (tokenizer took so long the connection dropped)

these are the LARGEST conversations -- the ones where compression matters most.

### tamp processing time

| payload size | time per conversation |
|-------------|----------------------|
| <100KB | <1s |
| 100-500KB | 1-6s |
| 500KB-1MB | 5-10s |
| 1-2MB | 15-27s |
| 2-5MB | 34-72s |
| 5-10MB | 59-117s |

root cause: `@anthropic-ai/tokenizer countTokens()` is O(n) at ~8s per 100KB, called twice per compressed block. purely cosmetic -- it measures tokens saved but doesn't affect compression. terse skips it entirely.

## cacheSafe is counterproductive

tamp defaults `cacheSafe=true`, which only compresses the newest user message (to "preserve prompt caching"). this is wrong:

1. **breaks caching**: cacheSafe=true changes whether a message is compressed between requests (compressed when newest, uncompressed when it becomes older). the prefix changes every turn, invalidating the cache.
2. **deterministic compression preserves caching**: cacheSafe=false compresses every message the same way every time. same input = same output. the compressed prefix is identical across requests, so the cache actually works.
3. **minimal cost**: one-time cache miss when first enabling compression. after that, caching works normally.

terse has no cacheSafe flag. it always compresses everything deterministically.

## optimization targets

based on the detailed analysis:

1. **tabular compression coverage**: only 16/361 tabular targets (4.4%) are compressed. the 345 missed targets represent ~1.3MB of potential savings. improving the tabular classifier/compressor is the single biggest opportunity.
2. **minimum size threshold**: 1,354 targets (104.7KB) skipped at <200 bytes. lowering the threshold or removing it could recover ~5KB (matching tamp's behavior on these).
3. **text targets**: 900 eligible targets (2.1MB) are "unchanged" after all stages. many of these are text that doesn't trigger strip-lines or whitespace normalization. investigating what content they contain could reveal new compression opportunities.

## methodology

- **segmentation**: JSONL files split at `compact_boundary` markers. each segment treated as independent conversation.
- **minimum segment size**: 6 messages (smaller segments skipped as too trivial)
- **original bytes**: `JSON.stringify` of full Anthropic API request body
- **compressed bytes**: `JSON.stringify` after compression pipeline
- **tamp comparison**: real tamp v0.4.8 proxy with `TAMP_CACHE_SAFE=false`, `TAMP_UPSTREAM` pointed at Rust mock server on :7779 to capture forwarded body
- **token estimate**: bytes / 4 (standard approximation, avoids tokenizer overhead)
- **cost estimate**: tokens saved * $3/MTok (Sonnet input pricing)

### how to run

```bash
# terse-only benchmark (all segments)
cargo run --release

# terse-only (N largest segments)
cargo run --release -- --limit 10

# head-to-head terse vs tamp
# terminal 1: start tamp with mock upstream
TAMP_UPSTREAM=http://127.0.0.1:7779 TAMP_CACHE_SAFE=false \
  node tamp-bench/node_modules/@sliday/tamp/bin/tamp.js -y

# terminal 2: run benchmark
cargo run --release -- --bench --limit 10
```
