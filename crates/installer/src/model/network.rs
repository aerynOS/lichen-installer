// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Network settings to carry onto the installed system
//!
//! Deliberately holds no credentials. NetworkManager writes a keyfile on the
//! live system when the connection is made; the installer copies that file
//! rather than storing a key of its own.

#[derive(Debug, Default)]
pub struct Model {
    /// NetworkManager profile to copy onto the installed system
    pub profile: Option<String>,
}
