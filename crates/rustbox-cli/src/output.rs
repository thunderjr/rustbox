use serde::Serialize;

pub fn format_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    if rows.is_empty() {
        return headers.join("\t");
    }

    let mut col_widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < col_widths.len() {
                col_widths[i] = col_widths[i].max(cell.len());
            }
        }
    }

    let mut output = String::new();

    // Header
    let header_line: Vec<String> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| format!("{:<width$}", h, width = col_widths[i]))
        .collect();
    output.push_str(&header_line.join("  "));
    output.push('\n');

    // Rows
    for row in rows {
        let row_line: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let width = col_widths.get(i).copied().unwrap_or(cell.len());
                format!("{:<width$}", cell, width = width)
            })
            .collect();
        output.push_str(&row_line.join("  "));
        output.push('\n');
    }

    output
}

pub fn format_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_table_with_data() {
        let headers = &["ID", "STATUS", "RUNTIME"];
        let rows = vec![
            vec!["abc123".into(), "running".into(), "node24".into()],
            vec!["def456".into(), "stopped".into(), "python313".into()],
        ];
        let output = format_table(headers, &rows);
        assert!(output.contains("ID"));
        assert!(output.contains("abc123"));
        assert!(output.contains("def456"));
        assert!(output.contains("running"));
    }

    #[test]
    fn format_table_empty() {
        let headers = &["ID", "STATUS"];
        let rows: Vec<Vec<String>> = vec![];
        let output = format_table(headers, &rows);
        assert!(output.contains("ID"));
    }

    #[test]
    fn format_json_valid() {
        let data = serde_json::json!({"id": "abc123", "status": "running"});
        let output = format_json(&data);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["id"], "abc123");
    }
}
