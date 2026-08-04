// SPDX-FileCopyrightText: Copyright © 2025 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Frontend module for disk selection during installation
//!
//! This module provides the disk selection step of the installation process,
//! allowing users to choose which disk to install AerynOS on, preview the
//! partitioning strategy, and apply it.

use crate::{CliStep, FrontendStep, install_model, selections};
use installer::{DisplayInfo, Icon, Installer, Model, StepError, register_step};
use protocols::lichen::{
    osinfo::OsInfo,
    storage::{
        disks::{Disk, ListDisksRequest},
        provisioner::{StrategyDefinition, StrategyPlan, TryStrategyRequest},
    },
};
use std::{collections::BTreeSet, env, path::Path};

/// Root filesystem choices as strategy id suffixes, first entry is default
const FILESYSTEM_CHOICES: &[(&str, &str, &str)] = &[
    ("_xfs", "xfs", "Recommended for most users"),
    ("_f2fs", "f2fs", "Flash-friendly filesystem"),
    ("_ext4", "ext4", "The traditional Linux filesystem"),
    ("_btrfs", "btrfs", "Copy-on-write with checksumming"),
    ("_bcachefs", "bcachefs", "Copy-on-write; needs a kernel-matched module"),
];

/// Userspace packages the installed system needs for its root filesystem
const FILESYSTEM_PACKAGES: &[(&str, &[&str])] = &[
    ("btrfs", &["btrfs-progs", "udisks-btrfs"]),
    ("bcachefs", &["bcachefs-tools", "bcachefs-module-stable"]),
];

pub async fn run(info: &OsInfo, installer: &Installer, model: &mut Model) -> Result<(), StepError> {
    // Grab the list of disks. Loopback devices stay hidden unless explicitly
    // requested, which allows safe end-to-end testing against a losetup disk.
    let mut client = installer.disks().await?;
    let exclude_loopback = env::var_os("LICHEN_INCLUDE_LOOPBACK").is_none();
    let disks = client
        .list_disks(ListDisksRequest { exclude_loopback })
        .await?
        .into_inner();
    let renderable_devices = disks
        .disks
        .iter()
        .enumerate()
        .map(|(index, disk)| (index, render_disk(disk), "".to_string()))
        .collect::<Vec<_>>();
    let os_name = info
        .metadata
        .as_ref()
        .and_then(|meta| meta.identity.as_ref())
        .map(|ident| ident.display.clone())
        .unwrap_or("Unknown OS".into());
    let initial_index = disks
        .disks
        .iter()
        .position(|disk| disk.device == model.storage.disk)
        .unwrap_or(0);
    let selected_index = cliclack::select(format!("What disk would you like to install {os_name} on?"))
        .items(&renderable_devices)
        .initial_value(initial_index)
        .interact()
        .map_err(|_| StepError::UserAborted)?;
    let selected_disk = disks.disks.get(selected_index).ok_or(StepError::UserAborted)?;

    tracing::info!("Selected disk: {:?}", selected_disk.device);

    let mut provisioner = installer.provisioner().await?;
    let strategies = provisioner.list_strategies(()).await?.into_inner().strategies;

    // Keep only the strategies that yield at least one plan for the chosen disk
    let mut viable = Vec::new();

    for strategy in &strategies {
        let plans = provisioner
            .try_strategy(TryStrategyRequest {
                strategy: strategy.id.clone(),
                disks: vec![selected_disk.device.clone()],
            })
            .await?
            .into_inner()
            .plans;

        if let Some(plan) = plans.into_iter().next() {
            viable.push((strategy.clone(), plan));
        }
    }

    if viable.is_empty() {
        return Err(StepError::Failed(format!(
            "No partitioning strategy is applicable to {}",
            selected_disk.device
        )));
    }

    // Look for a system-model left by a previous installation on this disk
    let mut install = installer.install().await?;
    let discovered = install
        .discover_system_models(())
        .await?
        .into_inner()
        .models
        .into_iter()
        .find(|model| selected_disk.partitions.iter().any(|part| part.device == model.device));
    let mut display: Vec<usize> = Vec::new();

    viable.iter().enumerate().for_each(|(idx, (strategy, _))| {
        let base = base_strategy_id(&strategy.id);
        if !display.iter().any(|&seen| base_strategy_id(&viable[seen].0.id) == base) {
            display.push(idx);
        }
    });

    let mut items = display
        .iter()
        .enumerate()
        .map(|(pos, &idx)| {
            let (strategy, _) = &viable[idx];
            (pos, base_strategy_id(&strategy.name), strategy.description.clone())
        })
        .collect::<Vec<_>>();
    let refresh_index = viable.len();

    if discovered.is_some() {
        items.push((
            refresh_index,
            "Refresh OS",
            "Reinstall using the settings and package selection found on the disk".to_string(),
        ));
    }

    let initial_choice = if model.imported {
        display
            .iter()
            .position(|&idx| base_strategy_id(&viable[idx].0.id) == base_strategy_id(&model.storage.strategy_id))
            .unwrap_or(0)
    } else if discovered.is_some() {
        refresh_index
    } else {
        0
    };

    let choice = cliclack::select("How should the disk be partitioned?")
        .items(&items)
        .initial_value(initial_choice)
        .interact()
        .map_err(|_| StepError::UserAborted)?;

    if choice == refresh_index
        && let Some(discovered_model) = &discovered
    {
        *model = install_model::from_kdl(&discovered_model.contents).map_err(|e| {
            StepError::Failed(format!(
                "failed to parse model from {}: {}",
                discovered_model.device,
                install_model::parse_error_detail(&e)
            ))
        })?;
        model.imported = true;

        let mut packages: BTreeSet<String> = model.software.packages.iter().cloned().collect();
        packages.extend(selections::mandatory(&model.software.selection)?);
        model.software.packages = packages.into_iter().collect();
    }

    let (strategy, plan) = if choice == refresh_index {
        // Partition with the strategy recorded in the discovered model,
        // falling back to the first viable strategy for this disk
        viable
            .iter()
            .find(|(strategy, _)| strategy.id == model.storage.strategy_id)
            .unwrap_or(&viable[0])
    } else {
        let (base, _) = &viable[display[choice]];
        let chosen_id = select_filesystem(base, &viable, &model.storage.strategy_id)?;

        viable
            .iter()
            .find(|(strategy, _)| strategy.id == chosen_id)
            .ok_or_else(|| StepError::Failed(format!("strategy {chosen_id} disappeared from the viable list")))?
    };

    cliclack::note(
        format!("Planned changes for {}", selected_disk.device),
        render_plan(plan),
    )
    .map_err(|_| StepError::UserAborted)?;

    model.storage.disk = selected_disk.device.clone();
    model.storage.disk_display = render_disk(selected_disk);
    model.storage.strategy_id = strategy.id.clone();
    model.storage.strategy_name = strategy.name.clone();
    model.storage.plan = Some(plan.clone());

    Ok(())
}

/// A strategy id with any filesystem-variant suffix removed
fn base_strategy_id(id: &str) -> &str {
    FILESYSTEM_CHOICES
        .iter()
        .find_map(|(suffix, _, _)| id.strip_suffix(suffix))
        .unwrap_or(id)
}

/// Whether the live media can create this filesystem
fn mkfs_available(filesystem: &str) -> bool {
    let helper = format!("mkfs.{filesystem}");
    ["/usr/sbin", "/usr/bin", "/sbin", "/bin"]
        .iter()
        .any(|dir| Path::new(dir).join(&helper).exists())
}

/// Add the userspace tooling the chosen root filesystem needs.
pub fn ensure_filesystem_packages(model: &mut Model) {
    let Some(filesystem) = FILESYSTEM_CHOICES
        .iter()
        .find(|(suffix, _, _)| model.storage.strategy_id.ends_with(suffix))
        .map(|(_, name, _)| *name)
    else {
        return;
    };

    for package in FILESYSTEM_PACKAGES
        .iter()
        .filter(|(filesystem_name, _)| *filesystem_name == filesystem)
        .flat_map(|(_, packages)| packages.iter().copied())
    {
        if !model.software.packages.iter().any(|have| have == package) {
            model.software.packages.push(package.to_string());
        }
    }

    model.software.packages.sort();
}

/// Ask which root filesystem to use
fn select_filesystem(
    representative: &StrategyDefinition,
    viable: &[(StrategyDefinition, StrategyPlan)],
    recorded_id: &str,
) -> Result<String, StepError> {
    let base = base_strategy_id(&representative.id);
    let variants = FILESYSTEM_CHOICES
        .iter()
        .map(|(suffix, name, hint)| (format!("{base}{suffix}"), *name, *hint))
        .filter(|(id, _, _)| viable.iter().any(|(strategy, _)| &strategy.id == id))
        .collect::<Vec<_>>();

    // Never hide everything: if the probe finds no mkfs helpers at all it is
    // more likely wrong about where they live than right about their absence
    let creatable = variants
        .iter()
        .filter(|(_, name, _)| mkfs_available(name))
        .cloned()
        .collect::<Vec<_>>();

    let available = if creatable.is_empty() { variants } else { creatable };

    // Not a filesystem-variant strategy: nothing to ask
    if available.is_empty() {
        return Ok(representative.id.clone());
    }

    // A strategy without variants has nothing to ask about
    if available.len() == 1 {
        return Ok(available[0].0.clone());
    }

    let init = available.iter().position(|(id, _, _)| id == recorded_id).unwrap_or(0);
    let items = available
        .iter()
        .enumerate()
        .map(|(idx, (_, name, hint))| (idx, name.to_string(), hint.to_string()))
        .collect::<Vec<_>>();
    let picked = cliclack::select("Which filesystem should the root partition use?")
        .items(&items)
        .initial_value(init)
        .interact()
        .map_err(|_| StepError::UserAborted)?;

    Ok(available[picked].0.clone())
}

fn render_disk(disk: &Disk) -> String {
    format!(
        "{} - {} - {}",
        disk.device,
        disk.model.as_ref().unwrap_or(&"Unknown".into()),
        disk.display_size,
    )
}

pub fn render_plan(plan: &StrategyPlan) -> String {
    let mut out = String::new();

    plan.disk_plans
        .iter()
        .for_each(|disk_plan| out.push_str(&format!("{}:\n{}\n", disk_plan.device, disk_plan.description)));
    if !plan.filesystems.is_empty() {
        out.push_str("\nFilesystems:\n");

        plan.filesystems.iter().for_each(|planned_filesystem| {
            if let Some(filesystem) = &planned_filesystem.filesystem {
                out.push_str(&format!(
                    "  {} -> {} ({})\n",
                    planned_filesystem.device,
                    filesystem.filesystem_type,
                    filesystem.label.as_deref().unwrap_or("no label"),
                ));
            }
        });
    }

    if !plan.role_mounts.is_empty() {
        // The @/@home layout is applied backend-side for the btrfs root, so it
        // isn't in the role_mounts; reconstruct it here for the preview.
        let root_device = plan
            .role_mounts
            .iter()
            .find(|role_mount| role_mount.mountpoint == "/")
            .map(|role_mount| role_mount.device.as_str());
        let root_btrfs = root_device.is_some_and(|device| {
            plan.filesystems.iter().any(|plan_fs| {
                plan_fs.device == device
                    && plan_fs
                        .filesystem
                        .as_ref()
                        .is_some_and(|filesystem| filesystem.filesystem_type == "btrfs")
            })
        });
        let has_home = plan
            .role_mounts
            .iter()
            .any(|role_mount| role_mount.mountpoint == "/home");

        out.push_str("\nMounts:\n");
        plan.role_mounts.iter().for_each(|role_mount| {
            if root_btrfs && role_mount.mountpoint == "/" {
                out.push_str(&format!("  / <- {} (subvol=@)\n", role_mount.device));
            } else {
                out.push_str(&format!("  {} <- {}\n", role_mount.mountpoint, role_mount.device));
            }
        });

        if root_btrfs && !has_home
            && let Some(device) = root_device {
                out.push_str(&format!("  /home <- {device} (subvol=@home)\n"));
            }
    }

    out
}

register_step! {
    id: "storage",
    author: "AerynOS Developers",
    description: "Select the disk to install on",
    create: || Box::new(
        CliStep {
            info: DisplayInfo {
                title: "Configure storage".to_string(),
                description: "Select the disk to install on".to_string(),
                icon: Some(Icon::Emoji("💾".to_string())),
            },
            step: FrontendStep::Storage,
        }
    )
}
