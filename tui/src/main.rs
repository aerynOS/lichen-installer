// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Terminal user interface for the Lichen installer

use color_eyre::{Result, config::HookBuilder};
use ratatui::{crossterm::event, widgets::Paragraph};
use std::{fs::File, panic};
use tracing_subscriber::{
    EnvFilter, Layer,
    fmt::{self, format::Format, time::uptime},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

/// File only logging: the alternate screen owns the terminal, so nothing may
/// print to stdout or stderr while the TUI is running.
fn configure_tracing() -> Result<()> {
    let file = File::create("installer-tui.log")?;
    let format = Format::default()
        .with_ansi(false)
        .with_timer(uptime())
        .with_target(true);

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .event_format(format)
                .with_writer(file)
                .with_filter(EnvFilter::new("trace")),
        )
        .init();

    Ok(())
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
    configure_tracing()?;

    let mut terminal = ratatui::init();
    terminal.draw(|frame| {
        frame.render_widget(Paragraph::new("lichen tui - press any key"), frame.area());
    })?;

    let _ = event::read()?;

    ratatui::restore();
    Ok(())
}
