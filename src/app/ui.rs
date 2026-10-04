//! Drawing. Nothing here changes the app: every function takes `&self` and only reads.

mod help;
pub(in crate::app) mod mono;
mod popups;
mod status;
mod tab_bar;
mod text;
mod tile_view;

pub(in crate::app) use help::{help_inner_width, help_lines};

use crate::app::{App, Areas};
use crate::source::Source;
use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

impl App {
    /// `&self` means: read-only borrow. The App stays valid after the call.
    pub(super) fn render(&self, frame: &mut Frame) {
        let areas = self.areas(frame.area());

        self.render_tabbar(frame, areas.tabbar);
        match self.current_source() {
            None => self.render_overview(frame, &areas),
            Some(source) => self.render_source(frame, source, &areas),
        }
        self.render_status(frame, areas.status);
        self.render_menu(frame, &areas);
        self.render_candidates(frame, &areas);

        if self.show_help {
            self.render_help(frame);
        }
    }

    fn render_overview(&self, frame: &mut Frame, areas: &Areas) {
        if self.sources.is_empty() {
            let hint = Paragraph::new(vec![
                Line::from("No sources yet."),
                Line::from("Add one with :add <file | ssh:host:/path | docker:name | kube:pod>")
                    .dark_gray(),
                Line::from("or restore a session with :load <name>").dark_gray(),
            ])
            .alignment(Alignment::Center);
            let body = areas.body;
            let area = Rect::new(
                body.x,
                body.y + body.height.saturating_sub(3) / 2,
                body.width,
                3,
            );
            frame.render_widget(hint, area);
            return;
        }

        for (index, (source, rect)) in self.sources.iter().zip(&areas.tiles).enumerate() {
            // Numbered like the tab of the same source, so a tile can be matched to its tab.
            let title = format!("{}: {}", index + 1, source.name());
            self.render_tile(
                frame,
                index,
                source.main(),
                &title,
                *rect,
                index == self.selected,
            );
        }
    }

    fn render_source(&self, frame: &mut Frame, source: &Source, areas: &Areas) {
        // The main tile is titled with where the source comes from (the tab already shows the
        // short name). A merged timeline has no single location.
        let location = if source.is_merged() {
            source.name()
        } else {
            source.path()
        };
        for (index, (tile, rect)) in source.tiles().iter().zip(&areas.tiles).enumerate() {
            self.render_tile(frame, index, tile, location, *rect, index == source.focus());
        }
    }
}
