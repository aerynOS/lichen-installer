// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! The application shell: step list, model, event loop and chrome.

use crate::{
    events::{self, Action, Msg},
    keyboard::Keyboard,
    screens::{
        Context, Screen, accounts::Accounts, desktop::Desktop, install::Install, locale::Locale, network::Network,
        storage::Storage, strategy::Strategy, summary::Summary, timezone::Timezone, welcome::Welcome,
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
use std::time::Duration;
use tokio::{
    sync::mpsc::{UnboundedReceiver, unbounded_channel},
    time::interval,
};
use tonic::transport::Channel;

/// Below this the layout cannot be drawn honestly, so it isn't drawn at all.
const MIN_WIDTH: u16 = 80;
const MIN_HEIGHT: u16 = 24;
/// Width of the step rail, including it border column
const SIDEBAR_WIDTH: u16 = 16;
const HEARTBEAT_PERIOD: Duration = Duration::from_millis(150);

/// Where the installer is in its lifecycle.
///
/// Navigation is free while `Choosing`. Confirming on the Summary screen moves
/// to `Committed` and locks it: pas that point the disk has been written to,
/// and offering to go back would be a lie about what is on it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Choosing,
    Committed,
}

/// A modal over the content pane. While one is up it takes every key.
enum Overlay {
    None,
    Quit,
    Help,
    Keyboard,
    Error(String),
}

/// Keys that work on every screen, for the help overlay.
///
/// The per-screen half of that overlay is read from `Screen::hints`, which is
/// also what the footer renders, so the two can never disagree.
const GLOBAL_KEYS: &[(&str, &str)] = &[
    ("Tab", "move to the next field or button"),
    ("Shift+Tab", "move back"),
    ("Enter", "press the focused button"),
    ("F1 / ?", "show the help"),
    ("F2", "keyboard layout"),
    ("Esc", "close an overlay"),
    ("Ctrl+P", "refresh screen"),
    ("Ctrl+C", "quit the installer"),
];

/// What Tab is currently moving through
///
/// `Screen` delegates to `Screen::focus`, which walks the screen's own stops and
/// then declines; the two buttons take it form there.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Screen,
    Previous,
    Next,
}

pub struct App {
    ctx: Context,
    os_name: String,
    model: Model,
    screens: Vec<Box<dyn Screen>>,
    current: usize,
    goto: Option<usize>,
    phase: Phase,
    focus: Focus,
    overlay: Overlay,
    keyboard: Keyboard,
    rx: UnboundedReceiver<Msg>,
    redraw: bool,
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
            Box::new(Network::new()),
            Box::new(Storage::new()),
            Box::new(Strategy::new()),
            Box::new(Locale::new()),
            Box::new(Timezone::new()),
            Box::new(Desktop::new()),
            Box::new(Accounts::new()),
            Box::new(Summary::new()),
            Box::new(Install::new()),
        ];

        Self {
            ctx: Context { channel, tx },
            os_name,
            model: Model::default(),
            screens,
            current: 0,
            goto: None,
            phase: Phase::Choosing,
            focus: Focus::Screen,
            overlay: Overlay::None,
            keyboard: Keyboard::new(),
            rx,
            redraw: false,
            quit: false,
        }
    }

    /// Draw then wait. Every wake-up, a key, a RPC result, a failure,
    /// arrives on the one channel, so the UI is never stale and
    /// never spins.
    pub async fn run(mut self, mut terminal: DefaultTerminal) -> Result<()> {
        self.screens[self.current].on_enter(&self.ctx, &self.model);
        self.keyboard.start(&self.ctx);

        let mut heartbeat = interval(HEARTBEAT_PERIOD);

        while !self.quit {
            // Anything that writes to the terminal while the TUI is running
            // leaves cells it will never repaint on its own, because ratatui
            // diffs against what it drew rather than against the screen.
            if self.redraw {
                terminal.clear()?;
                self.redraw = false;
            }

            // Check to see if an imported install-model.kdl and sync the keyboard layout
            self.keyboard.sync(&self.model);

            terminal.draw(|frame| self.render(frame))?;

            tokio::select! {
                msg = self.rx.recv() => match msg {
                    Some(msg) => self.handle(msg),
                    None => break,
                },
                _ = heartbeat.tick() => self.handle(Msg::Tick),
            }
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
        self.keyboard.on_message(&msg, &self.model);
        self.return_from_goto();
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

        // The manual way out of a foreign output painted over the interface
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p') {
            self.redraw = true;
            return;
        }

        // Toggle the help overlay
        match (key.code, &self.overlay) {
            (KeyCode::F(1), Overlay::None) => {
                self.overlay = Overlay::Help;
                return;
            }
            (KeyCode::F(1), Overlay::Help) => {
                self.overlay = Overlay::None;
                return;
            }
            (KeyCode::F(2), Overlay::None) => {
                self.overlay = Overlay::Keyboard;
                return;
            }
            (KeyCode::F(2), Overlay::Keyboard) => {
                self.overlay = Overlay::None;
                return;
            }
            _ => {}
        }

        if !matches!(self.overlay, Overlay::None) {
            self.on_overlay_key(key);
            return;
        }

        // Tab belongs to the application, ahead of the screen.
        match key.code {
            KeyCode::Tab => {
                self.move_focus(true);
                return;
            }
            KeyCode::BackTab => {
                self.move_focus(false);
                return;
            }
            _ => {}
        }

        if self.focus != Focus::Screen {
            self.on_button_key(key);
            return;
        }

        match self.screens[self.current].handle_key(key, &mut self.model) {
            Action::Consumed => {}
            Action::Ready => self.next(),
            Action::Goto(title) => self.goto(title),
            Action::Ignored => self.on_global_key(key),
            Action::Commit => self.commit(),
            Action::Failed(err) => self.overlay = Overlay::Error(err),
        }
    }

    fn on_overlay_key(&mut self, key: KeyEvent) {
        // The keyboard picker takes everything except Esc, because its
        // filter box needs the letters. That's why it can't join the
        // match below.
        if matches!(self.overlay, Overlay::Keyboard) {
            if key.code == KeyCode::Esc {
                self.overlay = Overlay::None;
                return;
            }
            if self.keyboard.handle_key(key, &mut self.model) {
                self.overlay = Overlay::None;
            }
            return;
        }
        match (&self.overlay, key.code) {
            (Overlay::Quit, KeyCode::Char('y' | 'Y')) => self.quit = true,
            (Overlay::Quit, KeyCode::Esc | KeyCode::Char('n' | 'N')) => self.overlay = Overlay::None,
            (Overlay::Help, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?')) => self.overlay = Overlay::None,
            (Overlay::Error(_), KeyCode::Esc | KeyCode::Enter) => self.overlay = Overlay::None,
            _ => {}
        }
    }

    /// Walk Screen -> Prev -> Next and back around.
    ///
    /// The screen gets first refusal, so a form walks its own fields before
    /// the buttons see anything. Scren 0 has no Previous to stop on.
    fn move_focus(&mut self, forward: bool) {
        if self.phase == Phase::Committed {
            return;
        }

        let has_previous = self.current > 0;

        self.focus = match (self.focus, forward) {
            (Focus::Screen, true) => match self.screens[self.current].focus(true) {
                true => Focus::Screen,
                false if has_previous => Focus::Previous,
                false => Focus::Next,
            },
            (Focus::Previous, true) => Focus::Next,
            (Focus::Next, true) => Focus::Screen,
            (Focus::Screen, false) => match self.screens[self.current].focus(false) {
                true => Focus::Screen,
                false => Focus::Next,
            },
            (Focus::Previous, false) => Focus::Screen,
            (Focus::Next, false) if has_previous => Focus::Previous,
            (Focus::Next, false) => Focus::Screen,
        }
    }

    /// Keys while a button has focus. Nothing reaches the screen.
    fn on_button_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left | KeyCode::Up if self.current > 0 => self.focus = Focus::Previous,
            KeyCode::Right | KeyCode::Down => self.focus = Focus::Next,
            KeyCode::Esc => self.focus = Focus::Screen,
            KeyCode::Enter | KeyCode::Char(' ') => self.press(),
            KeyCode::Char('?') => self.overlay = Overlay::Help,
            _ => {}
        }
    }

    /// Activate the focused button. The screen gets to refuse Next, enforcing any gates.
    fn press(&mut self) {
        if self.focus == Focus::Previous {
            self.back();
            return;
        }

        match self.screens[self.current].proceed(&mut self.model) {
            Action::Ignored | Action::Ready => self.next(),
            // The screen took it and is showing something: a confirm to answer,
            // or the field it just rejected. It needs the keyboard back.
            Action::Consumed => self.focus = Focus::Screen,
            Action::Commit => self.commit(),
            Action::Goto(title) => self.goto(title),
            Action::Failed(err) => self.overlay = Overlay::Error(err),
        }
    }

    /// Keys the active screen did not want
    fn on_global_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('?') {
            self.overlay = Overlay::Help;
        }
    }

    /// Step forward, but nver onto the last screen. That one writes to disk,
    /// and is reached only by confirming the summary.
    fn next(&mut self) {
        let last = self.screens.len() - 1;

        if self.phase == Phase::Choosing && self.current + 1 < last {
            self.goto = None;
            self.focus = Focus::Screen;
            self.current += 1;
            self.screens[self.current].on_enter(&self.ctx, &self.model);
        }
    }

    fn back(&mut self) {
        if self.phase == Phase::Choosing && self.current > 0 {
            self.goto = None;
            self.focus = Focus::Screen;
            self.current -= 1;
            self.screens[self.current].on_enter(&self.ctx, &self.model);
        }
    }

    /// Go to a specific screen, identified by title
    fn goto(&mut self, title: &str) {
        let Some(target) = self.screens.iter().position(|screen| screen.title() == title) else {
            return;
        };
        if target == self.current {
            return;
        }

        self.goto = Some(self.current);
        self.focus = Focus::Screen;
        self.current = target;
        self.screens[self.current].on_enter(&self.ctx, &self.model);
    }

    fn return_from_goto(&mut self) {
        let Some(origin) = self.goto else {
            return;
        };
        if !self.screens[self.current].is_complete(&self.model) {
            return;
        }

        self.goto = None;
        self.focus = Focus::Screen;
        self.current = origin;
        self.screens[self.current].on_enter(&self.ctx, &self.model);
    }

    fn commit(&mut self) {
        self.phase = Phase::Committed;
        self.goto = None;
        self.focus = Focus::Screen;
        self.current = self.screens.len() - 1;
        self.screens[self.current].on_enter(&self.ctx, &self.model);
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
        let layout = match self.model.region.layout.is_empty() {
            true => "us",
            false => &self.model.region.layout,
        };
        let style = if matches!(self.overlay, Overlay::Keyboard) {
            STEP_ACTIVE
        } else {
            HINT
        };
        let selector = Line::from(vec![
            Span::styled("F2", STEP_ACTIVE),
            Span::styled(format!(" [{layout}] "), style),
        ]);
        let [left, right] =
            Layout::horizontal([Constraint::Min(1), Constraint::Length(selector.width() as u16)]).areas(area);

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("Install {} ", self.os_name), TITLE),
                Span::styled(
                    format!(
                        "· {} ({}/{})",
                        self.screens[self.current].title(),
                        self.current + 1,
                        self.screens.len(),
                    ),
                    HINT,
                ),
            ])),
            left,
        );
        frame.render_widget(Paragraph::new(selector).right_aligned(), right);
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
                    (ACTIVE, STEP_ACTIVE)
                } else if screen.is_complete(&self.model) {
                    (COMPLETE, STEP_COMPLETE)
                } else {
                    (" ", STEP_PENDING)
                };

                Line::styled(format!(" {marker} {}", screen.title()), style)
            })
            .collect();

        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_content(&mut self, frame: &mut Frame<'_>, area: Rect) {
        // Breathing room on the left none stolen from the right edge
        let padded = Rect {
            x: area.x + 2,
            y: area.y + 1,
            width: area.width.saturating_sub(3),
            height: area.height.saturating_sub(1),
        };

        // Past the commit there is nothing to navigate, so the install
        // screen keeps the whole pane.
        if self.phase == Phase::Committed {
            self.screens[self.current].render(frame, padded, &self.model);
            return;
        }

        let [body, footing] = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(padded);

        self.screens[self.current].render(frame, body, &self.model);
        self.render_buttons(frame, footing);
    }

    /// Previous and Next, right-aligned under the content.
    fn render_buttons(&self, frame: &mut Frame<'_>, area: Rect) {
        // A blank row above, so the buttons are not hard against the content
        let row = Rect {
            y: area.y + 1,
            height: 1,
            ..area
        };
        let mut spans = Vec::new();

        if self.current > 0 {
            spans.push(button("< Previous", self.focus == Focus::Previous));
            spans.push(Span::raw("  "));
        }

        spans.push(button(
            &format!("{} >", self.screens[self.current].next_label()),
            self.focus == Focus::Next,
        ));

        frame.render_widget(Paragraph::new(Line::from(spans)).right_aligned(), row);
    }

    fn render_footer(&self, frame: &mut Frame<'_>, area: Rect) {
        let line = match &self.overlay {
            Overlay::Quit => Line::styled(" y quit · Esc to continue ", HINT),
            Overlay::Help => Line::styled(" Esc close ", HINT),
            Overlay::Keyboard => {
                let mut spans = vec![Span::raw(" ")];
                for (key, meaning) in self.keyboard.hints() {
                    spans.push(Span::styled(*key, STEP_ACTIVE));
                    spans.push(Span::styled(format!(" {meaning} · "), HINT));
                }
                Line::from(spans)
            }
            Overlay::Error(_) => Line::styled(" Esc dismiss ", HINT),
            Overlay::None => {
                let mut spans = vec![Span::raw(" ")];

                for (key, meaning) in self.screens[self.current].hints() {
                    spans.push(Span::styled(*key, STEP_ACTIVE));
                    spans.push(Span::styled(format!(" {meaning} · "), HINT));
                }

                // Past the commit the step keys do nothing. Advertising them
                // invites the user to press them and not trust the installer.
                if self.phase == Phase::Choosing {
                    spans.push(Span::styled("Tab", STEP_ACTIVE));
                    spans.push(Span::styled(" move · ", HINT));
                    spans.push(Span::styled("Enter", STEP_ACTIVE));
                    spans.push(Span::styled(" press · ", HINT));
                }
                spans.push(Span::styled("F1", STEP_ACTIVE));
                spans.push(Span::styled(" keys · ", HINT));
                spans.push(Span::styled("Ctrl+C", STEP_ACTIVE));
                spans.push(Span::styled(" quit ", HINT));
                Line::from(spans)
            }
        };

        frame.render_widget(Paragraph::new(line), area);
    }

    fn render_keyboard(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered(area, 60, 70);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(FRAME)
            .title(Line::styled(" Keyboard layout ", TITLE));
        let inner = block.inner(popup);

        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);
        self.keyboard.render(frame, inner);
    }

    fn render_help(&self, frame: &mut Frame<'_>, area: Rect) {
        let screen = &self.screens[self.current];
        let mut lines = vec![Line::styled("Anywhere", HEADING)];

        lines.extend(GLOBAL_KEYS.iter().map(|(key, meaning)| help_row(key, meaning)));

        if !screen.hints().is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::styled(screen.title(), HEADING));
            lines.extend(screen.hints().iter().map(|(key, meaning)| help_row(key, meaning)));
        }

        let popup = centered(area, 50, 70);

        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(FRAME)
                    .title(Line::styled(" Keys ", TITLE)),
            ),
            popup,
        );
    }

    fn render_overlay(&mut self, frame: &mut Frame<'_>, area: Rect) {
        match self.overlay {
            Overlay::Help => {
                self.render_help(frame, area);
                return;
            }
            Overlay::Keyboard => {
                self.render_keyboard(frame, area);
                return;
            }
            _ => {}
        }

        let (title, body, style) = match &self.overlay {
            Overlay::None | Overlay::Help | Overlay::Keyboard => return,
            Overlay::Quit => (
                " Quit the installer? ",
                match self.phase {
                    Phase::Choosing => "Nothing has been written to disk.\n\nPress y to quit, Esc to continue",
                    Phase::Committed
                        if self.screens[self.current].title() == "Install"
                            && self.screens[self.current].is_complete(&self.model) =>
                    {
                        self.quit = true;
                        return;
                    }
                    Phase::Committed => {
                        "The disk has already been written to. Quitting now leaves an unfinished \
                        installation behind.\n\nPress y to quit, Esc to continue"
                    }
                }
                .to_string(),
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

/// One key and what it does, in two columns
fn help_row(key: &str, meaning: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {key:<20}"), STEP_ACTIVE),
        Span::styled(meaning.to_string(), BODY),
    ])
}

/// One button, padded so the focus highlight has some body to it
fn button(label: &str, focused: bool) -> Span<'static> {
    let style = if focused { SELECTED } else { BUTTON };

    Span::styled(format!("  {label}  "), style)
}
