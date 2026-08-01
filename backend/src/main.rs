// SPDX-FileCopyrightText: Copyright © 2025 Serpent OS Developers
// SPDX-FileCopyrightText: Copyright © 2025 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! The disk service backend
//!
//! This service handles disk management operations and provides a gRPC interface
//! for clients to interact with disk devices.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::{env, fs::File};

use backend::auth::{AuthService, uds_interceptor};
use backend::{disk_service, install_service, locales_service, network_service, provisioner_service, system_service};
use color_eyre::eyre::bail;
use nix::libc::geteuid;
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::service::interceptor;
use tonic::transport::Server;

use color_eyre::Result;
pub use protocols::lichen::storage::disks;
use tracing::info;
use tracing_error::ErrorLayer;
use tracing_subscriber::{EnvFilter, Layer, fmt::format::Format, layer::SubscriberExt, util::SubscriberInitExt};

/// Configures color-eyre for enhanced error handling and reporting
///
/// Sets up error hooks with metadata about the environment and package version
fn setup_eyre() {
    console::set_colors_enabled(true);
    color_eyre::config::HookBuilder::default()
        .issue_url(concat!(env!("CARGO_PKG_REPOSITORY"), "/issues/new"))
        .add_issue_metadata("version", env!("CARGO_PKG_VERSION"))
        .add_issue_metadata("os", env::consts::OS)
        .add_issue_metadata("arch", env::consts::ARCH)
        .issue_filter(|_| true)
        .install()
        .unwrap();
}

/// Configures the tracing system for application logging
///
/// Creates a log file at "backend.log" and sets up formatting and filtering options
/// for the tracing subscriber
fn configure_tracing() -> Result<()> {
    let file = File::create("/tmp/lichen-backend.log")?;
    let file_format = Format::default()
        .with_ansi(false)
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .with_file(false)
        .with_line_number(false)
        .with_target(true)
        .with_thread_ids(true);

    let stdout_format = Format::default()
        .with_ansi(true)
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .with_target(true)
        .with_thread_ids(false)
        .with_source_location(false)
        .with_file(false);

    let file_filter = EnvFilter::new("trace");
    let stdout_filter = EnvFilter::new("info");

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .event_format(file_format)
                .with_writer(file)
                .with_filter(file_filter),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .event_format(stdout_format)
                .with_filter(stdout_filter),
        )
        .with(ErrorLayer::default())
        .init();

    Ok(())
}

/// Handles termination signals (SIGTERM and SIGINT)
///
/// Waits for either signal and returns when one is received, triggering
/// graceful shutdown
async fn signal_handler(mut recv: UnboundedReceiver<()>) {
    let mut sigterm = signal(SignalKind::terminate()).unwrap();
    let mut sigint = signal(SignalKind::interrupt()).unwrap();

    tokio::select! {
        _ = sigterm.recv() => {},
        _ = sigint.recv() => {},
        _ = recv.recv() => {},
    };
}

/// Main entry point for the disk service
///
/// Initializes the service, sets up error handling and logging, and starts
/// the gRPC server with the disk service implementation. Handles graceful
/// shutdown on termination signals.
#[tokio::main]
async fn main() -> Result<()> {
    setup_eyre();

    // Ensure we're euid 0
    let euid = unsafe { geteuid() };
    match euid {
        0 => (),
        _ => bail!("This service must be run as root"),
    }

    configure_tracing()?;

    // Remove the old socket if it exists
    let _ = std::fs::remove_file("/run/lichen.sock");

    let listener = UnixListener::bind("/run/lichen.sock")?;
    // Make it writable by everyone
    let _ = std::fs::set_permissions("/run/lichen.sock", std::fs::Permissions::from_mode(0o666));

    let uds_stream = UnixListenerStream::new(listener);
    let (send, recv) = unbounded_channel();

    let auth = Arc::new(AuthService::new());

    info!("🚀 Serving on /run/lichen.sock");

    Server::builder()
        .layer(interceptor(uds_interceptor))
        .add_service(disk_service::service(auth.clone()))
        .add_service(locales_service::service(auth.clone()).await?)
        .add_service(system_service::service(auth.clone(), send))
        .add_service(network_service::service(auth.clone()))
        .add_service(provisioner_service::service(auth.clone()).await?)
        .add_service(install_service::service(auth.clone()))
        .serve_with_incoming_shutdown(uds_stream, signal_handler(recv))
        .await?;

    info!("🛑 Shutting down");

    Ok(())
}
