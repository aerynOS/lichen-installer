// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! System locale.

use super::{Context, Screen};
use crate::{
    events::{Action, Msg},
    theme::*,
    widgets::{Entry, FilterList, Outcome},
};
use installer::Model;
use protocols::lichen::locales::locales_client::LocalesClient;
use ratatui::{
    Frame,
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::Paragraph,
};

pub struct Locale {
    list: FilterList,
    requested: bool,
    /// The model always carries a valid default, so completeness means the
    /// choice was actually made rather than just present.
    chosen: bool,
}

impl Locale {
    pub fn new() -> Self {
        Self {
            list: FilterList::default(),
            requested: false,
            chosen: false,
        }
    }
}

impl Screen for Locale {
    fn title(&self) -> &str {
        "Locale"
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

                model.region.language = entry.value.clone();
                self.chosen = true;

                Action::Next
            }
            Outcome::Consumed => Action::Consumed,
            Outcome::Ignored => Action::Ignored,
        }
    }

    fn on_enter(&mut self, ctx: &Context, _model: &Model) {
        if self.requested {
            return;
        }

        self.requested = true;

        let channel = ctx.channel.clone();

        ctx.spawn(async move {
            let locales = LocalesClient::new(channel).list_locales(()).await?.into_inner().locales;
            Ok(Msg::Locales(locales))
        });
    }

    fn on_message(&mut self, msg: &Msg, model: &mut Model) {
        let Msg::Locales(locales) = msg else {
            return;
        };

        // locales.proto carries a territory flag per locale
        let entries = locales
            .iter()
            .map(|locale| {
                Entry::new(
                    locale.name.clone().into(),
                    locale.display_name.clone().into(),
                    locale.name.clone().into(),
                )
            })
            .collect();

        self.list.set_entries(entries, &model.region.language);
        self.chosen = model.imported;
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, _model: &Model) {
        let [heading, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);

        frame.render_widget(Paragraph::new(Line::styled("Select your locale", HEADING)), heading);
        if self.list.is_empty() {
            frame.render_widget(Paragraph::new(Line::styled("Fetching locales...", HINT)), body);
            return;
        }

        self.list.render(frame, body);
    }
}
