//! The mouse: clicking tabs and windows, the wheel, dragging to select text.

use crate::app::{App, Areas, H_STEP, WHEEL_LINES};
use crate::select::Selection;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

impl App {
    pub(in crate::app) fn on_mouse(&mut self, mouse: MouseEvent, areas: &Areas) {
        if self.show_help {
            return;
        }
        let position = Position::new(mouse.column, mouse.row);
        let tile_under_mouse = areas.tiles.iter().position(|rect| rect.contains(position));

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.selection = None;
                if areas.tabbar.contains(position) {
                    if let Some(tab) = self.tab_at(mouse.column, areas.tabbar.width) {
                        self.go_to_tab(tab);
                    }
                } else if let Some(index) = tile_under_mouse {
                    self.select_tile(index);
                    // A drag from here selects text in this tile.
                    if let Some(point) = self.point_at(index, areas, mouse.column, mouse.row) {
                        self.selection = Some(Selection {
                            tile: index,
                            anchor: point,
                            head: point,
                            dragged: false,
                        });
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(selection) = self.selection
                    && let Some(head) =
                        self.point_at(selection.tile, areas, mouse.column, mouse.row)
                {
                    let dragged = selection.dragged || head != selection.anchor;
                    self.selection = Some(Selection {
                        head,
                        dragged,
                        ..selection
                    });
                }
            }
            // Letting go copies what was selected. The highlight stays until the next key or click.
            MouseEventKind::Up(MouseButton::Left) => match self.selection {
                Some(selection) if selection.dragged => self.copy_selection(),
                _ => self.selection = None,
            },
            // The wheel scrolls the tile under the pointer, selected or not. Sideways wheels
            // scroll sideways, and so does Shift+wheel for mice without one.
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => {
                let Some(index) = tile_under_mouse else {
                    return;
                };
                let (height, width) = (areas.tile_height(index), areas.tile_width(index));
                let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);

                if let Some(tile) = self.tile_mut(index) {
                    match (mouse.kind, shift) {
                        (MouseEventKind::ScrollUp, false) => tile.scroll_up(height, WHEEL_LINES),
                        (MouseEventKind::ScrollDown, false) => {
                            tile.scroll_down(height, WHEEL_LINES);
                        }
                        (MouseEventKind::ScrollLeft, _) | (MouseEventKind::ScrollUp, true) => {
                            tile.scroll_left(H_STEP);
                        }
                        (MouseEventKind::ScrollRight, _) | (MouseEventKind::ScrollDown, true) => {
                            tile.scroll_right(height, width, H_STEP);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}
