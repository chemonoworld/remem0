use std::collections::BTreeMap;

use color_eyre::eyre::{Result, eyre};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldValue {
    Null,
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
        if map.contains_key(key) {
            return Err(eyre!(
                "duplicate frontmatter key {key:?} on line {}",
                line_index + 1
            ));
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

pub fn get_optional_scalar(map: &Frontmatter, key: &str) -> Result<Option<String>> {
    match map.get(key) {
        Some(FieldValue::Null) | None => Ok(None),
        Some(FieldValue::Scalar(value)) => Ok(Some(value.clone())),
        Some(FieldValue::List(_)) => Err(eyre!("frontmatter field {key:?} must be a scalar")),
    }
}

pub fn get_list(map: &Frontmatter, key: &str) -> Vec<String> {
    match map.get(key) {
        Some(FieldValue::List(values)) => values.clone(),
        Some(FieldValue::Scalar(value)) => vec![value.clone()],
        Some(FieldValue::Null) => Vec::new(),
        None => Vec::new(),
    }
}

fn parse_value(value: &str) -> FieldValue {
    if is_quoted(value) {
        FieldValue::Scalar(unquote(value))
    } else if value == "null" {
        FieldValue::Null
    } else if value.starts_with('[') && value.ends_with(']') {
        let inner = &value[1..value.len().saturating_sub(1)];
        let values = split_list(inner)
            .into_iter()
            .map(|part| unquote(&part))
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        FieldValue::List(values)
    } else {
        FieldValue::Scalar(unquote(value))
    }
}

fn render_value(value: &FieldValue) -> String {
    match value {
        FieldValue::Null => "null".to_string(),
        FieldValue::Scalar(value) => render_scalar(value),
        FieldValue::List(values) => {
            let joined = values
                .iter()
                .map(|value| render_scalar(value))
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{joined}]")
        }
    }
}

fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        decode_double_quoted(&value[1..value.len() - 1])
    } else if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        value[1..value.len() - 1].replace("''", "'")
    } else {
        value.to_string()
    }
}

fn is_quoted(value: &str) -> bool {
    value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
}

fn render_scalar(value: &str) -> String {
    let needs_quotes = value.is_empty()
        || value == "null"
        || value.trim() != value
        || (value.starts_with('[') && value.ends_with(']'))
        || value.contains(['"', '\\', ',', '\n', '\r', '\t'])
        || (value.starts_with('\'') && value.ends_with('\''));
    if !needs_quotes {
        return value.to_string();
    }

    let mut rendered = String::with_capacity(value.len() + 2);
    rendered.push('"');
    for ch in value.chars() {
        match ch {
            '"' => rendered.push_str("\\\""),
            '\\' => rendered.push_str("\\\\"),
            '\n' => rendered.push_str("\\n"),
            '\r' => rendered.push_str("\\r"),
            '\t' => rendered.push_str("\\t"),
            _ => rendered.push(ch),
        }
    }
    rendered.push('"');
    rendered
}

fn decode_double_quoted(value: &str) -> String {
    let mut decoded = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }
        match chars.next() {
            Some('"') => decoded.push('"'),
            Some('\\') => decoded.push('\\'),
            Some('n') => decoded.push('\n'),
            Some('r') => decoded.push('\r'),
            Some('t') => decoded.push('\t'),
            Some(other) => {
                decoded.push('\\');
                decoded.push(other);
            }
            None => decoded.push('\\'),
        }
    }
    decoded
}

fn split_list(value: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if quote == Some('"') && ch == '\\' {
            current.push(ch);
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            current.push(ch);
            if ch == active {
                quote = None;
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            current.push(ch);
        } else if ch == ',' {
            parts.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(ch);
        }
    }
    parts.push(current.trim().to_string());
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalar_and_list_fields() {
        let fm = parse_frontmatter("id: abc\ntags: [rust, cli]\nsession: null").unwrap();

        assert_eq!(
            get_optional_scalar(&fm, "id").unwrap(),
            Some("abc".to_string())
        );
        assert_eq!(get_list(&fm, "tags"), vec!["rust", "cli"]);
        assert_eq!(get_optional_scalar(&fm, "session").unwrap(), None);
    }

    #[test]
    fn splits_markdown_document() {
        let (fm, body) = split_document("---\nid: abc\n---\n# Title\nBody").unwrap();

        assert_eq!(
            get_optional_scalar(&fm, "id").unwrap(),
            Some("abc".to_string())
        );
        assert_eq!(body, "# Title\nBody");
    }

    #[test]
    fn renders_special_scalars_and_lists_losslessly() {
        let rendered = render_frontmatter(&[
            ("missing", FieldValue::Null),
            ("literal_null", FieldValue::Scalar("null".to_string())),
            ("bracketed", FieldValue::Scalar("[event,1]".to_string())),
            ("quoted", FieldValue::Scalar("\"opaque\"".to_string())),
            ("spaced", FieldValue::Scalar(" event ".to_string())),
            (
                "list",
                FieldValue::List(vec!["plain".to_string(), "comma,value".to_string()]),
            ),
        ]);
        let (parsed, _) = split_document(&rendered).unwrap();

        assert_eq!(get_optional_scalar(&parsed, "missing").unwrap(), None);
        assert_eq!(
            get_optional_scalar(&parsed, "literal_null").unwrap(),
            Some("null".to_string())
        );
        assert_eq!(
            get_optional_scalar(&parsed, "bracketed").unwrap(),
            Some("[event,1]".to_string())
        );
        assert_eq!(
            get_optional_scalar(&parsed, "quoted").unwrap(),
            Some("\"opaque\"".to_string())
        );
        assert_eq!(
            get_optional_scalar(&parsed, "spaced").unwrap(),
            Some(" event ".to_string())
        );
        assert_eq!(get_list(&parsed, "list"), vec!["plain", "comma,value"]);
    }

    #[test]
    fn strict_scalar_access_rejects_list_aliasing() {
        let parsed = parse_frontmatter("source_id: [event, 1]").unwrap();
        let error = get_optional_scalar(&parsed, "source_id").unwrap_err();
        assert!(error.to_string().contains("must be a scalar"));
    }

    #[test]
    fn duplicate_frontmatter_keys_are_rejected() {
        let error = parse_frontmatter("source_id: first\nsource_id: second").unwrap_err();
        assert!(error.to_string().contains("duplicate frontmatter key"));
    }
}
