use ratatui::style::{Color, Style};
use tree_sitter_highlight::{HighlightConfiguration, Highlighter, HighlightEvent};
use ratatui::text::Span;

const RECOGNIZED_NAMES: &[&str] = &[
    "string",
    "function",
    "property",
    "keyword",
    "comment",
    "number",
    "embedded",
    "operator",
    "constant",
];

fn get_style_for_highlight(highlight: usize, style: Style) -> Style {
    match highlight {
        0 /*"string"*/ => style.fg(Color::Green),
        1 /*"function"*/ => style.fg(Color::Red),
        2 /*"property"*/ => style.fg(Color::Blue),
        3 /*"keyword"*/ => style.fg(Color::Yellow),
        4 /*"comment"*/ => style.fg(Color::Gray),
        5 /*"number"*/ => style.fg(Color::Cyan),
        6 /*"embedded"*/ => style,
        7 /*"operator"*/ => style,
        8 /*"constant"*/ => style,
        _ => style,
    }
}

pub(crate) fn highlight_command_to_spans<'c>(command: &'c str, base_style: Style) -> Result<Vec<Span<'c>>, tree_sitter_highlight::Error> {
    let language = tree_sitter_bash::LANGUAGE;

    let mut config = HighlightConfiguration::new(
        language.into(),
        "bash",
        tree_sitter_bash::HIGHLIGHT_QUERY,
        "",
        "",
    ).unwrap();
    config.configure(RECOGNIZED_NAMES);

    let mut highlighter = Highlighter::new();
    let highlights = highlighter.highlight(
        &config,
        command.as_bytes(),
        None,
        |_| None,
    ).unwrap();

    let mut spans = Vec::new();
    let mut span_start = 0;
    let mut span_end = 0;

    let mut styles = Vec::with_capacity(3);
    styles.push(base_style);

    for event in highlights {
        match event? {
            HighlightEvent::Source { start, end } => {
                assert_eq!(span_end, start);
                span_end = end;
            }
            HighlightEvent::HighlightStart(highlight) => {
                let style = *styles.last().unwrap();
                if span_start != span_end {
                    let text = &command[span_start..span_end];
                    spans.push(Span::styled(text, style));
                    span_start = span_end;
                }
                styles.push(get_style_for_highlight(highlight.0, style));
            }
            HighlightEvent::HighlightEnd => {
                let style = styles.pop().unwrap();
                let text = &command[span_start..span_end];
                span_start = span_end;
                spans.push(Span::styled(text, style));
            }
        }
    }

    if span_start != span_end {
        let text = &command[span_start..span_end];
        spans.push(Span::styled(text, base_style));
    }
    assert!(styles.len() == 1);

    Ok(spans)
}
