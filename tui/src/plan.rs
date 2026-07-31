// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Turning a strategy plan into something reviewable.
//!
//! The CLI's render_plan produces a flat string; this produces styled lines,
//! so the preview can carry emphasis.

use crate::theme::*;
use protocols::lichen::storage::provisioner::StrategyPlan;
use ratatui::text::Line;

/// What will change, what will be formatted, and while will be mounted where
pub fn describe(plan: &StrategyPlan) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    plan.disk_plans.iter().for_each(|disk_plan| {
        lines.push(Line::styled(disk_plan.device.clone(), HEADING));

        disk_plan.description.lines().for_each(|change| {
            lines.push(Line::styled(format!("  {change}"), BODY));
        });

        lines.push(Line::raw(""));
    });

    if !plan.filesystems.is_empty() {
        lines.push(Line::styled("Filesystems", HEADING));

        plan.filesystems.iter().for_each(|planned| {
            let Some(filesystem) = &planned.filesystem else {
                return;
            };

            lines.push(Line::styled(
                format!(
                    "  {} → {} ({})",
                    planned.device,
                    filesystem.filesystem_type,
                    filesystem.label.as_deref().unwrap_or("no label"),
                ),
                BODY,
            ));
        });

        lines.push(Line::raw(""));
    }

    lines.extend(mounts(plan));
    lines
}

/// The mount table.
///
/// The @/@home subvolume layout is applied backend-side for a btrfs root and
/// so never appears in role_mounts. Reconstruct it, or the preview quietyly
/// lies about where /home ends up.
fn mounts(plan: &StrategyPlan) -> Vec<Line<'static>> {
    if plan.role_mounts.is_empty() {
        return Vec::new();
    }

    let root_device = plan
        .role_mounts
        .iter()
        .find(|mount| mount.mountpoint == "/")
        .map(|mount| mount.device.as_str());
    let root_btrfs = root_device.is_some_and(|device| {
        plan.filesystems.iter().any(|planned| {
            planned.device == device
                && planned
                    .filesystem
                    .as_ref()
                    .is_some_and(|filesystem| filesystem.filesystem_type == "btrfs")
        })
    });
    let has_home = plan.role_mounts.iter().any(|mount| mount.mountpoint == "/home");
    let mut lines = vec![Line::styled("Mounts", HEADING)];

    plan.role_mounts.iter().for_each(|mount| {
        let text = if root_btrfs && mount.mountpoint == "/" {
            format!("  / ← {} (subvolume=@)", mount.device)
        } else {
            format!("  {} ← {}", mount.mountpoint, mount.device)
        };

        lines.push(Line::styled(text, BODY));
    });

    if root_btrfs
        && !has_home
        && let Some(device) = root_device
    {
        lines.push(Line::styled(format!("  /home ← {device} (subvolume=@home"), BODY));
    }

    lines
}
