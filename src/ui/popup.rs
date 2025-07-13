use crossterm::{event::{Event, KeyCode, KeyModifiers}};
use ratatui::{
    layout::{Alignment, Constraint, Flex, Layout, Rect}, style::{Color, Style}, text::{Line, Span, Text}, widgets::Block, Frame
};
use ratatui::widgets::Clear;

use crate::ui::action::Action;

pub(crate) struct KeysPopup {
    keys: Vec<(KeyModifiers, KeyCode, &'static str, Action)>,
}

impl KeysPopup {
    pub(crate) fn new(keys: Vec<(KeyModifiers, KeyCode, &'static str, Action)>) -> Self {
        Self { keys }
    }

    pub fn render(&self, f: &mut Frame, area: Rect) {
        let key_style = Style::default().bold().black();

        let mut keys_text = Vec::new();
        let mut descriptions_text = Vec::new();
        for (mods, code, description, _) in &self.keys {
            keys_text.push(Line::from(vec![
                Span::styled(self.format_key_event(*mods, code), key_style),
            ]));
            descriptions_text.push(Line::from(vec![
                Span::raw(*description),
            ]));
        }

        let keys_text = Text::from(keys_text);
        let descriptions_text = Text::from(descriptions_text);

        let [popup_area] = Layout::vertical([
                Constraint::Length(keys_text.height() as u16),
            ])
            .flex(Flex::Center)
            .areas(area);

        let [keys_area, descriptions_area] = Layout::horizontal([
                Constraint::Length(keys_text.width() as u16),
                Constraint::Length(descriptions_text.width() as u16),
            ])
            .spacing(2)
            .flex(Flex::Center)
            .areas(popup_area);

        let mut whole = keys_area.union(descriptions_area);
        whole.x -= 1;
        whole.y -= 1;
        whole.width += 2;
        whole.height += 2;

        f.render_widget(Clear, whole);
        f.render_widget(Block::new().style(Style::new().bg(Color::LightCyan)), whole);
        f.render_widget(
            keys_text.alignment(Alignment::Right),
            keys_area,
        );
        f.render_widget(descriptions_text, descriptions_area);
    }

    pub fn get_action(&self, event: Event) -> Option<Action> {
        if let Event::Key(key) = event
            && key.is_press()
            && let Some((_, _, _, action)) = self.keys.iter().find(|(m, k, _, _)| *m == key.modifiers && *k == key.code)
        {
            return Some(action.clone());
        }
        None
    }

    fn format_key_event(&self, mods: KeyModifiers, code: &KeyCode) -> String {
        let mut result = String::new();

        if mods.contains(KeyModifiers::CONTROL) {
            result.push_str("Ctrl-");
        }
        if mods.contains(KeyModifiers::SHIFT) {
            result.push_str("Shift-");
        }
        if mods.contains(KeyModifiers::ALT) {
            result.push_str("Alt-");
        }

        match code {
            KeyCode::Char(' ') => result.push_str("Space"),
            KeyCode::Char(c) => result.push(*c),
            KeyCode::Enter => result.push_str("Enter"),
            KeyCode::Esc => result.push_str("Esc"),
            KeyCode::Backspace => result.push_str("Backspace"),
            KeyCode::Tab => result.push_str("Tab"),
            KeyCode::Up => result.push('↑'),
            KeyCode::Down => result.push('↓'),
            KeyCode::Left => result.push('←'),
            KeyCode::Right => result.push('→'),
            _ => result.push_str(&format!("{code:?}")),
        }

        result
    }
}
