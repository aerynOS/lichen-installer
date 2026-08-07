// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

/// A user account to create on the installed system
#[derive(Clone, Debug)]
pub struct User {
    /// Login name
    pub username: String,
    /// Real name, stored in the GECOS field
    pub real_name: String,
    /// yescrypt password hash
    pub password_hash: String,
}

/// Account installation settings
///
/// Passwords are only ever carried as crypt(3) hashes; plaintext must never
/// be stored in the model.
#[derive(Debug, Default)]
pub struct Model {
    /// yescrypt hash of the root password
    pub root_password_hash: Option<String>,
    /// The primary admin account
    pub user: Option<User>,
}
