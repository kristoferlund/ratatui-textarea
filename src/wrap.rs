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
) -> Vec<WrappedLine> {
    let mut rows = Vec::new();

    for (row, line) in lines.iter().enumerate() {
        let ranges = line_ranges(line, mode, width, tab_len);
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
) -> Vec<(usize, usize)> {
    if mode == WrapMode::None {
        return vec![(0, line.len())];
    }

    let width = width.max(1);
    let mut out = match mode {
        WrapMode::None => vec![(0, line.len())],
        WrapMode::Glyph => {
            let mut chunks = Vec::new();
            split_range_by_grapheme_width(line, 0, line.len(), width, tab_len, &mut chunks);
            chunks
        }
        WrapMode::Word => wrap_word_chunks(line, width, tab_len, false),
        WrapMode::WordOrGlyph => wrap_word_chunks(line, width, tab_len, true),
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

    let mut out = Vec::new();
    let mut seg_start = 0usize;
    let mut seg_end = 0usize;
    let mut seg_width = 0usize;
    // The row ends on a word wider than the row, and takes nothing more but the whitespace after it
    let mut closed = false;
    // Whitespace hangs past the right edge of the row, which takes nothing more
    let mut hung = false;

    for chunk in chunks {
        let text = chunk_text(line, chunk);

        // Whitespace at a break stays on the row before it, so that a row never starts with the space that separates
        // it from the previous one. What fits is drawn, and at most one character hangs past the right edge, where it
        // is not drawn. Any more starts the next row, so that only the last character of a full row, the whitespace
        // hung after it and the end of the line after that share a cell, the last one of the row.
        // A tab hangs like a space, whatever its width, since nothing past the edge is drawn.
        if text.chars().all(char::is_whitespace) {
            for (offset, c) in text.char_indices() {
                if hung {
                    out.push((seg_start, seg_end));
                    seg_start = seg_end;
                    seg_width = 0;
                    closed = false;
                }
                let end = offset + c.len_utf8();
                seg_width = display_width_to(&text[offset..end], seg_width, tab_len);
                seg_end = chunk.start + end;
                hung = seg_width > width;
            }
            continue;
        }

        let chunk_width = display_width_from(text, seg_width, tab_len);
        if !closed && seg_width + chunk_width <= width {
            seg_end = chunk.end;
            seg_width += chunk_width;
            continue;
        }

        if seg_end > seg_start {
            out.push((seg_start, seg_end));
            seg_start = seg_end;
            closed = false;
            hung = false;
        }

        let chunk_width = display_width_from(text, 0, tab_len);
        if chunk_width <= width {
            seg_end = chunk.end;
            seg_width = chunk_width;
            continue;
        }

        // A word wider than the row is split, or kept whole on a row of its own
        if fallback_to_glyph {
            split_range_by_grapheme_width(line, chunk.start, chunk.end, width, tab_len, &mut out);
            (seg_start, seg_end) = out.pop().unwrap_or((chunk.start, chunk.end));
        } else {
            (seg_start, seg_end) = (chunk.start, chunk.end);
        }
        seg_width = display_width_from(&line[seg_start..seg_end], 0, tab_len);
        closed = true;
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
            let next_width = display_width_to(grapheme, segment_width, tab_len);
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

fn display_width_from(text: &str, start_width: usize, tab_len: u8) -> usize {
    display_width_to(text, start_width, tab_len).saturating_sub(start_width)
}

fn display_width_to(text: &str, mut width: usize, tab_len: u8) -> usize {
    for c in text.chars() {
        if c == '\t' {
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
        line_ranges(line, mode, width, 4)
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
    fn word_wrap_hangs_whitespace_at_a_break_on_the_row_before() {
        for mode in [WrapMode::Word, WrapMode::WordOrGlyph] {
            let have = segments("aaaa bbbb cccc", mode, 4);
            assert_eq!(have, vec!["aaaa ", "bbbb ", "cccc"], "{mode:?}");

            // A tab hangs like a space, at the edge or across it
            let have = segments("aaa\tbb", mode, 3);
            assert_eq!(have, vec!["aaa\t", "bb"], "{mode:?}");
            let have = segments("aa\tbb", mode, 3);
            assert_eq!(have, vec!["aa\t", "bb"], "{mode:?}");
        }
    }

    #[test]
    fn word_wrap_hangs_at_most_one_cell_of_whitespace() {
        for mode in [WrapMode::Word, WrapMode::WordOrGlyph] {
            // The whitespace that fits stays on the row, one more character hangs past the edge, and the rest starts
            // the next row
            let have = segments("aa        bb", mode, 4);
            assert_eq!(have, vec!["aa   ", "     ", "bb"], "{mode:?}");

            let have = segments("aa   bb", mode, 2);
            assert_eq!(have, vec!["aa ", "  ", "bb"], "{mode:?}");

            let have = segments("aa \t bb", mode, 3);
            assert_eq!(have, vec!["aa \t", " bb"], "{mode:?}");
        }
    }

    #[test]
    fn word_wrap_hangs_whitespace_after_a_long_word() {
        let have = segments("helloworld   x", WrapMode::Word, 4);
        assert_eq!(have, vec!["helloworld ", "  x"]);

        let have = segments("helloworld x", WrapMode::WordOrGlyph, 4);
        assert_eq!(have, vec!["hell", "owor", "ld ", "x"]);
    }

    #[test]
    fn word_wrap_breaks_leading_whitespace_of_a_line_like_any_other() {
        for mode in [WrapMode::Word, WrapMode::WordOrGlyph] {
            let have = segments("    ab", mode, 2);
            assert_eq!(have, vec!["   ", " ", "ab"], "{mode:?}");
        }
    }
}
