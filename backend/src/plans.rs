// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Translation between disks-rs provisioning plans and the gRPC protocol,
//! plus the destructive application of a plan to real disks.
//! `provisioning::Plan` borrows the strategies and block devices it was
//! built from, so it can never be stored or cross an await point. Every
//! function here runs synchronously and returns owned protobuf messages.

use disks::BlockDevice;
use partitioning::{
    Formatter, GptAttributes, PartitionAttributes, gpt::partition_types::OperatingSystem, planner::Change,
    writer::DiskWriter,
};
use protocols::lichen::storage::{
    provisioner::{DiskPlan, PlannedChange, PlannedFilesystem, RoleMount, StrategyPlan, planned_change},
    types::{self, operating_system::Kind},
};
use provisioning::{Filesystem, PartitionRole, Plan, Provisioner, StrategyDefinition};
use std::collections::HashMap;
use tonic::Status;
use tracing::info;

/// Compute all viable plans for the named strategy against the given devices.
pub(crate) fn try_strategy(
    strategies: &HashMap<String, StrategyDefinition>,
    name: &str,
    devices: &[BlockDevice],
) -> Vec<StrategyPlan> {
    let mut provisioner = Provisioner::new();

    strategies.values().for_each(|strategy| {
        provisioner.add_strategy(strategy);
    });

    devices.iter().for_each(|dev| {
        provisioner.push_device(dev);
    });

    provisioner
        .plan()
        .iter()
        .filter(|plan| plan.strategy.name == name)
        .map(plan_to_proto)
        .collect()
}

/// DESTRUCTIVE: re-plan the named strategy and apply it to the devices.
///
/// Refuses to act unless exactly one plan matches. All disks are simulated
/// before any disk is written. Then: partition tables are written -> synced
/// with kernel -> filesystems are created.
pub(crate) fn apply_strategy(
    strategies: &HashMap<String, StrategyDefinition>,
    name: &str,
    devices: &[BlockDevice],
) -> Result<StrategyPlan, Status> {
    let mut provisioner = Provisioner::new();

    strategies.values().for_each(|strategy| {
        provisioner.add_strategy(strategy);
    });
    devices.iter().for_each(|dev| {
        provisioner.push_device(dev);
    });

    let all_plans = provisioner.plan();
    let mut matching = all_plans.iter().filter(|plan| plan.strategy.name == name);
    let plan = matching.next().ok_or_else(|| {
        Status::failed_precondition(format!("strategy `{name}` is not applicable to the provided disks"))
    })?;

    if matching.next().is_some() {
        return Err(Status::failed_precondition(format!(
            "strategy `{name}` produced multiple candidate plans; refusing to apply ambiguously"
        )));
    }

    // Validate every disk before mutating any of them: failing on the second
    // disk of a multi-disk plan would leave the first one already wiped and
    // the user with no installed system and no way back
    for (disk, device_plan) in &plan.device_assignments {
        DiskWriter::new(device_plan.device, &device_plan.planner)
            .simulate()
            .map_err(|err| Status::internal(format!("simulation failed for {disk}: {err}")))?;
    }

    // Validate every disk before mutating any of them: failing on the second
    // disk of a multi-disk plan would leave the first one already wiped and
    // the user with no installed system and no way back.
    for (disk, device_plan) in &plan.device_assignments {
        info!(
            disk = %disk,
            device = %device_plan.device.device().display(),
            "writing partition table"
        );
        DiskWriter::new(device_plan.device, &device_plan.planner)
            .write()
            .map_err(|err| Status::internal(format!("failed to partition {disk}: {err}")))?;
    }

    for (device, filesystem) in &plan.filesystems {
        info!(device = %device.display(), filesystem = ?filesystem, "creating filesystem");

        // DiskWriter zeroes only the first 2MiB of a new partition. btrfs and
        // xfs keep superblock past that, and an unforced mkfs refuses to run
        // when it finds one, aborting after the disk has already been repartitioned.
        // Forcing is safe here only because the plan is user-confirmed before apply_strategy
        // is reached.
        let output = Formatter::new(filesystem.clone())
            .force()
            .format(device)
            .output()
            .map_err(|err| Status::internal(format!("failed to run mkfs for {}: {err}", device.display())))?;

        if !output.status.success() {
            return Err(Status::internal(format!(
                "mkfs failed for {}: {}",
                device.display(),
                String::from_utf8_lossy(&output.stderr)
            )));
        }
    }

    Ok(plan_to_proto(plan))
}

fn plan_to_proto(plan: &Plan<'_>) -> StrategyPlan {
    let mut disk_plans = plan
        .device_assignments
        .values()
        .map(|device_plan| DiskPlan {
            device: device_plan.device.device().display().to_string(),
            changes: device_plan
                .planner
                .changes()
                .iter()
                .map(|change| change_to_proto(change, device_plan.device.size()))
                .collect(),
            description: device_plan.planner.describe_changes(),
        })
        .collect::<Vec<_>>();
    disk_plans.sort_by(|a, b| a.device.cmp(&b.device));

    let mut filesystems = plan
        .filesystems
        .iter()
        .map(|(device, filesystem)| PlannedFilesystem {
            device: device.display().to_string(),
            filesystem: Some(filesystem_to_proto(filesystem)),
        })
        .collect::<Vec<_>>();
    filesystems.sort_by(|a, b| a.device.cmp(&b.device));

    let mut role_mounts = plan
        .role_mounts
        .iter()
        .map(|(role, device)| RoleMount {
            role: role_to_proto(role) as i32,
            device: device.display().to_string(),
            mountpoint: role.as_path().to_string(),
        })
        .collect::<Vec<_>>();
    role_mounts.sort_by(|a, b| a.mountpoint.cmp(&b.mountpoint));

    StrategyPlan {
        disk_plans,
        filesystems,
        role_mounts,
    }
}

fn change_to_proto(change: &Change, disk_size: u64) -> PlannedChange {
    let converted = match change {
        Change::AddPartition {
            start,
            end,
            partition_id,
            attributes,
        } => planned_change::Change::AddPartition(types::AddPartitionChange {
            start: *start,
            end: *end,
            partition_id: *partition_id,
            attributes: attributes.as_ref().map(attributes_to_proto),
        }),
        Change::DeletePartition {
            original_index,
            partition_id,
        } => planned_change::Change::DeletePartition(types::DeletePartitionChange {
            original_index: *original_index as u32,
            partition_id: *partition_id,
        }),
    };

    PlannedChange {
        description: change.describe(disk_size),
        change: Some(converted),
    }
}

fn attributes_to_proto(attributes: &PartitionAttributes) -> types::PartitionAttributes {
    types::PartitionAttributes {
        table_attributes: attributes
            .table
            .as_gpt()
            .map(|gpt| types::partition_attributes::TableAttributes::GptTableAttributes(gpt_to_proto(gpt))),
        role: attributes.role.as_ref().map(|role| role_to_proto(role) as i32),
        // disks-rs has no custom-role concept at this revision
        custom_role: None,
        filesystem: attributes.filesystem.as_ref().map(filesystem_to_proto),
    }
}

fn gpt_to_proto(gpt: &GptAttributes) -> types::GptTableAttributes {
    types::GptTableAttributes {
        r#type: Some(types::GptPartitionType {
            os: Some(os_to_proto(&gpt.type_guid.os)),
            uuid: Some(types::Uuid {
                uuid: gpt.type_guid.guid.to_string(),
            }),
        }),
        name: gpt.name.clone(),
        uuid: gpt.uuid.map(|uuid| types::Uuid { uuid: uuid.to_string() }),
    }
}

fn os_to_proto(os: &OperatingSystem) -> types::OperatingSystem {
    let (kind, custom_os) = match os {
        OperatingSystem::None => (Kind::OsNone, None),
        OperatingSystem::Android => (Kind::OsAndroid, None),
        OperatingSystem::Atari => (Kind::OsAtari, None),
        OperatingSystem::Ceph => (Kind::OsCeph, None),
        OperatingSystem::Chrome => (Kind::OsChrome, None),
        OperatingSystem::CoreOs => (Kind::OsCoreOs, None),
        OperatingSystem::Custom(name) => (Kind::OsNone, Some(name.clone())),
        OperatingSystem::FreeBsd => (Kind::OsFreeBsd, None),
        OperatingSystem::FreeDesktop => (Kind::OsFreeDesktop, None),
        OperatingSystem::Haiku => (Kind::OsHaiku, None),
        OperatingSystem::HpUnix => (Kind::OsHpUnix, None),
        OperatingSystem::Linux => (Kind::OsLinux, None),
        OperatingSystem::MidnightBsd => (Kind::OsMidnightBsd, None),
        OperatingSystem::MacOs => (Kind::OsMacOs, None),
        OperatingSystem::NetBsd => (Kind::OsNetBsd, None),
        OperatingSystem::Onie => (Kind::OsOnie, None),
        OperatingSystem::OpenBsd => (Kind::OsOpenBsd, None),
        OperatingSystem::Plan9 => (Kind::OsPlan9, None),
        OperatingSystem::PowerPc => (Kind::OsPowerPc, None),
        OperatingSystem::Solaris => (Kind::OsSolaris, None),
        OperatingSystem::VmWare => (Kind::OsVmWare, None),
        OperatingSystem::Windows => (Kind::OsWindows, None),
        OperatingSystem::QNX => (Kind::OsQnx, None),
        OperatingSystem::DragonFlyBsd => (Kind::OsDragonflyBsd, None),
    };

    types::OperatingSystem {
        kind: kind as i32,
        custom_os,
    }
}

fn filesystem_to_proto(filesystem: &Filesystem) -> types::Filesystem {
    match filesystem {
        Filesystem::Fat32 { label, volume_id } => types::Filesystem {
            filesystem_type: "fat32".to_string(),
            label: label.clone(),
            uuid: volume_id.map(|id| id.to_string()),
        },
        Filesystem::Standard {
            filesystem_type,
            label,
            uuid,
        } => types::Filesystem {
            filesystem_type: filesystem_type.to_string(),
            label: label.clone(),
            uuid: uuid.clone(),
        },
    }
}

fn role_to_proto(role: &PartitionRole) -> types::PartitionRole {
    match role {
        PartitionRole::Boot => types::PartitionRole::Boot,
        PartitionRole::ExtendedBoot => types::PartitionRole::ExtendedBoot,
        PartitionRole::Root => types::PartitionRole::Root,
        PartitionRole::Home => types::PartitionRole::Home,
        PartitionRole::Swap => types::PartitionRole::Swap,
    }
}
