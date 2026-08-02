// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Choosing the desktop environment.

use super::{Context, Screen};
use crate::{
    events::Action,
    selections::{Selection, desktops, packages_for},
    theme::*,
    widgets::{Entry, FilterList, Outcome},
};
use installer::Model;
use ratatui::{
    Frame,
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
};

pub struct Desktop {
    list: FilterList,
    /// Kept alongside the list so the highlighted row can show its description
    available: Vec<Selection>,
    loaded: bool,
}

impl Desktop {
    pub fn new() -> Self {
        Self {
            list: FilterList::default(),
            available: Vec::new(),
            loaded: false,
        }
    }

    fn description(&self) -> Option<&str> {
        let value = &self.list.selected()?.value;
        self.available
            .iter()
            .find(|selection| selection.name == *value)
            .map(|selection| selection.description.as_str())
    }
}

impl Screen for Desktop {
    fn title(&self) -> &str {
        "Desktop"
    }

    fn hints(&self) -> &[(&str, &str)] {
        &[("type", "filter"), ("↑↓", "choose"), ("⏎", "select")]
    }

    fn is_complete(&self, model: &Model) -> bool {
        !model.software.selection.is_empty()
    }

    fn handle_key(&mut self, key: KeyEvent, model: &mut Model) -> Action {
        match self.list.handle_key(key) {
            Outcome::Picked => {
                let Some(entry) = self.list.selected() else {
                    return Action::Consumed;
                };
                let picked = entry.value.clone();

                model.software.selection = picked;
                // Derived here rather than the summary, so a selection that
                // cannot be satisfied is reported while it can still be changed.
                match packages_for(model) {
                    Ok(()) => Action::Next,
                    Err(error) => Action::Failed(error.to_string()),
                }
            }
            Outcome::Consumed => Action::Consumed,
            Outcome::Ignored => Action::Ignored,
        }
    }

    fn on_enter(&mut self, _ctx: &Context, model: &Model) {
        if self.loaded {
            return;
        }

        self.loaded = true;
        self.available = desktops();

        let entries = self
            .available
            .iter()
            .map(|selection| {
                Entry::new(
                    selection.name.clone().into(),
                    selection.summary.clone().into(),
                    selection.name.clone().into(),
                )
            })
            .collect();

        // Lands on an imported model's choice when there is one
        self.list.set_entries(entries, &model.software.selection);
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, _model: &Model) {
        let [heading, body, detail] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(5)]).areas(area);

        frame.render_widget(
            Paragraph::new(Line::styled("Select your desktop environment", HEADING)),
            heading,
        );

        if self.list.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled("No selections were pre-loaded...", WARNING)),
                body,
            );
            return;
        }

        // Taken before the list is rendered, which borrows mutably
        let description = self.description().unwrap_or_default().to_string();

        self.list.render(frame, body);

        let block = Block::default().borders(Borders::TOP).border_style(FRAME);
        let inner = block.inner(detail);

        frame.render_widget(block, detail);
        frame.render_widget(
            Paragraph::new(description).style(HINT).wrap(Wrap { trim: false }),
            inner,
        );
    }
}
