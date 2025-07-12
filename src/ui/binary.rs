use bstr::ByteSlice as _;
use itertools::Itertools as _;
use ratatui::{layout::Size, style::Stylize as _, text::{Line, Span, Text}};

pub(crate) const UNICODE_REPLACEMENT_CODEPOINT: &str = "\u{FFFD}";

/// Renders binary data into given `Text`.
/// Valid UTF8 graphemes are rendered as is, invalid ones are replaced with
/// Unicode Replacement codepoints.
///
/// This function is designed to reuse existing text. If size of area
/// and data haven't changed since last render, then no new allocations should occur.
pub(crate) fn render_binary<'t, 'i: 't, 'b: 't>(
    buf: &'b [u8], area: Size, text: &mut Text<'t>, invalid_slice: &'i mut String
) {
    // Line full of Unicode Replacement codepoint, which is used to represent
    // invalid bytes. We slice it to represent any number of such bytes in line
    // without requiring additional memory.
    if invalid_slice.len() < area.width as usize {
        *invalid_slice = UNICODE_REPLACEMENT_CODEPOINT.repeat(area.width as usize);
    }

    let output = bstr::BStr::new(buf);

    let mut current_line = 0;
    text.lines.resize(area.height as usize, Line::default());
    for line in &mut text.lines {
        line.spans.clear();
    }

    'outer: for (newline, graphemes) in output.grapheme_indices()
        .chunk_by(|(_, _, str)| *str == "\n")
        .into_iter()
    {
        if newline {
            current_line += graphemes.count();
            if current_line >= text.lines.len() {
                break 'outer;
            }
            continue;
        }

        for line in graphemes.chunks(area.width as usize).into_iter() {
            for (invalid, mut span) in line.chunk_by(|(_, _, g)| *g == UNICODE_REPLACEMENT_CODEPOINT).into_iter() {
                let span = if invalid {
                    Span::from(&invalid_slice[..UNICODE_REPLACEMENT_CODEPOINT.len()*span.count()])
                        .light_red()
                } else {
                    let first = span.next().unwrap();
                    let last = span.last().unwrap_or(first);
                    let (start, end) = (first.0, last.1);
                    Span::from(str::from_utf8(&buf[start..end]).unwrap())
                };

                text.lines[current_line].spans.push(span);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{layout::Size, style::Stylize, text::{Line, Span, Text}};
    use std::default::Default::default;

    use super::{render_binary, UNICODE_REPLACEMENT_CODEPOINT};

    #[test]
    fn test_render_binary_valid_utf8() {
        let mut invalid_slice = String::new();
        let mut text = Text::default();
        render_binary(b"abcdef", Size::new(10, 1), &mut text, &mut invalid_slice);
        assert_eq!(
            text,
            Text {
                lines: vec![
                    Line::from("abcdef"),
                ],
                ..default()
            }
        );
    }

    #[test]
    fn test_render_binary_invalid_utf8() {
        let mut invalid_slice = String::new();
        let mut text = Text::default();
        render_binary(b"abc\xffdef", Size::new(10, 1), &mut text, &mut invalid_slice);
        assert_eq!(
            text,
            Text::from(vec![
                Line::default().spans(vec![
                    Span::from("abc"),
                    Span::from(UNICODE_REPLACEMENT_CODEPOINT).light_red(),
                    Span::from("def"),
                ]),
            ])
        );
    }

    #[test]
    fn test_render_binary_renders_sequential_invalid_bytes_as_single_span() {
        let mut invalid_slice = String::new();
        let mut text = Text::default();
        render_binary(b"abc\xff\xffdef", Size::new(10, 1), &mut text, &mut invalid_slice);
        assert_eq!(
            text,
            Text::from(vec![
                Line::default().spans(vec![
                    Span::from("abc"),
                    Span::from(UNICODE_REPLACEMENT_CODEPOINT.repeat(2)).light_red(),
                    Span::from("def"),
                ]),
            ])
        );
    }

    /// Tests that lines are allocated for whole area, even if there is less
    /// real lines in buffer.
    #[test]
    fn test_render_binary_allocates_lines_for_whole_area() {
        let mut invalid_slice = String::new();
        let mut text = Text::default();
        render_binary(b"abc\xffdef", Size::new(10, 2), &mut text, &mut invalid_slice);
        assert_eq!(
            text,
            // We render just one line, but are has two rows.
            Text::from(vec![
                Line::default().spans(vec![
                    Span::from("abc"),
                    Span::from(UNICODE_REPLACEMENT_CODEPOINT).light_red(),
                    Span::from("def"),
                ]),
                Line::default(),
            ])
        );
    }

    /// Tests that number of rendered lines is limited by area height.
    #[test]
    fn test_render_binary_number_of_lines_limited_by_area_height() {
        let mut invalid_slice = String::new();
        let mut text = Text::default();
        render_binary(b"abc\ndef\nghi", Size::new(10, 2), &mut text, &mut invalid_slice);
        assert_eq!(
            text,
            // We render just one line, but are has two rows.
            Text::from(vec![
                Line::from("abc"),
                Line::from("def"),
            ])
        );
    }
}