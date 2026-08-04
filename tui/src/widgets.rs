// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

pub mod browser;
pub mod filter_list;
pub mod form;

// Re-exports
pub use browser::{Browser, Outcome as BrowserOutcome};
pub use filter_list::{Entry, FilterList, Outcome};
pub use form::{Field, Form, Outcome as FormOutcome};
