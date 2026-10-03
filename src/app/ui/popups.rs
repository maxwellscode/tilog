//! The boxes that open above the status line: the command menu and the matches of Tab.

use super::text::ellipsize_end;
use crate::app::{App, Areas, BORDER_ROWS};
use crate::command;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Clear;
use ratatui::widgets::List;
use ratatui::widgets::ListItem;
use ratatui::widgets::ListState;

impl App {
    /// A popup that sits directly above the status line, as wide as the screen.
    pub(super) fn popup_above_status(areas: &Areas, rows: usize) -> Rect {
        let height = (rows as u16)
            .saturating_add(BORDER_ROWS)
            .min(areas.body.height);
        Rect::new(
            areas.status.x,
            areas.status.y - height,
            areas.status.width,
            height,
        )
    }

    /// The command menu.
    pub(super) fn render_menu(&self, frame: &mut Frame, areas: &Areas) {
        let items = self.menu_items();
        if items.is_empty() {
            return;
        }

        let width = command::usage_column_width();
        let area = Self::popup_above_status(areas, items.len());
        // What is left of the line after the usage column and the border: longer descriptions end in `…`.
        let room = usize::from(area.width).saturating_sub(width + 2);
        let rows: Vec<ListItem> = items
            .iter()
            .map(|spec| {
                ListItem::new(Line::from(vec![
                    Span::from(format!("{:<width$}", spec.usage)).cyan(),
                    Span::raw(ellipsize_end(spec.help, room)),
                ]))
            })
            .collect();

        let hint = Line::from(" ↑↓ select · Tab complete · Enter run · Esc cancel ").dark_gray();
        let list = List::new(rows)
            .block(Block::bordered().title(" Commands ").title_bottom(hint))
            .highlight_style(Style::new().bg(Color::DarkGray).bold());
        // `ListState` remembers which row is selected and scrolls the list to keep it visible.
        let mut state =
            ListState::default().with_selected(Some(self.menu_selected.min(items.len() - 1)));

        frame.render_widget(Clear, area);
        frame.render_stateful_widget(list, area, &mut state);
    }

    /// The matches left after Tab on an ambiguous argument: a path, a session name or an ssh
    /// host. The arrows move the highlight; Tab or Enter put it into the prompt.
    pub(super) fn render_candidates(&self, frame: &mut Frame, areas: &Areas) {
        let candidates = &self.candidates;
        if candidates.is_empty() {
            return;
        }
        let rows: Vec<ListItem> = candidates
            .items
            .iter()
            .map(|item| ListItem::new(item.as_str()))
            .collect();
        let area = Self::popup_above_status(areas, rows.len());

        let hint =
            Line::from(" ↑↓ select · Tab or Enter insert · keep typing to narrow ").dark_gray();
        let list = List::new(rows)
            .block(Block::bordered().title(" Matches ").title_bottom(hint))
            .highlight_style(Style::new().bg(Color::DarkGray).bold());
        // The list scrolls to keep the highlighted row visible when there are more than fit.
        let selected = candidates.selected.min(candidates.items.len() - 1);
        let mut state = ListState::default().with_selected(Some(selected));

        frame.render_widget(Clear, area);
        frame.render_stateful_widget(list, area, &mut state);
    }
}
