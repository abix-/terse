# tamp benchmarks -- real-world measurements

## test corpus

108 real Claude Code conversations from 9 projects (datacenter ops, ansible, C#, kubernetes, storage monitoring).

| project | convos | tool results | payload size |
|---------|--------|-------------|-------------|
| C--Code | 10 | 5,225 | 21.5MB |
| C--Code-bdw-infra-console | 11 | 4,914 | 24.4MB |
| C--Code-blackdiamond-infrastructure-ansible | 26 | 3,805 | 14.3MB |
| C--Code-ssnc-purestorage | 14 | 3,387 | 13.7MB |
| C--code-claude-blueprints-dc | 31 | 3,687 | 15.8MB |
| C--Code-k3sc | 7 | 1,583 | 5.3MB |
| C--Code-DC-Automation | 4 | 420 | 1.5MB |
| C--code-awx-ui | 2 | 86 | 469.8KB |
| C--Code-bdw-infra-console-csharp | 3 | 163 | 441.6KB |
| **TOTAL** | **108** | **23,270** | **97.3MB** |

payload size range: 3.8KB to 9.9MB.
56 of 108 conversations have 200+ messages (92.9MB -- 95.5% of total data).

## tamp (node.js) results

tested via rust harness that sends reconstructed conversations through tamp proxy.

### cacheSafe=true (default mode)

| convo size | count | original | compressed | savings |
|-----------|-------|----------|-----------|---------|
| <10 msgs | 2 | 9.4KB | 9.4KB | 0.0% |
| 10-30 msgs | 14 | 255.7KB | 255.7KB | 0.0% |
| 30-80 msgs | 17 | 1.1MB | 1.1MB | 0.2% |
| 80-200 msgs | 19 | 3.0MB | 3.0MB | 0.0% |
| 200+ msgs | 56 | 92.9MB | 92.9MB | 0.0% |
| **TOTAL** | **108** | **97.3MB** | **97.3MB** | **0.0%** |

effectively useless in default mode. only compressed 3 blocks total (587 tokens).

### cacheSafe=false (full scan)

| convo size | count | original | compressed | savings |
|-----------|-------|----------|-----------|---------|
| <10 msgs | 2 | 9.4KB | 9.4KB | 0.0% |
| 10-30 msgs | 14 | 255.7KB | 248.2KB | 2.9% |
| 30-80 msgs | 17 | 1.1MB | 1.1MB | 3.6% |
| 80-200 msgs | 19 | 3.0MB | 2.9MB | 3.5% |
| 200+ msgs | 56 | 92.9MB | 90.4MB | 2.7% |
| **TOTAL** | **108** | **97.3MB** | **94.7MB** | **2.7%** |

tamp self-reported 38.2% on 9,823 compressed blocks (2,022,805 tokens saved).
the 38.2% is on the blocks it touched, not the full payload. actual end-to-end: 2.7%.

### failures

11 of 108 conversations failed:
- 7x 413 Payload Too Large (>2.5MB -- node http.createServer body limit)
- 4x 502 Bad Gateway (tokenizer took so long the connection dropped)

these are the LARGEST conversations -- the ones where compression matters most.

### processing time (cacheSafe=false)

| payload size | time per conversation |
|-------------|----------------------|
| <100KB | <1s |
| 100-500KB | 1-6s |
| 500KB-1MB | 5-10s |
| 1-2MB | 15-27s |
| 2-5MB | 34-72s |
| 5-10MB | 59-117s |

root cause: `@anthropic-ai/tokenizer countTokens()` is O(n) at ~8s per 100KB, called 6+ times per compressed block. a 2MB conversation with 50 tool_results = 300+ tokenizer calls.

## what tamp-rust should achieve

target: process all 108 conversations in <10 seconds total (vs tamp's ~15 minutes + 11 failures).

expected savings should be HIGHER than tamp's 2.7% because:
1. we can actually process the >2.5MB conversations that tamp drops
2. no tokenizer bottleneck means we can run all stages on all content
3. same dedup/diff logic should produce identical or better compression

## methodology notes

- "original bytes" = JSON.stringify of the full anthropic API request body
- "compressed bytes" = JSON.stringify after compression pipeline runs
- token estimate = bytes / 4 (standard approximation)
- cost estimate = tokens saved * $3/MTok (sonnet input pricing)
- conversations with <6 messages skipped (too small to be meaningful)
- payloads >50MB skipped (synthetic, not real)
