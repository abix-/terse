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

    // check tabular (CSV/TSV with header + data rows)
    if is_tabular(text) {
        return ContentType::Tabular;
    }

    // check JSON
    if serde_json::from_str::<serde_json::Value>(text).is_ok() {
        return ContentType::Json;
    }

    // check JSON with line numbers
    let stripped = strip_line_numbers(text);
    if stripped != text && serde_json::from_str::<serde_json::Value>(&stripped).is_ok() {
        return ContentType::JsonLined;
    }

    ContentType::Text
}

/// detect CSV/TSV: header row with delimiter, consistent column count across rows
fn is_tabular(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().take(10).collect();
    if lines.len() < 3 {
        return false;
    }

    // try comma first, then tab
    for delim in [',', '\t'] {
        let header_cols = count_fields(lines[0], delim);
        if header_cols < 2 {
            continue;
        }

        // check that at least 3 of the first data rows have the same column count
        let mut matching = 0;
        for line in &lines[1..] {
            if line.is_empty() {
                continue;
            }
            if count_fields(line, delim) == header_cols {
                matching += 1;
            }
        }
        if matching >= 2 {
            return true;
        }
    }

    false
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
        let csv = ",result,table,_time,_value,_field\n,_result,0,2026-04-09T14:15:00Z,0,readLatency\n,_result,0,2026-04-09T14:20:00Z,1,readLatency\n";
        assert_eq!(classify(csv), ContentType::Tabular);
    }

    #[test]
    fn test_strip_line_numbers() {
        let text = "     1\tfn main() {\n     2\t    println!(\"hello\");\n     3\t}\n";
        let stripped = strip_line_numbers(text);
        assert_eq!(stripped, "fn main() {\n    println!(\"hello\");\n}\n");
    }
}
