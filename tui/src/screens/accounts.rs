// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! The root password and the primary user account.
//!
//! Passwords are hashed the moment they are accepted; only hashes reach the
//! model, which never carries plaintext.

use super::{Context, Screen};
use crate::{
    events::Action,
    theme::*,
    widgets::{Field, Form, FormOutcome},
};
use installer::{Model, User};
use ratatui::{
    Frame,
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::Paragraph,
};
use sha_crypt::{ROUNDS_DEFAULT, Sha512Params, sha512_simple};

const REAL_NAME: usize = 0;
const USERNAME: usize = 1;
const PASSWORD: usize = 2;
const CONFIRM: usize = 3;
const ROOT_PASSWORD: usize = 4;
const ROOT_CONFIRM: usize = 5;
const KEPT: &str = "imported; leave blank to keep, type to replace";

pub struct Accounts {
    form: Form,
    problem: Option<String>,
    prefilled: bool,
}

impl Accounts {
    pub fn new() -> Self {
        Self {
            form: Form::new(vec![
                Field::new("Real name", false),
                Field::new("Username", false),
                Field::new("Password", true),
                Field::new("Confirm password", true),
                Field::new("Root password", true),
                Field::new("Confirm root password", true),
            ]),
            problem: None,
            prefilled: false,
        }
    }

    /// Report a problem inline and send the user to the field that caused it.
    fn reject(&mut self, problem: &str, field: usize) -> Action {
        self.problem = Some(problem.to_string());
        self.form.focus_on(field);
        Action::Consumed
    }

    fn submit(&mut self, model: &mut Model) -> Action {
        let real_name = self.form.value(REAL_NAME).to_string();
        let username = self.form.value(USERNAME).to_string();
        let password = self.form.value(PASSWORD).to_string();
        let confirm = self.form.value(CONFIRM).to_string();
        let root = self.form.value(ROOT_PASSWORD).to_string();
        let root_confirm = self.form.value(ROOT_CONFIRM).to_string();

        if let Err(problem) = check_username(&username) {
            return self.reject(problem, USERNAME);
        }

        let existing_user = model.accounts.user.as_ref().map(|user| user.password_hash.clone());
        let existing_root = model.accounts.root_password_hash.clone();

        let user_hash = match self.resolve(&password, &confirm, existing_user, (PASSWORD, CONFIRM), "user") {
            Ok(hash) => hash,
            Err(action) => {
                return action;
            }
        };
        let root_hash = match self.resolve(
            &root,
            &root_confirm,
            existing_root,
            (ROOT_PASSWORD, ROOT_CONFIRM),
            "root",
        ) {
            Ok(hash) => hash,
            Err(action) => return action,
        };

        Action::Next
    }

    /// One password pair into one hash.
    ///
    /// Both fields blank means keep whatever the imported model already had.
    fn resolve(
        &mut self,
        typed: &str,
        confirmation: &str,
        existing: Option<String>,
        fields: (usize, usize),
        subject: &str,
    ) -> Result<String, Action> {
        let (first, second) = fields;

        if typed.is_empty() && confirmation.is_empty() {
            return match existing {
                Some(hash) => Ok(hash),
                None => Err(self.reject(&format!("the {subject} password cannot be empty"), first)),
            };
        }

        if typed != confirmation {
            self.form.clear(first);
            self.form.clear(second);
            return Err(self.reject(&format!("the {subject} passwords do not match..."), first));
        }

        match hash(typed) {
            Ok(hash) => Ok(hash),
            Err(()) => Err(Action::Failed(format!("failed to hash the {subject} password..."))),
        }
    }
}

impl Screen for Accounts {
    fn title(&self) -> &str {
        "Accounts"
    }

    fn hints(&self) -> &[(&str, &str)] {
        &[("↑↓", "field"), ("⏎", "next / submit")]
    }

    fn is_complete(&self, model: &Model) -> bool {
        model.accounts.user.is_some() && model.accounts.root_password_hash.is_some()
    }

    fn handle_key(&mut self, key: KeyEvent, model: &mut Model) -> Action {
        match self.form.handle_key(key) {
            FormOutcome::Submit => self.submit(model),
            FormOutcome::Edited => {
                self.problem = None;
                Action::Consumed
            }
            FormOutcome::Moved => Action::Consumed,
            FormOutcome::Ignored => Action::Ignored,
        }
    }

    fn on_enter(&mut self, _ctx: &Context, model: &Model) {
        if self.prefilled {
            return;
        }
        self.prefilled = true;

        // Names carry over from an imported model
        if let Some(user) = &model.accounts.user {
            self.form.set_value(REAL_NAME, &user.real_name);
            self.form.set_value(USERNAME, &user.username);

            if !user.password_hash.is_empty() {
                self.form.set_placeholder(PASSWORD, KEPT);
                self.form.set_placeholder(CONFIRM, KEPT);
            }
        }

        if model.accounts.root_password_hash.is_some() {
            self.form.set_placeholder(ROOT_PASSWORD, KEPT);
            self.form.set_placeholder(ROOT_CONFIRM, KEPT);
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, _model: &Model) {
        let [heading, problem, body] =
            Layout::vertical([Constraint::Length(2), Constraint::Length(1), Constraint::Min(1)]).areas(area);

        frame.render_widget(
            Paragraph::new(Line::styled("Set the root password and create your user", HEADING)),
            heading,
        );

        if let Some(reason) = &self.problem {
            frame.render_widget(Paragraph::new(Line::styled(reason.clone(), ERROR)), problem);
        }

        self.form.render(frame, body);
    }
}

// Helpers

fn check_username(username: &str) -> Result<(), &'static str> {
    let starts_ok = username
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_lowercase() || character == '_');
    let rest_ok = username.chars().all(|character| {
        character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_' || character == '-'
    });

    if username.is_empty() || username.len() > 32 || !starts_ok || !rest_ok {
        return Err("use lowercase letters, digits, -, and _; start with a letter or _; max 32 characters");
    }

    Ok(())
}

fn hash(plain: &str) -> Result<String, ()> {
    let params = Sha512Params::new(ROUNDS_DEFAULT).map_err(|_| ())?;
    sha512_simple(plain, &params).map_err(|_| ())
}
