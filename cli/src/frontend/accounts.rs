// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Account configuration step
//!
//! Passwords are hashed with sha512-crypt immediately after confirmation;
//! only the hashes enter the model.

use crate::{CliStep, FrontendStep};
use installer::{DisplayInfo, Icon, Installer, Model, StepError, User, register_step};
use yescrypt::{PasswordHasher, Yescrypt};

pub async fn run(_installer: &Installer, model: &mut Model) -> Result<(), StepError> {
    if model.accounts.root_password_hash.is_some() && model.accounts.user.is_some() {
        let keep = cliclack::confirm("Keep the imported account settings?")
            .initial_value(true)
            .interact()
            .map_err(|_| StepError::UserAborted)?;

        if keep {
            return Ok(());
        }
    }
    let root_password = ask_password("root")?;
    model.accounts.root_password_hash = Some(hash_password(&root_password)?);

    let username: String = cliclack::input("Username for the new user")
        .validate(|input: &String| {
            let starts_ok = input
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_lowercase() || ch == '_');
            let rest_ok = input
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-');
            if input.is_empty() || input.len() > 32 || !starts_ok || !rest_ok {
                Err("use lowercase letters, digits, - and _; start with a letter or _; max 32 chars")
            } else {
                Ok(())
            }
        })
        .interact()
        .map_err(|_| StepError::UserAborted)?;
    let real_name: String = cliclack::input("Real name")
        .required(false)
        .interact()
        .map_err(|_| StepError::UserAborted)?;
    let user_pass = ask_password(&username)?;

    model.accounts.user = Some(User {
        username,
        real_name,
        password_hash: hash_password(&user_pass)?,
    });

    Ok(())
}

fn ask_password(who: &str) -> Result<String, StepError> {
    loop {
        let first = cliclack::password(format!("Password for {who}"))
            .mask('*')
            .validate(|input: &String| {
                if input.is_empty() {
                    Err("password cannot be empty")
                } else {
                    Ok(())
                }
            })
            .interact()
            .map_err(|_| StepError::UserAborted)?;
        let second = cliclack::password(format!("Confirm password for {who}"))
            .mask('*')
            .interact()
            .map_err(|_| StepError::UserAborted)?;

        if first == second {
            return Ok(first);
        }

        let _ = cliclack::log::warning("Passwords do not match, please try again");
    }
}

fn hash_password(plain: &str) -> Result<String, StepError> {
    Yescrypt::default()
        .hash_password(plain.as_bytes())
        .map_err(|_| StepError::Failed("failed to hash password".to_string()))
        .map(|hash| hash.to_string())
}

register_step! {
    id: "accounts",
    author: "AerynOS Developers",
    description: "Configure system accounts",
    create: || Box::new(
        CliStep {
            info: DisplayInfo {
                title: "Accounts".to_string(),
                description: "Set the root password and create your user".to_string(),
                icon: Some(Icon::Emoji("👤".to_string())),
            },
            step: FrontendStep::Accounts,
        }
    )
}
