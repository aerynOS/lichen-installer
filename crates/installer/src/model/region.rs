// SPDX-FileCopyrightText: Copyright © 2025 Serpent OS Developers
// SPDX-FileCopyrightText: Copyright © 2025 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

/// Region specific installation settings
#[derive(Debug)]
pub struct Model {
    /// System language (i.e. "en_US")
    pub language: String,
    /// System timezone (i.e. "Europe/London")
    pub timezone: String,
    /// X11 keyboard layout (i.e. "gb")
    pub layout: String,
    /// Console keymap the layout maps to (i.e. "uk"). Empty for the 57 of 99
    /// layouts systemd has no console equivalent for; the console then falls
    /// back to `us` while the graphical session still gets the right layout.
    pub keymap: String,
}

impl Default for Model {
    fn default() -> Self {
        Self {
            language: String::from("en_US.UTF-8"),
            timezone: String::from("UTC"),
            layout: String::from("us"),
            keymap: String::from("us"),
        }
    }
}
