// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Keybord layout

use crate::{
    events::Msg,
    screens::Context,
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
    /// Cloned on entry: the apply RPC fires from a key press, and `handle_key`
    /// is given no context.
    ctx: Option<Context>,
    /// The layout this picker last asked for. Compared against the model so an
    /// import can be noticed without re-applying every frame.
    current: String,
}

impl Keyboard {
    pub fn new() -> Self {
        Self {
            list: FilterList::default(),
            available: Vec::new(),
            applied: None,
            requested: false,
            ctx: None,
            current: String::new(),
        }
    }

    /// Fetch the layouts once, at startup.
    pub fn start(&mut self, ctx: &Context) {
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

    /// Re-apply when an install-model.kdl changed the model's layout.
    pub fn sync(&mut self, model: &Model) {
        if self.available.is_empty() || self.current == model.region.layout {
            return;
        }

        let layout = model.region.layout.clone();
        let console = match model.region.keymap.is_empty() {
            true => self.console_for(&layout),
            false => model.region.keymap.clone(),
        };
        let entries = self
            .available
            .iter()
            .map(|keymap| {
                Entry::new(
                    keymap.layout.clone().into(),
                    keymap.description.clone().into(),
                    keymap.layout.clone().into(),
                )
            })
            .collect();

        // FilterList can only be repositioned by being handed its entries again
        self.list.set_entries(entries, &layout);
        self.current = layout.clone();
        self.apply(layout, console);
    }

    /// The console keymap a layout maps to, empty for the layouts systemd
    /// has no equivalent for.
    fn console_for(&self, layout: &str) -> String {
        self.available
            .iter()
            .find(|keymap| keymap.layout == layout)
            .map(|keymap| keymap.console.clone())
            .unwrap_or_default()
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

    /// Returns true when the overlay should close, which is the moment a
    /// layout is picked.
    pub fn handle_key(&mut self, key: KeyEvent, model: &mut Model) -> bool {
        match self.list.handle_key(key) {
            Outcome::Picked => {
                let Some(entry) = self.list.selected() else {
                    return false;
                };
                let layout = entry.value.clone();
                let console = self.console_for(&layout);

                model.region.layout = layout.clone();
                model.region.keymap = console.clone();
                self.apply(layout, console);
                true
            }
            Outcome::Consumed | Outcome::Ignored => false,
        }
    }

    pub fn on_message(&mut self, msg: &Msg, model: &Model) {
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
                self.current = model.region.layout.clone();
            }
            Msg::KeymapApplied(applied) => self.applied = applied.clone(),
            _ => {}
        }
    }

    pub fn hints(&self) -> &[(&str, &str)] {
        &[
            ("type", "filter"),
            ("↑↓", "choose"),
            ("Enter", "select"),
            ("Esc", "close"),
        ]
    }

    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
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
