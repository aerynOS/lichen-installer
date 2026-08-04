// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

use super::storage;
use crate::{CliStep, FrontendStep, install_model};
use installer::{DisplayInfo, Icon, Installer, Model, StepError, register_step};
use protocols::lichen::{
    install::{InstallSystemRequest, RepoSpec, TargetMount, UserSpec, WriteSystemModelRequest},
    storage::provisioner::ApplyStrategyRequest,
};

pub async fn run(installer: &Installer, model: &mut Model) -> Result<(), StepError> {
    storage::ensure_filesystem_packages(model);

    let plan = model
        .storage
        .plan
        .as_ref()
        .ok_or_else(|| StepError::Failed("no storage configuration was selected".to_string()))?;
    let mut text = String::new();

    text.push_str(&format!("Target disk:  {}\n", model.storage.disk_display));
    text.push_str(&format!("Strategy:     {}\n", model.storage.strategy_name));
    text.push_str(&format!("Locale:       {}\n", model.region.language));
    text.push_str(&format!("Timezone:     {}\n", model.region.timezone));
    text.push_str(&format!(
        "Desktop:      {} ({} packages)\n",
        model.software.selection,
        model.software.packages.len()
    ));

    match &model.accounts.user {
        Some(user) => text.push_str(&format!("User: {} ({})\n", user.username, user.real_name)),
        None => text.push_str("User: not configured\n"),
    }

    text.push_str(&format!(
        "Root account: {}\n",
        if model.accounts.root_password_hash.is_some() {
            "password set"
        } else {
            "not configured"
        }
    ));
    text.push('\n');
    text.push_str(&storage::render_plan(plan));

    cliclack::note("Installation summary", text).map_err(|_| StepError::UserAborted)?;

    let confirmed = cliclack::confirm(format!(
        "Erase {} and install? ALL DATA ON THIS DISK WILL BE DESTROYED.",
        model.storage.disk,
    ))
    // Default to no: the six prompts before this one all default to yes, and
    // a reflexive Enter here wipes the disk.
    .initial_value(false)
    .interact()
    .map_err(|_| StepError::UserAborted)?;

    if !confirmed {
        return Err(StepError::UserAborted);
    }

    let mut provisioner = installer.provisioner().await?;
    let applied = provisioner
        .apply_strategy(ApplyStrategyRequest {
            strategy: model.storage.strategy_id.clone(),
            disks: vec![model.storage.disk.clone()],
        })
        .await?
        .into_inner();
    let applied_plan = applied
        .plan
        .ok_or_else(|| StepError::Failed("backend returned no applied plan".to_string()))?;
    let root_device = applied_plan
        .role_mounts
        .iter()
        .find(|role_mount| role_mount.mountpoint == "/")
        .map(|role_mount| role_mount.device.clone())
        .ok_or_else(|| StepError::Failed("applied plan has no root mount".to_string()))?;
    let system_model = install_model::system_model_kdl(model);
    let install_record = install_model::to_kdl(model);
    let repositories = install_model::repositories(&system_model)
        .map_err(|e| StepError::Failed(format!("generated system-model failed to parse: {e}")))?
        .into_iter()
        .map(|repo| RepoSpec {
            id: repo.id,
            uri: repo.uri,
        })
        .collect();
    let mut install = installer.install().await?;
    install
        .write_system_model(WriteSystemModelRequest {
            root_device,
            system_model,
            install_model: install_record,
        })
        .await?;

    let mounts = applied_plan
        .role_mounts
        .iter()
        .filter(|role_mount| role_mount.mountpoint.starts_with('/'))
        .map(|role_mount| TargetMount {
            device: role_mount.device.clone(),
            mountpoint: role_mount.mountpoint.clone(),
        })
        .collect();

    let spinner = cliclack::spinner();
    // Count-match, not subset-match: this is the last gate before an
    // irreversible whole-disk wipe, and applying the strategy to only the
    // disks that happened to resolve is not what the user approved.
    spinner.start("Installing AerynOS to the target disk (this can take several minutes)");

    let mut stream = match install
        .install_system(InstallSystemRequest {
            mounts,
            locale: model.region.language.clone(),
            timezone: model.region.timezone.clone(),
            root_password_hash: model.accounts.root_password_hash.clone().unwrap_or_default(),
            user: model.accounts.user.as_ref().map(|user| UserSpec {
                username: user.username.clone(),
                real_name: user.real_name.clone(),
                password_hash: user.password_hash.clone(),
            }),
            repositories,
            // The CLI has no keyboard or network step yet; empty leaves the target's
            // console, X11, and NetworkManager config exactly as the packages ship
            // it, just as before these were added.
            keymap: String::new(),
            x11_layout: String::new(),
            network_profile: None,
        })
        .await
    {
        Ok(response) => response.into_inner(),
        Err(e) => {
            spinner.error("Installation failed");
            return Err(e.into());
        }
    };

    loop {
        match stream.message().await {
            Ok(Some(update)) => {
                if update.finished {
                    spinner.stop("AerynOS installed");
                    break;
                }

                if !update.message.is_empty() {
                    spinner.set_message(update.message);
                }
            }
            Ok(None) => {
                spinner.error("Installation failed");
                return Err(StepError::Failed("install stream ended without completing".to_string()));
            }
            Err(e) => {
                spinner.error("Installation failed");
                return Err(e.into());
            }
        }
    }

    Ok(())
}

register_step! {
    id: "summary",
    author: "AerynOS Developers",
    description: "Review the installation summary",
    create: || Box::new(CliStep { info: DisplayInfo {
        title: "Summary".to_string(),
        description: "Review the installation summary".to_string(),
        icon: Some(Icon::Emoji("📝".to_string())),
    }, step: FrontendStep::Summary })
}
