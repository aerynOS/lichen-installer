// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Keybord layout
//!
//! Applied to the live session the moment it is chosen rather than only written
//! to the target: Accounts is three steps later, and a password typed on the
//! wrong layout is one the user cannot type after the first reboot.

use super::{Context, Screen};
use crate::{
    events::{Action, Msg},
    theme::*,
    widgets::{Entry, FilterList, Outcome},
};
use installer::Model;
use protocols::lichen::locales::{Keymap, SetKeymapRequest, locales_client::LocalesClient};
use ratatui::{
    Frame,
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::Paragraph,
};

pub struct Keyboard {
    list: FilterList,
    /// Kept so a chosen layout resolves back to its console keymap
    available: Vec<Keymap>,
    /// What actually took effect, which is not always what was asked for
    applied: Option<Keymap>,
    requested: bool,
    chosen: bool,
    /// Cloned on entry: the apply RPC fires from a key press, and `handle_key`
    /// is given no context.
    ctx: Option<Context>,
}

impl Keyboard {
    pub fn new() -> Self {
        Self {
            list: FilterList::default(),
            available: Vec::new(),
            applied: None,
            requested: false,
            chosen: false,
            ctx: None,
        }
    }

    /// Apply to the live session. A failure here is not fatal; the target
    /// still gets the right configuration, the user just keeps typing on the
    /// old layout, so it never reaches the error overlay.
    fn apply(&self, layout: String, console: String) {
        let Some(ctx) = self.ctx.clone() else {
            return;
        };
        let channel = ctx.channel.clone();

        ctx.spawn(async move {
            let response = LocalesClient::new(channel)
                .set_keymap(SetKeymapRequest { layout, console })
                .await;

            Ok(Msg::KeymapApplied(match response {
                Ok(response) => response.into_inner().applied,
                Err(_) => None,
            }))
        });
    }

    fn status(&self) -> Line<'static> {
        let Some(applied) = &self.applied else {
            return Line::styled(
                "Applied immediately, so the password you set later matches your keyboard.",
                HINT,
            );
        };

        if applied.console.is_empty() {
            return Line::styled(
                format!(
                    "{} applied. No console keymap exists for it, so the text console stays US.",
                    applied.description,
                ),
                WARNING,
            );
        }

        Line::styled(
            format!("{} applied, console keymap {}.", applied.description, applied.console),
            SUCCESS,
        )
    }
}

impl Screen for Keyboard {
    fn title(&self) -> &str {
        "Keyboard"
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
                let layout = entry.value.clone();
                let console = self
                    .available
                    .iter()
                    .find(|keymap| keymap.layout == layout)
                    .map(|keymap| keymap.console.clone())
                    .unwrap_or_default();

                model.region.layout = layout.clone();
                model.region.keymap = console.clone();
                self.chosen = true;
                self.apply(layout, console);
                Action::Next
            }
            Outcome::Consumed => Action::Consumed,
            Outcome::Ignored => Action::Ignored,
        }
    }

    fn on_enter(&mut self, ctx: &Context, _model: &Model) {
        if self.ctx.is_none() {
            self.ctx = Some(ctx.clone());
        }

        if self.requested {
            return;
        }

        self.requested = true;

        let channel = ctx.channel.clone();

        ctx.spawn(async move {
            let keymaps = LocalesClient::new(channel).list_keymaps(()).await?.into_inner().keymaps;

            Ok(Msg::Keymaps(keymaps))
        });
    }

    fn on_message(&mut self, msg: &Msg, model: &mut Model) {
        match msg {
            Msg::Keymaps(keymaps) => {
                let entries = keymaps
                    .iter()
                    .map(|keymap| {
                        Entry::new(
                            keymap.layout.clone().into(),
                            keymap.description.clone().into(),
                            keymap.layout.clone().into(),
                        )
                    })
                    .collect();

                self.list.set_entries(entries, &model.region.layout);
                self.available = keymaps.clone();
                self.chosen = model.imported;
            }
            Msg::KeymapApplied(applied) => self.applied = applied.clone(),
            _ => {}
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, _model: &Model) {
        let [heading, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);

        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("Select your keyboard layout", HEADING),
                self.status(),
            ]),
            heading,
        );

        if self.list.is_empty() {
            frame.render_widget(Paragraph::new(Line::styled("Fetching layouts...", HINT)), body);
            return;
        }

        self.list.render(frame, body);
    }
}
