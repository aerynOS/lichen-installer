// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Styles are defined here.
//!
//! Named ANSI colors only, deliberately: the installer runs on whatever
//! terminal the ISO happens to land in, and named colors honor the user's
//! palette instead of fighting it. Screens must never build a `Style` inline;
//! doing it in one place keeps all screens stylistically in sync.

use ratatui::style::{Color, Modifier, Style};

/// Borders and rules around the content
pub const FRAME: Style = Style::new().fg(Color::DarkGray);
/// The installer name in the header
pub const TITLE: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
/// Ordinary body text
pub const BODY: Style = Style::new();
/// A heading inside the screen
pub const HEADING: Style = Style::new().add_modifier(Modifier::BOLD);
/// Secondary text: descriptions, sizes, key hints
pub const HINT: Style = Style::new().fg(Color::DarkGray);
/// A step not yet visited
pub const STEP_PENDING: Style = Style::new().fg(Color::DarkGray);
/// A step whose choices have been made
pub const STEP_COMPLETE: Style = Style::new().fg(Color::Green);
/// The step currently on screen
pub const STEP_ACTIVE: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
/// The highlighted row of a list
pub const SELECTED: Style = Style::new()
    .fg(Color::Black)
    .bg(Color::Cyan)
    .add_modifier(Modifier::BOLD);
/// Something the user must read before continuing
pub const WARNING: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
/// A failure
pub const ERROR: Style = Style::new().fg(Color::Red).add_modifier(Modifier::BOLD);
/// Confirmation that something succeeded
pub const SUCCESS: Style = Style::new().fg(Color::Green).add_modifier(Modifier::BOLD);
