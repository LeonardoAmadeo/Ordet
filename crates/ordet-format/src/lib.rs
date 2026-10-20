use regex::Regex;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static ATOMIC_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub const DEFAULT_TITLE: &str = "Untitled";
pub const DEFAULT_STATUS: &str = "No Status";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrdetDocument {
    pub title: String,
    pub status: String,
    pub auto_title: bool,
    pub label_color: String,
    pub content_html: String,
    pub plain_text: String,
}

impl Default for OrdetDocument {
    fn default() -> Self {
        Self {
            title: DEFAULT_TITLE.to_string(),
            status: DEFAULT_STATUS.to_string(),
            auto_title: true,
            label_color: String::new(),
            content_html: String::new(),
            plain_text: String::new(),
        }
    }
}

#[derive(Debug)]
pub enum FormatError {
    Io(std::io::Error),
    InvalidDocument,
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "{error}"),
            Self::InvalidDocument => write!(f, "That file is not a valid Ordet document."),
        }
    }
}

impl std::error::Error for FormatError {}

impl From<std::io::Error> for FormatError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

pub fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn unescape_xml(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

pub fn serialize(document: &OrdetDocument) -> String {
    let mut lines = vec![
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>".to_string(),
        "<ordet-document version=\"1.0\">".to_string(),
        format!("  <title>{}</title>", escape_xml(&document.title)),
        format!("  <status>{}</status>", escape_xml(&document.status)),
        format!(
            "  <autoTitle>{}</autoTitle>",
            if document.auto_title { "true" } else { "false" }
        ),
        format!(
            "  <labelColor>{}</labelColor>",
            escape_xml(&document.label_color)
        ),
    ];

    lines.extend([
        "  <content format=\"html\">".to_string(),
        escape_xml(&document.content_html),
        "  </content>".to_string(),
        "  <plainText>".to_string(),
        escape_xml(&document.plain_text),
        "  </plainText>".to_string(),
        "</ordet-document>".to_string(),
    ]);

    lines.join("\n")
}

pub fn parse(xml: &str, fallback_title: &str) -> Result<OrdetDocument, FormatError> {
    if !xml.contains("<ordet-document") {
        return Err(FormatError::InvalidDocument);
    }

    let plain_text = extract_xml_tag(xml, "plainText").unwrap_or_default();
    let content_html = extract_content_tag(xml).unwrap_or_else(|| plain_text_to_html(&plain_text));
    let title = extract_xml_tag(xml, "title")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback_title.to_string());
    let status = extract_xml_tag(xml, "status")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_STATUS.to_string());
    let auto_title = extract_xml_tag(xml, "autoTitle")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes"
            )
        })
        .unwrap_or(false);
    let label_color = extract_xml_tag(xml, "labelColor").unwrap_or_default();

    Ok(OrdetDocument {
        title,
        status,
        auto_title,
        label_color,
        content_html,
        plain_text,
    })
}

pub fn read_from_path(path: &Path) -> Result<OrdetDocument, FormatError> {
    let xml = fs::read_to_string(path)?;
    let fallback_title = fallback_title_from_path(path);
    parse(&xml, &fallback_title)
}

pub fn write_to_path(path: &Path, document: &OrdetDocument) -> Result<(), FormatError> {
    write_bytes_atomically(path, serialize(document).as_bytes())?;
    Ok(())
}

/// Replace `path` atomically: write and sync a sibling temporary file, then
/// rename it over the destination. A crash mid-write leaves the old file
/// intact instead of truncating the draft.
pub fn write_bytes_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("ordet-file");
    let temporary = path.with_file_name(format!(
        ".{file_name}.tmp-{}-{}",
        std::process::id(),
        ATOMIC_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        // Keep the original mode when replacing an existing file. Do this
        // after writing: a read-only source must still accept the temp write.
        if let Ok(metadata) = fs::metadata(path) {
            let _ = fs::set_permissions(&temporary, metadata.permissions());
        }
        fs::rename(&temporary, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn fallback_title_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| DEFAULT_TITLE.to_string())
}

pub fn sanitize_save_name(title: Option<&str>) -> String {
    let raw = title.unwrap_or(DEFAULT_TITLE).trim();
    let filtered: String = raw
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();

    let collapsed = filtered.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim().trim_matches('.');
    if trimmed.is_empty() {
        DEFAULT_TITLE.to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn default_save_file_name(title: Option<&str>) -> String {
    let base = sanitize_save_name(title);
    if Path::new(&base).extension().is_some() {
        base
    } else {
        format!("{base}.ord")
    }
}

pub fn plain_text_to_html(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.trim().is_empty() {
        return String::new();
    }

    normalized
        .split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                "<p><br></p>".to_string()
            } else {
                format!("<p>{}</p>", escape_xml(line))
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

pub fn extract_xml_tag(xml: &str, tag: &str) -> Option<String> {
    let pattern = Regex::new(&format!(r"(?s)<{tag}>(.*?)</{tag}>")).ok()?;
    extract_xml_capture(xml, &pattern)
}

pub fn extract_content_tag(xml: &str) -> Option<String> {
    let pattern = Regex::new(r#"(?s)<content\b[^>]*>(.*?)</content>"#).ok()?;
    extract_xml_capture(xml, &pattern)
}

fn extract_xml_capture(xml: &str, pattern: &Regex) -> Option<String> {
    pattern
        .captures(xml)
        .and_then(|captures| captures.get(1))
        .map(|capture| unescape_xml(capture.as_str().trim()))
}

pub fn normalize_ord_path(path: PathBuf) -> PathBuf {
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("ord"))
        .unwrap_or(false)
    {
        path
    } else {
        path.with_extension("ord")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse, plain_text_to_html, serialize, write_bytes_atomically, OrdetDocument, DEFAULT_STATUS,
    };
    use std::fs;

    #[test]
    fn atomic_write_replaces_the_file_and_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("ordet-format-atomic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("draft.txt");
        fs::write(&path, "old").expect("seed file");

        write_bytes_atomically(&path, b"new contents").expect("atomic replace");
        assert_eq!(
            fs::read_to_string(&path).expect("read back"),
            "new contents"
        );

        let leftovers: Vec<_> = fs::read_dir(&dir)
            .expect("temp dir")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "draft.txt")
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn format_round_trips() {
        let document = OrdetDocument {
            title: "The Hobbit".to_string(),
            status: DEFAULT_STATUS.to_string(),
            auto_title: true,
            label_color: "#ffaa00".to_string(),
            content_html: plain_text_to_html("The Hobbit"),
            plain_text: "The Hobbit".to_string(),
        };

        let xml = serialize(&document);
        let parsed = parse(&xml, "Untitled").expect("valid ordet document");
        assert_eq!(parsed.title, "The Hobbit");
        assert_eq!(parsed.plain_text, "The Hobbit");
        assert!(parsed.auto_title);
    }
}
