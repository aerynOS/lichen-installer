// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! The last change to change anything.
//!
//! Every choice made so far, plush the partition plan, on one scrollable pane.
//! Confirming here is the point of no return, so the confirm is a separate
//! stage that answer to `y` alone; never a reflexive Enter.

use super::Screen;
use crate::{events::Action, plan, theme::*};
use installer::Model;
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

enum Stage {
    Review,
    Confirm,
}

pub struct Summary {
    stage: Stage,
    scroll: u16,
    problem: Option<String>,
}

impl Summary {
    pub fn new() -> Self {
        Self {
            stage: Stage::Review,
            scroll: 0,
            problem: None,
        }
    }

    /// Everything the install needs, named in the order its step appears
    fn missing(&self, model: &Model) -> Vec<&'static str> {
        let mut missing = Vec::new();

        if model.storage.disk.is_empty() {
            missing.push("a target disk");
        }
        if model.storage.plan.is_none() {
            missing.push("a partitioning strategy");
        }
        if model.region.language.is_empty() {
            missing.push("a locale");
        }
        if model.region.timezone.is_empty() {
            missing.push("a timezone");
        }
        if model.software.selection.is_empty() {
            missing.push("a desktop environment or non-DE variant");
        }
        if model.accounts.user.is_none() {
            missing.push("a user account");
        }
        if model.accounts.root_password_hash.is_none() {
            missing.push("a root password");
        }

        missing
    }

    /// Every choice, then partition plan underneath it
    fn review(&self, model: &Model) -> Vec<Line<'static>> {
        let mut lines = vec![
            row("Target disk", model.storage.disk_display.clone()),
            row("Strategy", model.storage.strategy_name.clone()),
            row("Locale", model.region.language.clone()),
            row("Timezone", model.region.timezone.clone()),
            row(
                "Keyboard",
                if model.region.keymap.is_empty() {
                    format!("{} (console fass back to us)", model.region.layout)
                } else {
                    format!("{} (console {})", model.region.layout, model.region.keymap)
                },
            ),
            row(
                "Desktop",
                format!(
                    "{} ({} packages)",
                    model.software.selection,
                    model.software.packages.len()
                ),
            ),
            row(
                "User",
                match &model.accounts.user {
                    Some(user) => format!("{} ({})", user.username, user.real_name),
                    None => "not configured".to_string(),
                },
            ),
            row(
                "Root account",
                if model.accounts.root_password_hash.is_some() {
                    "password set".to_string()
                } else {
                    "not configured".to_string()
                },
            ),
            Line::raw(""),
        ];

        if let Some(plan) = &model.storage.plan {
            lines.extend(plan::describe(plan));
        }
        lines
    }

    fn render_confirm(&self, frame: &mut Frame<'_>, area: Rect, model: &Model) {
        let [prompt, _] = Layout::vertical([Constraint::Length(9), Constraint::Min(0)]).areas(area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(ERROR)
            .title(Line::styled(" Point of no return ", ERROR));
        let inner = block.inner(prompt);
        let lines = vec![
            Line::styled(format!("Erase {} and install AerynOS?", model.storage.disk), WARNING),
            Line::raw(""),
            Line::styled("Everything on this disk will be destroyed. Nothing has been", BODY),
            Line::styled("written yet; this is the last moment as which that is true.", BODY),
            Line::raw(""),
            Line::from(vec![
                Span::styled("y", ERROR),
                Span::styled(" erase and install    ", HINT),
                Span::styled("n/Esc", STEP_ACTIVE),
                Span::styled(" go back", HINT),
            ]),
        ];

        frame.render_widget(block, prompt);
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }

    fn on_review_key(&mut self, key: KeyEvent, model: &Model) -> Action {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll = self.scroll.saturating_sub(1);
                Action::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll = self.scroll.saturating_add(1);
                Action::Consumed
            }
            KeyCode::PageUp => {
                self.scroll = self.scroll.saturating_sub(10);
                Action::Consumed
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_add(10);
                Action::Consumed
            }
            KeyCode::Home => {
                self.scroll = 0;
                Action::Consumed
            }
            KeyCode::Enter => {
                let missing = self.missing(model);

                if !missing.is_empty() {
                    self.problem = Some(format!("Still needs {}", missing.join(", ")));
                    return Action::Consumed;
                }

                self.problem = None;
                self.stage = Stage::Confirm;
                Action::Consumed
            }
            _ => Action::Ignored,
        }
    }

    fn on_confirm_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            // Deliberately not Enter. Mitigates any muscle memory for
            // Enter being "Next" on the previous screens and accidentally
            // confirming when it's not wanted.
            KeyCode::Char('y' | 'Y') => Action::Commit,
            KeyCode::Esc | KeyCode::Char('n' | 'N') => {
                self.stage = Stage::Review;
                Action::Consumed
            }
            // Nothing falls through while the confirm overlay is up, including Tab
            _ => Action::Consumed,
        }
    }
}

impl Screen for Summary {
    fn title(&self) -> &str {
        "Summary"
    }

    fn hints(&self) -> &[(&str, &str)] {
        match self.stage {
            Stage::Review => &[("↑↓", "scroll"), ("Enter", "install")],
            Stage::Confirm => &[("y", "erase and install"), ("n/Esc", "go back")],
        }
    }

    fn is_complete(&self, model: &Model) -> bool {
        self.missing(model).is_empty()
    }

    fn handle_key(&mut self, key: KeyEvent, model: &mut Model) -> Action {
        match self.stage {
            Stage::Review => self.on_review_key(key, model),
            Stage::Confirm => self.on_confirm_key(key),
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, model: &Model) {
        let [heading, problem, body] =
            Layout::vertical([Constraint::Length(2), Constraint::Length(1), Constraint::Min(1)]).areas(area);

        frame.render_widget(
            Paragraph::new(Line::styled("Review before anything is written", HEADING)),
            heading,
        );

        if let Some(reason) = &self.problem {
            frame.render_widget(Paragraph::new(Line::styled(reason.clone(), WARNING)), problem);
        }

        if matches!(self.stage, Stage::Confirm) {
            self.render_confirm(frame, body, model);
            return;
        }

        let lines = self.review(model);
        // Prevent scrolling past the end
        let limit = (lines.len() as u16).saturating_sub(body.height);

        self.scroll = self.scroll.min(limit);
        frame.render_widget(
            Paragraph::new(lines)
                .scroll((self.scroll, 0))
                .wrap(Wrap { trim: false }),
            body,
        );
    }
}

// Helpers

/// One labelled line of the review
fn row(label: &str, value: String) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<14}"), HINT),
        Span::styled(value, BODY),
    ])
}
