// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Opening screen: states the contract before anything else happens.

use super::Screen;
use crate::{
    events::{Action, Msg},
    install_model::{is_install_model, parse_error_detail},
    screens::Context,
    theme::*,
    widgets::{Browser, BrowserOutcome, Field, Form, FormOutcome},
};
use installer::Model;
use protocols::lichen::{
    install::{FetchModelRequest, install_client::InstallClient},
    osinfo::OsInfo,
};
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent},
    layout::Rect,
    text::{Line, Span},
    widgets::{ListState, Paragraph, Wrap},
};
use std::collections::BTreeSet;

/// Imported model.kdl indices
const INSTALL_MODEL: usize = 0;
const SYSTEM_MODEL: usize = 1;

/// What each model slot is called and what it will accept.
const MODEL_SLOTS: [(&str, &str); 2] = [
    ("install-model", "installer settings, and optionally a package set"),
    ("system-model", "a package set only"),
];

/// Which part of the screen has the keyboard
enum Stage {
    /// The welcome text
    Intro,
    /// The two model slots, one highlighted
    Slots,
    /// Walking the filesystem for the highlighted slot
    Browsing,
    /// Typing a URI for the highlighted model slot
    Typing,
    /// Waiting on FetchModel
    Fetching,
}

pub struct Welcome {
    os_name: String,
    stage: Stage,
    model_slots: ListState,
    /// Where each model slot's document came from, for display
    sources: [Option<String>; 2],
    /// The documents themselves, applied in order when the screen is left
    documents: [Option<String>; 2],
    browser: Browser,
    uri: Form,
    /// The reason an import failed
    problem: Option<String>,
    /// Cloned on entry: fetching is started from handle_key
    ctx: Option<Context>,
}

impl Welcome {
    pub fn new(info: &OsInfo) -> Self {
        let os_name = info
            .metadata
            .as_ref()
            .and_then(|meta| meta.identity.as_ref())
            .map(|identity| identity.display.clone())
            .unwrap_or_else(|| "Unknown OS".into());
        let mut uri = Form::new(vec![Field::new("URI", false)]);

        uri.set_placeholder(0, "https://codeberg.org/.../system-model.kdl");

        Self {
            os_name,
            stage: Stage::Intro,
            model_slots: ListState::default().with_selected(Some(INSTALL_MODEL)),
            sources: [None, None],
            documents: [None, None],
            browser: Browser::new(".kdl"),
            uri,
            problem: None,
            ctx: None,
        }
    }

    /// Ask the backend for a document. Every scheme goes the same way, so the
    /// screen never has to know whether it was handed a path or URL.
    fn fetch(&mut self, uri: String) -> Action {
        let Some(ctx) = self.ctx.clone() else {
            return Action::Failed("not connected to the backend".to_string());
        };
        let channel = ctx.channel.clone();

        self.problem = None;
        self.stage = Stage::Fetching;

        ctx.spawn(async move {
            let contents = InstallClient::new(channel)
                .fetch_model(FetchModelRequest { uri: uri.clone() })
                .await?
                .into_inner()
                .contents;

            Ok(Msg::ModelFetched { uri, contents })
        });
        Action::Consumed
    }

    /// Accept the fetched document if it's the right type.
    ///
    /// The two documents are not interchangeable, and one in the other's model slot
    /// is a mistake worth naming rather than quietly working around.
    fn accept(&mut self, uri: &str, contents: &str) {
        let Some(model_slot) = self.model_slots.selected() else {
            return;
        };

        self.stage = Stage::Slots;
        self.problem = match is_install_model(contents) {
            Ok(true) if model_slot == SYSTEM_MODEL => {
                Some(format!("{uri} is an install-model; load it in the install-model slot"))
            }
            Ok(true) if model_slot == INSTALL_MODEL => {
                Some(format!("{uri} is a system-model; loat it in the system-model slot"))
            }
            Err(err) => Some(format!("cannot parse {uri}: {}", parse_error_detail(&err))),
            Ok(_) => None,
        };

        if self.problem.is_none() {
            self.sources[model_slot] = Some(uri.to_string());
            self.documents[model_slot] = Some(contents.to_string());
        }
    }
}

impl Screen for Welcome {
    fn title(&self) -> &str {
        "Welcome"
    }

    fn hints(&self) -> &[(&str, &str)] {
        &[("Enter", "begin")]
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

    fn on_enter(&mut self, ctx: &Context, _model: &Model) {
        self.ctx = Some(ctx.clone());
    }

    fn on_message(&mut self, msg: &Msg, _model: &mut Model) {
        match msg {
            // A failed fetch leaves the model slots as they were; the overlay says why
            Msg::Failed(_) if matches!(self.stage, Stage::Fetching) => self.stage = Stage::Slots,
            Msg::ModelFetched { uri, contents } => self.accept(uri, contents),
            _ => {}
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
