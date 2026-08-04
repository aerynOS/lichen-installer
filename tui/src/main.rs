// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Terminal user interface for the Lichen installer

mod app;
mod backend;
mod events;
mod filesystems;
mod install_model;
mod plan;
mod screens;
mod selections;
mod theme;
mod widgets;

use color_eyre::{Result, config::HookBuilder};
use protocols::lichen::system::system_client::SystemClient;
use std::{env, fs::File, panic, path::PathBuf};
use tracing_subscriber::{
    EnvFilter, Layer,
    fmt::{self, format::Format, time::uptime},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

use crate::app::App;

const SOCKET: &str = "/run/lichen.sock";
const LOG_PATH: &str = "/tmp/lichen-tui.log";

/// File only logging: the alternate screen owns the terminal, so nothing may
/// print to stdout or stderr while the TUI is running.
fn configure_tracing() -> Option<PathBuf> {
    let log_path = PathBuf::from(env::var("LICHEN_TUI_LOG").unwrap_or_else(|_| LOG_PATH.to_string()));
    let file = File::create(&log_path).ok()?;
    let format = Format::default()
        .with_ansi(false)
        .with_timer(uptime())
        .with_target(true);

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .event_format(format)
                .with_writer(file)
                .with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("trace"))),
        )
        .init();

    Some(log_path)
}

/// Restore the terminal before anything is reported, so a panic never leaves
/// the installer's console unusable.
fn install_hooks() -> Result<()> {
    let (panic_hook, eyre_hook) = HookBuilder::default().into_hooks();

    eyre_hook.install()?;

    let panic_hook = panic_hook.into_panic_hook();

    panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        panic_hook(info);
    }));

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    install_hooks()?;
    let tui_log = configure_tracing();

    // Connect before entering the alternate screen. If the backend is not
    // running, start it and wait.
    let (channel, spawned) = backend::connect(SOCKET).await?;
    let info = SystemClient::new(channel.clone()).get_os_info(()).await?.into_inner();
    let terminal = ratatui::init();
    let result = App::new(channel.clone(), &info).run(terminal).await;

    // Restore first, report second.
    ratatui::restore();
    if let Some(spawned) = spawned {
        spawned.stop(channel).await;
    }

    match &tui_log {
        Some(path) => println!("Installer log: {}", path.display()),
        None => eprintln!("Installer log: could not be opened"),
    }
    result
}
