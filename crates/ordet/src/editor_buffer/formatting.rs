use super::*;

pub(super) fn has_alignment_marker(tag: &str, keyword: &str) -> bool {
    let compact = tag.replace(' ', "");
    compact.contains(&format!("text-align:{keyword}"))
        || compact.contains(&format!("align=\"{keyword}\""))
        || compact.contains(&format!("align='{keyword}'"))
        || compact.contains(&format!("ql-align-{keyword}"))
        || compact.contains(&format!("align-{keyword}"))
        || compact.contains(&format!("text-{keyword}"))
}

/// Decodes the entities `escape_xml` writes, plus numeric references, so
/// styled offsets line up with the decoded plain text.
pub(super) fn decode_xml_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let Some(end) = tail.find(';') else {
            out.push_str(tail);
            break;
        };
        // Cap entity length: a stray `&` with a far-away `;` is text.
        if end > 10 {
            out.push('&');
            rest = &tail[1..];
            continue;
        }
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some("&".to_string()),
            "lt" => Some("<".to_string()),
            "gt" => Some(">".to_string()),
            "quot" => Some("\"".to_string()),
            "apos" => Some("'".to_string()),
            _ if entity.starts_with('#') => parse_numeric_entity(&entity[1..]),
            _ => None,
        };
        match decoded {
            Some(text) => out.push_str(&text),
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

pub(super) fn parse_numeric_entity(body: &str) -> Option<String> {
    let code = if let Some(hex) = body.strip_prefix('x').or_else(|| body.strip_prefix('X')) {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        body.parse::<u32>().ok()?
    };
    char::from_u32(code).map(|ch| ch.to_string())
}

/// Inner HTML of each block element (`<p>`, `<li>`, headings…), in order,
/// mirroring what the alignment scanner counts as paragraphs. Tolerant of
/// missing close tags: runs to the next block open or the end.
pub(super) fn html_block_inners(html: &str) -> Vec<String> {
    fn is_block_open(tag: &str) -> bool {
        tag.starts_with("<p")
            || tag.starts_with("<li")
            || tag.starts_with("<blockquote")
            || tag.starts_with("<h1")
            || tag.starts_with("<h2")
            || tag.starts_with("<h3")
            || tag.starts_with("<h4")
            || tag.starts_with("<h5")
            || tag.starts_with("<h6")
    }

    let mut inners = Vec::new();
    let mut search_from = 0usize;
    while let Some(relative) = html[search_from..].find('<') {
        let start = search_from + relative;
        let rest = &html[start..];
        let Some(end_relative) = rest.find('>') else {
            break;
        };
        let tag = &rest[..=end_relative];
        let tag_lower = tag.to_ascii_lowercase();
        if tag_lower.starts_with("</") || !is_block_open(&tag_lower) {
            search_from = start.saturating_add(1);
            continue;
        }
        let inner_start = start + end_relative + 1;
        // Matching close tag, else the next block open, else the end.
        let after_lt = &tag_lower[1.min(tag_lower.len())..];
        let name_end = after_lt
            .char_indices()
            .find(|(_, ch)| !ch.is_ascii_alphanumeric())
            .map(|(index, _)| index)
            .unwrap_or(after_lt.len());
        let name = &after_lt[..name_end];
        if name.is_empty() {
            search_from = start.saturating_add(1);
            continue;
        }
        let close_tag = format!("</{name}");
        let after = &html[inner_start..];
        let inner_end = after
            .to_ascii_lowercase()
            .find(&close_tag)
            .map(|i| inner_start + i)
            .or_else(|| {
                let mut probe = inner_start;
                loop {
                    let Some(rel) = html[probe..].find('<') else {
                        return Some(html.len());
                    };
                    let candidate = probe + rel;
                    let crest = &html[candidate..];
                    let Some(close) = crest.find('>') else {
                        return Some(html.len());
                    };
                    if is_block_open(&crest[..=close].to_ascii_lowercase()) {
                        return Some(candidate);
                    }
                    probe = candidate.saturating_add(1);
                }
            })
            .unwrap_or(html.len());
        inners.push(html[inner_start..inner_end].to_string());
        search_from = inner_end;
    }
    inners
}

/// `(decoded length, bold, italic)` runs inside one block's inner HTML.
/// Depths tolerate mismatched tags; unknown tags pass through silently.
pub(super) fn inline_runs(inner: &str) -> Vec<(usize, bool, bool)> {
    let mut runs = Vec::new();
    let mut bold_depth = 0usize;
    let mut italic_depth = 0usize;
    let mut rest = inner;
    let mut pending = String::new();

    let flush = |pending: &mut String,
                 runs: &mut Vec<(usize, bool, bool)>,
                 bold_depth: usize,
                 italic_depth: usize| {
        if pending.is_empty() {
            return;
        }
        let decoded = decode_xml_entities(pending);
        let len = decoded.chars().count();
        pending.clear();
        if len == 0 {
            return;
        }
        let run = (len, bold_depth > 0, italic_depth > 0);
        if let Some(last) = runs.last_mut() {
            if last.1 == run.1 && last.2 == run.2 {
                last.0 += len;
                return;
            }
        }
        runs.push(run);
    };

    while let Some(start) = rest.find('<') {
        pending.push_str(&rest[..start]);
        let tag_rest = &rest[start..];
        let Some(end) = tag_rest.find('>') else {
            pending.push_str(tag_rest);
            break;
        };
        flush(&mut pending, &mut runs, bold_depth, italic_depth);
        let tag = tag_rest[..=end].to_ascii_lowercase();
        let name = tag
            .trim_start_matches('<')
            .trim_start_matches('/')
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .next()
            .unwrap_or("");
        let closing = tag.starts_with("</");
        let adjust = |depth: &mut usize| {
            if closing {
                *depth = depth.saturating_sub(1);
            } else if !tag.ends_with("/>") {
                *depth = depth.saturating_add(1);
            }
        };
        match name {
            "b" | "strong" => adjust(&mut bold_depth),
            "i" | "em" => adjust(&mut italic_depth),
            _ => {}
        }
        rest = &tag_rest[end + 1..];
    }
    pending.push_str(rest);
    flush(&mut pending, &mut runs, bold_depth, italic_depth);
    runs
}

/// Bold/italic spans from `.ord` HTML, mapped onto rope char offsets. Only
/// well-formed runs inside block elements survive; everything else degrades
/// to plain text rather than erroring.
pub(super) fn parse_inline_attrs(html: &str, rope: &ropey::Rope) -> Vec<AttrSpan> {
    let mut spans = Vec::new();
    for (line_index, inner) in html_block_inners(html).iter().enumerate() {
        if line_index >= rope.len_lines() {
            break;
        }
        let mut pos = rope.line_to_char(line_index);
        let line_end = if line_index + 1 < rope.len_lines() {
            rope.line_to_char(line_index + 1)
        } else {
            rope.len_chars()
        };
        for (len, bold, italic) in inline_runs(inner) {
            let run_end = (pos + len).min(line_end);
            if bold && pos < run_end {
                spans.push(AttrSpan {
                    start: pos,
                    end: run_end,
                    kind: AttrKind::Bold,
                });
            }
            if italic && pos < run_end {
                spans.push(AttrSpan {
                    start: pos,
                    end: run_end,
                    kind: AttrKind::Italic,
                });
            }
            pos = run_end;
        }
    }
    normalize_attr_spans(&mut spans);
    spans
}

pub(super) fn parse_paragraph_alignments(html: &str, line_count: usize) -> Vec<ParagraphAlignment> {
    let lower = html.to_ascii_lowercase();
    let mut alignments = Vec::new();
    let mut search_from = 0usize;

    while let Some(relative) = lower[search_from..].find('<') {
        let start = search_from + relative;
        let rest = &lower[start..];
        let is_supported = rest.starts_with("<p")
            || rest.starts_with("<li")
            || rest.starts_with("<blockquote")
            || rest.starts_with("<h1")
            || rest.starts_with("<h2")
            || rest.starts_with("<h3")
            || rest.starts_with("<h4")
            || rest.starts_with("<h5")
            || rest.starts_with("<h6");
        if !is_supported {
            search_from = start.saturating_add(1);
            continue;
        }

        let Some(end_relative) = rest.find('>') else {
            break;
        };
        let tag = &rest[..=end_relative];
        alignments.push(ParagraphAlignment::from_html_tag(tag));
        search_from = start + end_relative + 1;
    }

    alignments.resize(line_count, ParagraphAlignment::Left);
    alignments.truncate(line_count);
    alignments
}

pub(super) trait EmptyStringFallback {
    fn if_empty_then(self, fallback: String) -> String;
}

impl EmptyStringFallback for String {
    fn if_empty_then(self, fallback: String) -> String {
        if self.trim().is_empty() {
            fallback
        } else {
            self
        }
    }
}

/// Which case a run of text is already in, so cycling can advance from there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CaseStyle {
    Upper,
    Lower,
    Title,
    Sentence,
    Mixed,
}

pub(super) fn detect_case_style(text: &str) -> CaseStyle {
    let mut saw_upper = false;
    let mut saw_lower = false;
    for ch in text.chars().filter(|ch| ch.is_alphabetic()) {
        if ch.is_uppercase() {
            saw_upper = true;
        } else {
            saw_lower = true;
        }
    }

    match (saw_upper, saw_lower) {
        (true, false) => CaseStyle::Upper,
        (false, true) => CaseStyle::Lower,
        _ => {
            if text == capitalize_words(text) {
                CaseStyle::Title
            } else if text == sentence_case(text) {
                CaseStyle::Sentence
            } else {
                CaseStyle::Mixed
            }
        }
    }
}

pub(super) fn capitalize_words(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at_word_start = true;
    for ch in text.chars() {
        if ch.is_alphabetic() {
            if at_word_start {
                out.extend(ch.to_uppercase());
            } else {
                out.extend(ch.to_lowercase());
            }
            at_word_start = false;
        } else {
            out.push(ch);
            at_word_start = true;
        }
    }
    out
}

/// First letter uppercase, the rest lowercase: the sentence step of the
/// case cycle.
pub(super) fn sentence_case(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut first_alpha = true;
    for ch in text.chars() {
        if ch.is_alphabetic() {
            if first_alpha {
                out.extend(ch.to_uppercase());
            } else {
                out.extend(ch.to_lowercase());
            }
            first_alpha = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Shifts styled ranges for `n` inserted chars at `pos`. Text typed strictly
/// inside a styled range inherits it; text inserted exactly at a range start
/// does not (it belongs to what came before the cursor).
pub(super) fn shift_attrs_for_insert(spans: &mut [AttrSpan], pos: usize, n: usize) {
    if n == 0 {
        return;
    }
    for span in spans.iter_mut() {
        if span.start >= pos {
            span.start += n;
            span.end += n;
        } else if span.end > pos {
            span.end += n;
        }
    }
}

/// Shifts styled ranges for the deleted `[start, end)`: overlapped parts are
/// cut out, everything after slides left, empties are dropped.
pub(super) fn shift_attrs_for_delete(spans: &mut Vec<AttrSpan>, start: usize, end: usize) {
    if start >= end {
        return;
    }
    let len = end - start;
    let mut kept = Vec::with_capacity(spans.len());
    for span in spans.iter() {
        // Head before `start` survives; tail after `end` slides left; the
        // middle vanishes. Fully covered spans collapse to empty below.
        let new_start = if span.start >= end {
            span.start - len
        } else {
            span.start.min(start)
        };
        let new_end = if span.end > end {
            span.end - len
        } else {
            new_start.max(span.end.min(start))
        };
        if new_start < new_end {
            kept.push(AttrSpan {
                start: new_start,
                end: new_end,
                kind: span.kind,
            });
        }
    }
    *spans = kept;
    normalize_attr_spans(spans);
}

/// Whether every char in `[start, end)` already carries `kind`. Empty ranges
/// count as uncovered so toggling them always styles.
pub(super) fn range_fully_has_attr(
    spans: &[AttrSpan],
    kind: AttrKind,
    start: usize,
    end: usize,
) -> bool {
    if start >= end {
        return false;
    }
    let mut pos = start;
    for span in spans.iter().filter(|sp| sp.kind == kind) {
        if span.end <= pos {
            continue;
        }
        if span.start > pos {
            return false;
        }
        pos = pos.max(span.end);
        if pos >= end {
            return true;
        }
    }
    false
}

/// Cuts `kind` out of `[start, end)`, splitting spans as needed.
pub(super) fn subtract_attr_range(
    spans: &mut Vec<AttrSpan>,
    kind: AttrKind,
    start: usize,
    end: usize,
) {
    let mut kept = Vec::with_capacity(spans.len() + 1);
    for span in spans.iter() {
        if span.kind != kind || span.end <= start || span.start >= end {
            kept.push(*span);
            continue;
        }
        if span.start < start {
            kept.push(AttrSpan {
                start: span.start,
                end: start.min(span.end),
                kind,
            });
        }
        if span.end > end {
            kept.push(AttrSpan {
                start: end.max(span.start),
                end: span.end,
                kind,
            });
        }
    }
    *spans = kept;
    normalize_attr_spans(spans);
}

/// Sorts spans and merges adjacent same-kind ones. The toggle depends on
/// this: coverage walks assume order, and rendering assumes no redundancy.
pub(super) fn normalize_attr_spans(spans: &mut Vec<AttrSpan>) {
    spans.retain(|sp| sp.start < sp.end);
    spans.sort_by_key(|sp| (sp.start, sp.end));
    let mut merged: Vec<AttrSpan> = Vec::with_capacity(spans.len());
    for span in spans.drain(..) {
        if let Some(last) = merged.last_mut() {
            if last.kind == span.kind && last.end >= span.start {
                last.end = last.end.max(span.end);
                continue;
            }
        }
        merged.push(span);
    }
    *spans = merged;
}

/// Char length of a block marker (`# `, `> `, `- `, `1. `, checkboxes…)
/// at the start of `text`, or 0 when there is none. Requires the blank
/// after the marker, so `**bold**` and `#nospace` stay literal text.
pub(super) fn block_marker_len(text: &str) -> usize {
    let chars: Vec<char> = text.chars().collect();
    if chars.first() == Some(&'#') {
        let mut hashes = 0;
        while chars.get(hashes) == Some(&'#') {
            hashes += 1;
        }
        if hashes <= 6 && (chars.get(hashes).is_none() || chars[hashes] == ' ') {
            return hashes + usize::from(chars.get(hashes) == Some(&' '));
        }
        return 0;
    }
    if chars.first() == Some(&'>') {
        return 1 + usize::from(chars.get(1) == Some(&' '));
    }
    let mut len;
    if matches!(chars.first(), Some('-') | Some('*') | Some('+')) && chars.get(1) == Some(&' ') {
        len = 2;
    } else {
        let mut digits = 0;
        while chars.get(digits).is_some_and(|ch| ch.is_ascii_digit()) {
            digits += 1;
        }
        if digits > 0
            && matches!(chars.get(digits), Some('.') | Some(')'))
            && chars.get(digits + 1) == Some(&' ')
        {
            len = digits + 2;
        } else {
            return 0;
        }
    }
    // Optional task checkbox after a list marker.
    if chars.get(len) == Some(&'[')
        && matches!(chars.get(len + 1), Some(' ') | Some('x') | Some('X'))
        && chars.get(len + 2) == Some(&']')
    {
        len += 3;
        if chars.get(len) == Some(&' ') {
            len += 1;
        }
    }
    len
}

/// How far a position moves when every line in `[first_line, last_line]` gains
/// one level of indentation.
pub(super) fn indent_shift(
    line: usize,
    first_line: usize,
    last_line: usize,
    touched: usize,
    width: usize,
) -> usize {
    if line >= last_line {
        width * touched
    } else if line >= first_line {
        width
    } else {
        0
    }
}
