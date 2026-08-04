// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Choosing the disk to install onto.
//!
//! The disk list arrives from the backend on a background task; the screen is
//! interactive the whole time it is in flight.

use super::{Context, Screen};
use crate::{
    events::{Action, Msg},
    theme::*,
};
use installer::Model;
use protocols::lichen::storage::disks::{Disk, ListDisksRequest, disks_client::DisksClient};
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph, Wrap},
};
use std::env;

enum State {
    Loading,
    Ready(Vec<Disk>),
}

pub struct Storage {
    state: State,
    list: ListState,
    requested: bool,
}

impl Storage {
    pub fn new() -> Self {
        Self {
            state: State::Loading,
            list: ListState::default(),
            requested: false,
        }
    }

    fn disks(&self) -> &[Disk] {
        match &self.state {
            State::Ready(disks) => disks,
            State::Loading => &[],
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let count = self.disks().len();

        if count == 0 {
            return;
        }

        let current = self.list.selected().unwrap_or(0) as isize;
        let next = current.saturating_add(delta).clamp(0, count as isize - 1);

        self.list.select(Some(next as usize));
    }

    fn choose(&mut self, model: &mut Model) -> Action {
        let Some(disk) = self.list.selected().and_then(|index| self.disks().get(index)) else {
            return Action::Consumed;
        };

        model.storage.disk = disk.device.clone();
        model.storage.disk_display = describe(disk);

        // A plan computed for the previous disk says nothing about this one
        model.storage.plan = None;

        Action::Next
    }
}

impl Screen for Storage {
    fn title(&self) -> &str {
        "Storage"
    }

    fn hints(&self) -> &[(&str, &str)] {
        &[("↑↓", "choose"), ("Enter", "select")]
    }

    fn is_complete(&self, model: &Model) -> bool {
        !model.storage.disk.is_empty()
    }

    fn handle_key(&mut self, key: KeyEvent, model: &mut Model) -> Action {
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
            KeyCode::Enter => self.choose(model),
            _ => Action::Ignored,
        }
    }

    fn on_enter(&mut self, ctx: &Context, _model: &Model) {
        if self.requested {
            return;
        }

        self.requested = true;

        let channel = ctx.channel.clone();

        // Loopback devices stay hidden unless explicitly asked for, which is
        // what makes end-to-end testing against a losetup disk safe.
        let exclude_loopback = env::var_os("LICHEN_INCLUDE_LOOPBACK").is_none();

        ctx.spawn(async move {
            let disks = DisksClient::new(channel)
                .list_disks(ListDisksRequest { exclude_loopback })
                .await?
                .into_inner()
                .disks;

            Ok(Msg::Disks(disks))
        });
    }

    fn on_message(&mut self, msg: &Msg, model: &mut Model) {
        let Msg::Disks(disks) = msg else {
            return;
        };

        // Land on whatever the model already says, so coming back to this
        // screen shows the current choice instead of resetting to the top.
        let selected = disks
            .iter()
            .position(|disk| disk.device == model.storage.disk)
            .unwrap_or(0);

        self.list.select((!disks.is_empty()).then_some(selected));
        self.state = State::Ready(disks.clone());
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, _model: &Model) {
        let [heading, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);

        frame.render_widget(
            Paragraph::new(vec![
                Line::styled("Where should the system be installed?", HEADING),
                Line::styled("Nothing is written until you confirm on the Summary screen.", HINT),
            ]),
            heading,
        );

        match &self.state {
            State::Loading => {
                frame.render_widget(Paragraph::new(Line::styled("Looking for disks...", HINT)), body);
                return;
            }
            State::Ready(disks) if disks.is_empty() => {
                frame.render_widget(
                    Paragraph::new(
                        "The backend offered no disks.\n\n\
                         When testing against a loopback device, set LICHEN_INCLUDE_LOOPBACK=1.",
                    )
                    .style(WARNING)
                    .wrap(Wrap { trim: false }),
                    body,
                );
                return;
            }
            State::Ready(_) => {}
        }

        let items: Vec<ListItem<'_>> = self.disks().iter().map(|disk| ListItem::new(entry(disk))).collect();

        frame.render_stateful_widget(
            List::new(items).highlight_style(SELECTED).highlight_symbol(CURSOR),
            body,
            &mut self.list,
        );
    }
}

// Helpers

/// One disk over two lines: its size and what it is
fn entry(disk: &Disk) -> Vec<Line<'static>> {
    vec![
        Line::from(vec![
            Span::styled(disk.device.clone(), BODY),
            Span::styled(format!("   {}", disk.display_size), HINT),
        ]),
        Line::styled(
            format!("  {}", disk.model.clone().unwrap_or_else(|| "Unknown model".into())),
            HINT,
        ),
    ]
}

/// How the disk is named back to the user in the summary
fn describe(disk: &Disk) -> String {
    format!(
        "{} - {} - {}",
        disk.device,
        disk.model.as_deref().unwrap_or("Unknown"),
        disk.display_size,
    )
}
