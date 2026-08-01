// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! The application shell: step list, model, event loop and chrome.

use crate::{
    events::{self, Action, Msg},
    screens::{
        Context, Placeholder, Screen, locale::Locale, storage::Storage, strategy::Strategy, timezone::Timezone,
        welcome::Welcome,
    },
    theme::*,
};
use color_eyre::Result;
use installer::Model;
use protocols::lichen::osinfo::OsInfo;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tonic::transport::Channel;

/// Below this the layout cannot be drawn honestly, so it isn't drawn at all.
const MIN_WIDTH: u16 = 80;
const MIN_HEIGHT: u16 = 24;
/// Width of the step rail, including it border column
const SIDEBAR_WIDTH: u16 = 16;

/// Where the installer is in its lifecycle.
///
/// Navigation is free while `Choosing`. Confirming on the Summary screen moves
/// to `Committed` and locks it: pas that point the disk has been written to,
/// and offering to go back would be a lie about what is on it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Choosing,
    #[allow(dead_code)]
    Committed,
}

/// A modal over the content pane. While one is up it takes every key.
enum Overlay {
    None,
    Quit,
    Error(String),
}

pub struct App {
    ctx: Context,
    os_name: String,
    model: Model,
    screens: Vec<Box<dyn Screen>>,
    current: usize,
    phase: Phase,
    overlay: Overlay,
    rx: UnboundedReceiver<Msg>,
    quit: bool,
}

impl App {
    pub fn new(channel: Channel, info: &OsInfo) -> Self {
        let (tx, rx) = unbounded_channel();
        events::spawn_input(tx.clone());

        let os_name = info
            .metadata
            .as_ref()
            .and_then(|meta| meta.identity.as_ref())
            .map(|identity| identity.display.clone())
            .unwrap_or_else(|| "Unknown OS".into());
        let screens: Vec<Box<dyn Screen>> = vec![
            Box::new(Welcome::new(info)),
            Box::new(Placeholder::new("Network")),
            Box::new(Storage::new()),
            Box::new(Strategy::new()),
            Box::new(Locale::new()),
            Box::new(Timezone::new()),
            Box::new(Placeholder::new("Desktop")),
            Box::new(Placeholder::new("Accounts")),
            Box::new(Placeholder::new("Summary")),
        ];

        Self {
            ctx: Context { channel, tx },
            os_name,
            model: Model::default(),
            screens,
            current: 0,
            phase: Phase::Choosing,
            overlay: Overlay::None,
            rx,
            quit: false,
        }
    }

    /// Draw then wait. Every wake-up, a key, a RPC result, a failure,
    /// arrives on the one channel, so the UI is never stale and
    /// never spins.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
        self.screens[self.current].on_enter(&self.ctx, &self.model);

        while !self.quit {
            terminal.draw(|frame| self.render(frame))?;

            let Some(msg) = self.rx.recv().await else {
                break;
            };

            self.handle(msg);
        }

        Ok(())
    }

    fn handle(&mut self, msg: Msg) {
        if let Msg::Terminal(event) = &msg {
            if let Event::Key(key) = event
                && key.kind == KeyEventKind::Press
            {
                let key = *key;
                self.on_key(key);
            }
            return;
        }

        if let Msg::Failed(reason) = &msg {
            self.overlay = Overlay::Error(reason.clone());
        }

        // Offered to every screen, not just the active one: navigating away
        // while RPC is in flight must not lose its answer.
        self.screens.iter_mut().for_each(|screen| {
            screen.on_message(&msg, &mut self.model);
        });
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Ctrl+C is the one key nothing is allowed to swallow. It is the quit
        // key rather than `q` because text fields make `q` unusable.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            match self.overlay {
                Overlay::Quit => self.quit = true,
                _ => self.overlay = Overlay::Quit,
            }
            return;
        }

        if !matches!(self.overlay, Overlay::None) {
            self.on_overlay_key(key);
            return;
        }

        match self.screens[self.current].handle_key(key, &mut self.model) {
            Action::Consumed => {}
            Action::Next => self.next(),
            Action::Back => self.back(),
            Action::Ignored => self.on_global_key(key),
        }
    }

    fn on_overlay_key(&mut self, key: KeyEvent) {
        match (&self.overlay, key.code) {
            (Overlay::Quit, KeyCode::Char('y' | 'Y')) => self.quit = true,
            (Overlay::Quit, KeyCode::Esc | KeyCode::Char('n' | 'N')) => self.overlay = Overlay::None,
            (Overlay::Error(_), KeyCode::Esc | KeyCode::Enter) => self.overlay = Overlay::None,
            _ => {}
        }
    }

    /// Keys the active screen did not want
    fn on_global_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Tab | KeyCode::PageDown => self.next(),
            KeyCode::BackTab | KeyCode::PageUp => self.back(),
            _ => {}
        }
    }

    fn next(&mut self) {
        if self.phase == Phase::Choosing && self.current + 1 < self.screens.len() {
            self.current += 1;
            self.screens[self.current].on_enter(&self.ctx, &self.model);
        }
    }

    fn back(&mut self) {
        if self.phase == Phase::Choosing && self.current > 0 {
            self.current -= 1;
            self.screens[self.current].on_enter(&self.ctx, &self.model);
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area();

        if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
            self.render_too_small(frame, area);
            return;
        }

        let [header, body, footer] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(area);
        let [sidebar, content] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(1)]).areas(body);

        self.render_header(frame, header);
        self.render_sidebar(frame, sidebar);
        self.render_content(frame, content);
        self.render_footer(frame, footer);
        self.render_overlay(frame, area);
    }

    fn render_too_small(&self, frame: &mut Frame<'_>, area: Rect) {
        let message = format!(
            "The installer needs a terminal of at least {MIN_WIDTH}x{MIN_HEIGHT}.\n\
             This one is {}x{}. Resize to continue.",
            area.width, area.height,
        );

        frame.render_widget(Paragraph::new(message).style(WARNING).wrap(Wrap { trim: false }), area);
    }

    fn render_header(&self, frame: &mut Frame<'_>, area: Rect) {
        let line = Line::from(vec![
            Span::styled(format!(" Install {} ", self.os_name), TITLE),
            Span::styled(
                format!(
                    "· {} ({}/{})",
                    self.screens[self.current].title(),
                    self.current + 1,
                    self.screens.len(),
                ),
                HINT,
            ),
        ]);

        frame.render_widget(Paragraph::new(line), area);
    }

    fn render_sidebar(&self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::default().borders(Borders::RIGHT).border_style(FRAME);
        let inner = block.inner(area);

        frame.render_widget(block, area);

        let lines: Vec<Line<'_>> = self
            .screens
            .iter()
            .enumerate()
            .map(|(index, screen)| {
                let (marker, style) = if index == self.current {
                    ("·", STEP_ACTIVE)
                } else if screen.is_complete(&self.model) {
                    ("✔", STEP_COMPLETE)
                } else {
                    (" ", STEP_PENDING)
                };

                Line::styled(format!(" {marker} {}", screen.title()), style)
            })
            .collect();

        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_content(&mut self, frame: &mut Frame<'_>, area: Rect) {
        // Breathing room on the lef,t none stolen from the right edge
        let padded = Rect {
            x: area.x + 2,
            y: area.y + 1,
            width: area.width.saturating_sub(3),
            height: area.width.saturating_sub(1),
        };

        self.screens[self.current].render(frame, padded, &self.model);
    }

    fn render_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        let line = match &self.overlay {
            Overlay::Quit => Line::styled(" y quit · Esc to continue ", HINT),
            Overlay::Error(_) => Line::styled("Esc dismiss ", HINT),
            Overlay::None => {
                let mut spans = vec![Span::raw(" ")];

                for (key, meaning) in self.screens[self.current].hints() {
                    spans.push(Span::styled(*key, STEP_ACTIVE));
                    spans.push(Span::styled(format!(" {meaning} · "), HINT));
                }

                spans.push(Span::styled("Tab/⇧Tab", STEP_ACTIVE));
                spans.push(Span::styled(" step · ", HINT));
                spans.push(Span::styled("Ctrl+C", STEP_ACTIVE));
                spans.push(Span::styled(" quit ", HINT));
                Line::from(spans)
            }
        };

        frame.render_widget(Paragraph::new(line), area);
    }

    fn render_overlay(&self, frame: &mut Frame<'_>, area: Rect) {
        let (title, body, style) = match &self.overlay {
            Overlay::None => return,
            Overlay::Quit => (
                " Quit the installer? ",
                "Nothing has been written to disk.\n\nPress y to quit, Esc to continue".to_string(),
                WARNING,
            ),
            Overlay::Error(reason) => (" Something went wrong ", reason.clone(), ERROR),
        };
        let popup = centered(area, 60, 30);

        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(body).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(style)
                    .title(Line::styled(title, style)),
            ),
            popup,
        );
    }
}

/// A rectangle centered in `area`, sized as a percentage of it
fn centered(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let [_, middle, _] = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .areas(area);

    let [_, center, _] = Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .areas(middle);

    center
}
