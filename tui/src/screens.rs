// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! The screen abstraction shared by every installation step.

pub mod storage;
pub mod welcome;

use crate::{
    events::{Action, Msg},
    theme::*,
};
use installer::Model;
use ratatui::{Frame, crossterm::event::KeyEvent, layout::Rect, widgets::Paragraph};
use std::future::Future;
use tokio::sync::mpsc::UnboundedSender;
use tonic::{Status, transport::Channel};

/// What a screen needs in order to start background work.
///
/// Cloned into spawned tasks: the channel builds RPC slients the same way
/// `installer::Installer` does, and the sender delivers the result back into
/// the applicaiton loop as a `Msg`.
#[derive(Clone)]
pub struct Context {
    pub channel: Channel,
    pub tx: UnboundedSender<Msg>,
}

impl Context {
    /// Run an RPC on a background task, delivering its result back into the
    /// application loop.
    pub fn spawn<F>(&self, task: F)
    where
        F: Future<Output = Result<Msg, Status>> + Send + 'static,
    {
        let tx = self.tx.clone();

        tokio::spawn(async move {
            let msg = match task.await {
                Ok(msg) => msg,
                Err(status) => Msg::Failed(status.message().to_string()),
            };

            let _ = tx.send(msg);
        });
    }
}

/// One installation step, prendered as a full screen.
///
/// Screens render FROM the model and write INTO it; they never keep a shadow
/// copy of a choice. That is what makes free backward navigation safe;
/// revisting a screen always shows what the model currently says.
pub trait Screen {
    /// Name shown in the sidebar
    fn title(&self) -> &str;
    /// Draw into the content pane
    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, model: &Model);
    /// Handle a key press. Return `Ignored` to let the application use it.
    fn handle_key(&mut self, key: KeyEvent, model: &mut Model) -> Action;
    /// Whether this step's choices are made; drives the sidebar tick
    fn is_complete(&self, _model: &Model) -> bool {
        false
    }
    /// Called each time this screen becomes active. Start RPCs here.
    fn on_enter(&mut self, _ctx: &Context, _model: &Model) {}
    /// Called for every message the application receives, including those
    /// belonging to other screens. Delivered to all screens, not just the active
    /// one, so a result cannot be lost by navigating away while an RPC is still
    /// in flight.
    fn on_message(&mut self, _msg: &Msg, _model: &mut Model) {}
    /// Key hints for the footer, as (key, meaning) pairs.
    fn hints(&self) -> &[(&str, &str)] {
        &[]
    }
}

/// Stand in for a step that has not been built yet, so the sidebar shows the
/// whole journey from the first slice onward.
pub struct Placeholder {
    title: &'static str,
}

impl Placeholder {
    pub fn new(title: &'static str) -> Self {
        Self { title }
    }
}

impl Screen for Placeholder {
    fn title(&self) -> &str {
        self.title
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, _model: &Model) {
        frame.render_widget(
            Paragraph::new(format!("{} - not built yet", self.title)).style(HINT),
            area,
        );
    }

    fn handle_key(&mut self, _key: KeyEvent, _model: &mut Model) -> Action {
        Action::Ignored
    }
}
