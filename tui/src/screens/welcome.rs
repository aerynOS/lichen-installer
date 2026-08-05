// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Opening screen: states the contract before anything else happens.

use super::Screen;
use crate::{
    events::{Action, Msg},
    install_model::{apply_install_model, apply_system_model, is_install_model, parse_error_detail},
    screens::Context,
    selections::mandatory,
    theme::*,
    widgets::{Browser, BrowserOutcome, Field, Form, FormOutcome},
};
use installer::Model;
use protocols::lichen::{
    install::{FetchModelRequest, install_client::InstallClient},
    network::network_client::NetworkClient,
    osinfo::OsInfo,
};
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph, Wrap},
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
    /// A URI accepted into a model slot, but no yet fetched because it needs
    /// a network connection that doesn't exist yet.
    pending: [Option<String>; 2],
    /// Last known connectivity
    online: bool,
    /// Whether the status call has already gone out
    asked: bool,
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
            pending: [None, None],
            online: false,
            asked: false,
            problem: None,
            ctx: None,
        }
    }

    /// Ask the backend for a document. Every scheme goes the same way, so the
    /// screen never has to know whether it was handed a path or URL.
    fn fetch(&mut self, uri: String) -> Action {
        if needs_network(&uri) && !self.online {
            let Some(model_slot) = self.model_slots.selected() else {
                return Action::Consumed;
            };

            self.pending[model_slot] = Some(uri);
            self.problem = Some("that needs a network connection; setting one up first".to_string());
            self.stage = Stage::Slots;
            return Action::Goto("Network");
        }

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

    /// Fire the first model slot that has been waiting on a connection.
    ///
    /// One at a time: `fetch` moves to `Stage::Fetching`, and a second call
    /// would overwrite the first's result. The other model slot goes when this
    /// one lands, from `accept`.
    fn fetch_pending(&mut self) {
        let Some(model_slot) = self.pending.iter().position(Option::is_some) else {
            return;
        };
        let Some(uri) = self.pending[model_slot].take() else {
            return;
        };

        self.model_slots.select(Some(model_slot));
        let _ = self.fetch(uri);
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
            Ok(false) if model_slot == INSTALL_MODEL => {
                Some(format!("{uri} is a system-model; load it in the system-model slot"))
            }
            Err(err) => Some(format!("cannot parse {uri}: {}", parse_error_detail(&err))),
            Ok(_) => None,
        };

        if self.problem.is_none() {
            self.sources[model_slot] = Some(uri.to_string());
            self.documents[model_slot] = Some(contents.to_string());
            self.fetch_pending();
        }
    }

    /// The welcome text: begin, or go an import something first.
    fn on_intro_key(&mut self, key: KeyEvent, model: &mut Model) -> Action {
        match key.code {
            KeyCode::Enter | KeyCode::Tab | KeyCode::PageDown => self.commit(model),
            KeyCode::Char('i' | 'I') => {
                self.stage = Stage::Slots;
                Action::Consumed
            }
            _ => Action::Ignored,
        }
    }

    /// The two model slots: pick one, then choose how to fill it.
    fn on_slots_key(&mut self, key: KeyEvent, model: &mut Model) -> Action {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.model_slots.select(Some(INSTALL_MODEL));
                Action::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.model_slots.select(Some(SYSTEM_MODEL));
                Action::Consumed
            }
            KeyCode::Enter => {
                self.stage = Stage::Browsing;
                Action::Consumed
            }
            KeyCode::Char('u' | 'U') => {
                self.uri.set_value(0, "");
                self.stage = Stage::Typing;
                Action::Consumed
            }
            KeyCode::Char('x' | 'X') => {
                self.clear();
                Action::Consumed
            }
            KeyCode::Esc => {
                self.stage = Stage::Intro;
                Action::Consumed
            }
            KeyCode::Tab | KeyCode::PageDown => self.commit(model),
            _ => Action::Ignored,
        }
    }

    /// Walking for a local document. Every key is consumed.
    fn on_browser_key(&mut self, key: KeyEvent) -> Action {
        if key.code == KeyCode::Esc {
            self.stage = Stage::Slots;
            return Action::Consumed;
        }

        match self.browser.handle_key(key) {
            BrowserOutcome::Picked(path) => self.fetch(path.display().to_string()),
            BrowserOutcome::Consumed | BrowserOutcome::Ignored => Action::Consumed,
        }
    }

    /// Typing a URI, for everything a local walk cannot reach.
    fn on_typing_key(&mut self, key: KeyEvent) -> Action {
        if key.code == KeyCode::Esc {
            self.stage = Stage::Slots;
            return Action::Consumed;
        }

        match self.uri.handle_key(key) {
            FormOutcome::Submit => {
                let uri = self.uri.value(0).trim().to_string();
                if uri.is_empty() {
                    self.problem = Some("no URI given".to_string());
                    return Action::Consumed;
                }
                self.fetch(uri)
            }
            FormOutcome::Edited => {
                self.problem = None;
                Action::Consumed
            }
            FormOutcome::Moved | FormOutcome::Ignored => Action::Consumed,
        }
    }

    /// Clear the highlighted model slot
    fn clear(&mut self) {
        let Some(model_slot) = self.model_slots.selected() else {
            return;
        };

        self.sources[model_slot] = None;
        self.documents[model_slot] = None;
        self.pending[model_slot] = None;
        self.problem = None;
    }

    /// Apply whatever was loaded, then begin.
    ///
    /// Order matters: the install-model first, then a system-model
    /// overwriting its package set. Applying here rather than at
    /// load time is what makes that order independent of which slot
    /// the user happened to fill first.
    fn commit(&mut self, model: &mut Model) -> Action {
        if self.documents.iter().all(Option::is_none) {
            return Action::Next;
        }

        if let Some(contents) = &self.documents[INSTALL_MODEL]
            && let Err(err) = apply_install_model(model, contents)
        {
            return Action::Failed(format!(
                "failed to apply the install-model: {}",
                parse_error_detail(&err)
            ));
        }

        if let Some(contents) = &self.documents[SYSTEM_MODEL]
            && let Err(err) = apply_system_model(model, contents)
        {
            return Action::Failed(format!(
                "failed to apply the system-model: {}",
                parse_error_detail(&err)
            ));
        }

        model.imported = true;

        // An imported model never installs less than a bootable system
        let mut packages: BTreeSet<String> = model.software.packages.iter().cloned().collect();

        match mandatory(&model.software.selection) {
            Ok(required) => packages.extend(required),
            Err(err) => return Action::Failed(err.to_string()),
        }
        model.software.packages = packages.into_iter().collect();

        // Applied once. Coming back to this screen and pressing Enter again must
        // not undo choices made in betwee; `sources` stays for display.
        self.documents = [None, None];
        Action::Next
    }

    fn render_intro(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let loaded = self.sources.iter().filter(|source| source.is_some()).count();
        let import = match loaded {
            0 => "to import an install-model or system-model first.",
            1 => "to review the model already loaded.",
            _ => "to review the models already loaded.",
        };
        let lines = vec![
            Line::styled(format!("Welcome to the {} installer", self.os_name), HEADING),
            Line::raw(""),
            Line::styled("This is alpha quality software. User at your own risk!", WARNING),
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
                Span::styled(" to begin, or ", BODY),
                Span::styled("i", STEP_ACTIVE),
                Span::styled(format!(" {import} "), BODY),
            ]),
        ];
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
    }

    /// The two model slots, each showing where its document came from.
    fn render_slots(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let [heading, body, note] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(1), Constraint::Length(2)]).areas(area);

        frame.render_widget(
            Paragraph::new(Line::styled("Import an existing model", HEADING)),
            heading,
        );

        let items: Vec<ListItem<'_>> = MODEL_SLOTS
            .iter()
            .enumerate()
            .map(|(index, (label, accepts))| {
                let (detail, style) = match (&self.sources[index], &self.pending[index]) {
                    (Some(source), _) => (source.clone(), SUCCESS),
                    (None, Some(uri)) => (format!("waiting for a network connection - {uri}"), WARNING),
                    (None, None) => (format!("not loaded - {accepts}"), HINT),
                };
                ListItem::new(vec![
                    Line::styled((*label).to_string(), BODY),
                    Line::styled(format!("  {detail}"), style),
                ])
            })
            .collect();

        frame.render_stateful_widget(
            List::new(items).highlight_style(SELECTED).highlight_symbol(CURSOR),
            body,
            &mut self.model_slots,
        );

        let status = if let Some(problem) = &self.problem {
            Line::styled(problem.clone(), ERROR)
        } else if matches!(self.stage, Stage::Fetching) {
            Line::styled("Fetching...", HINT) // needs heartbeat animation
        } else {
            Line::raw("")
        };

        frame.render_widget(Paragraph::new(status).wrap(Wrap { trim: false }), note);
    }

    fn render_typing(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let [heading, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
        let model_slot = self.model_slots.selected().unwrap_or(INSTALL_MODEL);

        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(format!("Where is the {}?", MODEL_SLOTS[model_slot].0), HEADING),
                Line::styled("A path, or file://, https://, smb://, or nfs://", HINT),
            ]),
            heading,
        );
        self.uri.render(frame, body);
    }
}

impl Screen for Welcome {
    fn title(&self) -> &str {
        "Welcome"
    }

    fn hints(&self) -> &[(&str, &str)] {
        match self.stage {
            Stage::Intro => &[("Enter", "begin"), ("i", "import a model")],
            Stage::Slots => &[
                ("↑↓", "slot"),
                ("Enter", "browse"),
                ("u", "type a URI"),
                ("x", "clear"),
                ("Tab", "done"),
                ("Esc", "back"),
            ],
            Stage::Browsing => &[("↑↓", "choose"), ("Enter", "open"), ("Left", "up"), ("Esc", "back")],
            Stage::Typing => &[("Enter", "fetch"), ("Esc", "back")],
            Stage::Fetching => &[],
        }
    }

    fn is_complete(&self, _model: &Model) -> bool {
        true
    }

    fn handle_key(&mut self, key: KeyEvent, model: &mut Model) -> Action {
        match self.stage {
            Stage::Intro => self.on_intro_key(key, model),
            Stage::Slots => self.on_slots_key(key, model),
            Stage::Browsing => self.on_browser_key(key),
            Stage::Typing => self.on_typing_key(key),
            // Nothing to do but wait for the backend
            Stage::Fetching => Action::Consumed,
        }
    }

    fn on_enter(&mut self, ctx: &Context, _model: &Model) {
        self.ctx = Some(ctx.clone());

        // Asked here rather than left to the Network screen two steps ahead: an
        // https:// import needs to know whether it can go now. The answer is
        // broadcast, so it also lets the Network step be marked completed
        // without being visited.
        if self.asked {
            return;
        }

        let channel = ctx.channel.clone();

        ctx.spawn(async move {
            let status = NetworkClient::new(channel).status(()).await?.into_inner();
            Ok(Msg::NetworkState(status))
        });
    }

    fn on_message(&mut self, msg: &Msg, _model: &mut Model) {
        match msg {
            // A failed fetch leaves the model slots as they were; the overlay says why
            Msg::Failed(_) if matches!(self.stage, Stage::Fetching) => self.stage = Stage::Slots,
            Msg::ModelFetched { uri, contents } => self.accept(uri, contents),
            Msg::NetworkState(status) => {
                self.online = status.online;
                if self.online {
                    self.fetch_pending();
                }
            }
            _ => {}
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, _model: &Model) {
        match self.stage {
            Stage::Intro => self.render_intro(frame, area),
            Stage::Browsing => self.browser.render(frame, area),
            Stage::Typing => self.render_typing(frame, area),
            Stage::Slots | Stage::Fetching => self.render_slots(frame, area),
        }
    }
}

// Helpers

/// Whether a URI is remote or not.
fn needs_network(uri: &str) -> bool {
    !matches!(uri.split_once("://"), None | Some(("file", _)))
}
