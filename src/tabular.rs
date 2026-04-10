use crate::classify::detect_delimiter;
use std::collections::{BTreeMap, HashMap};

/// compress tabular data by factoring out low-cardinality columns
/// and shortening timestamps
pub fn compress_tabular(text: &str) -> Option<String> {
    let delim = detect_delimiter(text);
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 4 {
        return None; // need header + at least 3 data rows
    }

    let headers = parse_row(lines[0], delim);
    if headers.len() < 2 {
        return None;
    }

    // parse all data rows
    let mut rows: Vec<Vec<String>> = Vec::new();
    for line in &lines[1..] {
        if line.is_empty() {
            continue;
        }
        let fields = parse_row(line, delim);
        if fields.len() == headers.len() {
            rows.push(fields);
        }
    }

    if rows.len() < 3 {
        return None;
    }

    // analyze cardinality per column
    let num_rows = rows.len();
    let mut col_unique: Vec<HashMap<&str, usize>> = vec![HashMap::new(); headers.len()];
    for row in &rows {
        for (ci, val) in row.iter().enumerate() {
            *col_unique[ci].entry(val.as_str()).or_insert(0) += 1;
        }
    }

    // classify columns: low-cardinality (<= 10% of rows or <= 10 unique) = group-by
    // high-cardinality = data column
    let mut group_cols: Vec<usize> = Vec::new();
    let mut data_cols: Vec<usize> = Vec::new();

    for (ci, uniq) in col_unique.iter().enumerate() {
        let n_unique = uniq.len();
        // skip columns that are entirely empty or single-value with empty string
        if n_unique <= 1 && uniq.keys().next().map_or(true, |k| k.is_empty()) {
            continue;
        }
        let ratio = n_unique as f64 / num_rows as f64;
        if n_unique == 1 {
            group_cols.push(ci); // constant column -- always factor out
        } else if ratio <= 0.1 || (n_unique <= 10 && ratio < 0.4) {
            group_cols.push(ci);
        } else {
            data_cols.push(ci);
        }
    }

    // if no grouping possible, not worth compressing this way
    if group_cols.is_empty() || data_cols.is_empty() {
        return None;
    }

    // build output
    let mut out = String::new();

    // emit constant columns (cardinality = 1) as top-level key: value
    let mut non_const_group_cols: Vec<usize> = Vec::new();
    for &ci in &group_cols {
        if col_unique[ci].len() == 1 {
            let val = col_unique[ci].keys().next().unwrap();
            if !val.is_empty() {
                out.push_str(&headers[ci]);
                out.push_str(": ");
                out.push_str(val);
                out.push('\n');
            }
        } else {
            non_const_group_cols.push(ci);
        }
    }

    if !out.is_empty() {
        out.push('\n');
    }

    // group rows by the non-constant group columns
    let mut groups: BTreeMap<Vec<&str>, Vec<&Vec<String>>> = BTreeMap::new();
    for row in &rows {
        let key: Vec<&str> = non_const_group_cols.iter().map(|&ci| row[ci].as_str()).collect();
        groups.entry(key).or_default().push(row);
    }

    // detect if data columns contain timestamps and find common prefix
    let time_col_indices: Vec<usize> = data_cols
        .iter()
        .filter(|&&ci| is_timestamp_column(&headers[ci], &rows, ci))
        .copied()
        .collect();

    let time_prefixes: HashMap<usize, String> = time_col_indices
        .iter()
        .map(|&ci| {
            let prefix = find_common_prefix(&rows, ci);
            (ci, prefix)
        })
        .collect();

    // emit groups
    for (key, group_rows) in &groups {
        // group header
        let header_parts: Vec<String> = non_const_group_cols
            .iter()
            .zip(key.iter())
            .map(|(&ci, val)| format!("{}: {}", headers[ci], val))
            .collect();

        if !header_parts.is_empty() {
            out.push_str(&header_parts.join(", "));
        }

        out.push_str(&format!(" ({} rows):\n", group_rows.len()));

        // data column headers (only if >1 data column)
        if data_cols.len() > 1 {
            let col_names: Vec<&str> = data_cols.iter().map(|&ci| headers[ci].as_str()).collect();
            out.push_str("  ");
            out.push_str(&col_names.join(","));
            out.push('\n');
        }

        // data rows
        for row in group_rows {
            out.push_str("  ");
            let vals: Vec<String> = data_cols
                .iter()
                .map(|&ci| {
                    let val = &row[ci];
                    // shorten timestamps by removing common prefix
                    if let Some(prefix) = time_prefixes.get(&ci) {
                        if !prefix.is_empty() && val.starts_with(prefix.as_str()) {
                            return shorten_timestamp(val, prefix);
                        }
                    }
                    val.clone()
                })
                .collect();
            out.push_str(&vals.join(","));
            out.push('\n');
        }

        out.push('\n');
    }

    // trim trailing whitespace
    let out = out.trim_end().to_string();

    // only return if we actually saved space
    if out.len() < text.len() {
        Some(out)
    } else {
        None
    }
}

fn parse_row(line: &str, delim: char) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;

    for ch in line.chars() {
        if ch == '"' {
            in_quote = !in_quote;
        } else if ch == delim && !in_quote {
            fields.push(current.trim().to_string());
            current = String::new();
        } else {
            current.push(ch);
        }
    }
    fields.push(current.trim().to_string());
    fields
}

fn is_timestamp_column(header: &str, rows: &[Vec<String>], col: usize) -> bool {
    let h = header.to_lowercase();
    if h.contains("time") || h.contains("date") || h.contains("timestamp") {
        return true;
    }
    // check if values look like ISO timestamps
    let sample_count = rows.len().min(5);
    let matches = rows[..sample_count]
        .iter()
        .filter(|r| {
            let v = &r[col];
            v.contains('T') && v.contains(':') && (v.ends_with('Z') || v.contains('+'))
        })
        .count();
    matches >= sample_count / 2
}

fn find_common_prefix(rows: &[Vec<String>], col: usize) -> String {
    if rows.is_empty() {
        return String::new();
    }

    let first = &rows[0][col];
    let mut prefix_len = first.len();

    for row in &rows[1..] {
        let val = &row[col];
        prefix_len = prefix_len.min(val.len());
        for (i, (a, b)) in first.bytes().zip(val.bytes()).enumerate() {
            if a != b {
                prefix_len = prefix_len.min(i);
                break;
            }
        }
    }

    // don't break in the middle of a timestamp component -- snap to last separator
    let prefix = &first[..prefix_len];
    if let Some(pos) = prefix.rfind(|c: char| c == 'T' || c == '-' || c == ' ') {
        // include the separator in the prefix so the remainder is just the time part
        first[..=pos].to_string()
    } else {
        String::new()
    }
}

fn shorten_timestamp(val: &str, prefix: &str) -> String {
    let remainder = &val[prefix.len()..];
    // strip trailing Z or +00:00 timezone if all values have it
    let cleaned = remainder
        .trim_end_matches('Z')
        .trim_end_matches("+00:00")
        .trim_end_matches(":00"); // drop seconds if all :00
    cleaned.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_simple_csv() {
        let csv = "name,city,score\nalice,nyc,95\nbob,nyc,87\ncharlie,nyc,92\ndave,nyc,88\neve,nyc,91\nfrank,nyc,85\ngrace,nyc,97\nhank,nyc,79\niris,nyc,84\njack,nyc,90\n";
        let result = compress_tabular(csv);
        assert!(result.is_some());
        let out = result.unwrap();
        // city should be factored out since all rows have "nyc"
        assert!(out.contains("city: nyc"));
        assert!(!out.contains("nyc,"));
    }

    #[test]
    fn test_compress_influx_style() {
        let csv = ",result,table,_time,_value,_field,disk,host,vm\n,_result,0,2026-04-09T14:15:00Z,0,readIOPS,scsi0:0,esxi01,VM01\n,_result,0,2026-04-09T14:20:00Z,5,readIOPS,scsi0:0,esxi01,VM01\n,_result,0,2026-04-09T14:25:00Z,2,readIOPS,scsi0:0,esxi01,VM01\n,_result,0,2026-04-09T14:30:00Z,3,readIOPS,scsi0:0,esxi01,VM01\n,_result,0,2026-04-09T14:35:00Z,1,readIOPS,scsi0:0,esxi01,VM01\n,_result,0,2026-04-09T14:40:00Z,4,readIOPS,scsi0:0,esxi01,VM01\n,_result,1,2026-04-09T14:15:00Z,100,writeIOPS,scsi0:0,esxi01,VM01\n,_result,1,2026-04-09T14:20:00Z,150,writeIOPS,scsi0:0,esxi01,VM01\n,_result,1,2026-04-09T14:25:00Z,120,writeIOPS,scsi0:0,esxi01,VM01\n,_result,1,2026-04-09T14:30:00Z,110,writeIOPS,scsi0:0,esxi01,VM01\n,_result,1,2026-04-09T14:35:00Z,130,writeIOPS,scsi0:0,esxi01,VM01\n,_result,1,2026-04-09T14:40:00Z,140,writeIOPS,scsi0:0,esxi01,VM01\n";
        let result = compress_tabular(csv);
        assert!(result.is_some());
        let out = result.unwrap();
        assert!(out.contains("host: esxi01"));
        assert!(out.contains("vm: VM01"));
        // timestamps should be shortened
        assert!(out.contains("14:15") || out.contains("14:20"));
        assert!(out.len() < csv.len());
    }
}
