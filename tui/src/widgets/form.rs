// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! A stack of labelled text fields.
//!
//! Fields are navigated with ↑↓ rather than Tab, so Tab keeps its global
//! meaning of moving between installation steps.

use crate::theme::*;
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

/// Rows each field occupies: a bordered box around one line of text
const FIELD_HEIGHT: u16 = 3;

pub struct Field {
    label: String,
    value: String,
    placeholder: String,
    /// Position in characters, not bytes
    cursor: usize,
    masked: bool,
}

impl Field {
    pub fn new(label: impl Into<String>, masked: bool) -> Self {
        Self {
            label: label.into(),
            value: String::new(),
            placeholder: String::new(),
            cursor: 0,
            masked,
        }
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn len(&self) -> usize {
        self.value.chars().count()
    }

    /// Byte offset of the nth character
    fn byte_index(&self, chars: usize) -> usize {
        self.value
            .char_indices()
            .nth(chars)
            .map(|(index, _)| index)
            .unwrap_or(self.value.len())
    }

    fn insert(&mut self, character: char) {
        let at = self.byte_index(self.cursor);

        self.value.insert(at, character);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }

        let at = self.byte_index(self.cursor - 1);

        self.value.remove(at);
        self.cursor -= 1;
    }

    fn delete(&mut self) {
        if self.cursor >= self.len() {
            return;
        }

        let at = self.byte_index(self.cursor);

        self.value.remove(at);
    }

    /// What is drawn: the value itself, or one bullet per character
    fn display(&self) -> String {
        if self.masked {
            MASK.repeat(self.len())
        } else {
            self.value.clone()
        }
    }
}

pub enum Outcome {
    Ignored,
    Moved,
    Edited,
    Submit,
}

pub struct Form {
    fields: Vec<Field>,
    focus: usize,
}

impl Form {
    pub fn new(fields: Vec<Field>) -> Self {
        Self { fields, focus: 0 }
    }

    pub fn value(&self, index: usize) -> &str {
        self.fields.get(index).map(Field::value).unwrap_or_default()
    }

    pub fn set_value(&mut self, index: usize, value: &str) {
        if let Some(field) = self.fields.get_mut(index) {
            field.value = value.to_string();
            field.cursor = field.len();
        }
    }

    pub fn set_placeholder(&mut self, index: usize, text: &str) {
        if let Some(field) = self.fields.get_mut(index) {
            field.placeholder = text.to_string();
        }
    }

    pub fn clear(&mut self, index: usize) {
        self.set_value(index, "");
    }

    pub fn focus_on(&mut self, index: usize) {
        if index < self.fields.len() {
            self.focus = index;
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        // Field movement first, before anything borrows the focused field
        match key.code {
            KeyCode::Up => {
                self.focus = self.focus.saturating_sub(1);
                return Outcome::Moved;
            }
            KeyCode::Down => {
                if self.focus + 1 < self.fields.len() {
                    self.focus += 1;
                }
                return Outcome::Moved;
            }
            // Enter walks the form, and submits from the last field
            KeyCode::Enter => {
                if self.focus + 1 < self.fields.len() {
                    self.focus += 1;
                    return Outcome::Moved;
                }
                return Outcome::Submit;
            }
            _ => {}
        }

        let Some(field) = self.fields.get_mut(self.focus) else {
            return Outcome::Ignored;
        };

        match key.code {
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                field.insert(character);
                Outcome::Edited
            }
            KeyCode::Backspace => {
                field.backspace();
                Outcome::Edited
            }
            KeyCode::Delete => {
                field.delete();
                Outcome::Edited
            }
            KeyCode::Left => {
                field.cursor = field.cursor.saturating_sub(1);
                Outcome::Moved
            }
            KeyCode::Right => {
                field.cursor = (field.cursor + 1).min(field.len());
                Outcome::Moved
            }
            KeyCode::Home => {
                field.cursor = 0;
                Outcome::Moved
            }
            KeyCode::End => {
                field.cursor = field.len();
                Outcome::Moved
            }
            _ => Outcome::Ignored,
        }
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect) {
        for (index, field) in self.fields.iter().enumerate() {
            let y = area.y + index as u16 * FIELD_HEIGHT;

            if y + FIELD_HEIGHT > area.y + area.height {
                break;
            }

            let row = Rect {
                x: area.x,
                y,
                width: area.width,
                height: FIELD_HEIGHT,
            };
            let focused = index == self.focus;
            let style = if focused { STEP_ACTIVE } else { FRAME };
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(style)
                .title(Line::styled(format!(" {} ", field.label), style));
            let inner = block.inner(row);

            frame.render_widget(block, row);

            let shown = field.display();
            let line = if shown.is_empty() && !field.placeholder.is_empty() {
                let mut spans = Vec::new();

                if focused {
                    spans.push(Span::styled("| ", STEP_ACTIVE));
                }

                spans.push(Span::styled(field.placeholder.clone(), HINT));
                Line::from(spans)
            } else if focused {
                let (before, after) = split_at_char(&shown, field.cursor);

                Line::from(vec![
                    Span::styled(before.to_string(), BODY),
                    Span::styled("| ", STEP_ACTIVE),
                    Span::styled(after.to_string(), BODY),
                ])
            } else {
                Line::styled(shown, BODY)
            };

            frame.render_widget(Paragraph::new(line), inner);
        }
    }
}

/// Split a string at a character index
fn split_at_char(text: &str, chars: usize) -> (&str, &str) {
    let at = text
        .char_indices()
        .nth(chars)
        .map(|(index, _)| index)
        .unwrap_or(text.len());

    text.split_at(at)
}
