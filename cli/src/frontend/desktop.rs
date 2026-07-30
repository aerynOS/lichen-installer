// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

use crate::{CliStep, FrontendStep, selections};
use installer::{DisplayInfo, Icon, Installer, Model, StepError, register_step};

pub async fn run(_installer: &Installer, model: &mut Model) -> Result<(), StepError> {
    if model.imported && !model.software.selection.is_empty() || !model.software.packages.is_empty() {
        // Recover the name from the package set so the summary and the
        // regenerated install-model.kdl both record which desktop this was
        if model.software.selection.is_empty()
            && let Some(name) = selections::identify(&model.software.packages)
        {
            model.software.selection = name;
        }

        let _ = cliclack::log::info(match model.software.selection.as_str() {
            "" => format!(
                "Using imported package set of {} packages",
                model.software.packages.len()
            ),
            selection => format!(
                "Using imported desktop environment {selection} with {} packages",
                model.software.packages.len(),
            ),
        });
        return Ok(());
    }

    let desktops = selections::desktops();
    let items = desktops
        .iter()
        .map(|sel| (sel.name.clone(), sel.summary.clone(), sel.description.clone()))
        .collect::<Vec<_>>();
    let picked: String = cliclack::select("Select your desktop environment")
        .items(&items)
        .interact()
        .map_err(|_| StepError::UserAborted)?;
    let packages = selections::resolve(&picked)?;

    tracing::info!("Selected desktop environment {picked} with {} packages", packages.len());
    model.software.selection = picked;
    model.software.packages = packages;

    Ok(())
}

register_step! {
    id: "desktop",
    author: "AerynOS Developers",
    description: "Select the desktop experience",
    create: || Box::new(
        CliStep {
            info: DisplayInfo {
                title: "Desktop".to_string(),
                description: "Select the desktop environment".to_string(),
                icon: Some(Icon::Emoji("💻".to_string())),
            },
            step: FrontendStep::Desktop,
        }
    )
}
