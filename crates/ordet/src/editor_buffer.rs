use ordet_format::escape_xml;
use ropey::Rope;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::RefCell;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

mod formatting;
mod layout;

use formatting::*;
use layout::*;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttrKind {
    Bold,
    Italic,
}

/// A styled char range. Same-kind spans never overlap (toggle normalizes);
/// different kinds may cross freely.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
pub(crate) fn expand_tabs(text: &str) -> Cow<'_, str> {
    if !text.contains('\t') {
        return Cow::Borrowed(text);
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
    Cow::Owned(out)
}

#[derive(Clone, Debug)]
pub struct EditorBuffer {
    rope: Rope,
    cursor: usize,
    anchor: Option<usize>,
    /// First endpoint of a marked block, kept separately from the cursor so
    /// the destination can move before copy or move runs.
    block_start: Option<usize>,
    block: Option<(usize, usize)>,
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
    /// document. Unmeasured entries count as one line until they are warmed.
    fenwick: Option<Fenwick>,
    /// True while some entries still need measuring at the current width.
    warming: bool,
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

    fn estimated_count(entry: &CachedWrapLine) -> usize {
        if entry.dirty {
            entry.lines.len().max(1)
        } else {
            entry.lines.len()
        }
    }

    fn rebuild_fenwick(&mut self) {
        let counts: Vec<usize> = self.entries.iter().map(Self::estimated_count).collect();
        let tree = Fenwick::build(&counts);
        self.total_lines = tree.total();
        self.fenwick = Some(tree);
    }

    fn has_dirty(&self) -> bool {
        self.entries.iter().any(|entry| entry.dirty)
    }

    /// Record entry `index` as stale, without touching the structure.
    fn mark_dirty(&mut self, index: usize) {
        if let Some(indices) = &mut self.dirty {
            indices.push(index);
        }
        self.warming = true;
    }

    /// Everything is suspect: resize, wholesale replacement, splices.
    fn mark_all_dirty(&mut self) {
        self.dirty = None;
        self.fenwick = None;
        self.warming = true;
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

fn publish_stats(cache: &mut StatsCache) {
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
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

    pub(crate) fn alignments(&self) -> &[ParagraphAlignment] {
        &self.alignments
    }

    pub(crate) fn attrs(&self) -> &[AttrSpan] {
        &self.attrs
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
            block_start: None,
            block: None,
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
                warming: false,
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

    pub(crate) fn alignments(&self) -> &[ParagraphAlignment] {
        &self.paragraph_alignments
    }

    pub(crate) fn attrs(&self) -> &[AttrSpan] {
        &self.attrs
    }

    /// Replace every character and the rich formatting with a history
    /// snapshot. The old state stays in one undo record.
    pub(crate) fn replace_with_rich_text(
        &mut self,
        text: &str,
        alignments: &[ParagraphAlignment],
        attrs: &[AttrSpan],
    ) -> bool {
        if self.rope == text && self.paragraph_alignments == alignments && self.attrs == attrs {
            return false;
        }
        self.begin_undo();
        self.select_all();
        self.delete_selection_if_any();
        self.rope_insert(0, text);
        let lines = self.rope.len_lines();
        self.paragraph_alignments = alignments.to_vec();
        self.paragraph_alignments
            .resize(lines, ParagraphAlignment::Left);
        self.paragraph_alignments.truncate(lines);
        self.attrs = attrs.to_vec();
        normalize_attr_spans(&mut self.attrs);
        self.cursor = self.rope.len_chars();
        self.anchor = None;
        self.block_start = None;
        self.block = None;
        self.preferred_column = None;
        self.mark_wrap_all_dirty();
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
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
        self.rope_insert(self.cursor, &normalized);
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
                    .to_string()
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
            let line_text: String = self.rope.line(line).to_string();
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
        let start = self.prev_grapheme_start(self.cursor);
        self.rope_remove(start, self.cursor);
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
        self.rope_remove(self.cursor, end);
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
            self.cursor = prev_start;
        } else {
            self.rope_remove(line_start, line_end);
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
        self.rope_insert(self.cursor, "\n");
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
        self.block = None;
        self.block_start = Some(self.cursor);
        self.anchor = Some(self.cursor);
    }

    pub fn set_block_end(&mut self) {
        if let Some(start) = self.block_start.take() {
            self.block = Some((start.min(self.cursor), start.max(self.cursor)));
            self.anchor = Some(start);
        } else if self.anchor.is_none() {
            self.anchor = Some(0);
        }
    }

    pub fn hide_block(&mut self) {
        self.anchor = None;
        self.block_start = None;
        self.block = None;
    }

    pub fn marked_block(&self) -> Option<(usize, usize)> {
        self.block.or_else(|| self.selection_range())
    }

    pub fn copy_block(&mut self) -> bool {
        let Some((start, end)) = self.marked_block() else {
            return false;
        };
        if start >= end {
            return false;
        }
        let text = self.rope.slice(start..end).to_string();
        let len = text.chars().count();
        let destination = if self.cursor > start && self.cursor < end {
            end
        } else {
            self.cursor
        };
        self.begin_undo();
        self.rope_insert(destination, &text);
        self.cursor = destination + len;
        self.anchor = Some(destination);
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    pub fn move_block(&mut self) -> bool {
        let Some((start, end)) = self.marked_block() else {
            return false;
        };
        if start >= end || (self.cursor > start && self.cursor < end) {
            return false;
        }
        let text = self.rope.slice(start..end).to_string();
        let len = text.chars().count();
        let destination = if self.cursor >= end {
            self.cursor - len
        } else {
            self.cursor
        };
        if destination == start {
            return false;
        }
        self.begin_undo();
        self.rope_remove(start, end);
        self.rope_insert(destination, &text);
        self.cursor = destination + len;
        self.anchor = Some(destination);
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
    }

    pub fn delete_block(&mut self) -> bool {
        let Some((start, end)) = self.marked_block() else {
            return false;
        };
        if start >= end {
            return false;
        }
        self.begin_undo();
        self.rope_remove(start, end);
        self.cursor = start;
        self.anchor = None;
        self.preferred_column = None;
        self.ensure_alignment_capacity();
        self.redo.clear();
        true
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
        let margin_start = first_visual.saturating_sub(margin);
        self.measure_visible_entries(
            width,
            margin_start,
            rows.saturating_add(margin.saturating_mul(2)),
        );
        let start = {
            let cache = self.wrap_cache.borrow();
            first_visual.saturating_sub(margin).min(cache.total_lines)
        };
        self.measure_visible_entries(width, start, rows.saturating_add(margin.saturating_mul(2)));
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
        let cursor = cursor.min(self.rope.len_chars());
        let rope_line = self.rope.char_to_line(cursor);
        self.ensure_wrap_entry(width, rope_line);
        let cache = &self.wrap_cache.borrow();
        let rope_line = rope_line.min(cache.entries.len());
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
        self.ensure_wrap(width, 256);
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

    /// Bring the memo fully up to date for `width`. Tests and the reference
    /// layout use this; the frame path uses the budgeted warmers below.
    fn ensure_wrap_cache(&self, width: usize) {
        self.ensure_wrap(width, usize::MAX);
    }

    /// Measure at most `budget` more source lines and report whether any
    /// entries are still unmeasured. The app calls this from the draw loop so
    /// a cold document warms over several frames instead of one long stall.
    pub(crate) fn warm_wrap_cache(&self, width: usize, budget: usize) -> bool {
        self.ensure_wrap(width, budget)
    }

    fn ensure_wrap(&self, width: usize, budget: usize) -> bool {
        let wrap_width = width.max(1);
        {
            let cache = self.wrap_cache.borrow();
            if !cache.warming && cache.width == wrap_width && cache.edit_seq == self.edit_seq {
                return false;
            }
        }

        let mut cache = self.wrap_cache.borrow_mut();
        if cache.width != wrap_width {
            cache.width = wrap_width;
            for entry in cache.entries.iter_mut() {
                entry.dirty = true;
            }
            cache.dirty = None;
            cache.fenwick = None;
            cache.warming = true;
        }

        let line_count = self.rope.len_lines();
        if cache.entries.len() != line_count {
            cache.entries.truncate(line_count);
            while cache.entries.len() < line_count {
                cache.entries.push(CachedWrapLine {
                    lines: Vec::new(),
                    dirty: true,
                    shift: 0,
                });
            }
            cache.dirty = None;
            cache.fenwick = None;
            cache.warming = true;
        }
        if cache.fenwick.is_none() {
            cache.rebuild_fenwick();
        }

        let mut remaining = match cache.dirty.take() {
            Some(indices) => indices,
            None => (0..line_count).collect(),
        };
        let mut still = Vec::new();
        let mut measured = 0usize;
        for source_line in remaining.drain(..) {
            if source_line >= line_count || source_line >= cache.entries.len() {
                continue;
            }
            if !cache.entries[source_line].dirty {
                continue;
            }
            if measured >= budget {
                still.push(source_line);
                continue;
            }
            self.measure_wrap_entry(&mut cache, source_line, wrap_width);
            measured += 1;
        }

        cache.total_lines = cache.fenwick.as_ref().map_or(0, Fenwick::total);
        cache.warming = cache.has_dirty();
        if cache.warming {
            cache.dirty = Some(still);
        } else {
            cache.dirty = Some(Vec::new());
            cache.edit_seq = self.edit_seq;
        }
        cache.warming
    }

    fn measure_wrap_entry(&self, cache: &mut WrapCache, source_line: usize, wrap_width: usize) {
        if !cache.entries[source_line].dirty {
            return;
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
        let lines = wrap_source_line(display.as_ref(), start, alignment, wrap_width);
        let old_count = WrapCache::estimated_count(&cache.entries[source_line]);
        let new_count = lines.len();
        if old_count != new_count {
            if let Some(tree) = cache.fenwick.as_mut() {
                tree.add(source_line, new_count as isize - old_count as isize);
            }
        }
        let entry = &mut cache.entries[source_line];
        entry.lines = lines;
        entry.dirty = false;
        entry.shift = 0;
    }

    fn ensure_wrap_entry(&self, width: usize, source_line: usize) {
        let wrap_width = width.max(1);
        self.ensure_wrap(wrap_width, 256);
        let needs_measure = {
            let cache = self.wrap_cache.borrow();
            cache
                .entries
                .get(source_line)
                .is_some_and(|entry| entry.dirty)
        };
        if !needs_measure {
            return;
        }
        let mut cache = self.wrap_cache.borrow_mut();
        self.measure_wrap_entry(&mut cache, source_line, wrap_width);
        cache.total_lines = cache.fenwick.as_ref().map_or(0, Fenwick::total);
        cache.warming = cache.has_dirty();
        if !cache.warming {
            cache.dirty = Some(Vec::new());
            cache.edit_seq = self.edit_seq;
        }
    }

    fn measure_visible_entries(&self, width: usize, first_visual: usize, rows: usize) {
        let wrap_width = width.max(1);
        self.ensure_wrap(wrap_width, 256);
        for _ in 0..2 {
            let (start, end) = {
                let cache = self.wrap_cache.borrow();
                let (entry, _) = cache.locate(first_visual).unwrap_or((0, 0));
                let start = entry.saturating_sub(1);
                let end = entry
                    .saturating_add(rows.max(4))
                    .saturating_add(8)
                    .min(cache.entries.len());
                (start, end)
            };
            let indices: Vec<usize> = {
                let cache = self.wrap_cache.borrow();
                (start..end)
                    .filter(|&index| cache.entries[index].dirty)
                    .collect()
            };
            if indices.is_empty() {
                return;
            }
            let mut cache = self.wrap_cache.borrow_mut();
            for index in indices {
                self.measure_wrap_entry(&mut cache, index, wrap_width);
            }
            cache.total_lines = cache.fenwick.as_ref().map_or(0, Fenwick::total);
            cache.warming = cache.has_dirty();
            if !cache.warming {
                cache.dirty = Some(Vec::new());
                cache.edit_seq = self.edit_seq;
            }
        }
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
        self.block_start = None;
        self.block = None;
        if inserted_chars == 0 {
            return;
        }
        if added_lines > 0 {
            self.insert_alignment_lines(first_line, added_lines);
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
        let start_at_line_start = start == self.rope.line_to_char(first_line);
        let removed_chars = end.saturating_sub(start);
        let removed_lines = last_line.saturating_sub(first_line);
        let removed_text = self.rope.slice(start..end).to_string();
        self.rope.remove(start..end);
        self.edit_seq = self.edit_seq.wrapping_add(1);
        self.block_start = None;
        self.block = None;
        if removed_lines > 0 {
            self.remove_alignment_lines(first_line, removed_lines, start_at_line_start);
        }
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

    /// Advance the statistics fold by at most `budget` lines. The draw loop
    /// calls this with a budget; `stats()` calls it with `usize::MAX`.
    pub(crate) fn warm_stats_cache(&self, budget: usize) -> bool {
        let mut cache = self.stats_cache.borrow_mut();
        if cache.edit_seq == self.edit_seq {
            return false;
        }

        let line_count = self.rope.len_lines();
        if cache.lines.len() != line_count {
            cache.lines.clear();
            cache.lines.resize(line_count, CachedLineStats::default());
            cache.totals = StatsTotals::default();
            cache.sentences_dirty = false;
            cache.dirty = Some((0, line_count));
            cache.edit_seq = u64::MAX;
            cache.stats = crate::stats::WordStats::default();
        }

        let ends_with_newline =
            self.rope.len_chars() > 0 && self.rope.char(self.rope.len_chars() - 1) == '\n';
        let eof_line = if ends_with_newline {
            line_count
        } else {
            line_count.saturating_sub(1)
        };

        if let Some((first, last)) = cache.dirty {
            let first = first.min(line_count);
            let last = last.min(line_count);
            let next = if budget == usize::MAX {
                last
            } else {
                first.saturating_add(budget).min(last)
            };
            if first < next {
                self.fold_stats_range(&mut cache, first, next, eof_line);
            }
            cache.dirty = (next < last).then_some((next, last));
        }

        let complete = cache.dirty.is_none();
        if complete && cache.sentences_dirty {
            refold_sentence_chain(&mut cache);
            cache.sentences_dirty = false;
        }
        publish_stats(&mut cache);
        if complete {
            cache.edit_seq = self.edit_seq;
        }
        !complete
    }

    pub fn stats_ready(&self) -> bool {
        self.stats_cache.borrow().edit_seq == self.edit_seq
    }

    pub(crate) fn stats_snapshot(&self) -> crate::stats::WordStats {
        self.stats_cache.borrow().stats.clone()
    }

    pub(crate) fn stats_if_ready(&self) -> Option<crate::stats::WordStats> {
        // A small budget makes the status and panels correct for ordinary
        // documents on their first frame. The draw loop finishes the fold in
        // larger chunks for very large ones.
        self.warm_stats_cache(256);
        if self.stats_ready() {
            Some(self.stats_snapshot())
        } else {
            None
        }
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
    #[cfg(test)]
    pub fn stats(&self) -> crate::stats::WordStats {
        self.warm_stats_cache(usize::MAX);
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

    fn insert_alignment_lines(&mut self, first_line: usize, count: usize) {
        if count == 0 {
            return;
        }
        let inherited = self
            .paragraph_alignments
            .get(first_line)
            .copied()
            .unwrap_or_default();
        let index = first_line
            .saturating_add(1)
            .min(self.paragraph_alignments.len());
        self.paragraph_alignments
            .splice(index..index, std::iter::repeat_n(inherited, count));
    }

    fn remove_alignment_lines(&mut self, first_line: usize, count: usize, at_line_start: bool) {
        if count == 0 || self.paragraph_alignments.is_empty() {
            return;
        }
        let start = if at_line_start {
            first_line
        } else {
            first_line.saturating_add(1)
        }
        .min(self.paragraph_alignments.len());
        let end = start
            .saturating_add(count)
            .min(self.paragraph_alignments.len());
        self.paragraph_alignments.drain(start..end);
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

#[cfg(test)]
mod tests;

#[cfg(test)]
mod bench;
