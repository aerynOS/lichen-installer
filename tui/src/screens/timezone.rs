// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! System timezone. The list is compiled in, so there is no RPC to wait on.

use super::{Context, Screen};
use crate::{
    events::Action,
    theme::*,
    widgets::{Entry, FilterList, Outcome},
};
use chrono_tz::TZ_VARIANTS;
use installer::Model;
use ratatui::{
    Frame,
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::Paragraph,
};

pub struct Timezone {
    list: FilterList,
    loaded: bool,
    chosen: bool,
}

impl Timezone {
    pub fn new() -> Self {
        Self {
            list: FilterList::default(),
            loaded: false,
            chosen: false,
        }
    }
}

impl Screen for Timezone {
    fn title(&self) -> &str {
        "Timezone"
    }

    fn hints(&self) -> &[(&str, &str)] {
        &[("type", "filter"), ("↑↓", "choose"), ("Enter", "select")]
    }

    fn is_complete(&self, _model: &Model) -> bool {
        self.chosen
    }

    fn handle_key(&mut self, key: KeyEvent, model: &mut Model) -> Action {
        match self.list.handle_key(key) {
            Outcome::Picked => {
                let Some(entry) = self.list.selected() else {
                    return Action::Consumed;
                };

                model.region.timezone = entry.value.clone();
                self.chosen = true;
                Action::Next
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
        self.chosen = model.imported;

        let entries = TZ_VARIANTS
            .iter()
            .map(|zone| Entry::new(zone.to_string().into(), zone.to_string().into(), "".into()))
            .collect();

        self.list.set_entries(entries, &model.region.timezone);
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, _model: &Model) {
        let [heading, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);

        frame.render_widget(Paragraph::new(Line::styled("Select your timezone", HEADING)), heading);
        self.list.render(frame, body);
    }
}
