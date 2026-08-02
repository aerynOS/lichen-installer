// SPDX-FileCopyrightText: Copyright © 2025 Serpent OS Developers
// SPDX-FileCopyrightText: Copyright © 2025 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

mod accounts;
mod network;
mod region;
mod software;
mod storage;

pub use accounts::User;

/// Installation settings
///
/// The installer's in-memory state. Projected into all model output documents:
/// `software.packages` becomes the system-model, and the whole struct becomes
/// the install-model record. Neither document is the source of truth for it.
#[derive(Debug, Default)]
pub struct Model {
    /// Region specific installation settings
    pub region: region::Model,
    /// Storage and partitioning selections
    pub storage: storage::Model,
    /// Account selections
    pub accounts: accounts::Model,
    /// Software selections
    pub software: software::Model,
    /// Network settings carried onto the isntalled system
    pub network: network::Model,
    /// Set when the model came from an OS refresh or an imported document.
    pub imported: bool,
}
