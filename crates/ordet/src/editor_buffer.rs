use ordet_format::escape_xml;
use ropey::Rope;
use std::cell::RefCell;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const UNTITLED: &str = "Untitled";

/// Columns one indent level is worth.
pub const INDENT_WIDTH: usize = 4;

/// One undoable change. Typing a character logs a `Remove` of one char range
/// (offsets only, no text copied); deleting text logs an `Insert` carrying the
/// removed text. Either way the record is proportional to the edit, never to
/// the document, unlike the rope snapshots this replaced.
#[derive(Clone, Debug)]
enum UndoOp {
    /// Re-insert text that was removed.
    Insert { at: usize, text: String },
    /// Remove text that was inserted.
    Remove { start: usize, end: usize },
}

#[derive(Clone, Debug)]
struct UndoRecord {
    /// Inverse ops in the order they were logged; undo applies them reversed.
    ops: Vec<UndoOp>,
    cursor: usize,
    anchor: Option<usize>,
    preferred_column: Option<usize>,
    previous_cursor: Option<usize>,
    /// Paragraph alignments ride along whole because they are one byte per
    /// line; snapshotting them costs nothing next to the text itself.
    paragraph_alignments: Vec<ParagraphAlignment>,
    /// Inline bold/italic spans ride along for the same reason: tiny next
    /// to the text, and replaying them through ops would be fiddly.
    attrs: Vec<AttrSpan>,
}

/// Inline style kind. Bold and italic live as independent dimensions so
/// bold-italic overlap just works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttrKind {
    Bold,
    Italic,
}

/// A styled char range. Same-kind spans never overlap (toggle normalizes);
/// different kinds may cross freely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttrSpan {
    pub start: usize,
    pub end: usize,
    pub kind: AttrKind,
}

/// Block style applied by `apply_block_prefix`. Body means plain: strip
/// whatever marker is there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockPrefix {
    Title,
    Heading,
    Quote,
    Body,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Boundary {
    pub char_index: usize,
    pub column: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrappedLine {
    pub start_char: usize,
    pub end_char: usize,
    pub text: String,
    pub display_offsets: Vec<usize>,
    pub boundaries: Vec<Boundary>,
}

#[derive(Clone, Debug, Default)]
pub struct WrapLayout {
    pub lines: Vec<WrappedLine>,
    /// Global index of `lines[0]`. A windowed layout is only a slice of the
    /// document, so callers that think in global visual lines need the offset.
    first_visual_line: usize,
    /// Visual lines in the whole document, not just this window. Scrolling
    /// clamps against this, not against `lines.len()`.
    total_lines: usize,
}

impl WrapLayout {
    /// Global visual index of the line holding `cursor`.
    ///
    /// Only the parity tests use this now; production movement answers from
    /// the cache without building a layout. It stays as the reference the
    /// cache-direct helpers are verified against.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn line_index_for_cursor(&self, cursor: usize) -> usize {
        let local = self
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| cursor >= line.start_char && cursor <= line.end_char)
            .map(|(index, _)| index)
            .next_back()
            .unwrap_or_else(|| self.lines.len().saturating_sub(1));
        self.first_visual_line + local
    }

    /// Visual lines in the whole document.
    pub fn total_lines(&self) -> usize {
        self.total_lines
    }

    /// The `rows` visual lines starting at global index `first_visual`, clipped
    /// to what this window actually holds.
    pub fn rows_from(&self, first_visual: usize, rows: usize) -> &[WrappedLine] {
        let start = first_visual
            .saturating_sub(self.first_visual_line)
            .min(self.lines.len());
        let end = start.saturating_add(rows).min(self.lines.len());
        &self.lines[start..end]
    }

    /// Translate a global visual index into one this window can answer for,
    /// clamping instead of panicking when the line is off-screen.
    fn local_index(&self, line_index: usize) -> usize {
        line_index
            .saturating_sub(self.first_visual_line)
            .min(self.lines.len().saturating_sub(1))
    }

    pub fn column_for_cursor(&self, line_index: usize, cursor: usize) -> usize {
        self.lines
            .get(self.local_index(line_index))
            .and_then(|line| {
                line.boundaries
                    .iter()
                    .find(|boundary| boundary.char_index == cursor)
                    .or_else(|| {
                        line.boundaries
                            .iter()
                            .rev()
                            .find(|boundary| boundary.char_index <= cursor)
                    })
                    .map(|boundary| boundary.column)
            })
            .unwrap_or(0)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn cursor_for_column(&self, line_index: usize, column: usize) -> usize {
        let Some(line) = self.lines.get(self.local_index(line_index)) else {
            return 0;
        };

        line.boundaries
            .iter()
            .min_by_key(|boundary| boundary.column.abs_diff(column))
            .map(|boundary| boundary.char_index)
            .unwrap_or(line.start_char)
    }
}

/// The tab grid: a tab advances to the next multiple of eight display
/// columns. The ruler's `!` marks use the same grid, so a tab in the text
/// visibly lands under them.
pub(crate) const TAB_STOP: usize = 8;

/// Columns a tab starting at `column` advances by.
pub(crate) fn tab_advance(column: usize) -> usize {
    TAB_STOP - (column % TAB_STOP)
}

/// Expand tabs to the eight-column grid. Tabs are only display sugar: the
/// buffer always stores them literally, so files round-trip byte for byte.
pub(crate) fn expand_tabs(text: &str) -> String {
    if !text.contains('\t') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + TAB_STOP);
    let mut column = 0;
    for ch in text.chars() {
        match ch {
            '\t' => {
                let advance = tab_advance(column);
                out.extend(std::iter::repeat_n(' ', advance));
                column += advance;
            }
            '\n' => {
                out.push('\n');
                column = 0;
            }
            _ => {
                out.push(ch);
                column += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            }
        }
    }
    out
}

#[derive(Clone, Debug)]
pub struct EditorBuffer {
    rope: Rope,
    cursor: usize,
    anchor: Option<usize>,
    preferred_column: Option<usize>,
    paragraph_alignments: Vec<ParagraphAlignment>,
    /// Inline bold/italic ranges. Same-kind spans are kept sorted,
    /// non-overlapping and merged; different kinds cross freely.
    attrs: Vec<AttrSpan>,
    previous_cursor: Option<usize>,
    undo: Vec<UndoRecord>,
    redo: Vec<UndoRecord>,
    edit_seq: u64,
    indent_width: usize,
    /// Memoised wrap results. Rendering asks for the layout every frame, so
    /// recomputing it over the whole document would cost one full re-wrap per
    /// keystroke. The cache is `RefCell` because `Component::render` only gets
    /// `&self`; it is a pure memo, so a shared borrow of the buffer is enough.
    wrap_cache: RefCell<WrapCache>,
    /// Incremental word/character stats. Same `RefCell` reasoning as the wrap
    /// cache: `stats()` is read from `&self` on the render path and folds the
    /// dirty range once, lazily.
    stats_cache: RefCell<StatsCache>,
}

/// One source line's wrap results, kept so an edit elsewhere in the document
/// does not have to re-measure this one. `dirty` lines are re-wrapped by the
/// next `ensure_wrap_cache`; every other entry is trusted as-is, which is what
/// makes a keystroke O(edited lines) instead of O(document).
#[derive(Clone, Debug)]
struct CachedWrapLine {
    lines: Vec<WrappedLine>,
    dirty: bool,
    /// Accumulated char-offset delta from edits earlier in the document, not
    /// yet baked into `lines`. Shifting this one number per entry, instead of
    /// every stored boundary, keeps a mid-document keystroke O(entries).
    shift: isize,
}

/// Clamp a stored offset plus a pending shift back into `usize`.
fn shift_offset(offset: usize, shift: isize) -> usize {
    (offset as isize + shift).max(0) as usize
}

/// Bake an entry's pending offset shift into a line just cloned out of the
/// cache, so callers keep seeing absolute char offsets.
fn apply_cached_shift(line: &mut WrappedLine, shift: isize) {
    if shift == 0 {
        return;
    }
    line.start_char = shift_offset(line.start_char, shift);
    line.end_char = shift_offset(line.end_char, shift);
    for boundary in line.boundaries.iter_mut() {
        boundary.char_index = shift_offset(boundary.char_index, shift);
    }
}

/// Record that every stored char offset in a cached entry moved by `shift`,
/// for lines whose text did not change but moved because of an edit earlier in
/// the document. Accumulating one number per entry instead of rewriting every
/// boundary keeps a mid-document keystroke O(entries), not O(graphemes).
fn shift_cached_offsets(entry: &mut CachedWrapLine, shift: isize) {
    entry.shift += shift;
}

#[derive(Clone, Debug, Default)]
struct WrapCache {
    width: usize,
    /// Bumped by the buffer whenever the text or alignments change, so a frame
    /// that only moved the cursor reuses the cache outright.
    edit_seq: u64,
    entries: Vec<CachedWrapLine>,
    total_lines: usize,
    /// Entry indices whose wrap may be stale. `None` means "check every
    /// entry": a resize or a structural splice invalidates positions, not
    /// just the marked entries.
    dirty: Option<Vec<usize>>,
    /// Fenwick tree of per-entry visual line counts: prefix sums give an
    /// entry's global first visual line and locating a visual line gives its
    /// entry in O(log n), so scrolling and cursor queries never walk the
    /// document. `None` until built (or after a structural change).
    fenwick: Option<Fenwick>,
}

/// Fenwick tree over the visual line counts of the wrap entries. Rope line
/// indices are positional, so the entries vec is positional too; the tree is
/// updated per entry (typing) and rebuilt only when entries are spliced
/// (newlines) or the width changes.
#[derive(Clone, Debug, Default)]
struct Fenwick {
    /// 1-based: `tree[0]` is unused, `tree[i]` covers a range ending at `i`.
    tree: Vec<usize>,
}

/// The lowest set bit of a nonzero Fenwick index.
#[inline]
fn lowest_bit(index: usize) -> usize {
    debug_assert!(index != 0);
    1usize << index.trailing_zeros()
}

impl Fenwick {
    fn build(counts: &[usize]) -> Self {
        let mut tree = Vec::with_capacity(counts.len() + 1);
        tree.push(0);
        tree.extend_from_slice(counts);
        for index in 1..=counts.len() {
            let parent = index + lowest_bit(index);
            if parent < tree.len() {
                let value = tree[index];
                tree[parent] += value;
            }
        }
        Self { tree }
    }

    /// Visual lines in entries `[0, index)`.
    fn prefix(&self, index: usize) -> usize {
        let mut index = index.min(self.tree.len().saturating_sub(1));
        let mut sum = 0usize;
        while index > 0 {
            sum += self.tree[index];
            index -= lowest_bit(index);
        }
        sum
    }

    fn add(&mut self, index: usize, delta: isize) {
        let mut node = index + 1;
        while node < self.tree.len() {
            self.tree[node] = (self.tree[node] as isize + delta).max(0) as usize;
            node += lowest_bit(node);
        }
    }

    fn total(&self) -> usize {
        self.prefix(self.tree.len().saturating_sub(1))
    }

    /// The entry holding visual line `target` and the offset inside it, or
    /// `None` when `target` is past the end.
    fn locate(&self, target: usize) -> Option<(usize, usize)> {
        let entries = self.tree.len().saturating_sub(1);
        if entries == 0 {
            return None;
        }
        let mut index = 0usize;
        let mut remaining = target;
        let mut bit = 1usize << (usize::BITS - 1 - entries.leading_zeros());
        while bit > 0 {
            let next = index + bit;
            if next <= entries && self.tree[next] <= remaining {
                index = next;
                remaining -= self.tree[next];
            }
            bit >>= 1;
        }
        if index >= entries {
            return None;
        }
        Some((index, remaining))
    }
}

impl WrapCache {
    /// Global first visual line of entry `index`.
    fn lines_before(&self, index: usize) -> usize {
        match &self.fenwick {
            Some(tree) => tree.prefix(index),
            None => self
                .entries
                .iter()
                .take(index)
                .map(|entry| entry.lines.len())
                .sum(),
        }
    }

    /// The entry holding visual line `index` and the offset inside it.
    fn locate(&self, index: usize) -> Option<(usize, usize)> {
        match &self.fenwick {
            Some(tree) => tree.locate(index),
            None => {
                let mut base = 0usize;
                for (entry_index, entry) in self.entries.iter().enumerate() {
                    let next = base + entry.lines.len();
                    if index < next {
                        return Some((entry_index, index - base));
                    }
                    base = next;
                }
                None
            }
        }
    }

    /// Record entry `index` as stale, without touching the structure.
    fn mark_dirty(&mut self, index: usize) {
        if let Some(indices) = &mut self.dirty {
            indices.push(index);
        }
    }

    /// Everything is suspect: resize, wholesale replacement, splices.
    fn mark_all_dirty(&mut self) {
        self.dirty = None;
        self.fenwick = None;
    }
}

/// Per-line counters for the word/character/sentence stats. One entry per
/// rope line, mirroring the rope's own line structure exactly (the wrap
/// cache's eager splicing keeps the two in step).
#[derive(Clone, Copy, Debug)]
struct CachedLineStats {
    /// Whitespace-separated words on this line.
    words: usize,
    /// Characters, with `\r` excluded like the previous whole-text scan.
    chars: usize,
    whitespace: usize,
    /// Sentence completion, one entry per incoming "the chunk before this
    /// line already has content" state: `(sentences finished inside this
    /// line, chunk-has-content at the line end)`. A chunk is the text between
    /// terminators; only chunks with non-whitespace content count, matching
    /// `split(['.', '!', '?'])` with empty trimmed pieces filtered out.
    sentence: [(usize, bool); 2],
    /// The sentence-chunk state after this line in the last fold, so a fold
    /// can resume from any line instead of walking from the document start.
    open_out: bool,
    /// The trimmed line is not empty, so it is a paragraph of its own.
    occupied: bool,
}

impl Default for CachedLineStats {
    fn default() -> Self {
        // An unseen line behaves like an empty one: no counters of its own,
        // and the sentence-chunk state passes through untouched. Placeholder
        // entries spliced for new lines depend on that.
        Self {
            words: 0,
            chars: 0,
            whitespace: 0,
            sentence: [(0, false), (0, true)],
            open_out: false,
            occupied: false,
        }
    }
}

impl CachedLineStats {
    fn scan(line: &str) -> Self {
        let chars = line.chars().filter(|&ch| ch != '\r').count();
        let words = line
            .split_whitespace()
            .filter(|token| token.contains(|ch: char| ch != '\r'))
            .count();
        let whitespace = line
            .chars()
            .filter(|&ch| ch != '\r' && ch.is_whitespace())
            .count();
        let mut sentence = [(0usize, false); 2];
        for (slot, transition) in sentence.iter_mut().enumerate() {
            let mut finished = 0usize;
            let mut open = slot == 1;
            for ch in line.chars() {
                if matches!(ch, '.' | '!' | '?') {
                    if open {
                        finished += 1;
                    }
                    open = false;
                } else if !ch.is_whitespace() {
                    open = true;
                }
            }
            *transition = (finished, open);
        }
        let occupied = !line.trim().is_empty();
        Self {
            words,
            chars,
            whitespace,
            sentence,
            open_out: false,
            occupied,
        }
    }
}

/// Document statistics in `EditorBuffer`, maintained incrementally: a
/// keystroke invalidates at most the edited line(s), and the whole-document
/// fold happens once on the next read. This replaced a per-keystroke
/// `rope.to_string()` plus full-text scan that cost 0.29ms on a small
/// document and scaled with it.
#[derive(Clone, Debug, Default)]
struct StatsCache {
    /// One entry per rope line. The rope helpers splice this in step with
    /// the rope; a length mismatch falls back to a full rebuild.
    lines: Vec<CachedLineStats>,
    /// Lines whose entries are stale, if any.
    dirty: Option<(usize, usize)>,
    /// `edit_seq` the cache was last folded at.
    edit_seq: u64,
    stats: crate::stats::WordStats,
    /// Running whole-document totals, adjusted line by line on each edit.
    totals: StatsTotals,
    /// Set when line entries carrying sentence-chain state were dropped
    /// (deleting a newline): the chain is rebuilt from the surviving tables
    /// on the next fold.
    sentences_dirty: bool,
}

/// Whole-document counters kept running across edits. `sentences` counts the
/// chunks finished inside lines; `end_open` says whether an unterminated
/// final chunk adds one more.
#[derive(Clone, Copy, Debug, Default)]
struct StatsTotals {
    words: usize,
    chars: usize,
    whitespace: usize,
    sentences: usize,
    paragraphs: usize,
    end_open: bool,
}

/// Recompute the whole sentence chain from the per-line transition tables.
/// This is the fallback after a splice dropped line entries whose old
/// contributions are no longer recorded anywhere.
fn refold_sentence_chain(cache: &mut StatsCache) {
    let mut sentences = 0usize;
    let mut open = false;
    for line in cache.lines.iter_mut() {
        let (finished, next) = line.sentence[usize::from(open)];
        sentences += finished;
        line.open_out = next;
        open = next;
    }
    cache.totals.sentences = sentences;
    cache.totals.end_open = open;
    cache.sentences_dirty = false;
}

/// Widen the stats cache's dirty range to cover `[first, last)` lines.
fn mark_stats_dirty(cache: &mut StatsCache, first: usize, last: usize) {
    let last = last.min(cache.lines.len());
    let first = first.min(last);
    if first >= last {
        return;
    }
    cache.dirty = Some(match cache.dirty {
        Some((dirty_first, dirty_last)) => (dirty_first.min(first), dirty_last.max(last)),
        None => (first, last),
    });
}

/// How find and replace interpret the query. Default is the historical
/// behavior: plain text, case-insensitive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    pub regex: bool,
    pub whole_word: bool,
    pub case_sensitive: bool,
}

impl SearchOptions {
    /// Short flag list for prompts and the status line: "plain",
    /// "regex + whole word", and so on.
    pub fn describe(self) -> String {
        let mut flags = Vec::new();
        if self.regex {
            flags.push("regex");
        }
        if self.whole_word {
            flags.push("whole word");
        }
        if self.case_sensitive {
            flags.push("case on");
        }
        if flags.is_empty() {
            "plain".to_string()
        } else {
            flags.join(" + ")
        }
    }

    /// Compile the query: escaped unless `regex`, case-folded unless
    /// `case_sensitive`, word-bounded when `whole_word`.
    pub fn to_regex(self, query: &str) -> Result<regex::Regex, String> {
        let mut pattern = String::new();
        if !self.case_sensitive {
            pattern.push_str("(?i)");
        }
        if self.whole_word {
            pattern.push_str(r"\b(?:");
        }
        if self.regex {
            pattern.push_str(query);
        } else {
            pattern.push_str(&regex::escape(query));
        }
        if self.whole_word {
            pattern.push(')');
            pattern.push_str(r"\b");
        }
        regex::Regex::new(&pattern).map_err(|error| format!("Invalid pattern: {error}"))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ParagraphAlignment {
    #[default]
    Left,
    Center,
    Right,
    Justify,
}

impl ParagraphAlignment {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Center => "center",
            Self::Right => "right",
            Self::Justify => "justify",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "left" | "l" => Some(Self::Left),
            "center" | "centre" | "c" => Some(Self::Center),
            "right" | "r" => Some(Self::Right),
            "justify" | "justified" | "j" => Some(Self::Justify),
            _ => None,
        }
    }

    fn style_attr(self) -> &'static str {
        match self {
            Self::Left => "",
            Self::Center => r#" style="text-align:center""#,
            Self::Right => r#" style="text-align:right""#,
            Self::Justify => r#" style="text-align:justify""#,
        }
    }

    fn from_html_tag(tag: &str) -> Self {
        let lower = tag.to_ascii_lowercase();
        if has_alignment_marker(&lower, "justify") {
            Self::Justify
        } else if has_alignment_marker(&lower, "right") {
            Self::Right
        } else if has_alignment_marker(&lower, "center") || has_alignment_marker(&lower, "centre") {
            Self::Center
        } else {
            Self::Left
        }
    }
}

impl Default for EditorBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the `.ord` document HTML for `rope` with the given paragraph
/// alignments and inline attribute spans. Shared by `EditorBuffer::to_ord_html`
/// and `DraftSnapshot`, so a background save produces byte-identical output to
/// a synchronous one.
pub(crate) fn ord_html_from(
    rope: &Rope,
    alignments: &[ParagraphAlignment],
    attrs: &[AttrSpan],
) -> String {
    let line_count = rope.len_lines();
    let mut paragraphs = Vec::with_capacity(line_count);
    for line_index in 0..line_count {
        let start = rope.line_to_char(line_index);
        // `display_line_end` without the buffer: every line but the last ends
        // just before the next line's first char.
        let end = if line_index + 1 >= line_count {
            rope.len_chars()
        } else {
            rope.line_to_char(line_index + 1).saturating_sub(1)
        };
        let alignment = alignments.get(line_index).copied().unwrap_or_default();
        let attr = alignment.style_attr();
        if start >= end {
            paragraphs.push(format!("<p{attr}><br></p>"));
            continue;
        }
        // Boundary cuts where styling changes; each segment wraps in
        // its own tags so crossed bold/italic ranges stay valid HTML.
        let mut cuts = vec![start, end];
        for span in attrs {
            if span.end <= start || span.start >= end {
                continue;
            }
            cuts.push(span.start.clamp(start, end));
            cuts.push(span.end.clamp(start, end));
        }
        cuts.sort_unstable();
        cuts.dedup();
        let mut body = String::new();
        for pair in cuts.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if a >= b {
                continue;
            }
            let text = escape_xml(&rope.slice(a..b).to_string());
            let bold = attrs
                .iter()
                .any(|sp| sp.kind == AttrKind::Bold && sp.start < b && sp.end > a);
            let italic = attrs
                .iter()
                .any(|sp| sp.kind == AttrKind::Italic && sp.start < b && sp.end > a);
            match (bold, italic) {
                (true, true) => body.push_str(&format!("<b><i>{text}</i></b>")),
                (true, false) => body.push_str(&format!("<b>{text}</b>")),
                (false, true) => body.push_str(&format!("<i>{text}</i>")),
                (false, false) => body.push_str(&text),
            }
        }
        paragraphs.push(format!("<p{attr}>{body}</p>"));
    }
    paragraphs.join("")
}

/// A save's view of the document: the rope clones in O(1)-ish time (shared
/// nodes), so the expensive whole-document HTML build can run on a worker
/// thread instead of stalling the event loop every autosave.
pub(crate) struct DraftSnapshot {
    rope: Rope,
    alignments: Vec<ParagraphAlignment>,
    attrs: Vec<AttrSpan>,
}

impl DraftSnapshot {
    pub(crate) fn capture(buffer: &EditorBuffer) -> Self {
        Self {
            rope: buffer.rope.clone(),
            alignments: buffer.paragraph_alignments.clone(),
            attrs: buffer.attrs.clone(),
        }
    }

    pub(crate) fn to_ord_html(&self) -> String {
        ord_html_from(&self.rope, &self.alignments, &self.attrs)
    }
}

impl EditorBuffer {
    pub fn new() -> Self {
        Self::from_text("")
    }

    pub fn from_text(text: &str) -> Self {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let rope = Rope::from_str(&normalized);
        let paragraph_alignments = vec![ParagraphAlignment::Left; rope.len_lines()];
        Self {
            rope,
            cursor: 0,
            anchor: None,
            preferred_column: None,
            paragraph_alignments,
            attrs: Vec::new(),
            previous_cursor: None,
            undo: Vec::new(),
            redo: Vec::new(),
            edit_seq: 0,
            indent_width: INDENT_WIDTH,
            wrap_cache: RefCell::new(WrapCache {
                width: 0,
                edit_seq: 0,
                entries: Vec::new(),
                total_lines: 0,
                dirty: None,
                fenwick: None,
            }),
            stats_cache: RefCell::new(StatsCache {
                edit_seq: u64::MAX,
                ..StatsCache::default()
            }),
        }
    }

    pub fn set_indent_width(&mut self, width: usize) {
        self.indent_width = match width {
            2 | 4 | 8 => width,
            _ => INDENT_WIDTH,
        };
    }

    pub fn from_rich_text(plain_text: &str, html: &str) -> Self {
        let mut buffer = Self::from_text(plain_text);
        buffer.paragraph_alignments = parse_paragraph_alignments(html, buffer.rope.len_lines());
        buffer.attrs = parse_inline_attrs(html, &buffer.rope);
        buffer
    }

    pub fn plain_text(&self) -> String {
        self.rope.to_string()
    }

    pub fn char_count(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn title(&self) -> String {
        self.rope
            .line(0)
            .to_string()
            .trim_end_matches('\n')
            .trim()
            .to_string()
            .if_empty_then(UNTITLED.to_string())
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Test-only cursor placement without the selection side effects of the
    /// movement commands, so parity tests can address any char directly.
    #[cfg(test)]
    fn set_cursor_for_test(&mut self, cursor: usize) {
        self.cursor = cursor.min(self.rope.len_chars());
        self.anchor = None;
        self.preferred_column = None;
    }

    pub fn move_cursor_to_end(&mut self) {
        self.cursor = self.rope.len_chars();
        self.anchor = None;
        self.preferred_column = None;
    }

    pub fn current_alignment(&self) -> ParagraphAlignment {
        self.paragraph_alignments
            .get(self.current_paragraph_index())
            .copied()
            .unwrap_or_default()
    }

    pub fn set_alignment_for_selection(&mut self, alignment: ParagraphAlignment) -> bool {
        let (start_line, end_line) = self.selected_paragraph_range();
        let needs_change = (start_line..=end_line).any(|line| {
            self.paragraph_alignments
                .get(line)
                .copied()
                .unwrap_or_default()
                != alignment
        });

        if !needs_change {
            return false;
        }

        self.begin_undo();
        self.ensure_alignment_capacity();
        for line in start_line..=end_line {
            if let Some(slot) = self.paragraph_alignments.get_mut(line) {
                *slot = alignment;
            }
        }
        self.mark_wrap_range_dirty(start_line, end_line);
        self.redo.clear();
        true
    }

    pub fn to_ord_html(&self) -> String {
        ord_html_from(&self.rope, &self.paragraph_alignments, &self.attrs)
    }

    pub fn selection_range(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        if anchor == self.cursor {
            return None;
        }
        Some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        Some(self.rope.slice(start..end).to_string())
    }

    /// Drop the selection, keeping the cursor where it is.
    pub fn collapse_selection(&mut self) {
        self.anchor = None;
        self.preferred_column = None;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.rope.len_chars();
        self.preferred_column = None;
    }

    pub fn move_left(&mut self, extend: bool) {
        let next = self.prev_grapheme_start(self.cursor);
        self.set_cursor(next, extend);
        self.preferred_column = None;
    }

    pub fn move_right(&mut self, extend: bool) {
        let next = self.next_grapheme_end(self.cursor);
        self.set_cursor(next, extend);
        self.preferred_column = None;
    }

    pub fn move_home(&mut self, width: usize, extend: bool) {
        let line_index = self.visual_line_for_cursor(width, self.cursor);
        let target = self.visual_line_start(width, line_index).unwrap_or(0);
        self.set_cursor(target, extend);
        self.preferred_column = None;
    }

    pub fn move_end(&mut self, width: usize, extend: bool) {
        let line_index = self.visual_line_for_cursor(width, self.cursor);
        let target = self
            .visual_line_end(width, line_index)
            .unwrap_or(self.cursor);
        self.set_cursor(target, extend);
        self.preferred_column = None;
    }

    pub fn move_up(&mut self, width: usize, extend: bool) {
        self.move_vertical(width, extend, true);
    }

    pub fn move_down(&mut self, width: usize, extend: bool) {
        self.move_vertical(width, extend, false);
    }

    pub fn move_word_left(&mut self, extend: bool) {
        let target = self.prev_word_start(self.cursor);
        self.set_cursor(target, extend);
        self.preferred_column = None;
    }

    pub fn move_word_right(&mut self, extend: bool) {
        let target = self.next_word_start(self.cursor);
        self.set_cursor(target, extend);
        self.preferred_column = None;
    }

    pub fn insert_text(&mut self, text: &str) {
        self.begin_undo();
        self.delete_selection_if_any();
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let insertion_line = self.current_paragraph_index();
        let inserted_newlines = normalized.chars().filter(|ch| *ch == '\n').count();
        self.rope_insert(self.cursor, &normalized);
        if inserted_newlines > 0 {
            let inherited = self
                .paragraph_alignments
                .get(insertion_line)
                .copied()
                .unwrap_or_default();
            let index = insertion_line.saturating_add(1);
            self.paragraph_alignments.splice(
                index..index,
                std::iter::repeat_n(inherited, inserted_newlines),
            );
        }
        self.cursor += normalized.chars().count();
        self.anchor = None;
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
    }

    /// How far the given line is indented, in display columns.
    pub fn line_indent_columns(&self, line: usize) -> usize {
        let start = self.rope.line_to_char(line);
        let end = self.rope.line_to_char(line + 1);
        let mut columns = 0;
        for offset in start..end {
            match self.rope.char(offset) {
                ' ' => columns += 1,
                '\t' => columns += tab_advance(columns),
                _ => break,
            }
        }
        columns
    }

    /// Enter: start a new line carrying the current line's indentation over.
    pub fn insert_newline_with_indent(&mut self) -> bool {
        let indent = self.line_indent_columns(self.current_paragraph_index());
        let mut text = String::from("\n");
        text.push_str(&" ".repeat(indent));
        self.insert_text(&text);
        true
    }

    /// The lines a block indent or outdent should touch.
    fn indent_target_lines(&self) -> Option<(usize, usize)> {
        match self.selection_range() {
            Some((start, end)) => {
                if end <= start {
                    return None;
                }
                let last_offset = end.saturating_sub(1).max(start);
                Some((
                    self.rope.char_to_line(start),
                    self.rope.char_to_line(last_offset),
                ))
            }
            None => None,
        }
    }

    /// Tab: indent every selected line, or insert one level when there is no
    /// multi-line selection.
    pub fn indent(&mut self) -> bool {
        let Some((first_line, last_line)) = self.indent_target_lines() else {
            let pad = " ".repeat(self.indent_width);
            self.insert_text(&pad);
            return true;
        };

        let cursor_line = self.rope.char_to_line(self.cursor);
        let anchor_line = self.anchor.map(|anchor| self.rope.char_to_line(anchor));
        let pad = " ".repeat(self.indent_width);
        let touched = last_line - first_line + 1;
        let width = self.indent_width;

        self.begin_undo();
        for line in (first_line..=last_line).rev() {
            let at = self.rope.line_to_char(line);
            self.rope_insert(at, &pad);
        }

        let limit = self.rope.len_chars();
        self.cursor = (self.cursor
            + indent_shift(cursor_line, first_line, last_line, touched, width))
        .min(limit);
        if let (Some(anchor), Some(anchor_line)) = (self.anchor, anchor_line) {
            self.anchor = Some(
                (anchor + indent_shift(anchor_line, first_line, last_line, touched, width))
                    .min(limit),
            );
        }
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    /// Shift-Tab: remove one level of indentation from the current line or
    /// every selected line.
    pub fn outdent(&mut self) -> bool {
        let (first_line, last_line) = match self.indent_target_lines() {
            Some(range) => range,
            None => {
                let line = self.current_paragraph_index();
                (line, line)
            }
        };

        let cursor_line = self.rope.char_to_line(self.cursor);
        let anchor_line = self.anchor.map(|anchor| self.rope.char_to_line(anchor));

        let mut removed_per_line = Vec::new();
        for line in first_line..=last_line {
            removed_per_line.push(self.removable_indent_on_line(line));
        }
        let removed_total: usize = removed_per_line.iter().sum();
        if removed_total == 0 {
            return false;
        }

        self.begin_undo();
        for offset in (0..removed_per_line.len()).rev() {
            let remove = removed_per_line[offset];
            if remove == 0 {
                continue;
            }
            let start = self.rope.line_to_char(first_line + offset);
            self.rope_remove(start, start + remove);
        }

        let limit = self.rope.len_chars();
        self.cursor = self.clamp_after_outdent(
            self.cursor,
            cursor_line,
            first_line,
            &removed_per_line,
            removed_total,
            limit,
        );
        if let (Some(anchor), Some(anchor_line)) = (self.anchor, anchor_line) {
            self.anchor = Some(self.clamp_after_outdent(
                anchor,
                anchor_line,
                first_line,
                &removed_per_line,
                removed_total,
                limit,
            ));
        }
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    /// Cycle the selection's letter case.
    ///
    /// Cycles through UPPERCASE, lowercase, and Capitalize Each Word, starting
    /// from whichever the selection already looks like.
    pub fn cycle_case(&mut self) -> bool {
        let Some((start, end)) = self.selection_range() else {
            return false;
        };
        let original = self.rope.slice(start..end).to_string();
        if !original.chars().any(char::is_alphabetic) {
            return false;
        }

        let next = match detect_case_style(&original) {
            CaseStyle::Upper => original.to_lowercase(),
            CaseStyle::Lower => capitalize_words(&original),
            CaseStyle::Title => {
                let sentence = sentence_case(&original);
                if sentence == original {
                    // Single capitalized word is both title and sentence
                    // case; keep cycling instead of stalling.
                    original.to_uppercase()
                } else {
                    sentence
                }
            }
            CaseStyle::Sentence => original.to_uppercase(),
            CaseStyle::Mixed => original.to_uppercase(),
        };
        if next == original {
            return false;
        }

        self.begin_undo();
        self.rope_remove(start, end);
        self.rope_insert(start, &next);
        self.cursor = start + next.chars().count();
        self.anchor = Some(start);
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    /// Sorts the full lines touched by the selection, A–Z, case-insensitive
    /// and stable. Undoable. Reached through the Edit menu.
    pub fn sort_selected_lines(&mut self) -> bool {
        let Some((start, end)) = self.selection_range() else {
            return false;
        };
        if start >= end {
            return false;
        }
        let first_line = self.rope.char_to_line(start);
        let last_line = self.rope.char_to_line(end.saturating_sub(1));
        if first_line >= last_line {
            return false;
        }
        let mut lines: Vec<String> = (first_line..=last_line)
            .map(|line| {
                self.rope
                    .line(line)
                    .as_str()
                    .unwrap_or("")
                    .trim_end_matches(['\r', '\n'])
                    .to_string()
            })
            .collect();
        lines.sort_by(|a, b| {
            a.to_lowercase()
                .cmp(&b.to_lowercase())
                .then_with(|| a.cmp(b))
        });

        let range_start = self.rope.line_to_char(first_line);
        let range_end = if last_line + 1 < self.rope.len_lines() {
            self.rope.line_to_char(last_line + 1)
        } else {
            self.rope.len_chars()
        };
        let original: String = self.rope.slice(range_start..range_end).to_string();
        let mut replacement = lines.join("\n");
        if original.ends_with('\n') {
            replacement.push('\n');
        }
        if replacement == original {
            return false;
        }

        self.begin_undo();
        self.rope_remove(range_start, range_end);
        self.rope_insert(range_start, &replacement);
        self.cursor = range_start + replacement.chars().count();
        self.anchor = Some(range_start);
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    /// Block styles, markdown-honest: Title (`# `), Heading (`## `), Quote
    /// (`> `), Body (strip any block marker). Applies to every touched line
    /// (or the current line), preserving indentation, replacing whatever
    /// marker was there. Blank lines are skipped. Undoable.
    pub fn apply_block_prefix(&mut self, prefix: BlockPrefix) -> bool {
        let (first_line, last_line) = match self.indent_target_lines() {
            Some(range) => range,
            None => {
                let line = self.current_paragraph_index();
                (line, line)
            }
        };

        struct LineEdit {
            remove_start: usize,
            remove_end: usize,
            insert: String,
        }
        let mut edits = Vec::new();
        for line in first_line..=last_line {
            let line_start = self.rope.line_to_char(line);
            let line_text: String = self.rope.line(line).as_str().unwrap_or("").to_string();
            if line_text.trim().is_empty() {
                continue;
            }
            // Indent is spaces/tabs (ASCII), so char and byte counts agree.
            let indent_len = line_text
                .chars()
                .take_while(|ch| *ch == ' ' || *ch == '\t')
                .count();
            let body = &line_text[indent_len..];
            let marker_len = block_marker_len(body);
            let new_marker = match prefix {
                BlockPrefix::Title => "# ",
                BlockPrefix::Heading => "## ",
                BlockPrefix::Quote => "> ",
                BlockPrefix::Body => "",
            };
            let old_marker: String = body.chars().take(marker_len).collect();
            if old_marker == new_marker {
                continue;
            }
            edits.push(LineEdit {
                remove_start: line_start + indent_len,
                remove_end: line_start + indent_len + marker_len,
                insert: new_marker.to_string(),
            });
        }
        if edits.is_empty() {
            return false;
        }

        self.begin_undo();
        for edit in edits.iter().rev() {
            self.rope_remove(edit.remove_start, edit.remove_end);
            self.rope_insert(edit.remove_start, &edit.insert);
        }
        let limit = self.rope.len_chars();
        self.anchor = Some(self.rope.line_to_char(first_line).min(limit));
        let last_end = if last_line + 1 < self.rope.len_lines() {
            self.rope.line_to_char(last_line + 1)
        } else {
            limit
        };
        self.cursor = last_end.min(limit);
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    /// Toggles bold/italic over the selection: fully styled text loses the
    /// style, anything else gains it. Keeps the selection; undoable. Needs a
    /// selection because styling as-you-type is out of scope.
    pub fn toggle_attr(&mut self, kind: AttrKind) -> bool {
        let Some((start, end)) = self.selection_range() else {
            return false;
        };
        self.begin_undo();
        if range_fully_has_attr(&self.attrs, kind, start, end) {
            subtract_attr_range(&mut self.attrs, kind, start, end);
        } else {
            self.attrs.push(AttrSpan { start, end, kind });
            normalize_attr_spans(&mut self.attrs);
        }
        self.redo.clear();
        true
    }

    /// Clipped `(start, end, bold, italic)` runs overlapping `[start, end)`,
    /// for painting one row.
    pub fn attr_runs_in(&self, start: usize, end: usize) -> Vec<(usize, usize, bool, bool)> {
        if start >= end {
            return Vec::new();
        }
        let mut cuts = vec![start, end];
        for span in &self.attrs {
            if span.end <= start || span.start >= end {
                continue;
            }
            cuts.push(span.start.clamp(start, end));
            cuts.push(span.end.clamp(start, end));
        }
        cuts.sort_unstable();
        cuts.dedup();
        cuts.windows(2)
            .map(|pair| {
                let (a, b) = (pair[0], pair[1]);
                let bold = self
                    .attrs
                    .iter()
                    .any(|sp| sp.kind == AttrKind::Bold && sp.start < b && sp.end > a);
                let italic = self
                    .attrs
                    .iter()
                    .any(|sp| sp.kind == AttrKind::Italic && sp.start < b && sp.end > a);
                (a, b, bold, italic)
            })
            .filter(|(a, b, _, _)| a < b)
            .collect()
    }

    /// How many leading spaces one outdent should strip from a line.
    fn removable_indent_on_line(&self, line: usize) -> usize {
        let start = self.rope.line_to_char(line);
        let end = self.rope.line_to_char(line + 1);
        let mut removable = 0;
        for offset in 0..self.indent_width {
            if start + offset < end && self.rope.char(start + offset) == ' ' {
                removable += 1;
            } else {
                break;
            }
        }
        removable
    }

    /// Keep a position sensible after its line lost leading spaces.
    fn clamp_after_outdent(
        &self,
        position: usize,
        line: usize,
        first_line: usize,
        removed_per_line: &[usize],
        removed_total: usize,
        limit: usize,
    ) -> usize {
        if line < first_line {
            return position.min(limit);
        }
        if line >= first_line + removed_per_line.len() {
            return position.saturating_sub(removed_total).min(limit);
        }

        let offset = line - first_line;
        let removed_before: usize = removed_per_line[..offset].iter().sum();
        let removed_here = removed_per_line[offset];
        let line_start = self.rope.line_to_char(line);
        let column = position.saturating_sub(line_start + removed_before);
        (line_start + column.saturating_sub(removed_here)).min(limit)
    }

    pub fn backspace(&mut self) -> bool {
        self.begin_undo();
        if self.delete_selection_if_any() {
            self.redo.clear();
            return true;
        }
        if self.cursor == 0 {
            self.cancel_undo();
            return false;
        }
        let current_line = self.current_paragraph_index();
        let removed_newline = self.cursor > 0 && self.rope.char(self.cursor - 1) == '\n';
        let start = self.prev_grapheme_start(self.cursor);
        self.rope_remove(start, self.cursor);
        if removed_newline && current_line < self.paragraph_alignments.len() {
            self.paragraph_alignments.remove(current_line);
        }
        self.cursor = start;
        self.anchor = None;
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    pub fn delete_forward(&mut self) -> bool {
        self.begin_undo();
        if self.delete_selection_if_any() {
            self.redo.clear();
            return true;
        }
        let end = self.next_grapheme_end(self.cursor);
        if end == self.cursor {
            self.cancel_undo();
            return false;
        }
        let removed_newline =
            self.cursor < self.rope.len_chars() && self.rope.char(self.cursor) == '\n';
        let current_line = self.current_paragraph_index();
        self.rope_remove(self.cursor, end);
        if removed_newline {
            let next_line = current_line.saturating_add(1);
            if next_line < self.paragraph_alignments.len() {
                self.paragraph_alignments.remove(next_line);
            }
        }
        self.anchor = None;
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    pub fn undo(&mut self) -> bool {
        let Some(record) = self.undo.pop() else {
            return false;
        };
        // Open a fresh record first: it captures the current (post-edit) state,
        // and the inverse ops applied below log their forwards into it, so it
        // is a redo record by the time the loop finishes.
        self.begin_undo();
        for op in record.ops.iter().rev() {
            match op {
                UndoOp::Insert { at, text } => self.rope_insert(*at, text),
                UndoOp::Remove { start, end } => self.rope_remove(*start, *end),
            }
        }
        let redo = self.undo.pop().unwrap_or_else(|| {
            // Logging cannot fail to leave the record `begin_undo` opened.
            UndoRecord {
                ops: Vec::new(),
                cursor: self.cursor,
                anchor: self.anchor,
                preferred_column: self.preferred_column,
                previous_cursor: self.previous_cursor,
                paragraph_alignments: self.paragraph_alignments.clone(),
                attrs: self.attrs.clone(),
            }
        });
        self.redo.push(redo);
        self.cursor = record.cursor;
        self.anchor = record.anchor;
        self.preferred_column = record.preferred_column;
        self.previous_cursor = record.previous_cursor;
        self.paragraph_alignments = record.paragraph_alignments;
        self.attrs = record.attrs;
        // The rope ops above maintained the cache for text changes, but an
        // alignment-only record logs no rope ops, so its wrap entries would
        // stay stale without this. Undo and redo run at human speed, never at
        // frame rate, so re-measuring is cheap here.
        self.mark_wrap_all_dirty();
        self.ensure_alignment_capacity();
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(record) = self.redo.pop() else {
            return false;
        };
        self.begin_undo();
        for op in record.ops.iter().rev() {
            match op {
                UndoOp::Insert { at, text } => self.rope_insert(*at, text),
                UndoOp::Remove { start, end } => self.rope_remove(*start, *end),
            }
        }
        self.cursor = record.cursor;
        self.anchor = record.anchor;
        self.preferred_column = record.preferred_column;
        self.previous_cursor = record.previous_cursor;
        self.paragraph_alignments = record.paragraph_alignments;
        self.attrs = record.attrs;
        self.mark_wrap_all_dirty();
        self.ensure_alignment_capacity();
        if self.undo.len() > 256 {
            let overflow = self.undo.len() - 256;
            self.undo.drain(0..overflow);
        }
        true
    }

    pub fn record_jump(&mut self) {
        self.previous_cursor = Some(self.cursor);
    }

    pub fn move_to_top(&mut self) {
        self.record_jump();
        self.cursor = 0;
        self.anchor = None;
        self.preferred_column = None;
    }

    pub fn move_to_screen_top(&mut self, scroll: usize, width: usize) {
        self.record_jump();
        let total = self.total_visual_lines(width);
        let target_line = scroll.min(total.saturating_sub(1));
        if let Some(start) = self.visual_line_start(width, target_line) {
            self.cursor = start;
            self.anchor = None;
            self.preferred_column = None;
        }
    }

    pub fn move_to_screen_bottom(&mut self, scroll: usize, height: usize, width: usize) {
        self.record_jump();
        let total = self.total_visual_lines(width);
        let target_line = (scroll + height.saturating_sub(1)).min(total.saturating_sub(1));
        if let Some(start) = self.visual_line_start(width, target_line) {
            self.cursor = start;
            self.anchor = None;
            self.preferred_column = None;
        }
    }

    pub fn move_to_block_start(&mut self) {
        if let Some(anchor) = self.anchor {
            self.record_jump();
            self.cursor = anchor.min(self.cursor);
            self.preferred_column = None;
        }
    }

    pub fn move_to_block_end(&mut self) {
        if let Some(anchor) = self.anchor {
            self.record_jump();
            self.cursor = anchor.max(self.cursor);
            self.preferred_column = None;
        }
    }

    pub fn jump_to_previous_cursor(&mut self) {
        if let Some(prev) = self.previous_cursor {
            self.previous_cursor = Some(self.cursor);
            self.cursor = prev.min(self.rope.len_chars());
            self.preferred_column = None;
        }
    }

    pub fn move_to_line(&mut self, line_1_based: usize, width: usize) {
        self.record_jump();
        let total = self.total_visual_lines(width);
        let target_line = line_1_based.saturating_sub(1).min(total.saturating_sub(1));
        if let Some(start) = self.visual_line_start(width, target_line) {
            self.cursor = start;
            self.anchor = None;
            self.preferred_column = None;
        }
    }

    pub fn delete_word_forward(&mut self) -> bool {
        self.begin_undo();
        if self.delete_selection_if_any() {
            self.redo.clear();
            return true;
        }
        let target = self.next_word_start(self.cursor);
        if target == self.cursor {
            self.cancel_undo();
            return false;
        }
        self.rope_remove(self.cursor, target);
        self.anchor = None;
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    /// Delete from the previous word start to the cursor. A selection wins,
    /// like every other delete.
    pub fn delete_word_left(&mut self) -> bool {
        self.begin_undo();
        if self.delete_selection_if_any() {
            self.redo.clear();
            return true;
        }
        let target = self.prev_word_start(self.cursor);
        if target == self.cursor {
            self.cancel_undo();
            return false;
        }
        self.rope_remove(target, self.cursor);
        self.cursor = target;
        self.anchor = None;
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    pub fn delete_line(&mut self) -> bool {
        self.begin_undo();
        if self.rope.len_chars() == 0 {
            self.cancel_undo();
            return false;
        }
        let paragraph_idx = self.current_paragraph_index();
        let line_start = self.rope.line_to_char(paragraph_idx);
        let line_end = if paragraph_idx + 1 < self.rope.len_lines() {
            self.rope.line_to_char(paragraph_idx + 1)
        } else {
            self.rope.len_chars()
        };
        if line_start == line_end && paragraph_idx > 0 {
            let prev_start = self.rope.line_to_char(paragraph_idx - 1);
            self.rope_remove(prev_start, line_end);
            if paragraph_idx < self.paragraph_alignments.len() {
                self.paragraph_alignments.remove(paragraph_idx);
            }
            self.cursor = prev_start;
        } else {
            self.rope_remove(line_start, line_end);
            if paragraph_idx < self.paragraph_alignments.len()
                && self.paragraph_alignments.len() > 1
            {
                self.paragraph_alignments.remove(paragraph_idx);
            }
            self.cursor = line_start.min(self.rope.len_chars());
        }
        self.anchor = None;
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    pub fn delete_to_end_of_line(&mut self, width: usize) -> bool {
        self.begin_undo();
        let line_index = self.visual_line_for_cursor(width, self.cursor);
        let end_char = self
            .visual_line_end(width, line_index)
            .unwrap_or(self.cursor);
        let target = if end_char > self.cursor {
            end_char
        } else if self.cursor < self.rope.len_chars() && self.rope.char(self.cursor) == '\n' {
            self.cursor + 1
        } else {
            self.cursor
        };
        if target == self.cursor {
            self.cancel_undo();
            return false;
        }
        self.rope_remove(self.cursor, target);
        self.anchor = None;
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    pub fn open_line(&mut self) -> bool {
        self.begin_undo();
        let insertion_line = self.current_paragraph_index();
        self.rope_insert(self.cursor, "\n");
        let inherited = self
            .paragraph_alignments
            .get(insertion_line)
            .copied()
            .unwrap_or_default();
        let index = insertion_line.saturating_add(1);
        self.paragraph_alignments
            .splice(index..index, std::iter::once(inherited));
        self.anchor = None;
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    pub fn insert_char_with_mode(&mut self, ch: char, overwrite: bool) {
        self.begin_undo();
        if self.delete_selection_if_any() {
            self.rope_insert(self.cursor, &ch.to_string());
            self.cursor += 1;
            self.anchor = None;
            self.preferred_column = None;
            self.ensure_alignment_capacity();
            self.redo.clear();
            return;
        }
        if overwrite && self.cursor < self.rope.len_chars() && self.rope.char(self.cursor) != '\n' {
            let end = self.next_grapheme_end(self.cursor);
            self.rope_remove(self.cursor, end);
        }
        self.rope_insert(self.cursor, &ch.to_string());
        self.cursor += 1;
        self.anchor = None;
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
    }

    pub fn set_block_begin(&mut self) {
        self.anchor = Some(self.cursor);
    }

    pub fn set_block_end(&mut self) {
        if self.anchor.is_none() {
            self.anchor = Some(0);
        }
    }

    pub fn hide_block(&mut self) {
        self.anchor = None;
    }

    pub fn copy_block(&mut self) -> bool {
        let Some(text) = self.selected_text() else {
            return false;
        };
        self.insert_text(&text);
        true
    }

    pub fn move_block(&mut self) -> bool {
        let Some((start, end)) = self.selection_range() else {
            return false;
        };
        let text = self.rope.slice(start..end).to_string();
        self.begin_undo();
        self.rope_remove(start, end);
        let insert_pos = if self.cursor >= end {
            self.cursor - (end - start)
        } else if self.cursor > start {
            start
        } else {
            self.cursor
        };
        self.rope_insert(insert_pos, &text);
        self.cursor = insert_pos + text.chars().count();
        self.anchor = None;
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    pub fn delete_block(&mut self) -> bool {
        self.begin_undo();
        if self.delete_selection_if_any() {
            self.redo.clear();
            true
        } else {
            self.cancel_undo();
            false
        }
    }

    pub fn cursor_line_and_col(&self, width: usize) -> (usize, usize) {
        let line_idx = self.visual_line_for_cursor(width, self.cursor);
        let col = self.visual_column_for_cursor(width, self.cursor);
        (line_idx + 1, col + 1)
    }

    pub fn find_matches_with(
        &self,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Vec<(usize, usize)>, String> {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let pattern = options.to_regex(query)?;
        let text = self.rope.to_string();
        let mut matches = Vec::new();
        for mat in pattern.find_iter(&text) {
            let start_char = self.rope.byte_to_char(mat.start());
            let end_char = self.rope.byte_to_char(mat.end());
            // Zero-width matches (e.g. `x*`) would loop the cursor forever;
            // skip them the way most editors do.
            if end_char > start_char {
                matches.push((start_char, end_char));
            }
        }
        Ok(matches)
    }

    pub fn find_next_with(
        &mut self,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Option<(usize, usize, usize, usize)>, String> {
        let matches = self.find_matches_with(query, options)?;
        if matches.is_empty() {
            return Ok(None);
        }
        let total = matches.len();
        let idx = matches
            .iter()
            .position(|(start, _)| *start > self.cursor)
            .unwrap_or(0);

        let (m_start, m_end) = matches[idx];
        self.record_jump();
        self.anchor = Some(m_start);
        self.cursor = m_end;
        self.preferred_column = None;
        Ok(Some((idx + 1, total, m_start, m_end)))
    }

    /// Select the match immediately before the cursor, wrapping at the end.
    pub fn find_previous_with(
        &mut self,
        query: &str,
        options: &SearchOptions,
    ) -> Result<Option<(usize, usize, usize, usize)>, String> {
        let matches = self.find_matches_with(query, options)?;
        if matches.is_empty() {
            return Ok(None);
        }
        let total = matches.len();
        // `cursor` is parked at the end of the active match. Use its start
        // anchor as the boundary so Left never returns the same match.
        let boundary = self.anchor.unwrap_or(self.cursor);
        let idx = matches
            .iter()
            .rposition(|(start, _)| *start < boundary)
            .unwrap_or(total - 1);
        let (m_start, m_end) = matches[idx];
        self.record_jump();
        self.anchor = Some(m_start);
        self.cursor = m_end;
        self.preferred_column = None;
        Ok(Some((idx + 1, total, m_start, m_end)))
    }

    pub fn replace_current_and_find_next_with(
        &mut self,
        query: &str,
        replacement: &str,
        options: &SearchOptions,
    ) -> Result<Option<(usize, usize)>, String> {
        if let Some(sel) = self.selected_text() {
            let matches_selection = if options.regex {
                options
                    .to_regex(query)?
                    .find(&sel)
                    .is_some_and(|mat| mat.as_str() == sel)
            } else if options.case_sensitive {
                sel == query
            } else {
                sel.eq_ignore_ascii_case(query)
            };
            if matches_selection {
                self.insert_text(replacement);
            }
        }
        match self.find_next_with(query, options)? {
            Some((idx, total, _, _)) => Ok(Some((idx, total))),
            None => Ok(None),
        }
    }

    pub fn replace_all_with(
        &mut self,
        query: &str,
        replacement: &str,
        options: &SearchOptions,
    ) -> Result<usize, String> {
        let pattern = options.to_regex(query)?;
        let text = self.rope.to_string();
        // Collect the expanded replacement per match first: regex captures
        // (`$1`, `$name`) expand against the matched text, and every match is
        // then applied back-to-front so earlier offsets stay valid.
        let mut edits = Vec::new();
        for mat in pattern.find_iter(&text) {
            let start_char = self.rope.byte_to_char(mat.start());
            let end_char = self.rope.byte_to_char(mat.end());
            if end_char > start_char {
                // `$1` captures expand in regex mode; plain mode stays
                // literal so a `$` in the replacement is never magic.
                let expanded = if options.regex {
                    pattern.replace(mat.as_str(), replacement).into_owned()
                } else {
                    replacement.to_string()
                };
                edits.push((start_char, end_char, expanded));
            }
        }
        if edits.is_empty() {
            return Ok(0);
        }
        let count = edits.len();
        self.begin_undo();
        for (start, end, expanded) in edits.into_iter().rev() {
            self.rope_remove(start, end);
            self.rope_insert(start, &expanded);
        }
        self.anchor = None;
        self.cursor = self.cursor.min(self.rope.len_chars());
        self.ensure_alignment_capacity();
        self.redo.clear();
        Ok(count)
    }

    /// Wrap the whole document at `width`.
    ///
    /// Prefer `visual_line_for_cursor`, `total_visual_lines` and
    /// `visible_layout` on any per-frame path: this builds a `WrappedLine` for
    /// every visual line in the document, which is fine for a one-off but far
    /// too much work to repeat 120 times a second. Only the reference tests
    /// use this now.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn layout(&self, width: usize) -> WrapLayout {
        self.ensure_wrap_cache(width);
        let cache = &self.wrap_cache.borrow();
        let lines = cache
            .entries
            .iter()
            .flat_map(|entry| {
                entry.lines.iter().map(|line| {
                    let mut line = line.clone();
                    apply_cached_shift(&mut line, entry.shift);
                    line
                })
            })
            .collect();
        WrapLayout {
            lines,
            first_visual_line: 0,
            total_lines: cache.total_lines,
        }
    }

    /// Wrap only the `rows` visual lines starting at `first_visual`, plus
    /// `margin` lines of context either side so vertical movement can look at
    /// the neighbouring line without a second wrap.
    pub fn visible_layout(
        &self,
        width: usize,
        first_visual: usize,
        rows: usize,
        margin: usize,
    ) -> WrapLayout {
        self.ensure_wrap_cache(width);
        let cache = &self.wrap_cache.borrow();
        let total = cache.total_lines;
        let start = first_visual.saturating_sub(margin).min(total);
        let end = (first_visual.saturating_add(rows).saturating_add(margin)).min(total);
        let mut lines = Vec::with_capacity(end.saturating_sub(start));
        if start < end {
            let (first_entry, _) = cache.locate(start).unwrap_or((0, 0));
            let mut base = cache.lines_before(first_entry);
            'entries: for entry in cache.entries[first_entry.min(cache.entries.len())..].iter() {
                for (offset, line) in entry.lines.iter().enumerate() {
                    let index = base + offset;
                    if index >= end {
                        break 'entries;
                    }
                    if index >= start {
                        let mut line = line.clone();
                        apply_cached_shift(&mut line, entry.shift);
                        lines.push(line);
                    }
                }
                base += entry.lines.len();
                if base >= end {
                    break;
                }
            }
        }
        WrapLayout {
            lines,
            first_visual_line: start,
            total_lines: total,
        }
    }

    /// Global visual index of the line holding `cursor`, without building a
    /// layout. This is what scrolling needs.
    pub fn visual_line_for_cursor(&self, width: usize, cursor: usize) -> usize {
        self.ensure_wrap_cache(width);
        let cache = &self.wrap_cache.borrow();
        let cursor = cursor.min(self.rope.len_chars());
        let rope_line = self.rope.char_to_line(cursor).min(cache.entries.len());
        let index = cache.lines_before(rope_line);
        let Some(entry) = cache.entries.get(rope_line) else {
            return index;
        };
        let shift = entry.shift;
        let local = entry
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| {
                cursor >= shift_offset(line.start_char, shift)
                    && cursor <= shift_offset(line.end_char, shift)
            })
            .map(|(offset, _)| offset)
            .next_back()
            .unwrap_or_else(|| entry.lines.len().saturating_sub(1));
        index + local
    }

    /// Visual lines in the whole document, without building a layout.
    pub fn total_visual_lines(&self, width: usize) -> usize {
        self.ensure_wrap_cache(width);
        self.wrap_cache.borrow().total_lines
    }

    /// The single wrapped line at global visual index `visual_index`, cloned
    /// on its own. Movement commands need one line, never the whole document.
    fn visual_wrapped_line(&self, width: usize, visual_index: usize) -> Option<WrappedLine> {
        self.ensure_wrap_cache(width);
        let cache = self.wrap_cache.borrow();
        let (entry_index, offset) = cache.locate(visual_index)?;
        let entry = cache.entries.get(entry_index)?;
        let offset = offset.min(entry.lines.len().saturating_sub(1));
        entry.lines.get(offset).map(|line| {
            let mut line = line.clone();
            apply_cached_shift(&mut line, entry.shift);
            line
        })
    }

    /// Char offset where global visual line `visual_index` starts.
    fn visual_line_start(&self, width: usize, visual_index: usize) -> Option<usize> {
        self.visual_wrapped_line(width, visual_index)
            .map(|line| line.start_char)
    }

    /// Char offset where global visual line `visual_index` ends.
    fn visual_line_end(&self, width: usize, visual_index: usize) -> Option<usize> {
        self.visual_wrapped_line(width, visual_index)
            .map(|line| line.end_char)
    }

    /// Display column of `cursor` on its visual line, without building a
    /// layout. Matches `layout(width).column_for_cursor(index, cursor)`.
    fn visual_column_for_cursor(&self, width: usize, cursor: usize) -> usize {
        let index = self.visual_line_for_cursor(width, cursor);
        self.visual_wrapped_line(width, index)
            .and_then(|line| {
                line.boundaries
                    .iter()
                    .find(|boundary| boundary.char_index == cursor)
                    .or_else(|| {
                        line.boundaries
                            .iter()
                            .rev()
                            .find(|boundary| boundary.char_index <= cursor)
                    })
                    .map(|boundary| boundary.column)
            })
            .unwrap_or(0)
    }

    /// Display column of the cursor at this wrap width, for chrome that
    /// mirrors the caret (the ruler's inverted cell). Matches what
    /// `render_editor_buffer` paints.
    pub fn cursor_visual_column(&self, width: usize) -> usize {
        self.visual_column_for_cursor(width.max(1), self.cursor)
    }

    /// Cursor char for `column` on global visual line `visual_index`, clamped
    /// to the last line exactly like `WrapLayout::cursor_for_column`. What
    /// mouse clicks hit-test against.
    pub fn visual_cursor_for_column(
        &self,
        width: usize,
        visual_index: usize,
        column: usize,
    ) -> usize {
        let total = self.total_visual_lines(width);
        let clamped = visual_index.min(total.saturating_sub(1));
        self.visual_wrapped_line(width, clamped)
            .and_then(|line| {
                line.boundaries
                    .iter()
                    .min_by_key(|boundary| boundary.column.abs_diff(column))
                    .map(|boundary| boundary.char_index)
            })
            .unwrap_or_else(|| {
                self.visual_wrapped_line(width, clamped)
                    .map(|line| line.start_char)
                    .unwrap_or(0)
            })
    }

    /// Bring the memo up to date for `width`.
    ///
    /// A frame that only moved the cursor matches `edit_seq` and returns
    /// immediately. After an edit, only entries the mutation marked `dirty` are
    /// re-measured: `rope_insert` and `rope_remove` already spliced the entry
    /// list and shifted every untouched line's offsets, so this loop never
    /// compares text and never walks lines it can prove are clean.
    fn ensure_wrap_cache(&self, width: usize) {
        let wrap_width = width.max(1);
        {
            let cache = self.wrap_cache.borrow();
            if cache.width == wrap_width && cache.edit_seq == self.edit_seq {
                return;
            }
        }

        let mut cache = self.wrap_cache.borrow_mut();
        if cache.width != wrap_width {
            // Per-line results are only meaningful at the width they were
            // measured for, so a resize has to start over.
            cache.width = wrap_width;
            for entry in cache.entries.iter_mut() {
                entry.dirty = true;
            }
            cache.mark_all_dirty();
        }
        let line_count = self.rope.len_lines();
        if cache.entries.len() > line_count {
            cache.entries.truncate(line_count);
            cache.mark_all_dirty();
        }
        while cache.entries.len() < line_count {
            cache.entries.push(CachedWrapLine {
                lines: Vec::new(),
                dirty: true,
                shift: 0,
            });
            cache.mark_all_dirty();
        }

        // Typing refreshes the one or two entries the edit touched; a resize
        // or a splice refreshes every entry that is marked dirty.
        let indices: Vec<usize> = match cache.dirty.take() {
            Some(indices) => indices,
            None => (0..line_count).collect(),
        };
        for source_line in indices {
            if source_line >= line_count || source_line >= cache.entries.len() {
                continue;
            }
            if !cache.entries[source_line].dirty {
                continue;
            }
            let start = self.rope.line_to_char(source_line);
            let end = self.display_line_end(source_line);
            let alignment = self
                .paragraph_alignments
                .get(source_line)
                .copied()
                .unwrap_or_default();
            let text = self.rope.slice(start..end).to_string();
            let display = expand_tabs(&text);
            let lines = wrap_source_line(&display, start, alignment, wrap_width);
            let old_len = cache.entries[source_line].lines.len();
            let new_len = lines.len();
            if old_len != new_len {
                if let Some(tree) = cache.fenwick.as_mut() {
                    tree.add(source_line, new_len as isize - old_len as isize);
                }
            }
            let entry = &mut cache.entries[source_line];
            entry.lines = lines;
            entry.dirty = false;
            // Freshly measured offsets are absolute; nothing is pending.
            entry.shift = 0;
        }

        if cache.fenwick.is_none() {
            let counts: Vec<usize> = cache
                .entries
                .iter()
                .map(|entry| entry.lines.len())
                .collect();
            cache.fenwick = Some(Fenwick::build(&counts));
        }
        cache.total_lines = cache.fenwick.as_ref().map_or(0, Fenwick::total);
        cache.dirty = Some(Vec::new());
        cache.edit_seq = self.edit_seq;
    }

    fn move_vertical(&mut self, width: usize, extend: bool, up: bool) {
        let line_index = self.visual_line_for_cursor(width, self.cursor);
        let target_column = self
            .preferred_column
            .unwrap_or_else(|| self.visual_column_for_cursor(width, self.cursor));
        let total = self.total_visual_lines(width);

        let next_index = if up {
            line_index.saturating_sub(1)
        } else {
            (line_index + 1).min(total.saturating_sub(1))
        };

        let target = self.visual_cursor_for_column(width, next_index, target_column);
        self.set_cursor(target, extend);
        self.preferred_column = Some(target_column);
    }

    fn set_cursor(&mut self, cursor: usize, extend: bool) {
        let clamped = cursor.min(self.rope.len_chars());
        if extend {
            self.anchor.get_or_insert(self.cursor);
            self.cursor = clamped;
        } else {
            self.cursor = clamped;
            self.anchor = None;
        }
    }

    fn delete_selection_if_any(&mut self) -> bool {
        let Some((start, end)) = self.selection_range() else {
            return false;
        };
        self.rope_remove(start, end);
        self.cursor = start;
        self.anchor = None;
        true
    }

    /// The only way text enters the rope. The wrap cache is maintained here,
    /// eagerly: the edited line and every line the insertion spawned are marked
    /// dirty, and untouched lines after them have their stored offsets shifted,
    /// so `ensure_wrap_cache` never has to compare text to find what changed.
    fn rope_insert(&mut self, char_idx: usize, text: &str) {
        let char_idx = char_idx.min(self.rope.len_chars());
        let first_line = self.rope.char_to_line(char_idx);
        let inserted_chars = text.chars().count();
        let added_lines = text.chars().filter(|&c| c == '\n').count();
        self.rope.insert(char_idx, text);
        self.edit_seq = self.edit_seq.wrapping_add(1);
        if inserted_chars == 0 {
            return;
        }
        shift_attrs_for_insert(&mut self.attrs, char_idx, inserted_chars);
        self.log_undo_op(UndoOp::Remove {
            start: char_idx,
            end: char_idx + inserted_chars,
        });
        let mut cache = self.wrap_cache.borrow_mut();
        if cache.entries.is_empty() {
            drop(cache);
            self.stats_after_insert(first_line, added_lines);
            return;
        }
        if let Some(entry) = cache.entries.get_mut(first_line) {
            entry.dirty = true;
        }
        cache.mark_dirty(first_line);
        if added_lines > 0 {
            for _ in 0..added_lines {
                cache.entries.insert(
                    first_line + 1,
                    CachedWrapLine {
                        lines: Vec::new(),
                        dirty: true,
                        shift: 0,
                    },
                );
            }
            // Positions after the splice moved: the pending-dirty list is
            // no longer trustworthy, so every entry is re-checked.
            cache.mark_all_dirty();
        }
        let shift = inserted_chars as isize;
        for entry in cache.entries.iter_mut().skip(first_line + 1 + added_lines) {
            shift_cached_offsets(entry, shift);
        }
        drop(cache);
        self.stats_after_insert(first_line, added_lines);
    }

    /// The only way text leaves the rope. Like `rope_insert`, this keeps the
    /// wrap cache spliced and shifted eagerly: entries for deleted lines are
    /// dropped, the merged line is marked dirty, and later lines shift left.
    fn rope_remove(&mut self, start: usize, end: usize) {
        if start == end {
            return;
        }
        let first_line = self.rope.char_to_line(start.min(self.rope.len_chars()));
        let last_line = self.rope.char_to_line(end.min(self.rope.len_chars()));
        let removed_chars = end.saturating_sub(start);
        let removed_lines = last_line.saturating_sub(first_line);
        let removed_text = self.rope.slice(start..end).to_string();
        self.rope.remove(start..end);
        self.edit_seq = self.edit_seq.wrapping_add(1);
        shift_attrs_for_delete(&mut self.attrs, start, end);
        self.log_undo_op(UndoOp::Insert {
            at: start,
            text: removed_text,
        });
        let mut cache = self.wrap_cache.borrow_mut();
        if cache.entries.is_empty() {
            drop(cache);
            self.stats_after_remove(first_line, removed_lines);
            return;
        }
        let dropped = removed_lines.min(cache.entries.len().saturating_sub(first_line + 1));
        cache
            .entries
            .drain(first_line + 1..first_line + 1 + dropped);
        if let Some(entry) = cache.entries.get_mut(first_line) {
            entry.dirty = true;
        }
        cache.mark_dirty(first_line);
        if dropped > 0 {
            cache.mark_all_dirty();
        }
        let shift = -(removed_chars as isize);
        for entry in cache.entries.iter_mut().skip(first_line + 1) {
            shift_cached_offsets(entry, shift);
        }
        drop(cache);
        self.stats_after_remove(first_line, removed_lines);
    }

    /// Call after anything that changes what a line wraps to without touching
    /// the rope, such as re-aligning a paragraph.
    fn mark_wrap_range_dirty(&mut self, first_line: usize, last_line: usize) {
        let mut cache = self.wrap_cache.borrow_mut();
        let last = last_line.saturating_add(1).min(cache.entries.len());
        for index in first_line.min(last)..last {
            cache.entries[index].dirty = true;
            cache.mark_dirty(index);
        }
        self.edit_seq = self.edit_seq.wrapping_add(1);
    }

    /// Stats bookkeeping after `added_lines` newlines were inserted with
    /// their first character landing on `first_line`: splice placeholder
    /// entries so later lines stay aligned, then mark the touched range.
    fn stats_after_insert(&mut self, first_line: usize, added_lines: usize) {
        let line_count = self.rope.len_lines();
        let mut cache = self.stats_cache.borrow_mut();
        if cache.lines.len().saturating_add(added_lines) == line_count {
            if added_lines > 0 {
                let at = (first_line + 1).min(cache.lines.len());
                cache.lines.splice(
                    at..at,
                    std::iter::repeat_n(CachedLineStats::default(), added_lines),
                );
            }
            mark_stats_dirty(&mut cache, first_line, first_line + added_lines + 1);
        }
        // A length mismatch means the line list was not in step; the fold
        // notices and rebuilds it from scratch.
    }

    /// Stats bookkeeping after a deletion that swallowed `removed_lines`
    /// newlines starting on `first_line`.
    fn stats_after_remove(&mut self, first_line: usize, removed_lines: usize) {
        let line_count = self.rope.len_lines();
        let mut cache = self.stats_cache.borrow_mut();
        if cache.lines.len() == line_count.saturating_add(removed_lines) {
            if removed_lines > 0 {
                let at = (first_line + 1).min(cache.lines.len());
                let end = (at + removed_lines).min(cache.lines.len());
                let mut words = 0usize;
                let mut chars = 0usize;
                let mut whitespace = 0usize;
                let mut paragraphs = 0usize;
                for line in &cache.lines[at..end] {
                    words += line.words;
                    chars += line.chars;
                    whitespace += line.whitespace;
                    paragraphs += usize::from(line.occupied);
                }
                cache.totals.words -= words;
                cache.totals.chars -= chars;
                cache.totals.whitespace -= whitespace;
                cache.totals.paragraphs -= paragraphs;
                cache.lines.drain(at..end);
                cache.sentences_dirty = true;
            }
            mark_stats_dirty(&mut cache, first_line, first_line + 1);
        }
    }

    /// Rescan the dirty lines and adjust the running totals, once per
    /// `edit_seq`. Line-count changes were already spliced into the line list
    /// by the rope helpers; anything else is caught by the length check.
    fn refresh_stats(&self) {
        let mut cache = self.stats_cache.borrow_mut();
        if cache.edit_seq == self.edit_seq {
            return;
        }

        let line_count = self.rope.len_lines();
        if cache.lines.len() != line_count {
            // A rope replacement slipped past the splicing helpers (or this
            // is the first fold): rebuild every line.
            cache.lines.clear();
            cache.lines.resize(line_count, CachedLineStats::default());
            cache.totals = StatsTotals::default();
            cache.sentences_dirty = false;
            cache.dirty = Some((0, line_count));
        }
        let dirty = cache
            .dirty
            .map(|(first, last)| (first.min(line_count), last.min(line_count)))
            .filter(|(first, last)| first < last);
        cache.dirty = None;

        // The document's last line carries no trailing newline; its text ends
        // at the rope end instead of one char earlier.
        let ends_with_newline =
            self.rope.len_chars() > 0 && self.rope.char(self.rope.len_chars() - 1) == '\n';
        let eof_line = if ends_with_newline {
            line_count
        } else {
            line_count.saturating_sub(1)
        };

        if let Some((first, last)) = dirty {
            self.fold_stats_range(&mut cache, first, last, eof_line);
        }
        if cache.sentences_dirty {
            refold_sentence_chain(&mut cache);
        }

        let totals = cache.totals;
        let word_count = totals.words;
        let reading_seconds = if word_count > 0 {
            ((word_count as f32 / 200.0) * 60.0).ceil() as usize
        } else {
            0
        };
        let pages = if word_count == 0 {
            0
        } else {
            word_count.div_ceil(250)
        };
        let empty = totals.chars == 0;
        cache.stats = crate::stats::WordStats {
            word_count,
            chars: totals.chars,
            chars_no_spaces: totals.chars.saturating_sub(totals.whitespace),
            paragraphs: if empty { 0 } else { totals.paragraphs },
            sentences: if empty {
                0
            } else {
                totals.sentences + usize::from(totals.end_open)
            },
            reading_seconds,
            pages,
        };
        cache.edit_seq = self.edit_seq;
    }

    /// Re-scan `[first, last)` from the rope and adjust the running totals.
    /// Only the sentence count depends on neighbouring lines (a chunk can
    /// span a line break), so when the range's exit state changes the chain
    /// is walked forward until it agrees with the old one again.
    fn fold_stats_range(&self, cache: &mut StatsCache, first: usize, last: usize, eof_line: usize) {
        let line_count = cache.lines.len();
        let entry_state = if first == 0 {
            false
        } else {
            cache.lines[first - 1].open_out
        };

        // Take back what the range contributed in the previous fold. When
        // entries were drained the whole chain is re-derived below instead.
        let mut old_open = entry_state;
        for index in first..last {
            let line = cache.lines[index];
            cache.totals.words -= line.words;
            cache.totals.chars -= line.chars;
            cache.totals.whitespace -= line.whitespace;
            cache.totals.paragraphs -= usize::from(line.occupied);
            if !cache.sentences_dirty {
                let (finished, open) = line.sentence[usize::from(old_open)];
                cache.totals.sentences -= finished;
                old_open = open;
            }
        }

        let mut new_open = entry_state;
        for index in first..last {
            let start = self.rope.line_to_char(index);
            let end = if index == eof_line {
                self.rope.len_chars()
            } else {
                self.rope.line_to_char(index + 1)
            };
            let text = if start >= end {
                String::new()
            } else {
                self.rope.slice(start..end).to_string()
            };
            let mut line = CachedLineStats::scan(&text);
            cache.totals.words += line.words;
            cache.totals.chars += line.chars;
            cache.totals.whitespace += line.whitespace;
            cache.totals.paragraphs += usize::from(line.occupied);
            if !cache.sentences_dirty {
                let (finished, open) = line.sentence[usize::from(new_open)];
                cache.totals.sentences += finished;
                new_open = open;
                line.open_out = open;
            }
            cache.lines[index] = line;
        }

        if cache.sentences_dirty {
            return;
        }

        // The range's exit state moved: keep walking the tail while the old
        // and new chains disagree, adjusting only the sentence count.
        let mut index = last;
        while index < line_count && old_open != new_open {
            let line = cache.lines[index];
            let (old_finished, old_next) = line.sentence[usize::from(old_open)];
            let (new_finished, new_next) = line.sentence[usize::from(new_open)];
            cache.totals.sentences += new_finished;
            cache.totals.sentences -= old_finished;
            cache.lines[index].open_out = new_next;
            old_open = old_next;
            new_open = new_next;
            index += 1;
        }
        if old_open != new_open {
            // The walk reached the document end: the trailing open chunk
            // changed, which adds or removes one sentence.
            cache.totals.end_open = new_open;
        }
    }

    /// Current whole-document statistics. Folds the dirty range once per
    /// edit; a cursor move or an idle frame reuses the result.
    pub fn stats(&self) -> crate::stats::WordStats {
        self.refresh_stats();
        self.stats_cache.borrow().stats.clone()
    }

    /// Open a new undo record capturing the cursor state and alignments.
    /// Every public mutation calls this first; rope ops logged afterwards land
    /// in the open record. Call `cancel_undo` if the mutation turns out to be
    /// a no-op.
    fn begin_undo(&mut self) {
        self.undo.push(UndoRecord {
            ops: Vec::new(),
            cursor: self.cursor,
            anchor: self.anchor,
            preferred_column: self.preferred_column,
            previous_cursor: self.previous_cursor,
            paragraph_alignments: self.paragraph_alignments.clone(),
            attrs: self.attrs.clone(),
        });
        if self.undo.len() > 256 {
            let overflow = self.undo.len() - 256;
            self.undo.drain(0..overflow);
        }
    }

    /// Drop the open record. For mutations that discovered they had nothing to
    /// change after opening it.
    fn cancel_undo(&mut self) {
        self.undo.pop();
    }

    /// Append an inverse op to the open record. Rope helpers call this; if no
    /// record is open the op is dropped, which only happens when undo and redo
    /// apply inverses through the raw rope (they always open one first).
    fn log_undo_op(&mut self, op: UndoOp) {
        if let Some(record) = self.undo.last_mut() {
            record.ops.push(op);
        }
    }

    /// Call after the rope is replaced wholesale, as in undo and redo: every
    /// entry is suspect, and `ensure_wrap_cache` reconciles the line count.
    fn mark_wrap_all_dirty(&mut self) {
        let mut cache = self.wrap_cache.borrow_mut();
        for entry in cache.entries.iter_mut() {
            entry.dirty = true;
        }
        cache.mark_all_dirty();
        self.edit_seq = self.edit_seq.wrapping_add(1);
    }

    fn prev_grapheme_start(&self, cursor: usize) -> usize {
        if cursor == 0 {
            return 0;
        }

        if self.rope.char(cursor - 1) == '\n' {
            return cursor - 1;
        }

        let line_index = self.rope.char_to_line(cursor.saturating_sub(1));
        let line_start = self.rope.line_to_char(line_index);
        let display_end = self.display_line_end(line_index);
        if cursor <= line_start {
            return if line_index == 0 {
                0
            } else {
                self.display_line_end(line_index - 1)
            };
        }

        let text = self.rope.slice(line_start..display_end).to_string();
        let local_cursor = cursor.min(display_end).saturating_sub(line_start);
        let mut last = 0usize;
        for (byte_index, grapheme) in text.grapheme_indices(true) {
            let char_index = text[..byte_index].chars().count();
            if char_index >= local_cursor {
                break;
            }
            last = char_index;
            let next = char_index + grapheme.chars().count();
            if next >= local_cursor {
                break;
            }
        }
        line_start + last
    }

    fn next_grapheme_end(&self, cursor: usize) -> usize {
        if cursor >= self.rope.len_chars() {
            return self.rope.len_chars();
        }

        let line_index = self.rope.char_to_line(cursor);
        let line_start = self.rope.line_to_char(line_index);
        let display_end = self.display_line_end(line_index);
        if cursor >= display_end {
            return if line_index + 1 < self.rope.len_lines() {
                self.rope.line_to_char(line_index + 1)
            } else {
                self.rope.len_chars()
            };
        }

        let text = self.rope.slice(line_start..display_end).to_string();
        let local_cursor = cursor.saturating_sub(line_start);
        for (byte_index, grapheme) in text.grapheme_indices(true) {
            let char_index = text[..byte_index].chars().count();
            let next = char_index + grapheme.chars().count();
            if char_index >= local_cursor {
                return line_start + next;
            }
            if next > local_cursor {
                return line_start + next;
            }
        }
        display_end
    }

    /// Last char of `line_index`, excluding the newline that ends it.
    ///
    /// Derived from the *next* line's start rather than from the length of this
    /// line's slice: `Rope::line(..).len_chars()` walks the text counting UTF-8
    /// sequences and builds a fresh slice to do it, so asking it per line made a
    /// single frame cost one pass over the whole document.
    fn display_line_end(&self, line_index: usize) -> usize {
        let total = self.rope.len_chars();
        let next = line_index + 1;
        if next >= self.rope.len_lines() {
            return total;
        }
        // Every line but the last is terminated by a newline, and that newline
        // is the char just before the following line begins.
        self.rope.line_to_char(next).saturating_sub(1)
    }

    fn prev_word_start(&self, cursor: usize) -> usize {
        let mut index = cursor.min(self.rope.len_chars());

        while index > 0 && classify_char(self.rope.char(index - 1)) == CharClass::Whitespace {
            index -= 1;
        }

        if index == 0 {
            return 0;
        }

        let class = classify_char(self.rope.char(index - 1));
        while index > 0 && classify_char(self.rope.char(index - 1)) == class {
            index -= 1;
        }

        index
    }

    fn next_word_start(&self, cursor: usize) -> usize {
        let len = self.rope.len_chars();
        let mut index = cursor.min(len);

        if index >= len {
            return len;
        }

        let class = classify_char(self.rope.char(index));
        if class == CharClass::Whitespace {
            while index < len && classify_char(self.rope.char(index)) == CharClass::Whitespace {
                index += 1;
            }
            return index;
        }

        while index < len && classify_char(self.rope.char(index)) == class {
            index += 1;
        }
        while index < len && classify_char(self.rope.char(index)) == CharClass::Whitespace {
            index += 1;
        }

        index
    }

    fn ensure_alignment_capacity(&mut self) {
        let lines = self.rope.len_lines();
        match self.paragraph_alignments.len().cmp(&lines) {
            std::cmp::Ordering::Less => self
                .paragraph_alignments
                .resize(lines, ParagraphAlignment::Left),
            std::cmp::Ordering::Greater => self.paragraph_alignments.truncate(lines),
            std::cmp::Ordering::Equal => {}
        }
    }

    /// Text of the source line holding the cursor, without its newline. What
    /// copy uses when nothing is selected: the whole line.
    pub fn current_line_text(&self) -> String {
        let line = self.current_paragraph_index();
        let start = self.rope.line_to_char(line);
        let end = self.display_line_end(line);
        self.rope.slice(start..end).to_string()
    }

    /// How many source lines the document holds. Sizes the line-number gutter.
    pub fn source_line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// Place the cursor with an explicit anchor for mouse drags. A collapsed
    /// anchor reads as no selection, matching keyboard behavior.
    pub fn set_cursor_with_anchor(&mut self, anchor: usize, cursor: usize) {
        let len = self.rope.len_chars();
        self.anchor = Some(anchor.min(len));
        self.cursor = cursor.min(len);
        self.preferred_column = None;
    }

    /// 1-based source line number holding `char_idx`, plus whether `char_idx`
    /// starts that source line (i.e. this visual line gets the gutter number
    /// rather than a blank continuation).
    pub fn source_line_info(&self, char_idx: usize) -> (usize, bool) {
        let clamped = char_idx.min(self.rope.len_chars());
        let line = self.rope.char_to_line(clamped);
        (line + 1, self.rope.line_to_char(line) == clamped)
    }

    fn current_paragraph_index(&self) -> usize {
        if self.rope.len_chars() == 0 {
            0
        } else {
            self.rope
                .char_to_line(self.cursor.min(self.rope.len_chars()))
        }
    }

    fn selected_paragraph_range(&self) -> (usize, usize) {
        if let Some((start, end)) = self.selection_range() {
            let start_line = self.rope.char_to_line(start.min(self.rope.len_chars()));
            let end_line = if end == 0 {
                0
            } else {
                self.rope.char_to_line(
                    end.saturating_sub(1)
                        .min(self.rope.len_chars().saturating_sub(1)),
                )
            };
            (start_line, end_line.max(start_line))
        } else {
            let current = self.current_paragraph_index();
            (current, current)
        }
    }
}

struct GraphemeInfo<'a> {
    /// Borrowed slice of the source line: wrapping never copies per grapheme.
    text: &'a str,
    chars: usize,
    /// True display width, as `UnicodeWidthStr` reports it.
    width: usize,
    /// Width used for fitting: a zero-width grapheme still occupies a cell.
    fit: usize,
    whitespace: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CharClass {
    Whitespace,
    Word,
    Punctuation,
}

fn classify_char(ch: char) -> CharClass {
    if ch.is_whitespace() {
        CharClass::Whitespace
    } else if ch.is_alphanumeric() || ch == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

/// Leading padding an alignment puts in front of a segment `width` display
/// columns wide. Shared by the wrap and by blank lines, which align as if
/// they were zero columns wide so the caret sits where text will start.
fn alignment_padding(alignment: ParagraphAlignment, width: usize, wrap_width: usize) -> usize {
    match alignment {
        ParagraphAlignment::Left | ParagraphAlignment::Justify => 0,
        ParagraphAlignment::Center => wrap_width.saturating_sub(width) / 2,
        ParagraphAlignment::Right => wrap_width.saturating_sub(width),
    }
}

/// Wrap one source line into the visual lines it occupies at `wrap_width`.
fn wrap_source_line(
    text: &str,
    start: usize,
    alignment: ParagraphAlignment,
    wrap_width: usize,
) -> Vec<WrappedLine> {
    let mut lines = Vec::new();
    if text.is_empty() {
        // A blank paragraph still hangs its caret where its first typed
        // character would land. Pinning it to column 0 left a centered
        // blank line looking left-aligned until something was typed.
        let column = alignment_padding(alignment, 0, wrap_width);
        lines.push(WrappedLine {
            start_char: start,
            end_char: start,
            text: String::new(),
            display_offsets: vec![0],
            boundaries: vec![Boundary {
                char_index: start,
                column,
            }],
        });
        return lines;
    }

    let graphemes = text
        .graphemes(true)
        .map(|grapheme| {
            let width = UnicodeWidthStr::width(grapheme);
            GraphemeInfo {
                text: grapheme,
                chars: grapheme.chars().count(),
                width,
                fit: width.max(1),
                whitespace: grapheme.chars().all(char::is_whitespace),
            }
        })
        .collect::<Vec<_>>();

    let mut char_prefix = Vec::with_capacity(graphemes.len() + 1);
    char_prefix.push(0);
    for grapheme in &graphemes {
        let next = char_prefix.last().copied().unwrap_or_default() + grapheme.chars;
        char_prefix.push(next);
    }

    let mut segment_start = 0usize;
    while segment_start < graphemes.len() {
        let mut segment_end = segment_start;
        let mut width_used = 0usize;
        let mut last_whitespace_fit = None;

        while segment_end < graphemes.len() {
            let next_width = width_used + graphemes[segment_end].fit;
            if next_width > wrap_width && segment_end > segment_start {
                break;
            }

            width_used = next_width;
            segment_end += 1;
            if graphemes[segment_end - 1].whitespace {
                last_whitespace_fit = Some(segment_end);
            }

            if next_width >= wrap_width {
                break;
            }
        }

        let chosen_end = if segment_end < graphemes.len() {
            last_whitespace_fit
                .filter(|value| *value > segment_start)
                .unwrap_or(segment_end)
        } else {
            segment_end
        };

        let chosen_end = chosen_end.max(segment_start + 1).min(graphemes.len());
        let start_char = start + char_prefix[segment_start];
        let end_char = start + char_prefix[chosen_end];
        let is_last_segment = chosen_end >= graphemes.len();
        let visible_end = if is_last_segment {
            chosen_end
        } else {
            trimmed_visible_end(&graphemes, segment_start, chosen_end)
        };
        let gap_indices = if alignment == ParagraphAlignment::Justify && !is_last_segment {
            graphemes[segment_start..visible_end]
                .iter()
                .enumerate()
                .filter_map(|(offset, grapheme)| grapheme.whitespace.then_some(offset))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let base_width = graphemes[segment_start..visible_end]
            .iter()
            .map(|grapheme| grapheme.fit)
            .sum::<usize>();
        let effective_alignment =
            if alignment == ParagraphAlignment::Justify && gap_indices.is_empty() {
                ParagraphAlignment::Left
            } else {
                alignment
            };
        let leading_padding = alignment_padding(effective_alignment, base_width, wrap_width);
        let extra_width = if effective_alignment == ParagraphAlignment::Justify {
            wrap_width.saturating_sub(base_width)
        } else {
            0
        };
        let mut boundaries = vec![Boundary {
            char_index: start_char,
            column: leading_padding,
        }];
        let mut text = " ".repeat(leading_padding);
        let mut display_offsets = vec![text.chars().count()];
        let mut display_char_count = text.chars().count();
        let mut column = leading_padding;
        for index in segment_start..chosen_end {
            let grapheme = &graphemes[index];
            let visible = index < visible_end;
            let display_width = if !visible {
                0
            } else if effective_alignment == ParagraphAlignment::Justify && grapheme.whitespace {
                let gap_position = gap_indices
                    .iter()
                    .position(|gap| *gap == (index - segment_start))
                    .unwrap_or(0);
                let gap_count = gap_indices.len().max(1);
                let expansion =
                    extra_width / gap_count + usize::from(gap_position < extra_width % gap_count);
                let spaces = grapheme.fit + expansion;
                for _ in 0..spaces {
                    text.push(' ');
                }
                display_char_count += spaces;
                spaces
            } else {
                text.push_str(grapheme.text);
                display_char_count += grapheme.chars;
                grapheme.width
            };
            for _ in 0..grapheme.chars {
                display_offsets.push(display_char_count);
            }
            column += display_width;
            boundaries.push(Boundary {
                char_index: start + char_prefix[index + 1],
                column,
            });
        }

        lines.push(WrappedLine {
            start_char,
            end_char,
            text,
            display_offsets,
            boundaries,
        });

        segment_start = chosen_end;
    }

    lines
}

fn trimmed_visible_end(graphemes: &[GraphemeInfo<'_>], start: usize, end: usize) -> usize {
    let mut visible_end = end;
    while visible_end > start && graphemes[visible_end - 1].whitespace {
        visible_end -= 1;
    }
    if visible_end == start {
        end
    } else {
        visible_end
    }
}

fn has_alignment_marker(tag: &str, keyword: &str) -> bool {
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
fn decode_xml_entities(text: &str) -> String {
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

fn parse_numeric_entity(body: &str) -> Option<String> {
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
fn html_block_inners(html: &str) -> Vec<String> {
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
fn inline_runs(inner: &str) -> Vec<(usize, bool, bool)> {
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
fn parse_inline_attrs(html: &str, rope: &ropey::Rope) -> Vec<AttrSpan> {
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

fn parse_paragraph_alignments(html: &str, line_count: usize) -> Vec<ParagraphAlignment> {
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

#[cfg(test)]
mod tests {
    use super::{expand_tabs, EditorBuffer, ParagraphAlignment, SearchOptions};

    #[test]
    fn search_options_cover_regex_word_and_case() {
        let buffer = EditorBuffer::from_text("The cat catalogs cats calmly");
        let plain = SearchOptions::default();
        let word = SearchOptions {
            whole_word: true,
            ..SearchOptions::default()
        };
        let case = SearchOptions {
            case_sensitive: true,
            ..SearchOptions::default()
        };
        let regex = SearchOptions {
            regex: true,
            ..SearchOptions::default()
        };

        // Plain is case-insensitive substring: "cat" hits cat, catalogs, cats.
        assert_eq!(buffer.find_matches_with("cat", &plain).unwrap().len(), 3);
        // Whole word narrows to the standalone word.
        let word_hits = buffer.find_matches_with("cat", &word).unwrap();
        assert_eq!(word_hits.len(), 1);
        assert_eq!(&buffer.plain_text()[word_hits[0].0..word_hits[0].1], "cat");
        // Case-sensitive misses the capital-less... "The" vs "the": only exact.
        assert!(buffer.find_matches_with("the", &case).unwrap().is_empty());
        assert_eq!(buffer.find_matches_with("The", &case).unwrap().len(), 1);
        // Regex character classes and anchors.
        let regex_hits = buffer.find_matches_with(r"\bcat\w*\b", &regex).unwrap();
        assert_eq!(regex_hits.len(), 3);
        // Captures expand in the replacement.
        let mut buffer = EditorBuffer::from_text("2026-10-05");
        let count = buffer
            .replace_all_with(r"(\d+)-(\d+)-(\d+)", "$3/$2/$1", &regex)
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(buffer.plain_text(), "05/10/2026");
        // A broken pattern is an error, never a panic or a silent no-op.
        assert!(buffer.find_matches_with("(", &regex).is_err());
        // Plain mode keeps `$` literal.
        let mut buffer = EditorBuffer::from_text("price five");
        let count = buffer.replace_all_with("five", "$5", &plain).unwrap();
        assert_eq!(count, 1);
        assert_eq!(buffer.plain_text(), "price $5");
    }

    #[test]
    fn incremental_stats_match_the_reference_scan() {
        let documents = [
            "",
            "hello",
            "alpha beta.\n\ngamma delta!",
            "one\ntwo\nthree",
            "one.\n.two",
            "a. . .b",
            "trailing newline\n",
            "\n\n\n",
            "  spaced   out  words \n second line",
            "unicode ✓ words…\n日本語 text.",
        ];
        for text in documents {
            let mut buffer = EditorBuffer::from_text(text);
            assert_eq!(
                buffer.stats(),
                crate::stats::compute_word_stats(&buffer.plain_text()),
                "fresh stats for {text:?}"
            );

            // Insert at the very top: every later line moves, so the per-line
            // list has to stay aligned or the counts drift.
            buffer.move_to_top();
            buffer.insert_text("INSERTED\n");
            assert_eq!(
                buffer.stats(),
                crate::stats::compute_word_stats(&buffer.plain_text()),
                "after top insert for {text:?}"
            );

            // Delete across the first line break and compare once more.
            buffer.move_to_top();
            for _ in 0..8 {
                buffer.delete_forward();
            }
            assert_eq!(
                buffer.stats(),
                crate::stats::compute_word_stats(&buffer.plain_text()),
                "after delete for {text:?}"
            );
        }
    }

    #[test]
    fn incremental_stats_survive_splices_and_joins() {
        let originals = [
            "alpha beta. gamma delta! epsilon? zeta",
            "one\n.two\n\nthree. \n four",
            "a. . .b\nc\nd. e",
        ];
        for original in originals {
            let mut buffer = EditorBuffer::from_text(original);
            // Split at every other character, left to right: each one splices
            // a line entry into the stats cache.
            let mut offset = 1;
            while offset < buffer.char_count() {
                buffer.set_cursor_with_anchor(offset, offset);
                buffer.insert_text("\n");
                offset += 2;
                assert_eq!(
                    buffer.stats(),
                    crate::stats::compute_word_stats(&buffer.plain_text()),
                    "after splitting {original:?} at {offset}"
                );
            }
            // Now join again: the break before a line is backspaced away,
            // which merges entries and hands the sentence chain to the full
            // refold.
            let mut guard = 0;
            while buffer.line_count() > 1 && guard < 400 {
                guard += 1;
                let Some(break_at) = buffer.plain_text().find('\n') else {
                    break;
                };
                buffer.set_cursor_with_anchor(break_at + 1, break_at + 1);
                buffer.backspace();
                assert_eq!(
                    buffer.stats(),
                    crate::stats::compute_word_stats(&buffer.plain_text()),
                    "after joining {original:?}"
                );
            }
        }
    }

    #[test]
    fn incremental_stats_survive_random_edits() {
        let mut buffer = EditorBuffer::from_text(
            "One sentence. Two more!\n\nNew paragraph, with words? Yes.\nlast line",
        );
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for step in 0..400 {
            let len = buffer.char_count();
            let offset = (next() as usize) % (len + 1);
            buffer.set_cursor_with_anchor(offset, offset);
            match next() % 3 {
                0 => buffer.insert_text("x"),
                1 => buffer.insert_text(" .y\n"),
                _ => {
                    buffer.delete_forward();
                }
            }
            assert_eq!(
                buffer.stats(),
                crate::stats::compute_word_stats(&buffer.plain_text()),
                "step {step}"
            );
        }
    }

    #[test]
    fn undo_and_redo_keep_the_incremental_stats_honest() {
        let mut buffer = EditorBuffer::from_text("one two three");
        for ch in " and more".chars() {
            buffer.insert_char_with_mode(ch, false);
        }
        let typed = buffer.stats();
        assert!(buffer.undo());
        assert_eq!(
            buffer.stats(),
            crate::stats::compute_word_stats(&buffer.plain_text())
        );
        assert!(buffer.redo());
        assert_eq!(buffer.stats(), typed);
        assert_eq!(
            buffer.stats(),
            crate::stats::compute_word_stats(&buffer.plain_text())
        );
    }

    #[test]
    fn source_line_info_numbers_lines_and_spots_continuations() {
        let buffer = EditorBuffer::from_text("short\na much longer line that wraps\nend");
        // Line 1 starts at 0 and is its own visual line start.
        assert_eq!(buffer.source_line_info(0), (1, true));
        // Line 2 starts after "short\n" (6 chars).
        assert_eq!(buffer.source_line_info(6), (2, true));
        assert_eq!(buffer.source_line_info(7), (2, false));
        let last_start = buffer.plain_text().rfind("end").unwrap();
        assert_eq!(buffer.source_line_info(last_start), (3, true));
        assert_eq!(buffer.source_line_count(), 3);
    }

    /// The wrap cache only pays off if it agrees with a cold wrap, so compare
    /// the two after every edit rather than trusting the memo.
    ///
    /// The reference buffer is rebuilt from the same text *and* the same
    /// paragraph alignments, because alignment changes how a line wraps and
    /// `from_text` alone would quietly default every line to left.
    fn assert_cache_matches_cold(buffer: &EditorBuffer, width: usize) {
        let cached = buffer.layout(width);
        let mut cold = EditorBuffer::from_text(&buffer.plain_text());
        cold.paragraph_alignments = buffer.paragraph_alignments.clone();
        cold.ensure_alignment_capacity();
        let cold = cold.layout(width);
        assert_eq!(
            cached.lines.len(),
            cold.lines.len(),
            "cached and cold wrap disagree on line count at width {width}"
        );
        for (index, (left, right)) in cached.lines.iter().zip(cold.lines.iter()).enumerate() {
            assert_eq!(
                left.start_char, right.start_char,
                "start_char on line {index}"
            );
            assert_eq!(left.end_char, right.end_char, "end_char on line {index}");
            assert_eq!(left.text, right.text, "text on line {index}");
            assert_eq!(
                left.display_offsets, right.display_offsets,
                "display_offsets on line {index}"
            );
            assert_eq!(
                left.boundaries, right.boundaries,
                "boundaries on line {index}"
            );
        }
        assert_eq!(buffer.total_visual_lines(width), cold.lines.len());
    }

    #[test]
    fn wrap_cache_survives_edits_at_the_top_of_a_long_document() {
        let mut buffer = EditorBuffer::from_text(
            &(0..200)
                .map(|index| format!("line {index} with enough words to wrap at most widths"))
                .collect::<Vec<_>>()
                .join("\n"),
        );

        assert_cache_matches_cold(&buffer, 40);

        // An edit near the top shifts every later line's char offsets, which is
        // the case that forces the cache to re-point rather than reuse.
        buffer.move_to_top();
        buffer.insert_text("a new opening line\n");
        assert_cache_matches_cold(&buffer, 40);
        assert_cache_matches_cold(&buffer, 80);

        buffer.move_to_top();
        buffer.delete_line();
        assert_cache_matches_cold(&buffer, 40);
    }

    /// The cache-direct movement helpers must give the same answers as the
    /// full-document `layout()` they replaced, including tabs, wrapped lines,
    /// and non-left alignment. `layout()` is the reference; the helpers are
    /// what the editor actually calls.
    #[test]
    fn cache_direct_movement_matches_full_layout() {
        let text = "a short line\n\tindented with a tab and words that wrap around the width\n\nlast line here";
        for width in [8usize, 20, 88] {
            let probe = EditorBuffer::from_text(text);
            let layout = probe.layout(width);
            let len = probe.char_count();

            // Home / End land on the wrapped line's edges.
            for cursor in (0..=len).step_by(3) {
                let mut buffer = EditorBuffer::from_text(text);
                buffer.set_cursor_for_test(cursor);
                let index = layout.line_index_for_cursor(cursor);
                let expected_start = layout.lines[index].start_char;
                let expected_end = layout.lines[index].end_char;
                buffer.move_home(width, false);
                assert_eq!(
                    buffer.cursor(),
                    expected_start,
                    "home at {cursor} w={width}"
                );
                buffer.set_cursor_for_test(cursor);
                buffer.move_end(width, false);
                assert_eq!(buffer.cursor(), expected_end, "end at {cursor} w={width}");
            }

            // Up / down track the preferred column through the same lines.
            for cursor in (0..=len).step_by(7) {
                let mut buffer = EditorBuffer::from_text(text);
                buffer.set_cursor_for_test(cursor);
                let index = layout.line_index_for_cursor(cursor);
                let column = layout.column_for_cursor(index, cursor);
                let next = (index + 1).min(layout.lines.len().saturating_sub(1));
                let expected = layout.cursor_for_column(next, column);
                buffer.move_down(width, false);
                assert_eq!(buffer.cursor(), expected, "down at {cursor} w={width}");

                let mut buffer = EditorBuffer::from_text(text);
                buffer.set_cursor_for_test(cursor);
                let prev = index.saturating_sub(1);
                let expected = layout.cursor_for_column(prev, column);
                buffer.move_up(width, false);
                assert_eq!(buffer.cursor(), expected, "up at {cursor} w={width}");
            }

            // Line/col readout, go-to-line, and screen jumps agree.
            for cursor in (0..=len).step_by(5) {
                let buffer = {
                    let mut buffer = EditorBuffer::from_text(text);
                    buffer.set_cursor_for_test(cursor);
                    buffer
                };
                let index = layout.line_index_for_cursor(cursor);
                assert_eq!(
                    buffer.cursor_line_and_col(width),
                    (index + 1, layout.column_for_cursor(index, cursor) + 1),
                    "line/col at {cursor} w={width}"
                );
            }
            for visual in 0..layout.lines.len() {
                let mut buffer = EditorBuffer::from_text(text);
                buffer.move_to_line(visual + 1, width);
                assert_eq!(
                    buffer.cursor(),
                    layout.lines[visual].start_char,
                    "go-to-line {} w={width}",
                    visual + 1
                );
            }
            let mut buffer = EditorBuffer::from_text(text);
            buffer.move_to_screen_bottom(0, 2, width);
            assert_eq!(
                buffer.cursor(),
                layout.lines[1.min(layout.lines.len() - 1)].start_char,
                "screen bottom w={width}"
            );

            // Delete-to-end removes exactly the wrapped line's tail.
            for cursor in (0..=len).step_by(11) {
                let mut buffer = EditorBuffer::from_text(text);
                buffer.set_cursor_for_test(cursor);
                let index = layout.line_index_for_cursor(cursor);
                let end = layout.lines[index].end_char;
                let newline = probe.plain_text().chars().nth(cursor) == Some('\n');
                let changed = buffer.delete_to_end_of_line(width);
                if end > cursor {
                    assert!(changed, "expected a deletion at {cursor} w={width}");
                    assert_eq!(
                        buffer.cursor(),
                        cursor,
                        "cursor stays put at {cursor} w={width}"
                    );
                    assert_eq!(
                        buffer.char_count(),
                        len - (end - cursor),
                        "deleted tail at {cursor} w={width}"
                    );
                } else if newline && cursor < len {
                    assert!(changed, "expected newline eat at {cursor} w={width}");
                } else {
                    assert!(!changed, "expected no-op at {cursor} w={width}");
                }
            }
        }
    }

    #[test]
    fn wrap_cache_tracks_in_place_edits_that_do_not_change_the_line_count() {
        // Overwriting a character in place keeps the char and line counts the
        // same, so only the content check can notice the cache went stale.
        let mut buffer = EditorBuffer::from_text("alpha beta gamma\ndelta epsilon");
        assert_cache_matches_cold(&buffer, 20);

        buffer.select_all();
        assert!(buffer.delete_block());
        assert_cache_matches_cold(&buffer, 20);

        buffer.insert_text("one two three four five six seven eight nine ten");
        assert_cache_matches_cold(&buffer, 20);
        buffer.cycle_case();
        assert_cache_matches_cold(&buffer, 20);
    }

    #[test]
    fn wrap_cache_notices_realigning_a_paragraph() {
        let mut buffer = EditorBuffer::from_text("a short line\nanother short line");
        assert_cache_matches_cold(&buffer, 20);

        buffer.select_all();
        assert!(buffer.set_alignment_for_selection(ParagraphAlignment::Justify));
        assert_cache_matches_cold(&buffer, 20);

        assert!(buffer.set_alignment_for_selection(ParagraphAlignment::Center));
        assert_cache_matches_cold(&buffer, 20);
    }

    #[test]
    fn tabs_advance_to_the_eight_column_grid() {
        // A tab is display sugar: the buffer keeps the raw byte.
        assert_eq!(expand_tabs("a\tb"), "a       b");
        assert_eq!(expand_tabs("\tx"), "        x");
        assert_eq!(expand_tabs("12345678\tx"), "12345678        x");
        assert_eq!(expand_tabs("1234567\tx"), "1234567 x");
        assert_eq!(expand_tabs("no tabs"), "no tabs");
        assert_eq!(expand_tabs("a\n\tx"), "a\n        x");

        // The indent reader shares the same grid.
        let buffer = EditorBuffer::from_text("\tx");
        assert_eq!(buffer.line_indent_columns(0), 8);
    }

    #[test]
    fn wrap_cache_compares_expanded_tabs() {
        let mut buffer = EditorBuffer::from_text("no tabs here\n\tindented with a tab");
        assert_cache_matches_cold(&buffer, 12);

        buffer.move_to_top();
        buffer.insert_text("\t\t");
        assert_cache_matches_cold(&buffer, 12);
    }

    #[test]
    fn visible_layout_matches_the_full_wrap_it_slices() {
        let buffer = EditorBuffer::from_text(
            &(0..120)
                .map(|index| format!("paragraph {index} holding a moderate amount of prose"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let width = 30;
        let full = buffer.layout(width);
        assert!(
            full.total_lines() > 10,
            "test needs a document that wraps a lot"
        );

        for first in [0usize, 1, 7, 40] {
            let window = buffer.visible_layout(width, first, 5, 0);
            let expected =
                &full.lines[first.min(full.lines.len())..(first + 5).min(full.lines.len())];
            let actual: Vec<_> = window.lines.iter().collect();
            let expected: Vec<_> = expected.iter().collect();
            assert_eq!(
                actual, expected,
                "window at {first} does not match the full wrap"
            );
            assert_eq!(window.total_lines(), full.total_lines());
        }
    }

    #[test]
    fn visual_line_for_cursor_agrees_with_the_full_layout() {
        let buffer = EditorBuffer::from_text(
            &(0..80)
                .map(|index| format!("row {index} wrapping across more than one visual line"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let width = 24;
        let full = buffer.layout(width);

        for cursor in 0..buffer.char_count() {
            assert_eq!(
                buffer.visual_line_for_cursor(width, cursor),
                full.line_index_for_cursor(cursor),
                "disagreement at char {cursor}"
            );
        }
    }

    #[test]
    fn vertical_movement_works_through_a_windowed_layout() {
        let mut buffer = EditorBuffer::from_text(
            &(0..60)
                .map(|index| format!("line {index} long enough to wrap across several rows"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let width = 25;
        let start = buffer.cursor();

        buffer.move_down(width, false);
        assert!(
            buffer.cursor() > start,
            "down should move the cursor forward"
        );

        buffer.move_up(width, false);
        assert_eq!(buffer.cursor(), start, "up should undo down");

        // A long run of down presses must not run off the end of the document.
        for _ in 0..500 {
            buffer.move_down(width, false);
        }
        let bottom = buffer.cursor();
        for _ in 0..500 {
            buffer.move_down(width, false);
        }
        assert_eq!(buffer.cursor(), bottom, "down at the end should stay put");

        for _ in 0..500 {
            buffer.move_up(width, false);
        }
        assert_eq!(
            buffer.cursor(),
            0,
            "up should reach the start of the document"
        );
    }

    #[test]
    fn insert_undo_and_redo_round_trip() {
        let mut buffer = EditorBuffer::new();

        buffer.insert_text("hello");
        assert_eq!(buffer.plain_text(), "hello");

        assert!(buffer.undo());
        assert_eq!(buffer.plain_text(), "");

        assert!(buffer.redo());
        assert_eq!(buffer.plain_text(), "hello");
    }

    /// Every mutation must round-trip through change-based undo and redo:
    /// text, cursor, selection anchor, and paragraph alignments all come back.
    /// This is the test that would catch a mutation bypassing the op log.
    #[test]
    fn every_mutation_round_trips_through_undo_and_redo() {
        let original = "first line here\n    second indented with spaces\nthird line here";
        let width = 20;

        let mut buffer = EditorBuffer::from_text(original);
        buffer.select_all();
        assert!(buffer.set_alignment_for_selection(ParagraphAlignment::Center));
        buffer.move_to_top();
        let aligned_html = buffer.to_ord_html();

        // Each entry positions the cursor first (`setup`), then performs one
        // user-visible mutation. `before_cursor` is captured between the two,
        // which is exactly what the undo record sees.
        type BufferSetup = Box<dyn Fn(&mut EditorBuffer)>;
        type BufferMutation<'a> = Box<dyn Fn(&mut EditorBuffer) -> bool + 'a>;
        let mutations: Vec<(&str, BufferSetup, BufferMutation<'_>)> = vec![
            (
                "type",
                Box::new(|_| {}),
                Box::new(|b| {
                    b.insert_text("X");
                    true
                }),
            ),
            (
                "newline",
                Box::new(|_| {}),
                Box::new(|b| b.insert_newline_with_indent()),
            ),
            (
                "backspace",
                Box::new(|b| {
                    b.move_end(80, false);
                }),
                Box::new(|b| b.backspace()),
            ),
            (
                "delete",
                Box::new(|b| {
                    b.move_to_top();
                }),
                Box::new(|b| b.delete_forward()),
            ),
            (
                "delete word",
                Box::new(|b| {
                    b.move_to_top();
                }),
                Box::new(|b| b.delete_word_forward()),
            ),
            (
                "delete line",
                Box::new(|b| {
                    b.move_to_top();
                }),
                Box::new(|b| b.delete_line()),
            ),
            (
                "delete to end",
                Box::new(|b| {
                    b.move_to_top();
                }),
                Box::new(|b| b.delete_to_end_of_line(width)),
            ),
            (
                "indent",
                Box::new(|b| {
                    b.move_to_top();
                }),
                Box::new(|b| b.indent()),
            ),
            (
                "outdent",
                Box::new(|b| {
                    // The second line starts life tab-indented, so outdent has
                    // something to remove. (`move_to_line` takes visual lines.)
                    b.move_to_line(2, 80);
                }),
                Box::new(|b| b.outdent()),
            ),
            (
                "open line",
                Box::new(|b| {
                    b.move_to_top();
                }),
                Box::new(|b| b.open_line()),
            ),
            (
                "case",
                Box::new(|b| {
                    b.select_all();
                }),
                Box::new(|b| b.cycle_case()),
            ),
            (
                "block move",
                Box::new(|b| {
                    b.set_block_begin();
                    b.move_end(80, true);
                }),
                Box::new(|b| b.move_block()),
            ),
            (
                "block copy",
                Box::new(|b| {
                    b.set_block_begin();
                    b.move_end(80, true);
                }),
                Box::new(|b| b.copy_block()),
            ),
            (
                "block delete",
                Box::new(|b| {
                    b.set_block_begin();
                    b.move_end(80, true);
                }),
                Box::new(|b| b.delete_block()),
            ),
            (
                "replace all",
                Box::new(|_| {}),
                Box::new(|b| {
                    b.replace_all_with("line", "row", &SearchOptions::default())
                        .map(|count| count > 0)
                        .unwrap_or(false)
                }),
            ),
            (
                "align",
                Box::new(|b| {
                    b.select_all();
                }),
                Box::new(|b| b.set_alignment_for_selection(ParagraphAlignment::Right)),
            ),
        ];

        for (name, setup, mutate) in mutations {
            let mut buffer = EditorBuffer::from_text(original);
            buffer.select_all();
            assert!(buffer.set_alignment_for_selection(ParagraphAlignment::Center));
            buffer.move_to_top();
            setup(&mut buffer);
            let before_text = buffer.plain_text();
            let before_html = buffer.to_ord_html();
            let before_cursor = buffer.cursor();

            assert!(mutate(&mut buffer), "{name} reported no change");
            let after_text = buffer.plain_text();
            let after_html = buffer.to_ord_html();

            assert!(buffer.undo(), "{name} has nothing to undo");
            assert_eq!(buffer.plain_text(), before_text, "{name} undo text");
            assert_eq!(buffer.to_ord_html(), before_html, "{name} undo html");
            assert_eq!(buffer.cursor(), before_cursor, "{name} undo cursor");
            assert_cache_matches_cold(&buffer, width);

            assert!(buffer.redo(), "{name} has nothing to redo");
            assert_eq!(buffer.plain_text(), after_text, "{name} redo text");
            assert_eq!(buffer.to_ord_html(), after_html, "{name} redo html");
            assert_cache_matches_cold(&buffer, width);

            // A second undo after the redo lands back at the start, so the
            // redo record captured the full forward change.
            assert!(buffer.undo(), "{name} second undo failed");
            assert_eq!(buffer.plain_text(), before_text, "{name} re-undo text");
            let _ = aligned_html;
        }
    }

    #[test]
    fn select_all_and_backspace_clears_the_document() {
        let mut buffer = EditorBuffer::from_text("alpha beta");

        buffer.select_all();
        assert!(buffer.backspace());
        assert_eq!(buffer.plain_text(), "");
    }

    #[test]
    fn wrapping_prefers_word_boundaries_when_space_allows() {
        let buffer = EditorBuffer::from_text("alpha beta gamma");
        let layout = buffer.layout(6);
        let lines = layout
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>();

        assert_eq!(lines, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn trailing_space_stays_visible_on_the_last_visual_line() {
        let mut buffer = EditorBuffer::from_text("alpha");
        buffer.move_end(20, false);
        buffer.insert_text(" ");

        let layout = buffer.layout(20);

        assert_eq!(layout.lines[0].text, "alpha ");
        assert_eq!(layout.column_for_cursor(0, buffer.cursor()), 6);
    }

    #[test]
    fn enter_carries_the_current_indentation_to_the_new_line() {
        let mut buffer = EditorBuffer::from_text("    alpha");
        buffer.move_end(80, false);

        buffer.insert_newline_with_indent();

        assert_eq!(buffer.plain_text(), "    alpha\n    ");
        assert_eq!(buffer.cursor(), 14);
    }

    #[test]
    fn enter_on_an_unindented_line_adds_no_padding() {
        let mut buffer = EditorBuffer::from_text("alpha");
        buffer.move_end(80, false);

        buffer.insert_newline_with_indent();

        assert_eq!(buffer.plain_text(), "alpha\n");
        assert_eq!(buffer.cursor(), 6);
    }

    #[test]
    fn tab_indents_a_single_line_when_nothing_is_selected() {
        let mut buffer = EditorBuffer::from_text("alpha");

        assert!(buffer.indent());

        assert_eq!(buffer.plain_text(), "    alpha");
        assert_eq!(buffer.cursor(), 4);
    }

    #[test]
    fn tab_indents_every_line_of_a_multiline_selection() {
        let mut buffer = EditorBuffer::from_text("alpha\nbeta\ngamma");
        buffer.move_to_top();
        buffer.move_down(80, true);
        buffer.move_right(true);
        assert!(buffer.selection_range().is_some());

        assert!(buffer.indent());

        assert_eq!(buffer.plain_text(), "    alpha\n    beta\ngamma");
    }

    #[test]
    fn outdent_strips_one_level_from_the_current_line() {
        let mut buffer = EditorBuffer::from_text("        alpha");
        buffer.move_home(80, false);

        assert!(buffer.outdent());

        assert_eq!(buffer.plain_text(), "    alpha");
    }

    #[test]
    fn outdent_is_a_no_op_when_the_line_has_no_leading_spaces() {
        let mut buffer = EditorBuffer::from_text("alpha");
        buffer.move_home(80, false);

        assert!(!buffer.outdent());

        assert_eq!(buffer.plain_text(), "alpha");
    }

    #[test]
    fn indent_then_outdent_returns_the_text_to_where_it_started() {
        let original = "alpha\n    beta\ngamma";
        let mut buffer = EditorBuffer::from_text(original);
        buffer.move_to_top();
        buffer.select_all();

        assert!(buffer.indent());
        assert_eq!(buffer.plain_text(), "    alpha\n        beta\n    gamma");

        assert!(buffer.outdent());
        assert_eq!(buffer.plain_text(), original);
    }

    #[test]
    fn cycling_case_walks_through_all_four_styles() {
        let mut buffer = EditorBuffer::from_text("hello wide world");
        buffer.move_to_top();
        buffer.select_all();

        // All-lowercase is its own style, so it advances to Capitalize first.
        assert!(buffer.cycle_case());
        assert_eq!(buffer.plain_text(), "Hello Wide World");

        assert!(buffer.cycle_case());
        assert_eq!(buffer.plain_text(), "Hello wide world");

        assert!(buffer.cycle_case());
        assert_eq!(buffer.plain_text(), "HELLO WIDE WORLD");

        assert!(buffer.cycle_case());
        assert_eq!(buffer.plain_text(), "hello wide world");
    }

    #[test]
    fn cycling_case_does_not_stall_on_a_single_capitalized_word() {
        let mut buffer = EditorBuffer::from_text("Hello");
        buffer.move_to_top();
        buffer.select_all();

        // One capitalized word reads as both title and sentence case, so it
        // keeps cycling instead of stalling.
        assert!(buffer.cycle_case());
        assert_eq!(buffer.plain_text(), "HELLO");
    }

    #[test]
    fn toggling_bold_styles_and_unstyles_a_selection() {
        use super::{AttrKind, AttrSpan};
        let mut buffer = EditorBuffer::from_text("hello world");
        buffer.move_to_top();
        buffer.select_all();

        assert!(buffer.toggle_attr(AttrKind::Bold));
        assert_eq!(
            buffer.attrs,
            vec![AttrSpan {
                start: 0,
                end: 11,
                kind: AttrKind::Bold,
            }]
        );

        // Fully styled: toggling again removes it.
        assert!(buffer.toggle_attr(AttrKind::Bold));
        assert!(buffer.attrs.is_empty());
    }

    #[test]
    fn toggling_styles_a_partially_styled_selection() {
        use super::{AttrKind, AttrSpan};
        let mut buffer = EditorBuffer::from_text("hello world");
        buffer.move_to_top();
        // Select "hello", bold it, then select all and toggle: the whole
        // range ends up bold.
        buffer.anchor = Some(0);
        buffer.cursor = 5;
        assert!(buffer.toggle_attr(AttrKind::Bold));
        buffer.select_all();
        assert!(buffer.toggle_attr(AttrKind::Bold));
        assert_eq!(
            buffer.attrs,
            vec![AttrSpan {
                start: 0,
                end: 11,
                kind: AttrKind::Bold,
            }]
        );
    }

    #[test]
    fn bold_and_italic_overlap_freely() {
        use super::{AttrKind, AttrSpan};
        let mut buffer = EditorBuffer::from_text("abcdef");
        buffer.move_to_top();
        buffer.anchor = Some(0);
        buffer.cursor = 4;
        assert!(buffer.toggle_attr(AttrKind::Bold));
        buffer.anchor = Some(2);
        buffer.cursor = 6;
        assert!(buffer.toggle_attr(AttrKind::Italic));
        assert_eq!(
            buffer.attrs,
            vec![
                AttrSpan {
                    start: 0,
                    end: 4,
                    kind: AttrKind::Bold,
                },
                AttrSpan {
                    start: 2,
                    end: 6,
                    kind: AttrKind::Italic,
                },
            ]
        );
        // Rows report both dimensions where they cross.
        let runs = buffer.attr_runs_in(0, 6);
        assert_eq!(
            runs,
            vec![(0, 2, true, false), (2, 4, true, true), (4, 6, false, true),]
        );
    }

    #[test]
    fn toggling_needs_a_selection() {
        use super::AttrKind;
        let mut buffer = EditorBuffer::from_text("hello");
        assert!(!buffer.toggle_attr(AttrKind::Bold));
        assert!(buffer.attrs.is_empty());
    }

    #[test]
    fn typing_inside_a_styled_range_inherits_the_style() {
        use super::{AttrKind, AttrSpan};
        let mut buffer = EditorBuffer::from_text("hello world");
        buffer.move_to_top();
        buffer.select_all();
        assert!(buffer.toggle_attr(AttrKind::Bold));
        // Insert inside the range: it grows.
        buffer.collapse_selection();
        buffer.cursor = 5;
        buffer.insert_char_with_mode('X', false);
        assert_eq!(
            buffer.attrs,
            vec![AttrSpan {
                start: 0,
                end: 12,
                kind: AttrKind::Bold,
            }]
        );
        // Insert exactly at its start: the new char stays plain.
        buffer.cursor = 0;
        buffer.insert_char_with_mode('Y', false);
        assert_eq!(
            buffer.attrs,
            vec![AttrSpan {
                start: 1,
                end: 13,
                kind: AttrKind::Bold,
            }]
        );
        assert_eq!(buffer.plain_text(), "YhelloX world");
    }

    #[test]
    fn deleting_trims_and_drops_styled_ranges() {
        use super::{AttrKind, AttrSpan};
        let mut buffer = EditorBuffer::from_text("aabbcc");
        buffer.move_to_top();
        buffer.anchor = Some(0);
        buffer.cursor = 6;
        assert!(buffer.toggle_attr(AttrKind::Bold));
        // Delete the middle: the range shrinks around the gap.
        buffer.anchor = Some(2);
        buffer.cursor = 4;
        assert!(buffer.delete_block());
        assert_eq!(buffer.plain_text(), "aacc");
        assert_eq!(
            buffer.attrs,
            vec![AttrSpan {
                start: 0,
                end: 4,
                kind: AttrKind::Bold,
            }]
        );
        // Delete everything styled: the span goes away, not empty.
        buffer.select_all();
        assert!(buffer.delete_block());
        assert!(buffer.attrs.is_empty());
    }

    #[test]
    fn undo_restores_styled_ranges() {
        use super::AttrKind;
        let mut buffer = EditorBuffer::from_text("hello");
        buffer.move_to_top();
        buffer.select_all();
        assert!(buffer.toggle_attr(AttrKind::Bold));
        assert!(!buffer.attrs.is_empty());
        assert!(buffer.undo());
        assert!(buffer.attrs.is_empty());
        assert!(buffer.redo());
        assert!(!buffer.attrs.is_empty());
    }

    #[test]
    fn ord_html_round_trips_bold_and_italic() {
        use super::AttrKind;
        let mut buffer = EditorBuffer::from_text("plain bold both");
        buffer.move_to_top();
        // "bold": chars 6..10.
        buffer.anchor = Some(6);
        buffer.cursor = 10;
        assert!(buffer.toggle_attr(AttrKind::Bold));
        // "bold both": chars 6..15 italic, crossing the bold range.
        buffer.anchor = Some(6);
        buffer.cursor = 15;
        assert!(buffer.toggle_attr(AttrKind::Italic));

        let html = buffer.to_ord_html();
        let reloaded = EditorBuffer::from_rich_text("plain bold both", &html);
        assert_eq!(reloaded.attrs, buffer.attrs);
        assert_eq!(reloaded.plain_text(), "plain bold both");
    }

    #[test]
    fn ord_html_parsing_survives_entities_and_unknown_tags() {
        let html = "<p>fish &amp; <b>chips</b> <u>yum</u></p><p>second</p>";
        let buffer = EditorBuffer::from_rich_text("fish & chips yum\nsecond", html);
        assert_eq!(buffer.plain_text(), "fish & chips yum\nsecond");
        // "chips" starts after "fish & " (7 chars).
        let bolds: Vec<(usize, usize)> = buffer
            .attrs
            .iter()
            .filter(|sp| sp.kind == super::AttrKind::Bold)
            .map(|sp| (sp.start, sp.end))
            .collect();
        assert_eq!(bolds, vec![(7, 12)]);
    }

    #[test]
    fn sorting_selected_lines_orders_them_case_insensitively() {
        let mut buffer = EditorBuffer::from_text("pear\napple\nFig");
        buffer.move_to_top();
        buffer.select_all();

        assert!(buffer.sort_selected_lines());
        assert_eq!(buffer.plain_text(), "apple\nFig\npear");

        assert!(!buffer.sort_selected_lines());
    }

    #[test]
    fn sorting_selected_lines_needs_more_than_one_line() {
        let mut buffer = EditorBuffer::from_text("only\n");
        buffer.move_to_top();
        buffer.select_all();

        assert!(!buffer.sort_selected_lines());
        assert_eq!(buffer.plain_text(), "only\n");
    }

    #[test]
    fn climbing_up_a_long_file_never_moves_down() {
        let text = (0..500)
            .map(|i| format!("line {i:03} with some words to wrap around"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut buffer = EditorBuffer::from_text(&text);
        buffer.move_cursor_to_end();

        let mut previous = buffer.cursor();
        for _ in 0..600 {
            buffer.move_up(60, false);
            let cursor = buffer.cursor();
            assert!(
                cursor <= previous,
                "cursor snapped down from {previous} to {cursor}"
            );
            previous = cursor;
        }
        // Column memory keeps the column on the top row (standard: Home or
        // Left reaches char 0 from here), so assert row membership, not 0.
        assert_eq!(buffer.visual_line_for_cursor(60, buffer.cursor()), 0);
    }

    #[test]
    fn caret_column_answers_against_scrolled_windows() {
        // The hardware caret is placed from a windowed layout with a global
        // line index; it must resolve against the window, or a scrolled
        // file draws the caret at column 0 and the tracer looks possessed.
        let text = (0..200)
            .map(|i| format!("line {i:03} with words"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut buffer = EditorBuffer::from_text(&text);
        buffer.move_cursor_to_end();

        let width = 60;
        let cursor = buffer.cursor();
        let global = buffer.visual_line_for_cursor(width, cursor);
        assert!(global > 20, "test needs a scrolled document");

        // A window around the cursor, as the renderer builds it.
        let start = global.saturating_sub(5);
        let window = buffer.visible_layout(width, start, 10, 0);
        let column = window.column_for_cursor(global, cursor);
        let full = buffer.layout(width);
        assert_eq!(
            column,
            full.column_for_cursor(global, cursor),
            "windowed caret column must match the full layout"
        );
        assert!(column > 0, "caret belongs at end of line, not column 0");
    }

    #[test]
    fn sorting_selected_lines_does_nothing_without_a_selection() {
        let mut buffer = EditorBuffer::from_text("b\na\n");

        assert!(!buffer.sort_selected_lines());
        assert_eq!(buffer.plain_text(), "b\na\n");
    }

    #[test]
    fn block_prefixes_add_and_replace_markers() {
        use super::BlockPrefix;
        let mut buffer = EditorBuffer::from_text("hello");
        buffer.move_to_top();

        assert!(buffer.apply_block_prefix(BlockPrefix::Title));
        assert_eq!(buffer.plain_text(), "# hello");
        assert!(buffer.apply_block_prefix(BlockPrefix::Heading));
        assert_eq!(buffer.plain_text(), "## hello");
        assert!(buffer.apply_block_prefix(BlockPrefix::Quote));
        assert_eq!(buffer.plain_text(), "> hello");
        assert!(buffer.apply_block_prefix(BlockPrefix::Body));
        assert_eq!(buffer.plain_text(), "hello");
        assert!(!buffer.apply_block_prefix(BlockPrefix::Body));
    }

    #[test]
    fn block_prefix_skips_blank_lines_and_keeps_indent() {
        use super::BlockPrefix;
        let mut buffer = EditorBuffer::from_text("  one\n\n  two");
        buffer.move_to_top();
        buffer.select_all();

        assert!(buffer.apply_block_prefix(BlockPrefix::Quote));
        assert_eq!(buffer.plain_text(), "  > one\n\n  > two");
    }

    #[test]
    fn block_body_strips_lists_and_checkboxes() {
        use super::BlockPrefix;
        let mut buffer = EditorBuffer::from_text("- [ ] task\n1. item\n- bullet");
        buffer.move_to_top();
        buffer.select_all();

        assert!(buffer.apply_block_prefix(BlockPrefix::Body));
        assert_eq!(buffer.plain_text(), "task\nitem\nbullet");
    }

    #[test]
    fn block_prefix_moves_styled_ranges_with_the_marker() {
        use super::{AttrKind, BlockPrefix};
        let mut buffer = EditorBuffer::from_text("hello");
        buffer.move_to_top();
        buffer.select_all();
        assert!(buffer.toggle_attr(AttrKind::Bold));

        assert!(buffer.apply_block_prefix(BlockPrefix::Title));
        assert_eq!(buffer.plain_text(), "# hello");
        // "hello" slid right by 2; its bold range followed.
        assert_eq!(
            buffer.attrs,
            vec![super::AttrSpan {
                start: 2,
                end: 7,
                kind: AttrKind::Bold,
            }]
        );
        assert!(buffer.undo());
        assert_eq!(buffer.plain_text(), "hello");
    }

    #[test]
    fn cycling_case_starts_from_a_selection_that_is_already_uppercase() {
        let mut buffer = EditorBuffer::from_text("SHOUT");
        buffer.move_to_top();
        buffer.select_all();

        assert!(buffer.cycle_case());
        assert_eq!(buffer.plain_text(), "shout");
    }

    #[test]
    fn cycling_case_does_nothing_without_a_selection() {
        let mut buffer = EditorBuffer::from_text("hello");

        assert!(!buffer.cycle_case());

        assert_eq!(buffer.plain_text(), "hello");
    }

    #[test]
    fn cycling_case_ignores_a_selection_with_no_letters() {
        let mut buffer = EditorBuffer::from_text("123 -- 456");
        buffer.move_to_top();
        buffer.select_all();

        assert!(!buffer.cycle_case());

        assert_eq!(buffer.plain_text(), "123 -- 456");
    }

    #[test]
    fn cycling_case_keeps_the_selection_covering_the_same_text() {
        let mut buffer = EditorBuffer::from_text("one two three");
        buffer.move_to_top();
        buffer.select_all();

        assert!(buffer.cycle_case());
        assert_eq!(
            buffer.selected_text(),
            Some("One Two Three".to_string()),
            "the selection should still cover exactly the converted text"
        );
    }

    #[test]
    fn outdent_removes_a_short_indent_entirely() {
        let mut buffer = EditorBuffer::from_text("  alpha");
        buffer.move_home(80, false);

        // Outdent removes up to one level, so a two-space indent goes to none
        // rather than being left half-applied.
        assert!(buffer.outdent());

        assert_eq!(buffer.plain_text(), "alpha");
    }

    #[test]
    fn backspace_at_line_start_removes_only_the_newline() {
        let mut buffer = EditorBuffer::from_text("alpha\nbeta");
        buffer.move_down(40, false);
        buffer.move_home(40, false);

        assert!(buffer.backspace());

        assert_eq!(buffer.plain_text(), "alphabeta");
        assert_eq!(buffer.cursor(), 5);
    }

    #[test]
    fn delete_forward_at_line_end_removes_only_the_newline() {
        let mut buffer = EditorBuffer::from_text("alpha\nbeta");
        buffer.move_end(40, false);

        assert!(buffer.delete_forward());

        assert_eq!(buffer.plain_text(), "alphabeta");
        assert_eq!(buffer.cursor(), 5);
    }

    #[test]
    fn word_motion_moves_between_word_starts() {
        let mut buffer = EditorBuffer::from_text("alpha beta gamma");

        buffer.move_word_right(false);
        assert_eq!(buffer.cursor(), 6);

        buffer.move_word_right(false);
        assert_eq!(buffer.cursor(), 11);

        buffer.move_word_left(false);
        assert_eq!(buffer.cursor(), 6);
    }

    #[test]
    fn selected_text_returns_the_active_selection() {
        let mut buffer = EditorBuffer::from_text("alpha beta");
        buffer.move_word_right(true);

        assert_eq!(buffer.selected_text().as_deref(), Some("alpha "));
    }

    #[test]
    fn non_latin_text_round_trips_through_editing() {
        // Kanji, Hebrew (RTL), hiragana, emoji ZWJ sequence, combining mark.
        let text = "日本語テスト\nשלום עולם\nあいうえお\nfamily 👨‍👩‍👧‍👦!\ncafé naive";
        let mut buffer = EditorBuffer::from_text(text);
        assert_eq!(buffer.plain_text(), text);
        // Chars, not bytes: multibyte text must not confuse lengths.
        assert!(buffer.plain_text().len() > text.chars().count());

        // Select all, copy, paste: duplicates exactly once.
        buffer.select_all();
        let selected = buffer.selected_text().expect("selection");
        assert_eq!(selected, text);
        buffer.collapse_selection();
        buffer.insert_text(&selected);
        assert_eq!(buffer.plain_text(), format!("{text}{text}"));

        // Undo restores the original text.
        assert!(buffer.undo());
        assert_eq!(buffer.plain_text(), text);
        assert!(buffer.redo());
        assert_eq!(buffer.plain_text(), format!("{text}{text}"));
    }

    #[test]
    fn grapheme_motion_crosses_emoji_and_combining_marks_whole() {
        let mut buffer = EditorBuffer::from_text("a👨‍👩‍👧‍👦b");
        assert_eq!(buffer.plain_text().chars().count(), 9);
        // Cursor to start, step right: plain char, whole emoji, plain char.
        buffer.set_cursor_with_anchor(0, 0);
        buffer.move_right(false);
        assert_eq!(buffer.cursor(), 1);
        buffer.move_right(false);
        // The 7-char ZWJ sequence crosses in a single step.
        assert_eq!(buffer.cursor(), 8);
        buffer.move_right(false);
        assert_eq!(buffer.cursor(), 9);

        // Backspace removes the whole emoji, not one code point.
        buffer.set_cursor_with_anchor(8, 8);
        assert!(buffer.backspace());
        assert_eq!(buffer.plain_text(), "ab");
    }

    #[test]
    fn word_motion_and_delete_survive_cjk_and_hebrew() {
        let mut buffer = EditorBuffer::from_text("hello 日本語テスト שלום world");
        buffer.select_all();
        assert!(buffer.selected_text().is_some());
        buffer.collapse_selection();

        // Word jumps move (never stall) across scripts.
        buffer.set_cursor_with_anchor(0, 0);
        let mut last = 0usize;
        for _ in 0..10 {
            buffer.move_word_right(false);
            assert!(buffer.cursor() >= last);
            if buffer.cursor() == buffer.plain_text().chars().count() {
                break;
            }
            last = buffer.cursor();
        }
        assert_eq!(buffer.cursor(), buffer.plain_text().chars().count());

        buffer.set_cursor_with_anchor(buffer.cursor(), buffer.cursor());
        for _ in 0..10 {
            let before = buffer.cursor();
            buffer.move_word_left(false);
            assert!(buffer.cursor() <= before);
            if buffer.cursor() == 0 {
                break;
            }
        }
        assert_eq!(buffer.cursor(), 0);

        // Delete word forward on a Hebrew word removes whole chars.
        buffer.set_cursor_with_anchor(0, 0);
        for _ in 0..6 {
            buffer.move_word_right(false);
        }
        assert!(buffer.delete_word_forward() || buffer.plain_text().contains("world"));
    }

    #[test]
    fn delete_word_left_removes_the_previous_word() {
        let mut buffer = EditorBuffer::from_text("hello big world");
        buffer.set_cursor_with_anchor(buffer.char_count(), buffer.char_count());

        assert!(buffer.delete_word_left());
        assert_eq!(buffer.plain_text(), "hello big ");
        assert_eq!(buffer.cursor(), "hello big ".chars().count());

        assert!(buffer.delete_word_left());
        assert_eq!(buffer.plain_text(), "hello ");
        assert_eq!(buffer.cursor(), "hello ".chars().count());

        // At the start there is nothing left to remove.
        buffer.set_cursor_with_anchor(0, 0);
        assert!(!buffer.delete_word_left());
        assert_eq!(buffer.plain_text(), "hello ");
    }

    #[test]
    fn wide_chars_wrap_on_display_columns_not_char_counts() {
        use unicode_segmentation::UnicodeSegmentation;
        // Each kanji is 2 columns wide: at width 6 only 3 fit per row.
        let buffer = EditorBuffer::from_text("日本語テストです");
        let layout = buffer.layout(6);
        assert!(layout.lines.len() > 1, "wide text must wrap");
        for line in &layout.lines {
            let width: usize = line
                .text
                .graphemes(true)
                .map(|g| unicode_width::UnicodeWidthStr::width(g).max(1))
                .sum();
            assert!(width <= 6, "row overflows: {:?}", line.text);
        }
        // Cursor columns track display width: after 2 kanji the column is 4.
        let (row, col) = buffer.cursor_line_and_col(20);
        assert_eq!((row, col), (1, 1));
        let mut moved = buffer.clone();
        moved.set_cursor_with_anchor(2, 2);
        let (_, col) = moved.cursor_line_and_col(20);
        assert_eq!(col, 5, "two wide chars occupy columns 1-4");
    }

    #[test]
    fn paragraph_alignment_round_trips_to_html() {
        let mut buffer = EditorBuffer::from_text("alpha beta\ngamma");
        buffer.set_alignment_for_selection(ParagraphAlignment::Center);
        buffer.move_down(40, false);
        buffer.set_alignment_for_selection(ParagraphAlignment::Justify);

        let html = buffer.to_ord_html();
        let parsed = EditorBuffer::from_rich_text("alpha beta\ngamma", &html);

        assert_eq!(parsed.current_alignment(), ParagraphAlignment::Center);
        assert_eq!(parsed.paragraph_alignments[1], ParagraphAlignment::Justify);
    }

    #[test]
    fn centered_layout_offsets_the_rendered_line() {
        let mut buffer = EditorBuffer::from_text("alpha");
        buffer.set_alignment_for_selection(ParagraphAlignment::Center);

        let layout = buffer.layout(11);

        assert_eq!(layout.lines[0].boundaries[0].column, 3);
    }

    #[test]
    fn justify_expands_internal_spacing() {
        let mut buffer = EditorBuffer::from_text("alpha beta gamma");
        buffer.set_alignment_for_selection(ParagraphAlignment::Justify);

        let layout = buffer.layout(11);

        assert!(layout.lines[0].text.contains("  "));
    }

    #[test]
    fn a_blank_paragraph_hangs_its_caret_where_its_text_will_start() {
        let mut buffer = EditorBuffer::from_text("alpha\n\nomega");

        buffer.move_to_line(2, 20);
        assert_eq!(buffer.cursor_visual_column(20), 0);

        // A blank centered line waits in the middle: centering a bare line
        // used to look left-aligned until a character was typed.
        buffer.set_alignment_for_selection(ParagraphAlignment::Center);
        assert_eq!(buffer.cursor_visual_column(20), 10);
        // Typing the first character does not shift the caret: the wrap
        // re-centers around the one-column line and lands on the same cell.
        buffer.insert_text("b");
        assert_eq!(buffer.cursor_visual_column(20), 10);
        assert_eq!(buffer.layout(20).lines[1].boundaries[0].column, 9);

        // Right hangs on the right margin, again where the text lands.
        buffer.undo();
        buffer.move_to_line(2, 20);
        buffer.set_alignment_for_selection(ParagraphAlignment::Right);
        assert_eq!(buffer.cursor_visual_column(20), 20);
    }
}

trait EmptyStringFallback {
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
enum CaseStyle {
    Upper,
    Lower,
    Title,
    Sentence,
    Mixed,
}

fn detect_case_style(text: &str) -> CaseStyle {
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

fn capitalize_words(text: &str) -> String {
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
fn sentence_case(text: &str) -> String {
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
fn shift_attrs_for_insert(spans: &mut [AttrSpan], pos: usize, n: usize) {
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
fn shift_attrs_for_delete(spans: &mut Vec<AttrSpan>, start: usize, end: usize) {
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
fn range_fully_has_attr(spans: &[AttrSpan], kind: AttrKind, start: usize, end: usize) -> bool {
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
fn subtract_attr_range(spans: &mut Vec<AttrSpan>, kind: AttrKind, start: usize, end: usize) {
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
fn normalize_attr_spans(spans: &mut Vec<AttrSpan>) {
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
fn block_marker_len(text: &str) -> usize {
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
fn indent_shift(
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

#[cfg(test)]
mod bench {
    use super::EditorBuffer;
    use std::time::Instant;

    fn sample_document(lines: usize) -> String {
        (0..lines)
            .map(|index| {
                format!(
                    "Paragraph {index} holds a sentence or two of ordinary prose so that the \
                     wrapper has real work to do at a realistic terminal width."
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn report(label: &str, iterations: u32, elapsed: std::time::Duration) {
        let per_frame = elapsed.as_secs_f64() / f64::from(iterations);
        println!(
            "{label:<34} {:>8.3} ms/frame  {:>9.0} frames/sec",
            per_frame * 1000.0,
            1.0 / per_frame
        );
    }

    /// A full frame as the app does it: scroll bookkeeping plus wrapping only
    /// the visible rows.
    #[test]
    #[ignore]
    fn typing_frame_cost() {
        let text = sample_document(400);
        let width = 88usize;
        let rows = 44usize;
        let iterations = 600;

        // Baseline: what the app used to pay, wrapping the whole document.
        let buffer = EditorBuffer::from_text(&text);
        let started = Instant::now();
        for _ in 0..iterations {
            let layout = buffer.layout(width);
            let _ = layout.line_index_for_cursor(buffer.cursor());
        }
        report("cold: whole-document wrap", iterations, started.elapsed());

        // Cached, typing at the end of the document.
        let mut buffer = EditorBuffer::from_text(&text);
        buffer.move_cursor_to_end();
        let _ = buffer.visible_layout(width, 0, rows, 1);
        let started = Instant::now();
        for _ in 0..iterations {
            buffer.insert_char_with_mode('a', false);
            let _ = buffer.visual_line_for_cursor(width, buffer.cursor());
            let _ = buffer.visible_layout(width, 0, rows, 1);
        }
        report("cached: type at end", iterations, started.elapsed());

        // Cached, typing at the top, which shifts every later line's offsets.
        let mut buffer = EditorBuffer::from_text(&text);
        let _ = buffer.visible_layout(width, 0, rows, 1);
        let started = Instant::now();
        for _ in 0..iterations {
            buffer.insert_char_with_mode('a', false);
            buffer.move_to_top();
            buffer.insert_char_with_mode('b', false);
            let _ = buffer.visual_line_for_cursor(width, buffer.cursor());
            let _ = buffer.visible_layout(width, 0, rows, 1);
        }
        report("cached: type at top", iterations, started.elapsed());

        // Cursor movement only, where the memo should do nothing at all.
        let mut buffer = EditorBuffer::from_text(&text);
        let _ = buffer.visible_layout(width, 0, rows, 1);
        let started = Instant::now();
        for _ in 0..iterations {
            buffer.move_left(false);
            buffer.move_right(false);
            let _ = buffer.visual_line_for_cursor(width, buffer.cursor());
            let _ = buffer.visible_layout(width, 0, rows, 1);
        }
        report("cached: move cursor only", iterations, started.elapsed());

        // Split the cached path to find where the time actually goes.
        let mut buffer = EditorBuffer::from_text(&text);
        buffer.move_cursor_to_end();
        let _ = buffer.total_visual_lines(width);
        let started = Instant::now();
        for _ in 0..iterations {
            buffer.insert_char_with_mode('a', false);
            let _ = buffer.total_visual_lines(width);
        }
        report("insert + cache validate", iterations, started.elapsed());

        let mut buffer = EditorBuffer::from_text(&text);
        buffer.move_cursor_to_end();
        let _ = buffer.total_visual_lines(width);
        let started = Instant::now();
        for _ in 0..iterations {
            buffer.insert_char_with_mode('a', false);
            let _ = buffer.visual_line_for_cursor(width, buffer.cursor());
        }
        report(
            "insert + visual_line_for_cursor",
            iterations,
            started.elapsed(),
        );

        // Which rope primitive is actually expensive?
        let rope = ropey::Rope::from_str(&text);
        let started = Instant::now();
        for _ in 0..iterations {
            for i in 0..rope.len_lines() {
                std::hint::black_box(rope.line_to_char(i));
            }
        }
        report("line_to_char x 400 lines", iterations, started.elapsed());

        let started = Instant::now();
        for _ in 0..iterations {
            for i in 0..rope.len_lines() {
                std::hint::black_box(rope.line(i).len_chars());
            }
        }
        report(
            "rope.line(i).len_chars() x 400",
            iterations,
            started.elapsed(),
        );

        let started = Instant::now();
        for _ in 0..iterations {
            for i in 0..rope.len_lines() {
                std::hint::black_box(rope.line_to_byte(i));
            }
        }
        report("line_to_byte x 400 lines", iterations, started.elapsed());

        let started = Instant::now();
        for _ in 0..iterations {
            for i in 0..rope.len_lines() {
                std::hint::black_box(rope.byte_to_char(rope.line_to_byte(i)));
            }
        }
        report("line_to_byte + byte_to_char", iterations, started.elapsed());

        // Stats: the old path rebuilt the whole text and rescanned it on
        // every keystroke. The new one records a line range and folds once
        // per read. Both are measured so the comparison stays honest.
        let mut buffer = EditorBuffer::from_text(&text);
        let started = Instant::now();
        for _ in 0..iterations {
            buffer.insert_char_with_mode('a', false);
            std::hint::black_box(buffer.stats());
        }
        report(
            "insert + stats (incremental)",
            iterations,
            started.elapsed(),
        );

        let mut buffer = EditorBuffer::from_text(&text);
        let started = Instant::now();
        for _ in 0..iterations {
            buffer.insert_char_with_mode('a', false);
            let plain = buffer.plain_text();
            std::hint::black_box(crate::stats::compute_word_stats(&plain));
        }
        report(
            "insert + whole-text stats (old)",
            iterations,
            started.elapsed(),
        );

        let buffer = EditorBuffer::from_text(&text);
        let started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(buffer.plain_text());
        }
        report("plain_text() (whole doc)", iterations, started.elapsed());

        // The render path: clear, fill, diff at a realistic terminal size.
        use ordet_tui_framework::{Buffer, Point, Rect, Style};
        let area = Rect::new(0, 0, 200, 50);
        let mut front = Buffer::new(area);
        let mut back = Buffer::new(area);
        let started = Instant::now();
        for _ in 0..iterations {
            front.clear(Style::default());
        }
        report("Buffer::clear 200x50", iterations, started.elapsed());

        let line = "the quick brown fox jumps over the lazy dog again and again ".repeat(3);
        let started = Instant::now();
        for _ in 0..iterations {
            for row in 0..44u16 {
                front.write_text(Point::new(4, row), &line, Style::default(), 88);
            }
        }
        report("write_text 44 rows x 88", iterations, started.elapsed());

        back.clear(Style::default());
        let started = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(front.diff(&back));
        }
        report("Buffer::diff 200x50 (vec)", iterations, started.elapsed());

        let started = Instant::now();
        for _ in 0..iterations {
            let mut changed = 0usize;
            front.diff_for_each(&back, |_, _| changed += 1);
            std::hint::black_box(changed);
        }
        report(
            "Buffer::diff_for_each 200x50",
            iterations,
            started.elapsed(),
        );

        // Isolate the undo snapshot, which clones the rope on every keystroke.
        let mut buffer = EditorBuffer::from_text(&text);
        let started = Instant::now();
        for _ in 0..iterations {
            buffer.insert_char_with_mode('a', false);
        }
        report(
            "typing: with rope-snapshot undo",
            iterations,
            started.elapsed(),
        );

        let rope = ropey::Rope::from_str(&text);
        let started = Instant::now();
        for _ in 0..iterations {
            let _clone = rope.clone();
            std::hint::black_box(&_clone);
        }
        report("rope clone alone", iterations, started.elapsed());

        let started = Instant::now();
        for _ in 0..iterations {
            buffer.insert_char_with_mode('a', false);
            buffer.undo();
        }
        report("typing + undo round trip", iterations, started.elapsed());

        println!("document: {} chars", text.len());
    }

    /// The one-shot cost of showing a large document: the first frame wraps
    /// every line, and the status bar's first read folds the whole-document
    /// stats. Neither is on the keystroke path, but both delay the first
    /// frame after opening a file.
    #[test]
    #[ignore]
    fn document_open_cost() {
        let width = 88usize;
        for lines in [2_000usize, 10_000, 50_000] {
            let text = sample_document(lines);

            let wrap_buffer = EditorBuffer::from_text(&text);
            let started = Instant::now();
            std::hint::black_box(wrap_buffer.total_visual_lines(width));
            report(
                &format!("cold wrap fold, {lines} lines"),
                1,
                started.elapsed(),
            );

            let stats_buffer = EditorBuffer::from_text(&text);
            let started = Instant::now();
            std::hint::black_box(stats_buffer.stats());
            report(
                &format!("cold stats fold, {lines} lines"),
                1,
                started.elapsed(),
            );
        }
    }

    /// The per-keystroke path on a big document, scrolled to the bottom,
    /// retyping a character. This is where an O(scroll) walk in
    /// `visible_layout` or `visual_line_for_cursor` shows up, because neither is
    /// exercised by the small-document bench above.
    #[test]
    #[ignore]
    fn large_document_frame_cost() {
        let width = 88usize;
        let rows = 44usize;
        let iterations = 300;

        for lines in [2_000usize, 10_000, 50_000] {
            let text = sample_document(lines);
            let mut buffer = EditorBuffer::from_text(&text);
            buffer.move_cursor_to_end();
            let total = buffer.total_visual_lines(width);
            let scroll = total.saturating_sub(rows);
            // Warm the cache exactly as a real frame would. Stats too: the
            // first `stats()` folds the whole document, and leaving that fold
            // inside the timed loops amortizes ~88ms at 50k lines over the
            // iterations, which reads as a per-frame cost that does not exist.
            let _ = buffer.visual_line_for_cursor(width, buffer.cursor());
            let _ = buffer.visible_layout(width, scroll, rows, 1);
            let _ = buffer.stats();

            let started = Instant::now();
            for _ in 0..iterations {
                std::hint::black_box(buffer.visual_line_for_cursor(width, buffer.cursor()));
            }
            report(
                &format!("cursor line, {lines} lines"),
                iterations,
                started.elapsed(),
            );

            let started = Instant::now();
            for _ in 0..iterations {
                std::hint::black_box(buffer.visible_layout(width, scroll, rows, 1));
            }
            report(
                &format!("visible slice, {lines} lines"),
                iterations,
                started.elapsed(),
            );

            // Home/End go through the cache-direct helpers, which clone one
            // wrapped line instead of the whole document.
            let started = Instant::now();
            for _ in 0..iterations {
                buffer.move_home(width, false);
                buffer.move_end(width, false);
            }
            report(
                &format!("Home/End cache-direct, {lines} lines"),
                iterations,
                started.elapsed(),
            );

            // What a keystroke pays before rendering: undo record, wrap
            // validation, stats fold, then the frame itself.
            let started = Instant::now();
            for _ in 0..iterations {
                buffer.insert_char_with_mode('a', false);
                std::hint::black_box(buffer.visual_line_for_cursor(width, buffer.cursor()));
                std::hint::black_box(buffer.visible_layout(width, scroll, rows, 1));
                std::hint::black_box(buffer.stats());
            }
            report(
                &format!("type + frame, {lines} lines"),
                iterations,
                started.elapsed(),
            );

            let started = Instant::now();
            for _ in 0..iterations {
                buffer.insert_char_with_mode('a', false);
                std::hint::black_box(buffer.stats());
            }
            report(
                &format!("  insert + stats fold, {lines} lines"),
                iterations,
                started.elapsed(),
            );

            // Split the keystroke cost: the undo rope clone versus the wrap
            // cache revalidation that the edit forces.
            let started = Instant::now();
            for _ in 0..iterations {
                buffer.insert_char_with_mode('a', false);
            }
            report(
                &format!("  undo snapshot only, {lines} lines"),
                iterations,
                started.elapsed(),
            );

            let started = Instant::now();
            for _ in 0..iterations {
                buffer.insert_char_with_mode('a', false);
                std::hint::black_box(buffer.visual_line_for_cursor(width, buffer.cursor()));
            }
            report(
                &format!("  undo + revalidate cache, {lines} lines"),
                iterations,
                started.elapsed(),
            );

            // A full keystroke frame typing into a mid-document paragraph.
            // This is where an O(entries) span shift or an O(lines) fold
            // shows up, because the edit is neither at the top nor at the
            // end of the document.
            let mut mid = EditorBuffer::from_text(&text);
            mid.move_to_line(lines / 2, width);
            let _ = mid.visual_line_for_cursor(width, mid.cursor());
            let _ = mid.stats();
            let mid_scroll = mid.total_visual_lines(width).saturating_sub(rows);
            let _ = mid.visible_layout(width, mid_scroll, rows, 1);
            let started = Instant::now();
            for _ in 0..iterations {
                mid.insert_char_with_mode('a', false);
                std::hint::black_box(mid.visual_line_for_cursor(width, mid.cursor()));
                std::hint::black_box(mid.visible_layout(width, mid_scroll, rows, 1));
                std::hint::black_box(mid.stats());
            }
            report(
                &format!("  mid-document frame, {lines} lines"),
                iterations,
                started.elapsed(),
            );
            println!();
        }
    }
}
