use super::*;

pub(super) fn load_buffer_from_path(
    path: &PathBuf,
) -> Result<LoadedDraft, Box<dyn std::error::Error>> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_default();

    if extension == "ord" {
        match read_from_path(path) {
            Ok(document) => {
                let mut buffer =
                    EditorBuffer::from_rich_text(&document.plain_text, &document.content_html);
                buffer.move_cursor_to_end();
                Ok(LoadedDraft {
                    buffer,
                    metadata: DocumentMetadata {
                        status: document.status,
                        auto_title: document.auto_title,
                        label_color: document.label_color,
                    },
                })
            }
            Err(FormatError::InvalidDocument) => {
                let raw = fs::read_to_string(path)?;
                let mut buffer = EditorBuffer::from_text(&raw);
                buffer.move_cursor_to_end();
                Ok(LoadedDraft {
                    buffer,
                    metadata: DocumentMetadata::default(),
                })
            }
            Err(error) => Err(Box::new(error)),
        }
    } else {
        let mut buffer = EditorBuffer::from_text(&fs::read_to_string(path)?);
        buffer.move_cursor_to_end();
        Ok(LoadedDraft {
            buffer,
            metadata: DocumentMetadata::default(),
        })
    }
}

#[cfg(test)]
pub(super) fn load_plain_text_from_path(
    path: &PathBuf,
) -> Result<String, Box<dyn std::error::Error>> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_default();
    let text = if extension == "ord" {
        match read_from_path(path) {
            Ok(document) => document.plain_text,
            Err(FormatError::InvalidDocument) => {
                let raw = fs::read_to_string(path)?;
                if raw.trim().is_empty() {
                    String::new()
                } else {
                    raw
                }
            }
            Err(error) => return Err(Box::new(error)),
        }
    } else {
        fs::read_to_string(path)?
    };
    Ok(text)
}

pub(super) fn sanitize_indent_width(width: usize) -> usize {
    match width {
        2 | 4 | 8 => width,
        _ => 4,
    }
}

pub(super) fn expand_path(path: PathBuf) -> PathBuf {
    if let Ok(stripped) = path.strip_prefix("~") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }
    path
}

pub(super) fn parse_save_path_input(input: &str, title: &str) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a file path like ./draft.ord or ./draft.txt.".to_string());
    }

    let mut path = expand_path(PathBuf::from(trimmed));
    if path.exists() && path.is_dir() {
        path = path.join(ordet_format::default_save_file_name(Some(title)));
    } else if path.extension().is_none() {
        path.set_extension("ord");
    }

    Ok(path)
}

/// " [regex + whole word]" after a search status, or nothing for the default
/// plain case-insensitive search.
pub(super) fn search_flag_suffix(options: SearchOptions) -> String {
    if options == SearchOptions::default() {
        String::new()
    } else {
        format!(" [{}]", options.describe())
    }
}

pub(super) fn parse_workspace_path_input(input: &str) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a folder to track.".to_string());
    }
    let path = expand_path(PathBuf::from(trimmed));
    if !path.is_dir() {
        return Err("That folder does not exist.".to_string());
    }
    Ok(path)
}

pub(super) fn parse_open_path_input(input: &str) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a file path to open.".to_string());
    }

    let path = expand_path(PathBuf::from(trimmed));
    if path.is_dir() {
        return Err("That path is a folder. Enter a document file instead.".to_string());
    }
    if !path.exists() {
        return Err(
            "That file does not exist yet. Use New Draft if you want a blank document.".to_string(),
        );
    }

    Ok(path)
}

pub(super) fn parse_pasted_single_path(input: &str) -> Option<PathBuf> {
    let candidates = parse_pasted_path_candidates(input);
    resolve_existing_pasted_path_candidate(&candidates)
}

pub(super) fn parse_pasted_path_candidates(input: &str) -> Vec<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    candidates.push(trimmed.to_string());

    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        candidates.push(line.to_string());
        if let Some(index) = line.find("file://") {
            candidates.push(line[index..].to_string());
        }
        if line.starts_with('/') || line.starts_with('~') {
            candidates.push(line.to_string());
        }

        for token in line.split_whitespace() {
            if token.starts_with("file://") || token.starts_with('/') || token.starts_with('~') {
                candidates.push(token.to_string());
            }
        }
    }

    candidates
}

pub(super) fn resolve_existing_pasted_path_candidate(candidates: &[String]) -> Option<PathBuf> {
    for candidate in candidates {
        let mut candidate = trim_pasted_path_wrappers(candidate.trim()).to_string();
        if candidate.is_empty() {
            continue;
        }

        if let Some(decoded_file_uri) = decode_file_uri_path(candidate.as_str()) {
            candidate = decoded_file_uri;
        }

        let candidate = unescape_terminal_path(&candidate);
        if candidate.is_empty() {
            continue;
        }

        let path = normalize_document_path(expand_path(PathBuf::from(candidate)));
        if path.exists() {
            return Some(path);
        }
    }

    None
}

pub(super) fn trim_pasted_path_wrappers(value: &str) -> &str {
    let mut value = value.trim();
    loop {
        let wrapped = value.len() >= 2
            && matches!(
                (value.as_bytes()[0], value.as_bytes()[value.len() - 1]),
                (b'<', b'>')
                    | (b'(', b')')
                    | (b'[', b']')
                    | (b'{', b'}')
                    | (b'"', b'"')
                    | (b'\'', b'\'')
            );
        let next = wrapped.then(|| &value[1..value.len() - 1]);

        match next {
            Some(next) => value = next.trim(),
            None => break,
        }
    }
    value
        .trim_matches(|ch: char| {
            matches!(
                ch,
                '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\''
            )
        })
        .trim_end_matches(['.', ',', ';'])
}

pub(super) fn decode_file_uri_path(candidate: &str) -> Option<String> {
    let file_uri = candidate.strip_prefix("file://")?;
    let decoded = urlencoding::decode(file_uri.trim()).ok()?.into_owned();
    #[cfg(windows)]
    let decoded = decoded.replace('\\', "/");

    #[cfg(windows)]
    if let Some(rest) = decoded.strip_prefix("localhost") {
        // Be forgiving of paths built with `Path::display` rather than a
        // proper URI, e.g. `file://localhostC:\\Users\\...`.
        if rest.len() >= 2 && rest.as_bytes()[1] == b':' {
            return Some(rest.to_string());
        }
    }

    let (authority, path) = match decoded.find('/') {
        Some(index) => (&decoded[..index], &decoded[index..]),
        None => (decoded.as_str(), ""),
    };
    #[cfg(windows)]
    if authority.len() == 2 && authority.as_bytes()[1] == b':' {
        // `file://C:/dir` is a drive-qualified local path, not a host.
        return Some(format!("{authority}{path}"));
    }
    let local = authority.is_empty() || authority.eq_ignore_ascii_case("localhost");
    if !local {
        // A file URI with a real host names a UNC share.
        #[cfg(windows)]
        {
            return Some(format!("//{authority}{path}").replace('/', "\\"));
        }
        #[cfg(not(windows))]
        {
            return Some(format!("//{authority}{path}"));
        }
    }

    #[cfg(windows)]
    if path.len() >= 3 && path.as_bytes()[0] == b'/' && path.as_bytes()[2] == b':' {
        return Some(path[1..].to_string());
    }

    Some(if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    })
}

#[cfg(not(windows))]
pub(super) fn unescape_terminal_path(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            output.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else {
            output.push(ch);
        }
    }
    if escaped {
        output.push('\\');
    }
    output
}

#[cfg(windows)]
pub(super) fn unescape_terminal_path(input: &str) -> String {
    // Backslashes are path separators on Windows, so only undo the terminal
    // convention of escaping a literal space. Other separators stay intact.
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' && chars.peek() == Some(&' ') {
            output.push(' ');
            chars.next();
        } else {
            output.push(ch);
        }
    }
    output
}

pub(super) fn parse_browser_target_input(input: &str, source: &Path) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a name or destination path.".to_string());
    }

    let raw = expand_path(PathBuf::from(trimmed));
    let target = if raw.is_absolute() {
        raw
    } else {
        source.parent().unwrap_or_else(|| Path::new(".")).join(raw)
    };

    Ok(target)
}

pub(super) fn parse_alignment_input(input: &str) -> Result<ParagraphAlignment, String> {
    ParagraphAlignment::parse(input)
        .ok_or_else(|| "Enter left, center, right, or justify.".to_string())
}

pub(super) fn retry_prompt(
    mut prompt: PromptState,
    value: String,
    hint: impl Into<String>,
) -> PromptState {
    prompt.value = value;
    prompt.hint = hint.into();
    prompt
}

pub(super) fn parse_line_number_input(input: &str) -> Result<usize, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a line number.".to_string());
    }
    trimmed
        .parse::<usize>()
        .map_err(|_| "Enter a line number like 42.".to_string())
}

pub(super) fn parse_block_target_input(input: &str) -> Result<PathBuf, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a file path.".to_string());
    }
    Ok(expand_path(PathBuf::from(trimmed)))
}

pub(super) fn suggest_copy_destination(source: &Path) -> PathBuf {
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| {
            source
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "Copy".to_string())
        });
    let extension = source
        .extension()
        .map(|ext| ext.to_string_lossy().to_string());

    let base_name = match extension {
        Some(ext) if !ext.is_empty() => format!("{stem} copy.{ext}"),
        _ => format!("{stem} copy"),
    };
    let mut candidate = parent.join(base_name);
    let mut suffix = 2usize;
    while candidate.exists() {
        let name = match source
            .extension()
            .map(|ext| ext.to_string_lossy().to_string())
        {
            Some(ext) if !ext.is_empty() => format!("{stem} copy {suffix}.{ext}"),
            _ => format!("{stem} copy {suffix}"),
        };
        candidate = parent.join(name);
        suffix += 1;
    }
    candidate
}

pub(super) fn save_task_key(draft_id: DraftId) -> TaskKey {
    TaskKey::from(format!("save:{}", draft_id.0))
}

/// Retypes macOS Option-letter output as the Alt chord the keymap binds.
/// Without "Use Option as Meta", the terminal sends the glyph (∂, ç, …)
/// rather than Alt+letter, so shortcuts would insert text instead of
/// firing. Covers every Alt chord the keymap binds (US layout); anything
/// else passes through untouched, and real Alt (Meta mode) never reaches
/// this table at all. Option-N is the one exception: it is a dead key the
/// terminal composes itself, so `⌥N` (next window) needs Meta mode.
///
/// Tradeoff, stated plainly: with Option-as-unicode these glyphs become
/// shortcuts instead of typable text. Terminals with Meta mode are
/// unaffected either way, and dead-key accents that terminals compose
/// themselves never arrive here as lone characters.
pub(super) fn normalize_macos_option_key(key: KeyEvent) -> KeyEvent {
    if key.modifiers != KeyModifiers::NONE {
        return key;
    }
    let KeyCode::Char(ch) = key.code else {
        return key;
    };
    let letter = match ch {
        'å' => 'a',
        '∫' => 'b',
        'ç' => 'c',
        '∂' => 'd',
        '´' => 'e',
        'ˆ' => 'i',
        'µ' => 'm',
        '®' => 'r',
        'ß' => 's',
        '†' => 't',
        '¨' => 'u',
        '√' => 'v',
        '∑' => 'w',
        '≈' => 'x',
        _ => return key,
    };
    KeyEvent {
        code: KeyCode::Char(letter),
        modifiers: KeyModifiers::ALT,
        kind: key.kind,
        state: key.state,
    }
}

/// Ctrl-V / Alt-V (paste) in every terminal encoding.
///
/// Both letter cases count: terminals disagree about whether Ctrl-Shift-V
/// arrives as `v`, `V`, or an explicit Shift flag, and all of them mean paste.
pub(super) fn is_paste_key(key: KeyEvent) -> bool {
    if !matches!(key.code, KeyCode::Char('v') | KeyCode::Char('V')) {
        return false;
    }
    key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

pub(super) fn copy_path(source: &Path, target: &Path, source_is_dir: bool) -> io::Result<()> {
    if source_is_dir {
        copy_dir_recursive(source, target)
    } else {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, target)?;
        Ok(())
    }
}

pub(super) fn copy_dir_recursive(source: &Path, target: &Path) -> io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let destination = target.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &destination)?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&path, &destination)?;
        }
    }
    Ok(())
}

pub(super) fn draft_id_from_task_key(key: Option<&TaskKey>) -> Option<DraftId> {
    key.and_then(|key| key.0.strip_prefix("save:"))
        .and_then(|suffix| suffix.parse::<u64>().ok())
        .map(DraftId)
}

pub(super) fn move_path_to_trash(path: &Path) -> io::Result<PathBuf> {
    let trash_dir = trash_directory()?;
    fs::create_dir_all(&trash_dir)?;
    let destination = unique_trash_destination(&trash_dir, path);
    if let Err(rename_error) = fs::rename(path, &destination) {
        // The file may live on another volume (common on Windows). Copy it
        // into the trash, then remove the source only after the copy lands.
        if fs::copy(path, &destination).is_err() {
            return Err(rename_error);
        }
        if let Err(remove_error) = fs::remove_file(path) {
            let _ = fs::remove_file(&destination);
            return Err(remove_error);
        }
    }
    Ok(destination)
}

pub(super) fn trash_directory() -> io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let home = dirs::home_dir().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "Home directory is not available")
        })?;
        Ok(home.join(".Trash"))
    }

    #[cfg(target_os = "windows")]
    {
        let data = dirs::data_local_dir().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Local data directory is not available",
            )
        })?;
        // Keep the beta trash location for the same reason as settings.
        Ok(data.join("ordet-tui").join("Trash").join("files"))
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let data = dirs::data_local_dir().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Local data directory is not available",
            )
        })?;
        Ok(data.join("Trash").join("files"))
    }
}

pub(super) fn unique_trash_destination(trash_dir: &Path, original_path: &Path) -> PathBuf {
    let file_name = original_path
        .file_name()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "Untitled".into());
    let base = PathBuf::from(&file_name);
    let stem = base
        .file_stem()
        .filter(|stem| !stem.is_empty())
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| "Untitled".to_string());
    let extension = base
        .extension()
        .map(|ext| ext.to_string_lossy().to_string());

    let mut candidate = trash_dir.join(&file_name);
    let mut suffix = 2usize;
    while candidate.exists() {
        let name = match &extension {
            Some(ext) if !ext.is_empty() => format!("{stem} {suffix}.{ext}"),
            _ => format!("{stem} {suffix}"),
        };
        candidate = trash_dir.join(name);
        suffix += 1;
    }

    candidate
}

pub(super) fn merge_recent_paths(
    mut recent_files: Vec<PathBuf>,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Vec<PathBuf> {
    for path in paths {
        let path = normalize_document_path(path);
        recent_files.retain(|candidate| !paths_match(candidate, &path));
        recent_files.insert(0, path);
    }
    recent_files.truncate(crate::state::MAX_RECENT_FILES);
    recent_files
}

/// A lone draft with no path and no text is the placeholder scratch buffer, so
/// recovered work can take its place instead of stacking up behind it.
pub(super) fn is_empty_scratch(drafts: &[(EditorBuffer, Option<PathBuf>)]) -> bool {
    drafts.len() == 1 && drafts[0].1.is_none() && drafts[0].0.plain_text().is_empty()
}

pub(super) fn current_local_hour() -> u8 {
    time::OffsetDateTime::now_local()
        .map(|value| value.hour())
        .unwrap_or(12)
}

/// Local date and time for the timestamp insert.
pub(crate) fn local_timestamp() -> String {
    let Ok(now) = time::OffsetDateTime::now_local() else {
        return String::new();
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute()
    )
}

pub(super) fn detect_system_dark_mode() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        let output = ProcessCommand::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
            .ok()?;
        if !output.status.success() {
            return Some(false);
        }
        let value = String::from_utf8_lossy(&output.stdout);
        return Some(value.trim().eq_ignore_ascii_case("dark"));
    }

    #[cfg(target_os = "windows")]
    {
        use winreg::enums::HKEY_CURRENT_USER;

        let personalize = winreg::RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize")
            .ok()?;
        let apps_use_light_theme: u32 = personalize.get_value("AppsUseLightTheme").ok()?;
        return Some(apps_use_light_theme == 0);
    }

    #[allow(unreachable_code)]
    None
}

pub(super) fn parse_word_target(input: &str) -> Result<Option<usize>, String> {
    let trimmed = input.trim();
    if trimmed.is_empty()
        || matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "0" | "off" | "none" | "clear"
        )
    {
        return Ok(None);
    }

    let digits = trimmed
        .chars()
        .filter(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if digits.is_empty() {
        return Err("Enter a number like 1000, or 0/off/none to clear.".to_string());
    }

    let value = digits
        .parse::<usize>()
        .map_err(|_| "That target is too large.".to_string())?;
    if value == 0 {
        return Ok(None);
    }

    Ok(Some(value))
}
