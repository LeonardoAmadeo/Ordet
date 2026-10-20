#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WordStats {
    pub word_count: usize,
    pub chars: usize,
    pub chars_no_spaces: usize,
    pub paragraphs: usize,
    pub sentences: usize,
    pub reading_seconds: usize,
    pub pages: usize,
}

/// Whole-text reference scan for the word/character/sentence stats. The
/// editor itself counts incrementally per line (see `EditorBuffer`'s stats
/// cache); this is the independent implementation the tests compare against.
#[cfg(test)]
pub fn compute_word_stats(text: &str) -> WordStats {
    let normalized = text.replace('\r', "");
    let word_count = normalized.split_whitespace().count();
    let chars = normalized.chars().count();
    let chars_no_spaces = normalized.chars().filter(|ch| !ch.is_whitespace()).count();
    let paragraphs = if normalized.trim().is_empty() {
        0
    } else {
        normalized
            .split('\n')
            .map(str::trim)
            .filter(|chunk| !chunk.is_empty())
            .count()
    };
    let sentences = if normalized.trim().is_empty() {
        0
    } else {
        normalized
            .split(['.', '!', '?'])
            .map(str::trim)
            .filter(|chunk| !chunk.is_empty())
            .count()
    };
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

    WordStats {
        word_count,
        chars,
        chars_no_spaces,
        paragraphs,
        sentences,
        reading_seconds,
        pages,
    }
}

pub fn format_reading_time(total_seconds: usize) -> String {
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

#[cfg(test)]
mod tests {
    use super::{compute_word_stats, format_reading_time};

    #[test]
    fn word_stats_match_reference_metrics() {
        let stats = compute_word_stats("alpha beta.\n\ngamma delta!");

        assert_eq!(stats.word_count, 4);
        assert_eq!(stats.paragraphs, 2);
        assert_eq!(stats.sentences, 2);
        assert_eq!(stats.pages, 1);
        assert!(stats.reading_seconds >= 1);
    }

    #[test]
    fn reading_time_is_rendered_as_hms() {
        assert_eq!(format_reading_time(65), "00:01:05");
    }
}
