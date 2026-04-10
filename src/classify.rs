use regex::Regex;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ContentType {
    Tabular,   // CSV, TSV, InfluxDB output
    Json,      // valid JSON
    JsonLined, // JSON with line number prefixes
    Text,      // plain text (code, command output)
    Unknown,   // empty or unclassifiable
}

static LINE_NUM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^ *\d+[\t\u{2192}]").unwrap());

static TOON_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\w+\[\d+\][{:]").unwrap());

pub fn classify(text: &str) -> ContentType {
    if text.is_empty() {
        return ContentType::Unknown;
    }

    // check TOON first -- already compressed, skip
    let first_line = text.trim_start().lines().next().unwrap_or("");
    if first_line.starts_with("[TOON]") || TOON_RE.is_match(first_line) {
        return ContentType::Unknown; // treat as already compressed
    }

    // strip line numbers BEFORE tabular check -- line-numbered code has tabs
    // that look like tabular columns (e.g. "  1\tfn main()" = 2-col TSV)
    let stripped = strip_line_numbers(text);
    let has_line_numbers = stripped.len() < text.len();
    let effective = if has_line_numbers { &stripped } else { text };

    // check tabular (CSV/TSV with header + data rows) on stripped content
    if is_tabular(effective) {
        return ContentType::Tabular;
    }

    // check JSON (on original text first, then stripped)
    if serde_json::from_str::<serde_json::Value>(text).is_ok() {
        return ContentType::Json;
    }

    if has_line_numbers && serde_json::from_str::<serde_json::Value>(&stripped).is_ok() {
        return ContentType::JsonLined;
    }

    ContentType::Text
}

/// detect CSV/TSV: header row with delimiter, consistent column count across rows.
/// rejects grep-like output (path:line:code) which has colons but isn't tabular.
pub fn is_tabular(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().take(20).collect();
    if lines.len() < 3 {
        return false;
    }

    // reject grep-style output: "path:number:code" -- colons aren't real delimiters
    if looks_like_grep(lines.as_slice()) {
        return false;
    }

    // try comma first, then tab
    for delim in [',', '\t'] {
        let header_cols = count_fields(lines[0], delim);
        if header_cols < 2 {
            continue;
        }

        // header row should look like column names, not code
        if !looks_like_header(lines[0], delim) {
            continue;
        }

        // check that most data rows have the same column count (>= 70%)
        let non_empty: Vec<&&str> = lines[1..].iter().filter(|l| !l.is_empty()).collect();
        let matching = non_empty.iter().filter(|l| count_fields(l, delim) == header_cols).count();
        if non_empty.len() >= 3 && matching * 10 >= non_empty.len() * 7 {
            return true;
        }
    }

    false
}

/// check if a line looks like a CSV/TSV header (short column names, no code syntax)
fn looks_like_header(line: &str, delim: char) -> bool {
    let fields: Vec<&str> = split_fields(line, delim);
    if fields.len() < 2 {
        return false;
    }
    // real headers: short identifiers, not code. reject if fields contain
    // code-like patterns (parens, braces, semicolons, long strings)
    let mut header_like = 0;
    for f in &fields {
        let f = f.trim().trim_matches('"');
        // header fields are typically short identifiers or empty
        if f.len() <= 30 && !f.contains('(') && !f.contains('{') && !f.contains(';') {
            header_like += 1;
        }
    }
    // majority of fields should look like headers
    header_like * 2 > fields.len()
}

fn split_fields<'a>(line: &'a str, delim: char) -> Vec<&'a str> {
    let mut fields = Vec::new();
    let mut start = 0;
    let mut in_quote = false;
    let bytes = line.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'"' {
            in_quote = !in_quote;
        } else if bytes[i] == delim as u8 && !in_quote {
            fields.push(&line[start..i]);
            start = i + 1;
        }
    }
    fields.push(&line[start..]);
    fields
}

static GREP_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z].*:\d+:").unwrap());

/// detect grep-like output: lines matching "filepath:lineno:content"
fn looks_like_grep(lines: &[&str]) -> bool {
    let non_empty: Vec<&&str> = lines.iter().filter(|l| !l.is_empty()).collect();
    if non_empty.len() < 3 {
        return false;
    }
    let matches = non_empty.iter().filter(|l| GREP_LINE_RE.is_match(l)).count();
    // if majority of lines look like grep output, it's grep
    matches * 2 > non_empty.len()
}

/// count fields respecting basic quoting
fn count_fields(line: &str, delim: char) -> usize {
    let mut count = 1;
    let mut in_quote = false;
    for ch in line.chars() {
        if ch == '"' {
            in_quote = !in_quote;
        } else if ch == delim && !in_quote {
            count += 1;
        }
    }
    count
}

/// strip line number prefixes (e.g. "  42\tcode here" -> "code here")
pub fn strip_line_numbers(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 2 {
        return text.to_string();
    }

    // check first 5 non-empty lines
    let mut matches = 0;
    for line in lines.iter().take(5) {
        if line.is_empty() {
            continue;
        }
        if LINE_NUM_RE.is_match(line) {
            matches += 1;
        }
    }

    if matches < 2 {
        return text.to_string();
    }

    lines
        .iter()
        .map(|l| LINE_NUM_RE.replace(l, "").to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// detect the delimiter used in tabular data
pub fn detect_delimiter(text: &str) -> char {
    let first_line = text.lines().next().unwrap_or("");
    let commas = first_line.matches(',').count();
    let tabs = first_line.matches('\t').count();
    if tabs > commas { '\t' } else { ',' }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_csv() {
        let csv = "name,age,city\nalice,30,nyc\nbob,25,sf\ncharlie,35,la\n";
        assert_eq!(classify(csv), ContentType::Tabular);
    }

    #[test]
    fn test_classify_json() {
        let json = r#"{"name":"test","value":42}"#;
        assert_eq!(classify(json), ContentType::Json);
    }

    #[test]
    fn test_classify_text() {
        let text = "this is just some plain text\nwith multiple lines\nnothing structured";
        assert_eq!(classify(text), ContentType::Text);
    }

    #[test]
    fn test_classify_influx_csv() {
        let csv = ",result,table,_time,_value,_field\n,_result,0,2026-04-09T14:15:00Z,0,readLatency\n,_result,0,2026-04-09T14:20:00Z,1,readLatency\n,_result,0,2026-04-09T14:25:00Z,2,readLatency\n,_result,0,2026-04-09T14:30:00Z,3,readLatency\n";
        assert_eq!(classify(csv), ContentType::Tabular);
    }

    #[test]
    fn test_strip_line_numbers() {
        let text = "     1\tfn main() {\n     2\t    println!(\"hello\");\n     3\t}";
        let stripped = strip_line_numbers(text);
        assert_eq!(stripped, "fn main() {\n    println!(\"hello\");\n}");
    }
}
