// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Getting this system, and the installed one, onto the network.
//!
//! Ethernet needs nothing from the user, so when a cable is already up the
//! screen says so and gets out of the way. Wireless is connected here on
//! the live system; the profile NetworkManager writes is named in
//! `model.network.profile` and copied onto the target during install.

use super::{Context, Screen};
use crate::{
    events::{Action, Msg},
    theme::*,
    widgets::{Field, Form, FormOutcome},
};
use installer::Model;
use protocols::lichen::network::{AccessPoint, ConnectWifiRequest, NetworkStatus, network_client::NetworkClient};
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph, Wrap},
};

/// Fields of the hidden-network form
const SSID: usize = 0;
const KEY: usize = 1;
/// WPA's own floor. A 64 character hex PMK clears it too.
const MIN_KEY: usize = 8;

/// What the body of the screen is showing
#[derive(Clone, Copy)]
enum Stage {
    /// Waiting on the first status call
    Loading,
    Networks,
    Password,
    Hidden,
    Connecting,
}

pub struct Network {
    stage: Stage,
    /// Where to return if a connection fails, so a mistyped password can be fixed
    /// rather than retyped
    previous: Stage,
    status: Option<NetworkStatus>,
    points: Vec<AccessPoint>,
    list: ListState,
    psk: Form,
    hidden: Form,
    problem: Option<String>,
    scanning: bool,
    ctx: Option<Context>,
}

impl Network {
    pub fn new() -> Self {
        Self {
            stage: Stage::Loading,
            previous: Stage::Networks,
            status: None,
            points: Vec::new(),
            list: ListState::default(),
            psk: Form::new(vec![Field::new("Password", true)]),
            hidden: Form::new(vec![Field::new("Network name", false), Field::new("Password", true)]),
            problem: None,
            scanning: false,
            ctx: None,
        }
    }

    fn online(&self) -> bool {
        self.status.as_ref().is_some_and(|status| status.online)
    }

    fn wifi_available(&self) -> bool {
        self.status.as_ref().is_some_and(|status| status.wifi_available)
    }

    /// The radio device to connect with
    ///
    /// Named explicitly rather than left to NetworkManager, so a machine with
    /// two wifi cards always uses the one the status line reports.
    fn wifi_device(&self) -> Option<String> {
        self.status
            .as_ref()?
            .devices
            .iter()
            .find(|device| device.kind == "wifi")
            .map(|device| device.name.clone())
    }

    fn selected(&self) -> Option<&AccessPoint> {
        self.list.selected().and_then(|index| self.points.get(index))
    }

    fn move_selection(&mut self, delta: isize) {
        if self.points.is_empty() {
            return;
        }

        let current = self.list.selected().unwrap_or(0) as isize;
        let next = current.saturating_add(delta).clamp(0, self.points.len() as isize - 1);

        self.list.select(Some(next as usize))
    }

    /// Ask the backend for the network status
    fn refresh(&self) {
        let Some(ctx) = self.ctx.clone() else {
            return;
        };
        let channel = ctx.channel.clone();

        ctx.spawn(async move {
            let status = NetworkClient::new(channel).status(()).await?.into_inner();

            Ok(Msg::NetworkState(status))
        });
    }

    /// Rescan for access points
    fn scan(&mut self) {
        let Some(ctx) = self.ctx.clone() else {
            return;
        };
        let channel = ctx.channel.clone();

        self.scanning = true;

        ctx.spawn(async move {
            let points = NetworkClient::new(channel)
                .scan_wifi(())
                .await?
                .into_inner()
                .access_points;

            Ok(Msg::AccessPoints(points))
        });
    }

    fn connect(&mut self, ssid: String, psk: Option<String>, hidden: bool) -> Action {
        let Some(ctx) = self.ctx.clone() else {
            return Action::Ignored;
        };
        let channel = ctx.channel.clone();
        let request = ConnectWifiRequest {
            ssid,
            psk,
            hidden,
            device: self.wifi_device(),
        };

        self.problem = None;
        self.previous = self.stage;
        self.stage = Stage::Connecting;

        ctx.spawn(async move {
            let profile = NetworkClient::new(channel)
                .connect_wifi(request)
                .await?
                .into_inner()
                .profile;

            Ok(Msg::WifiConnected(profile))
        });

        Action::Consumed
    }

    fn on_list_key(&mut self, key: KeyEvent) -> Action {
        if !self.wifi_available() {
            return Action::Ignored;
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                Action::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                Action::Consumed
            }
            KeyCode::Home => {
                self.move_selection(isize::MIN);
                Action::Consumed
            }
            KeyCode::End => {
                self.move_selection(isize::MAX);
                Action::Consumed
            }
            KeyCode::Char('r') => {
                self.problem = None;
                self.scan();
                Action::Consumed
            }
            KeyCode::Char('h') => {
                self.problem = None;
                self.hidden.focus_on(SSID);
                self.stage = Stage::Hidden;
                Action::Consumed
            }
            KeyCode::Enter => self.choose(),
            _ => Action::Ignored,
        }
    }

    /// An open network needs nothing typed; a secured one asks for a psk
    fn choose(&mut self) -> Action {
        let Some(point) = self.selected() else {
            return Action::Consumed;
        };
        let ssid = point.ssid.clone();
        let open = point.security.is_empty();

        if open {
            return self.connect(ssid, None, false);
        }

        self.problem = None;
        self.psk.clear(0);
        self.psk.focus_on(0);
        self.stage = Stage::Password;

        Action::Consumed
    }

    fn on_psk_key(&mut self, key: KeyEvent) -> Action {
        if key.code == KeyCode::Esc {
            self.stage = Stage::Networks;
            return Action::Consumed;
        }

        match self.psk.handle_key(key) {
            FormOutcome::Submit => {
                let secret = self.psk.value(0).to_string();

                if secret.chars().count() < MIN_KEY {
                    self.problem = Some(format!("a WPA key is at least {MIN_KEY} characters"));
                    return Action::Consumed;
                }

                let Some(ssid) = self.selected().map(|point| point.ssid.clone()) else {
                    return Action::Consumed;
                };

                self.connect(ssid, Some(secret), false)
            }
            FormOutcome::Edited => {
                self.problem = None;
                Action::Consumed
            }
            FormOutcome::Moved => Action::Consumed,
            FormOutcome::Ignored => Action::Ignored,
        }
    }

    fn on_hidden_key(&mut self, key: KeyEvent) -> Action {
        if key.code == KeyCode::Esc {
            self.stage = Stage::Networks;
            return Action::Consumed;
        }

        match self.hidden.handle_key(key) {
            FormOutcome::Submit => {
                let ssid = self.hidden.value(SSID).trim().to_string();
                let secret = self.hidden.value(KEY).to_string();

                if ssid.is_empty() {
                    self.problem = Some("a network name is required".into());
                    self.hidden.focus_on(SSID);
                    return Action::Consumed;
                }

                // Blank means the hidden network is open, which is rare but legal
                if !secret.is_empty() && secret.chars().count() < MIN_KEY {
                    self.problem = Some(format!("a WPA key is at least {MIN_KEY} characters"));
                    self.hidden.focus_on(KEY);
                    return Action::Consumed;
                }

                self.connect(ssid, (!secret.is_empty()).then_some(secret), true)
            }
            FormOutcome::Edited => {
                self.problem = None;
                Action::Consumed
            }
            FormOutcome::Moved => Action::Consumed,
            FormOutcome::Ignored => Action::Ignored,
        }
    }

    fn summary(&self) -> Line<'static> {
        let Some(status) = &self.status else {
            return Line::styled("Asking NetworkManager...", HINT);
        };
        let active: Vec<String> = status
            .devices
            .iter()
            .filter_map(|device| {
                device
                    .connection
                    .as_deref()
                    .map(|connection| format!("{} on {connection}", device.name))
            })
            .collect();

        if status.online {
            Line::from(vec![
                Span::styled("Online", SUCCESS),
                Span::styled(format!("    {}", active.join(", ")), HINT),
            ])
        } else {
            Line::from(vec![
                Span::styled("Offline", WARNING),
                Span::styled(format!("connectivity: {}", status.connectivity), HINT),
            ])
        }
    }

    fn render_networks(&mut self, frame: &mut Frame<'_>, area: Rect) {
        if !self.wifi_available() {
            let (message, style) = if self.online() {
                ("You are online over a wired connection. Press Tab to continue.", HINT)
            } else {
                (
                    "No wireless device was found and no Ethernet is connected.\n\n\
                     Plug in an Ethernet cable to continue.",
                    WARNING,
                )
            };
            frame.render_widget(Paragraph::new(message).style(style).wrap(Wrap { trim: false }), area);
            return;
        }

        if self.points.is_empty() {
            let message = if self.scanning {
                "Scanning for networks..."
            } else {
                "No wireless networks found. Press r to scan again, or h to enter a hidden network by name."
            };

            frame.render_widget(Paragraph::new(message).style(HINT).wrap(Wrap { trim: false }), area);
            return;
        }

        let items: Vec<ListItem<'_>> = self.points.iter().map(|point| ListItem::new(entry(point))).collect();

        frame.render_stateful_widget(
            List::new(items).highlight_style(SELECTED).highlight_symbol("▸ "),
            area,
            &mut self.list,
        );
    }
}

impl Screen for Network {
    fn title(&self) -> &str {
        "Network"
    }

    fn hints(&self) -> &[(&str, &str)] {
        match self.stage {
            Stage::Networks => &[("↑↓", "choose"), ("⏎", "connect"), ("r", "rescan"), ("h", "hidden")],
            Stage::Password | Stage::Hidden => &[("⏎", "connect"), ("Esc", "back")],
            Stage::Loading | Stage::Connecting => &[],
        }
    }

    fn is_complete(&self, _model: &Model) -> bool {
        self.online()
    }

    fn handle_key(&mut self, key: KeyEvent, _model: &mut Model) -> Action {
        match self.stage {
            Stage::Loading | Stage::Connecting => Action::Ignored,
            Stage::Networks => self.on_list_key(key),
            Stage::Password => self.on_psk_key(key),
            Stage::Hidden => self.on_hidden_key(key),
        }
    }

    fn on_enter(&mut self, ctx: &Context, _model: &Model) {
        if self.ctx.is_none() {
            self.ctx = Some(ctx.clone());
            self.refresh();
        }
    }

    fn on_message(&mut self, msg: &Msg, model: &mut Model) {
        match msg {
            Msg::NetworkState(status) => {
                // A live system already connected before the installer started
                // should carry that profile over without being asked.
                if let Some(profile) = status
                    .devices
                    .iter()
                    .filter(|device| device.kind == "wifi")
                    .find_map(|device| device.connection.clone())
                {
                    model.network.profile = Some(profile);
                }

                let first_look = status.wifi_available && self.points.is_empty() && !self.scanning;

                self.status = Some(status.clone());

                if matches!(self.stage, Stage::Loading) {
                    self.stage = Stage::Networks;
                }

                if first_look {
                    self.scan();
                }
            }
            Msg::AccessPoints(points) => {
                self.scanning = false;

                // Land on whatever is currently connected
                let selected = points.iter().position(|point| point.in_use).unwrap_or(0);

                self.list.select((!points.is_empty()).then_some(selected));
                self.points = points.clone();
            }
            Msg::WifiConnected(profile) => {
                model.network.profile = Some(profile.clone());

                for point in &mut self.points {
                    point.in_use = point.ssid == *profile;
                }

                self.stage = Stage::Networks;
                self.refresh();
            }
            Msg::Failed(reason) => {
                self.scanning = false;

                // The overlay says this too, but is has to survive being dismissed
                if matches!(self.stage, Stage::Connecting) {
                    self.problem = Some(reason.clone());
                    self.stage = self.previous;
                }
            }
            _ => {}
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, _model: &Model) {
        let [heading, problem, body] =
            Layout::vertical([Constraint::Length(3), Constraint::Length(1), Constraint::Min(1)]).areas(area);

        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("How should this system reach the network?", HEADING),
                self.summary(),
            ]),
            heading,
        );

        if let Some(reason) = &self.problem {
            frame.render_widget(Paragraph::new(Line::styled(reason.clone(), ERROR)), problem);
        };

        match self.stage {
            Stage::Loading => {
                frame.render_widget(Paragraph::new(Line::styled("Asking NetworkManager...", HINT)), body);
            }
            Stage::Connecting => {
                frame.render_widget(Paragraph::new(Line::styled("Connecting...", HINT)), body);
            }
            Stage::Networks => self.render_networks(frame, body),
            Stage::Password => {
                let ssid = self.selected().map(|point| point.ssid.clone()).unwrap_or_default();
                let [prompt, fields] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(body);

                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled("The key for ", BODY),
                        Span::styled(ssid, STEP_ACTIVE),
                    ])),
                    prompt,
                );
                self.psk.render(frame, fields);
            }
            Stage::Hidden => {
                let [prompt, fields] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(body);

                frame.render_widget(
                    Paragraph::new(Line::styled(
                        "A network that does not broadcast its name. Leave the password blank if it's open.",
                        HINT,
                    ))
                    .wrap(Wrap { trim: false }),
                    prompt,
                );
                self.hidden.render(frame, fields);
            }
        }
    }
}

// Helpers

/// One access point: strength, name, and security
fn entry(point: &AccessPoint) -> Line<'static> {
    let security = if point.security.is_empty() {
        Span::styled("open", WARNING)
    } else {
        Span::styled(point.security.clone(), HINT)
    };
    let mut spans = vec![
        Span::styled(bars(point.signal), HINT),
        Span::styled(format!(" {:<32} ", point.ssid), BODY),
        security,
    ];

    if point.in_use {
        spans.push(Span::styled("    connected", SUCCESS));
    }
    Line::from(spans)
}

/// Signal strength in four cells. The full block is in the VGA console font, so
/// this survives a bare TTY as well as a terminal emulator.
fn bars(signal: u32) -> String {
    let filled = (signal as usize).min(100).div_ceil(25);
    format!("[{}{}]", "█".repeat(filled), " ".repeat(4 - filled))
}
