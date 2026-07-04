use std::collections::BTreeMap;

use color_eyre::eyre::{Result, eyre};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldValue {
    Scalar(String),
    List(Vec<String>),
}

pub type Frontmatter = BTreeMap<String, FieldValue>;

pub fn split_document(raw: &str) -> Result<(Frontmatter, String)> {
    let mut lines = raw.lines();
    if lines.next() != Some("---") {
        return Err(eyre!("missing opening frontmatter delimiter"));
    }

    let mut frontmatter_lines = Vec::new();
    let mut body_lines = Vec::new();
    let mut in_body = false;

    for line in lines {
        if !in_body && line == "---" {
            in_body = true;
            continue;
        }

        if in_body {
            body_lines.push(line);
        } else {
            frontmatter_lines.push(line);
        }
    }

    if !in_body {
        return Err(eyre!("missing closing frontmatter delimiter"));
    }

    Ok((
        parse_frontmatter(&frontmatter_lines.join("\n"))?,
        body_lines.join("\n").trim().to_string(),
    ))
}

pub fn parse_frontmatter(raw: &str) -> Result<Frontmatter> {
    let mut map = Frontmatter::new();
    for (line_index, raw_line) in raw.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| eyre!("invalid frontmatter line {}: {line}", line_index + 1))?;
        let key = key.trim();
        if key.is_empty() {
            return Err(eyre!("empty frontmatter key on line {}", line_index + 1));
        }

        map.insert(key.to_string(), parse_value(value.trim()));
    }

    Ok(map)
}

pub fn render_frontmatter(fields: &[(&str, FieldValue)]) -> String {
    let mut output = String::new();
    output.push_str("---\n");
    for (key, value) in fields {
        output.push_str(key);
        output.push_str(": ");
        output.push_str(&render_value(value));
        output.push('\n');
    }
    output.push_str("---\n");
    output
}

pub fn get_scalar(map: &Frontmatter, key: &str) -> Option<String> {
    match map.get(key) {
        Some(FieldValue::Scalar(value)) => none_if_null(value),
        Some(FieldValue::List(values)) => Some(values.join(",")),
        None => None,
    }
}

pub fn get_list(map: &Frontmatter, key: &str) -> Vec<String> {
    match map.get(key) {
        Some(FieldValue::List(values)) => values.clone(),
        Some(FieldValue::Scalar(value)) => none_if_null(value).into_iter().collect(),
        None => Vec::new(),
    }
}

fn parse_value(value: &str) -> FieldValue {
    if value.starts_with('[') && value.ends_with(']') {
        let inner = &value[1..value.len().saturating_sub(1)];
        let values = inner
            .split(',')
            .map(|part| unquote(part.trim()))
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        FieldValue::List(values)
    } else {
        FieldValue::Scalar(unquote(value))
    }
}

fn render_value(value: &FieldValue) -> String {
    match value {
        FieldValue::Scalar(value) => value.clone(),
        FieldValue::List(values) => {
            let joined = values.join(", ");
            format!("[{joined}]")
        }
    }
}

fn unquote(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string()
}

fn none_if_null(value: &str) -> Option<String> {
    if value.is_empty() || value == "null" {
        None
    } else {
        Some(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalar_and_list_fields() {
        let fm = parse_frontmatter("id: abc\ntags: [rust, cli]\nsession: null").unwrap();

        assert_eq!(get_scalar(&fm, "id"), Some("abc".to_string()));
        assert_eq!(get_list(&fm, "tags"), vec!["rust", "cli"]);
        assert_eq!(get_scalar(&fm, "session"), None);
    }

    #[test]
    fn splits_markdown_document() {
        let (fm, body) = split_document("---\nid: abc\n---\n# Title\nBody").unwrap();

        assert_eq!(get_scalar(&fm, "id"), Some("abc".to_string()));
        assert_eq!(body, "# Title\nBody");
    }
}
