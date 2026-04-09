# tamp vs terse: architecture comparison

## tamp pipeline (from source: compress.js, providers.js, detect.js, index.js)

### request flow (index.js)

```
client POST /v1/messages
  -> read full body (up to maxBody, passthrough if larger)
  -> decompress if gzip/br/zstd
  -> JSON.parse body
  -> detectProvider(method, url) -> anthropic provider
  -> compressRequest(body, config, provider)
  -> JSON.stringify compressed body
  -> forward to upstream with updated content-length
  -> pipe response back to client
```

### compressRequest (compress.js:416-449)

this is the core pipeline. order matters:

```
1. targets = provider.extract(body)      // get tool_result content strings
2. deduplicateTargets(targets)           // cross-target: replace identical with back-ref
3. diffTargets(targets)                  // cross-target: replace similar with unified diff
4. for each target:
     if target.dedup or target.diffed -> skip (already compressed)
     compressBlock(target.text)         // per-block: classify + compress
5. provider.apply(body, targets)         // write compressed text back into body
```

KEY: dedup and diff run FIRST, before per-block compression.
this is intentional -- if two targets are identical, dedup catches it before
wasting time minifying both. diff likewise runs on original text.

### provider.extract (providers.js:22-65)

anthropic provider extracts targets from the messages array:

```
for each message in body.messages:
  if message.role != "user": skip
  for each block in message.content:
    if block.type != "tool_result": skip
    if block.is_error: skip (record as skipped)
    extract block.content as target with path ["messages", mi, "content", bi, "content"]
```

cacheSafe mode (providers.js:58-61):
- true (default): findLatestEligibleGroup() -- walks messages backwards,
  returns targets from the LAST user message that has non-skipped tool_results
- false: flatMap all messages -> extract all tool_results

### deduplicateTargets (compress.js:26-39)

```
seen = Map()
for each target (in order):
  if target.skip: continue
  key = SHA-256(text) if text.length >= 128, else text itself
  if seen.has(key) AND seen.get(key).text === text:
    target.compressed = "[see tool_result in message {mi}, block {bi} -- identical content]"
    target.dedup = true
  else:
    seen.set(key, target)
```

scope: within ONE request (one conversation's API call).
not across conversations.

### diffTargets (compress.js:43-67)

```
seen = []
for each target (in order):
  if target.skip or target.dedup or target.compressed: continue
  if target.text.length < 200: push to seen, continue
  for each prev in seen:
    sim = quickSimilarity(prev.text, target.text)  // jaccard on line sets
    if sim > 0.5 and sim < 1.0:
      patch = createPatch('file', prev.text, target.text, context=1)
      strip first 4 lines (header) to get diffBody
      if diffBody.length < target.text.length * 0.5:
        target.compressed = "[diff from tool_result in message {mi}, block {bi}]:\n{diffBody}"
        target.diffed = true
        break
  push target to seen
```

scope: within ONE request. compares each target against ALL previous targets
in that same request (not just last 50).

quickSimilarity (compress.js:69-76):
- early exit if length difference > 50%
- jaccard = |intersection(lines_a, lines_b)| / |union(lines_a, lines_b)|

### compressBlock -> compressText (compress.js:212-279)

runs per-target, only if not already dedup'd/diff'd:

```
1. if text.length < config.minSize (200): skip
2. classify = classifyContent(text)  // detect.js
3. if classify == "toon": skip (already compressed)

4. if classify == "text":
     a. strip-lines stage: stripLineNumbers(text) if in stages
     b. whitespace stage: normalizeWhitespace(text) if in stages
     c. strip-comments stage: stripComments(text) if in stages (opt-in)
     d. if result < 90% of original: return {text, method: "normalize"}
     e. else: try llmlingua/textpress/foundation-models (external, opt-in)
     f. else: skip

5. if classify == "json" or "json-lined":
     a. if json-lined: stripLineNumbers first
     b. prune stage: remove npm metadata keys, re-serialize
     c. minify: JSON.stringify(parsed) with no whitespace
     d. if minified < original: best = {text: minified, method: "minify"}
     e. toon stage: encode(parsed) via @toon-format/toon
     f. if toon < best: best = {text: toon, method: "toon"}
     g. return best

6. countTokens() called on both original and compressed (THE BOTTLENECK)
```

### classifyContent (detect.js:33-43)

```
1. isTOON(text): first line starts with [TOON] or matches \w+\[\d+\]{ or \w+\[\d+\]:
2. tryParseJSON(text): JSON.parse succeeds -> "json"
3. stripLineNumbers(text) then tryParseJSON -> "json-lined"
4. text.length > 0 -> "text"
5. else -> "unknown"
```

### provider.apply (providers.js:1-9)

walks each target's path and sets the compressed text back into the body object.

---

## terse pipeline (current state)

### compress_targets (compress.rs)

```
1. for each target:                      // per-block FIRST
     classify -> compress based on type
2. dedup (only targets not already compressed)
3. diff (only targets not already compressed)
```

### BUG: order is wrong

tamp: dedup -> diff -> per-block
terse: per-block -> dedup -> diff

this means:
- terse never deduplicates two identical targets that both got per-block compressed
  (they both have .compressed set, so dedup skips them)
- terse wastes time compressing targets that would have been dedup'd away
- diff in terse only sees uncompressed remainders, missing comparisons

FIX: match tamp's order.

### differences from tamp

| feature | tamp | terse | match? |
|---------|------|-------|--------|
| pipeline order | dedup -> diff -> per-block | per-block -> dedup -> diff | NO - WRONG |
| dedup scope | per-request (per-conversation) | per-request | yes |
| diff scope | per-request, all prev targets | per-request, last 50 targets | NO - CAPPED |
| diff O(n^2) | yes, unbounded | should be too -- it's per-conversation | need to fix |
| classify: tabular | no (not in tamp) | yes (NEW) | n/a -- new feature |
| classify: toon detect | yes | yes | yes |
| classify: json | yes | yes | yes |
| classify: json-lined | yes | yes | yes |
| classify: text | yes | yes | yes |
| minSize threshold | 200 chars | 200 chars | yes |
| strip-lines | yes | yes | yes |
| whitespace normalize | yes | yes | yes |
| json minify | yes | yes | yes |
| json prune (npm keys) | yes | yes | yes |
| toon encode | yes (@toon-format/toon) | no (skipped) | NO |
| strip-comments | yes (opt-in) | no (not in defaults) | ok (opt-in) |
| llmlingua/textpress | yes (opt-in, external) | no | ok (external) |
| tokenizer | countTokens() per block | chars/4 estimate | INTENTIONAL |
| cacheSafe extract | findLatestEligibleGroup | walks backwards | needs verify |
| text 90% threshold | skip if result >= 90% original | skip if result >= 90% original | yes |
| json: skip if >= original | yes | yes (95% threshold) | DIFFERENT (95 vs 100) |
| cross-convo state | none (per-request) | none (per-request) | yes |

### what terse adds that tamp doesn't have

1. **tabular compression**: CSV/TSV columnar grouping (THE BIG WIN)
   - tamp has no concept of tabular data
   - terse detects CSV headers, analyzes column cardinality
   - factors out low-cardinality columns, shortens timestamps
   - 89.8% savings on InfluxDB data in prototype

2. **json uniform array flattening**: array-of-objects -> CSV-style
   - tamp uses TOON for this (different format, similar idea)
   - terse converts to plain CSV header+rows

### per-stage benchmark approach

the benchmark must:
1. extract all targets from a conversation (one conversation = one API call)
2. run EACH stage in isolation on those targets and measure savings
3. run the FULL pipeline (tamp order: dedup -> diff -> per-block) and measure
4. attribute savings to the stage that compressed each target
5. report per-stage, per-content-type, per-conversation-size

critical: diff and dedup are per-conversation, not cross-conversation.
the benchmark must NOT pool all targets from all conversations together.
