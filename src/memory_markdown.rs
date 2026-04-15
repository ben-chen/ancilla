use anyhow::{Context, bail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryDocument {
    pub title: String,
    pub tags: Vec<String>,
    pub body_markdown: String,
}

pub fn parse_memory_document(markdown: &str) -> anyhow::Result<MemoryDocument> {
    let trimmed = markdown.trim();
    if trimmed.is_empty() {
        bail!("memory markdown cannot be empty");
    }

    let mut lines = trimmed.lines().peekable();
    let Some(first_line) = lines.next() else {
        bail!("memory markdown cannot be empty");
    };
    let title = first_line
        .trim()
        .strip_prefix("# ")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| infer_title(trimmed));

    while matches!(lines.peek(), Some(line) if line.trim().is_empty()) {
        lines.next();
    }

    let mut tags = Vec::new();
    if let Some(line) = lines.peek().copied()
        && let Some(rest) = line.trim().strip_prefix("Tags:")
    {
        tags = parse_tags_line(rest);
        lines.next();
        while matches!(lines.peek(), Some(line) if line.trim().is_empty()) {
            lines.next();
        }
    }

    let body_markdown = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    Ok(MemoryDocument {
        title,
        tags: normalize_tags(tags),
        body_markdown,
    })
}

pub fn render_memory_document(title: &str, tags: &[String], body_markdown: &str) -> String {
    let title = title.trim();
    let body_markdown = body_markdown.trim();
    let tags = normalize_tags(tags.to_vec());

    let mut lines = vec![format!(
        "# {}",
        if title.is_empty() {
            "Untitled Memory"
        } else {
            title
        }
    )];
    if !tags.is_empty() {
        lines.push(String::new());
        lines.push(format!("Tags: {}", tags.join(", ")));
    }
    if !body_markdown.is_empty() {
        lines.push(String::new());
        lines.push(body_markdown.to_string());
    }
    lines.join("\n").trim().to_string()
}

pub fn markdown_from_plain_text(text: &str, tags: &[String]) -> String {
    let body = text.trim();
    let title = infer_title(body);
    render_memory_document(&title, tags, body)
}

pub fn markdown_from_parts(title: &str, tags: &[String], markdown_body: &str) -> String {
    render_memory_document(title, tags, markdown_body)
}

pub fn derive_search_text(markdown: &str) -> anyhow::Result<String> {
    let document = parse_memory_document(markdown)?;
    Ok(search_text_for_document(&document))
}

pub fn search_text_for_document(document: &MemoryDocument) -> String {
    let mut parts = vec![document.title.trim().to_string()];
    if !document.tags.is_empty() {
        parts.push(document.tags.join(" "));
    }
    let body_text = flatten_markdown(&document.body_markdown);
    if !body_text.is_empty() {
        parts.push(body_text);
    }
    parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn markdown_excerpt(markdown: &str, max_chars: usize) -> String {
    let body = parse_memory_document(markdown)
        .map(|document| flatten_markdown(&document.body_markdown))
        .unwrap_or_else(|_| flatten_markdown(markdown));
    truncate(&body, max_chars)
}

pub fn infer_title(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "Untitled Memory".to_string();
    }

    for line in trimmed.lines() {
        let candidate = line.trim();
        if candidate.is_empty() {
            continue;
        }
        let candidate = candidate.trim_start_matches('#').trim();
        if !candidate.is_empty() {
            return truncate(candidate, 80);
        }
    }

    "Untitled Memory".to_string()
}

pub fn has_tag(tags: &[String], expected: &str) -> bool {
    let expected = normalize_tag(expected);
    tags.iter().any(|tag| normalize_tag(tag) == expected)
}

pub fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for tag in tags {
        let tag = normalize_tag(&tag);
        if !tag.is_empty() && !normalized.contains(&tag) {
            normalized.push(tag);
        }
    }
    normalized
}

pub fn normalize_tag(tag: &str) -> String {
    tag.trim()
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else if character.is_whitespace() || character == '_' {
                '-'
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

pub fn flatten_markdown(markdown: &str) -> String {
    markdown
        .lines()
        .map(|line| {
            line.trim()
                .trim_start_matches('#')
                .trim_start_matches('>')
                .trim_start_matches('-')
                .trim_start_matches('*')
                .trim()
        })
        .filter(|line| !line.is_empty() && !line.starts_with("```") && !line.starts_with("Tags:"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

fn parse_tags_line(rest: &str) -> Vec<String> {
    rest.split(',')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

pub fn parse_memory_list_json(raw: &str) -> anyhow::Result<Vec<MemoryDocument>> {
    #[derive(serde::Deserialize)]
    struct Payload {
        #[serde(default)]
        memories: Vec<Item>,
    }
    #[derive(serde::Deserialize)]
    struct Item {
        title: String,
        #[serde(default)]
        tags: Vec<String>,
        markdown_body: String,
    }

    let trimmed = raw.trim().trim_matches('`').trim();
    let start = trimmed
        .find('{')
        .with_context(|| "memory creation response did not include a JSON object")?;
    let end = trimmed
        .rfind('}')
        .with_context(|| "memory creation response did not include a JSON object")?;
    let payload: Payload = serde_json::from_str(&trimmed[start..=end])
        .with_context(|| "failed to parse memory creation JSON")?;
    let mut documents = Vec::new();
    for item in payload.memories {
        let title = item.title.trim().to_string();
        let body = item.markdown_body.trim().to_string();
        if title.is_empty() || body.is_empty() {
            continue;
        }
        documents.push(MemoryDocument {
            title,
            tags: normalize_tags(item.tags),
            body_markdown: body,
        });
    }
    Ok(documents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_header_and_tags() {
        let markdown = r#"# Building Ancilla

Tags: project, memory

I am building Ancilla.
"#;

        let document = parse_memory_document(markdown).unwrap();
        assert_eq!(document.title, "Building Ancilla");
        assert_eq!(document.tags, vec!["project", "memory"]);
        assert_eq!(document.body_markdown, "I am building Ancilla.");
    }

    #[test]
    fn renders_plain_markdown_document() {
        let markdown = render_memory_document(
            "Building Ancilla",
            &["project".to_string(), "memory".to_string()],
            "I am building Ancilla.",
        );
        assert!(markdown.starts_with("# Building Ancilla"));
        assert!(markdown.contains("Tags: project, memory"));
    }

    #[test]
    fn parses_structured_memory_json() {
        let raw = r#"{"memories":[{"title":"Building Ancilla","tags":["project"],"markdown_body":"I am building Ancilla."}]}"#;
        let memories = parse_memory_list_json(raw).unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].title, "Building Ancilla");
        assert_eq!(memories[0].tags, vec!["project"]);
    }
}
