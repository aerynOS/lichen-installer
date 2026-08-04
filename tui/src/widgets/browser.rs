// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Waling the filesystem to find a document.
//!
//! Browsing rather than typing, because `/run/media/.../system-model.kdl` type
//! blind on a console is the kind of thing that gets abandonded halfway. a
//! free-text field stays available for the URIs no local walk can reach.

use crate::theme::*;
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{List, ListItem, ListState, Paragraph},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

/// What a key did to the browser
pub enum Outcome {
    Ignored,
    Consumed,
    /// A file was chosen
    Picked(PathBuf),
}

pub struct Browser {
    cwd: PathBuf,
    entries: Vec<PathBuf>,
    list: ListState,
    problem: Option<String>,
    /// Only files whose name ends with this are offered
    suffix: &'static str,
}

impl Browser {
    pub fn new(suffix: &'static str) -> Self {
        let mut browser = Self {
            cwd: start(),
            entries: Vec::new(),
            list: ListState::default(),
            problem: None,
            suffix,
        };

        browser.reload();
        browser
    }

    /// List the current directory: directories first, then matching files,
    /// each sorted by name. Hidden entries are skipped.
    fn reload(&mut self) {
        self.entries.clear();
        self.problem = None;

        let read = match fs::read_dir(&self.cwd) {
            Ok(read) => read,
            Err(err) => {
                self.problem = Some(format!("cannot list {}: {err}", self.cwd.display()));
                self.list.select(None);
                return;
            }
        };
        let (mut directories, mut files): (Vec<PathBuf>, Vec<PathBuf>) = read
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| !name(path).starts_with('.'))
            .filter(|path| path.is_dir() || name(path).ends_with(self.suffix))
            .partition(|path| path.is_dir());

        directories.sort();
        files.sort();

        self.entries = directories;
        self.entries.append(&mut files);
        self.list.select((!self.entries.is_empty()).then_some(0));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                Outcome::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                Outcome::Consumed
            }
            KeyCode::Home => {
                self.move_selection(isize::MIN);
                Outcome::Consumed
            }
            KeyCode::End => {
                self.move_selection(isize::MAX);
                Outcome::Consumed
            }
            KeyCode::Left | KeyCode::Backspace => {
                self.ascend();
                Outcome::Consumed
            }
            KeyCode::Right | KeyCode::Enter => self.descend(),
            _ => Outcome::Ignored,
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }

        let current = self.list.selected().unwrap_or(0) as isize;
        let next = current.saturating_add(delta).clamp(0, self.entries.len() as isize - 1);
        self.list.select(Some(next as usize));
    }

    /// Enter the highlighted directory, or choose the highlighted file.
    fn descend(&mut self) -> Outcome {
        let Some(path) = self.list.selected().and_then(|index| self.entries.get(index)).cloned() else {
            return Outcome::Consumed;
        };

        if !path.is_dir() {
            return Outcome::Picked(path);
        }

        self.cwd = path;
        self.reload();
        Outcome::Consumed
    }

    /// Move to the parent, stopping at the root.
    fn ascend(&mut self) {
        if let Some(parent) = self.cwd.parent().map(Path::to_path_buf) {
            self.cwd = parent;
            self.reload()
        }
    }

    pub fn render(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let [heading, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);

        frame.render_widget(
            Paragraph::new(Line::styled(self.cwd.display().to_string(), HINT)),
            heading,
        );

        if let Some(problem) = &self.problem {
            frame.render_widget(Paragraph::new(Line::styled(problem.clone(), WARNING)), body);
            return;
        }

        if self.entries.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled(format!("Nothing here matching *{}", self.suffix), HINT)),
                body,
            );
            return;
        }

        let items: Vec<ListItem<'_>> = self
            .entries
            .iter()
            .map(|path| {
                // A trailing slash is the difference betwee somewhere to go and
                // something to choose.
                let label = match path.is_dir() {
                    true => format!("{}/", name(path)),
                    false => name(path).to_string(),
                };
                ListItem::new(Line::styled(label, BODY))
            })
            .collect();

        frame.render_stateful_widget(
            List::new(items).highlight_style(SELECTED).highlight_symbol(CURSOR),
            body,
            &mut self.list,
        );
    }
}

// Helpers

/// Where to open. Removable media first, because that is where a model
/// document arrives from on an installer ISO, then somewhere that always exists.
fn start() -> PathBuf {
    ["/run/media", "/media", "/mnt"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_dir())
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The final component of a paths as a str
fn name(path: &Path) -> &str {
    path.file_name().and_then(|name| name.to_str()).unwrap_or_default()
}
