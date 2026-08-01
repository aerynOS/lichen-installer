// SPDX-FileCopyrightText: Copyright © 2025 Serpent OS Developers
// SPDX-FileCopyrightText: Copyright © 2025 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

// tonic::Status is ~176 bytes and every service method must return
// Result<Response<T>, Status> to satisfy the generated traits, so there is no
// Err variant that can be shrank
#![allow(clippy::result_large_err)]
mod builtin_strategies;

pub mod auth;
pub mod disk_service;
pub mod install_service;
pub mod locales_service;
pub mod network_service;
pub mod plans;
pub mod provisioner_service;
pub mod system_service;

pub use lichen_macros::authorized;
