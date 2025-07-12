use ratatui::{
    layout::{Alignment, Constraint, Flex, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};

pub fn status_running_span() -> Span<'static> { Span::styled("•", Style::default().fg(Color::Yellow)) }
pub fn status_successful_span() -> Span<'static> { Span::styled("✔︎", Style::default().fg(Color::Green)) }
pub fn status_failed_span() -> Span<'static> { Span::styled("✖︎", Style::default().fg(Color::Red)) }
pub fn status_killed_span() -> Span<'static> { Span::styled("ѳ", Style::default().fg(Color::Red)) }
pub fn status_unknown_span() -> Span<'static> { Span::styled("?", Style::default().fg(Color::Red)) }

pub fn render_help(f: &mut Frame, area: Rect) {
    let key_style = Style::default().fg(Color::Yellow);

    let introduction_text = Text::from(vec![
        Line::from(""),
        Line::from(Span::styled("Welcome to Pipec!", Style::default().fg(Color::Cyan))),
        Line::from(""),
        Line::from(vec![Span::raw("Type a command and press "), Span::styled("Enter", key_style), Span::raw(" to get started.")]),
    ]);
    let keys_help = Text::from(vec![
        Line::from(vec![
            Span::styled("Enter        ", key_style),
            Span::raw("Execute command"),
        ]),
        Line::from(vec![
            Span::styled("Ctrl+Space   ", key_style),
            Span::raw("Show output of current stage"),
        ]),
        Line::from(vec![
            Span::styled("Ctrl+Q       ", key_style),
            Span::raw("Exit program"),
        ]),
        Line::from(vec![
            Span::styled("Ctrl-N       ", key_style),
            Span::raw("Add new stage below"),
        ]),
        Line::from(vec![
            Span::styled("Ctrl-P       ", key_style),
            Span::raw("Add new stage above"),
        ]),
        Line::from(vec![
            Span::styled("Ctrl-D       ", key_style),
            Span::raw("Delete focused stage"),
        ]),
        Line::from(vec![
            Span::styled("Ctrl-X       ", key_style),
            Span::raw("Enable/disable focused stage"),
        ]),
        Line::from(vec![
            Span::styled("Ctrl-Shift-C ", key_style),
            Span::raw("Hard-stop execution (send KILL)"),
        ]),
        Line::from(vec![
            Span::styled("↑/↓          ", key_style),
            Span::raw("Go to previous/next stage"),
        ]),
        Line::from(vec![
            Span::styled("Ctrl-L       ", key_style),
            Span::raw("View shown stage in pager"),
        ]),
        Line::from(vec![
            Span::styled("Ctrl-V       ", key_style),
            Span::raw("Edit focused stage in editor"),
        ]),
    ]);
    let legend = Text::from(vec![
        Line::from(vec![
            status_running_span(),    Span::raw(" running      "), status_killed_span(),  Span::raw(" killed"),
        ]),
        Line::from(vec![
            status_successful_span(), Span::raw(" successful   "), status_unknown_span(), Span::raw(" unknown"),
        ]),
        Line::from(vec![
            status_failed_span(), Span::raw(" failed"),
        ]),
    ]);

    let [introduction_area, keys_area, legend_area] =
        Layout::vertical([
            Constraint::Length(introduction_text.lines.len() as u16),
            Constraint::Length(keys_help.lines.len() as u16),
            Constraint::Length(legend.lines.len() as u16),
        ])
        .spacing(1)
        .flex(Flex::Center)
        .areas(area);
    let [introduction_area] =
        Layout::horizontal([Constraint::Length(introduction_text.width() as u16)])
        .flex(Flex::Center)
        .areas(introduction_area);
    let [keys_area] =
        Layout::horizontal([Constraint::Length(keys_help.width() as u16)])
        .flex(Flex::Center)
        .areas(keys_area);
    let [legend_area] =
        Layout::horizontal([Constraint::Length(legend.width() as u16)])
        .flex(Flex::Center)
        .areas(legend_area);

    f.render_widget(
        Paragraph::new(introduction_text) .alignment(Alignment::Center),
        introduction_area
    );
    f.render_widget(
        Paragraph::new(keys_help),
        keys_area
    );
    f.render_widget(
        Paragraph::new(legend),
        legend_area
    );
}