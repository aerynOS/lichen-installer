// SPDX-FileCopyrightText: Copyright © 2025 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Frontend module for disk selection during installation
//!
//! This module provides the disk selection step of the installation process,
//! allowing users to choose which disk to install AerynOS on, preview the
//! partitioning strategy, and apply it.

use installer::Model;
use std::path::Path;

/// Root filesystem choices as strategy id suffixes, first entry is default
pub const CHOICES: &[(&str, &str, &str)] = &[
    ("_xfs", "xfs", "Recommended for most users"),
    ("_f2fs", "f2fs", "Flash-friendly filesystem"),
    ("_ext4", "ext4", "The traditional Linux filesystem"),
    ("_btrfs", "btrfs", "Copy-on-write with checksumming"),
];

/// Userspace packages the installed system needs for its root filesystem
const FILESYSTEM_PACKAGES: &[(&str, &[&str])] = &[("btrfs", &["btrfs-progs", "udisks-btrfs"])];

/// A strategy id with any filesystem-variant suffix removed
pub fn base(id: &str) -> &str {
    CHOICES
        .iter()
        .find_map(|(suffix, _, _)| id.strip_suffix(suffix))
        .unwrap_or(id)
}

/// Whether the live media can create this filesystem
pub fn mkfs_available(filesystem: &str) -> bool {
    let helper = format!("mkfs.{filesystem}");
    ["/usr/sbin", "/usr/bin", "/sbin", "/bin"]
        .iter()
        .any(|dir| Path::new(dir).join(&helper).exists())
}

/// Add the userspace tooling the chosen root filesystem needs.
pub fn ensure_filesystem_packages(model: &mut Model) {
    let Some(filesystem) = CHOICES
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
