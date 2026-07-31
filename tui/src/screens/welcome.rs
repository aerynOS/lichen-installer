// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Opening screen: states the contract before anything else happens.

use super::Screen;
use crate::{events::Action, theme::*};
use installer::Model;
use protocols::lichen::osinfo::OsInfo;
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent},
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

pub struct Welcome {
    os_name: String,
}

impl Welcome {
    pub fn new(info: &OsInfo) -> Self {
        let os_name = info
            .metadata
            .as_ref()
            .and_then(|meta| meta.identity.as_ref())
            .map(|identity| identity.display.clone())
            .unwrap_or_else(|| "Unknown OS".into());

        Self { os_name }
    }
}

impl Screen for Welcome {
    fn title(&self) -> &str {
        "Welcome"
    }

    fn hints(&self) -> &[(&str, &str)] {
        &[("⏎", "begin")]
    }

    fn is_complete(&self, _model: &Model) -> bool {
        true
    }

    fn handle_key(&mut self, key: KeyEvent, _model: &mut Model) -> Action {
        match key.code {
            KeyCode::Enter => Action::Next,
            _ => Action::Ignored,
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, _model: &Model) {
        let lines = vec![
            Line::styled(format!("Welcome to the {} installer", self.os_name), HEADING),
            Line::raw(""),
            Line::styled("This is alpha quality software. Use at your own risk!", WARNING),
            Line::raw(""),
            Line::styled(
                "Nothing is written to disk until you confirm on the Summary screen. \
                 Until that point, every choice can be revisited.",
                BODY,
            ),
            Line::raw(""),
            Line::from(vec![
                Span::styled("Press ", BODY),
                Span::styled("Enter", STEP_ACTIVE),
                Span::styled(" to begin.", BODY),
            ]),
        ];

        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }
}
