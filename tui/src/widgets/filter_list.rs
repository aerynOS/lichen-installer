// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! A list narrowed by typing.
//!
//! Locales, timezones, desktops and access points are all the same problem,
//! filtering hunderds of rows for one, so they share this.

use crate::theme::*;
use ratatui::{
    Frame,
    buffer::CellDiffOption,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
};
use std::borrow::Cow;

/// One row: the value written to the model, what the user reads, and an aside
pub struct Entry {
    pub value: String,
    pub label: String,
    pub hint: String,
}

impl Entry {
    pub fn new(value: Cow<'static, str>, label: Cow<'static, str>, hint: Cow<'static, str>) -> Self {
        Self {
            value: value.into(),
            label: label.into(),
            hint: hint.into(),
        }
    }
}

/// What the owning screen should do once the widget has seen a key
pub enum Outcome {
    Ignored,
    Consumed,
    Picked,
}

#[derive(Default)]
pub struct FilterList {
    entries: Vec<Entry>,
    matching: Vec<usize>,
    list: ListState,
    filter: String,
}

impl FilterList {
    /// Replace the contents, landing on `selected` when it is still present
    pub fn set_entries(&mut self, entries: Vec<Entry>, selected: &str) {
        self.entries = entries;
        self.filter.clear();
        self.refilter();

        let position = self
            .matching
            .iter()
            .position(|&index| self.entries[index].value == selected)
            .unwrap_or(0);

        self.list.select((!self.matching.is_empty()).then_some(position));
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn selected(&self) -> Option<&Entry> {
        let position = self.list.selected()?;
        let index = *self.matching.get(position)?;

        self.entries.get(index)
    }

    /// Case-insensitive substring across both label and value, so `en_GB` and
    /// `English (United Kingdom)` find the same row.
    fn refilter(&mut self) {
        let needle = self.filter.to_lowercase();

        self.matching = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                needle.is_empty()
                    || entry.label.to_lowercase().contains(&needle)
                    || entry.value.to_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect();

        let position = self
            .list
            .selected()
            .unwrap_or(0)
            .min(self.matching.len().saturating_sub(1));

        self.list.select((!self.matching.is_empty()).then_some(position));
    }

    fn move_selection(&mut self, delta: isize) {
        if self.matching.is_empty() {
            return;
        }

        let current = self.list.selected().unwrap_or(0) as isize;
        let next = current.saturating_add(delta).clamp(0, self.matching.len() as isize - 1);

        self.list.select(Some(next as usize));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Up => {
                self.move_selection(-1);
                Outcome::Consumed
            }
            KeyCode::Down => {
                self.move_selection(1);
                Outcome::Consumed
            }
            KeyCode::PageUp => {
                self.move_selection(-10);
                Outcome::Consumed
            }
            KeyCode::PageDown => {
                self.move_selection(10);
                Outcome::Consumed
            }
            KeyCode::Home => {
                self.move_selection(isize::MIN);
                Outcome::Consumed
            }
            KeyCode::End => {
                self.move_selection(isize::MAX);
                Outcome::Consumed
            }
            KeyCode::Enter => Outcome::Picked,
            KeyCode::Backspace => {
                self.filter.pop();
                self.refilter();
                Outcome::Consumed
            }
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.refilter();
                Outcome::Consumed
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.push(character);
                self.refilter();
                Outcome::Consumed
            }
            _ => Outcome::Ignored,
        }
    }

    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let [search, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
        let buffer = frame.buffer_mut();

        for position in body.positions() {
            if let Some(cell) = buffer.cell_mut(position) {
                cell.set_diff_option(CellDiffOption::AlwaysUpdate);
            }
        }

        let search_line = if self.filter.is_empty() {
            Line::styled(format!("Type to filter · {} entries", self.entries.len()), HINT)
        } else {
            Line::from(vec![
                Span::styled("Filter: ", HINT),
                Span::styled(self.filter.clone(), BODY),
                Span::styled("| ", STEP_ACTIVE),
                Span::styled(format!("    {} of {}", self.matching.len(), self.entries.len()), HINT),
            ])
        };

        frame.render_widget(Paragraph::new(search_line), search);

        if self.matching.is_empty() {
            frame.render_widget(Paragraph::new(Line::styled("Nothing matches...", HINT)), body);
            return;
        }

        let items: Vec<ListItem<'_>> = self
            .matching
            .iter()
            .map(|&index| {
                let entry = &self.entries[index];
                let mut spans = vec![Span::styled(entry.label.clone(), BODY)];

                if !entry.hint.is_empty() {
                    spans.push(Span::styled(format!("   {}", entry.hint), HINT));
                }

                ListItem::new(Line::from(spans))
            })
            .collect();

        frame.render_stateful_widget(
            List::new(items).highlight_style(SELECTED).highlight_symbol("▸ "),
            body,
            &mut self.list,
        );
    }
}
