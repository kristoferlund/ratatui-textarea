#[cfg(feature = "arbitrary")]
use arbitrary::Arbitrary;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;

/// Specify how logical lines are soft-wrapped at render time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "arbitrary", derive(Arbitrary))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum WrapMode {
    /// Disable soft wrapping and keep horizontal scrolling behavior.
    None,
    /// Wrap only at word boundaries. Words wider than viewport are not split.
    Word,
    /// Wrap at grapheme boundaries.
    Glyph,
    /// Wrap at word boundaries, and fall back to grapheme wrapping for long words.
    WordOrGlyph,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct WrappedLine {
    pub row: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub first_in_row: bool,
    pub last_in_row: bool,
}

#[derive(Clone, Copy)]
struct Chunk {
    start: usize,
    end: usize,
}

pub(crate) fn effective_wrap_width(total_width: u16, line_number_len: Option<u8>) -> usize {
    let total_width = total_width as usize;
    let reserved = line_number_len.map(|len| len as usize + 2).unwrap_or(0);
    if total_width > reserved {
        total_width - reserved
    } else {
        1
    }
}

pub(crate) fn wrapped_rows(
    lines: &[String],
    mode: WrapMode,
    width: usize,
    tab_len: u8,
    mask: Option<char>,
) -> Vec<WrappedLine> {
    let mut rows = Vec::new();

    for (row, line) in lines.iter().enumerate() {
        let ranges = line_ranges(line, mode, width, tab_len, mask);
        let mut start_col = 0usize;
        for (i, (start_byte, end_byte)) in ranges.iter().copied().enumerate() {
            let end_col = start_col + line[start_byte..end_byte].chars().count();
            rows.push(WrappedLine {
                row,
                start_byte,
                end_byte,
                start_col,
                end_col,
                first_in_row: i == 0,
                last_in_row: i + 1 == ranges.len(),
            });
            start_col = end_col;
        }
    }

    rows
}

pub(crate) fn line_ranges(
    line: &str,
    mode: WrapMode,
    width: usize,
    tab_len: u8,
    mask: Option<char>,
) -> Vec<(usize, usize)> {
    if mode == WrapMode::None {
        return vec![(0, line.len())];
    }

    let width = width.max(1);
    let mut out = match mode {
        WrapMode::None => vec![(0, line.len())],
        WrapMode::Glyph => {
            let mut chunks = Vec::new();
            split_range_by_grapheme_width(line, 0, line.len(), width, tab_len, mask, &mut chunks);
            chunks
        }
        WrapMode::Word => wrap_word_chunks(line, width, tab_len, mask, false),
        WrapMode::WordOrGlyph => wrap_word_chunks(line, width, tab_len, mask, true),
    };

    if out.is_empty() {
        out.push((0, 0));
    }
    out
}

fn wrap_word_chunks(
    line: &str,
    width: usize,
    tab_len: u8,
    mask: Option<char>,
    fallback_to_glyph: bool,
) -> Vec<(usize, usize)> {
    let chunks: Vec<_> = UnicodeSegmentation::split_word_bound_indices(line)
        .map(|(start, text)| Chunk {
            start,
            end: start + text.len(),
        })
        .collect();

    if chunks.is_empty() {
        return vec![(0, 0)];
    }

    let mut out: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    let mut seg_start = chunks[0].start;
    let mut seg_end = seg_start;
    let mut seg_width = 0usize;

    while i < chunks.len() {
        let chunk = chunks[i];
        if seg_end == seg_start {
            seg_start = chunk.start;
        }

        let text = chunk_text(line, chunk);
        let chunk_width = display_width_from(text, seg_width, tab_len, mask);
        // Whitespace at a break hangs at the end of the row before it, past the right edge if need be, so that a row
        // never starts with the space that separates it from the previous one
        let is_space = text.chars().all(char::is_whitespace);
        if is_space && seg_end == seg_start {
            if let Some(prev) = out.last_mut().filter(|prev| prev.1 == chunk.start) {
                prev.1 = chunk.end;
                i += 1;
                seg_start = chunk.end;
                seg_end = chunk.end;
                continue;
            }
        }

        if seg_width + chunk_width <= width || (is_space && seg_end > seg_start) {
            seg_end = chunk.end;
            seg_width += chunk_width;
            i += 1;
            continue;
        }

        if seg_end > seg_start {
            out.push((seg_start, seg_end));
            seg_start = seg_end;
            seg_width = 0;
            continue;
        }

        if fallback_to_glyph {
            split_range_by_grapheme_width(
                line,
                chunk.start,
                chunk.end,
                width,
                tab_len,
                mask,
                &mut out,
            );
        } else {
            out.push((chunk.start, chunk.end));
        }

        i += 1;
        seg_start = chunk.end;
        seg_end = chunk.end;
        seg_width = 0;
    }

    if seg_end > seg_start {
        out.push((seg_start, seg_end));
    }

    out
}

fn split_range_by_grapheme_width(
    line: &str,
    start: usize,
    end: usize,
    width: usize,
    tab_len: u8,
    mask: Option<char>,
    out: &mut Vec<(usize, usize)>,
) {
    let mut segment_start = start;
    while segment_start < end {
        let mut segment_end = segment_start;
        let mut segment_width = 0usize;

        for (offset, grapheme) in
            UnicodeSegmentation::grapheme_indices(&line[segment_start..end], true)
        {
            let grapheme_start = segment_start + offset;
            let grapheme_end = grapheme_start + grapheme.len();
            let next_width = display_width_to(grapheme, segment_width, tab_len, mask);
            let grapheme_width = next_width.saturating_sub(segment_width);

            if segment_end != segment_start && segment_width + grapheme_width > width {
                break;
            }

            segment_end = grapheme_end;
            segment_width = next_width;
            if segment_width > width {
                break;
            }
        }

        if segment_end == segment_start {
            if let Some(ch) = line[segment_start..end].chars().next() {
                segment_end = segment_start + ch.len_utf8();
            } else {
                break;
            }
        }

        out.push((segment_start, segment_end));
        segment_start = segment_end;
    }
}

#[inline]
fn chunk_text(line: &str, chunk: Chunk) -> &str {
    &line[chunk.start..chunk.end]
}

fn display_width_from(text: &str, start_width: usize, tab_len: u8, mask: Option<char>) -> usize {
    display_width_to(text, start_width, tab_len, mask).saturating_sub(start_width)
}

// Every character is drawn as the mask character when a mask is set, so the width on screen is the mask's width
fn display_width_to(text: &str, mut width: usize, tab_len: u8, mask: Option<char>) -> usize {
    for c in text.chars() {
        if let Some(mask) = mask {
            width += mask.width().unwrap_or(0);
        } else if c == '\t' {
            if tab_len > 0 {
                let tab = tab_len as usize;
                let pad = tab - (width % tab);
                width += pad;
            }
        } else {
            width += c.width().unwrap_or(0);
        }
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segments(line: &str, mode: WrapMode, width: usize) -> Vec<&str> {
        line_ranges(line, mode, width, 4, None)
            .into_iter()
            .map(|(s, e)| &line[s..e])
            .collect()
    }

    fn masked_segments(line: &str, mode: WrapMode, width: usize, mask: char) -> Vec<&str> {
        line_ranges(line, mode, width, 4, Some(mask))
            .into_iter()
            .map(|(s, e)| &line[s..e])
            .collect()
    }

    #[test]
    fn word_wrap_keeps_long_word() {
        let have = segments("helloworld", WrapMode::Word, 4);
        assert_eq!(have, vec!["helloworld"]);
    }

    #[test]
    fn word_or_glyph_wrap_splits_long_word() {
        let have = segments("helloworld", WrapMode::WordOrGlyph, 4);
        assert_eq!(have, vec!["hell", "owor", "ld"]);
    }

    #[test]
    fn glyph_wrap_handles_wide_chars() {
        let have = segments("ab犬猫", WrapMode::Glyph, 4);
        assert_eq!(have, vec!["ab犬", "猫"]);
    }

    #[test]
    fn glyph_wrap_keeps_combining_grapheme_cluster() {
        let have = segments("e\u{301}x", WrapMode::Glyph, 1);
        assert_eq!(have, vec!["e\u{301}", "x"]);
    }

    #[test]
    fn tab_width_is_accounted_for_in_wrap() {
        let have = segments("\tX", WrapMode::WordOrGlyph, 2);
        assert_eq!(have, vec!["\t", "X"]);
    }

    #[test]
    fn glyph_wrap_preserves_full_mixed_width_row_capacity() {
        let have = segments("a中bcde", WrapMode::Glyph, 4);
        assert_eq!(have, vec!["a中b", "cde"]);
    }

    #[test]
    fn mask_wraps_by_the_mask_width() {
        let have = masked_segments("a中\tcde", WrapMode::Glyph, 4, '*');
        assert_eq!(have, vec!["a中\tc", "de"]);
        let have = masked_segments("ab cd", WrapMode::Word, 4, '＊');
        assert_eq!(have, vec!["ab ", "cd"]);
    }

    #[test]
    fn word_wrap_hangs_whitespace_at_a_break_on_the_row_before() {
        for mode in [WrapMode::Word, WrapMode::WordOrGlyph] {
            let have = segments("aaaa bbbb cccc", mode, 4);
            assert_eq!(have, vec!["aaaa ", "bbbb ", "cccc"], "{mode:?}");

            let have = segments("aa   bb", mode, 2);
            assert_eq!(have, vec!["aa   ", "bb"], "{mode:?}");

            let have = segments("aa \t bb", mode, 3);
            assert_eq!(have, vec!["aa \t ", "bb"], "{mode:?}");
        }
    }

    #[test]
    fn word_wrap_hangs_whitespace_after_a_long_word() {
        let have = segments("helloworld x", WrapMode::Word, 4);
        assert_eq!(have, vec!["helloworld ", "x"]);

        let have = segments("helloworld x", WrapMode::WordOrGlyph, 4);
        assert_eq!(have, vec!["hell", "owor", "ld ", "x"]);
    }

    #[test]
    fn word_wrap_keeps_leading_whitespace_of_a_line() {
        let have = segments("    ab", WrapMode::Word, 2);
        assert_eq!(have, vec!["    ", "ab"]);
    }
}
