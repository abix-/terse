# terse

Lossless context compression for LLM API requests. Reduces token count in tool_result content without losing any data.

## Problem

LLM API calls include the full conversation history. Tool results -- file reads, query outputs, API responses -- are the bulk of that payload. This data is full of redundancy: repeated column values, verbose JSON keys, duplicate content across tool calls. All of it inflates token count without adding information.

## Approach

Content-aware text-to-text re-encoding. Detect structure in tool_result content, then re-encode it into a denser representation that preserves all information. Different strategy per content type:

| Content Type | Strategy | Example Savings |
|---|---|---|
| Tabular (CSV/TSV) | Columnar grouping -- factor out low-cardinality columns, shorten timestamps | 89.8% on InfluxDB data |
| JSON arrays | Uniform array flattening to CSV-style header+rows | ~40% on uniform arrays |
| JSON objects | Minify + prune known-junk keys (npm metadata) | ~20% on verbose JSON |
| Plain text | Strip line numbers, normalize whitespace | ~10% on code/logs |
| Cross-target | Dedup identical content, diff similar content | varies |

Output is always valid, human-readable text. No binary encoding, no external dependencies at runtime.

## Usage

Currently a benchmark tool that measures compression effectiveness on real Claude Code conversation data:

```bash
cargo run --release              # all conversations
cargo run --release -- --limit 3 # smoke test
```

## Credits

Inspired by [tamp](https://github.com/AnthroPressure/tamp), a Node.js compression proxy for Anthropic API requests. terse started as an effort to measure and improve on tamp's approach, adding content-aware compression stages (particularly tabular data) that tamp doesn't have.

## License

GPL-3.0. See [LICENSE](LICENSE).
