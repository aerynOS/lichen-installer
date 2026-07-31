// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Messages into the application loop, and actions back out of a screen.

use protocols::lichen::storage::disks::Disk;
use ratatui::crossterm::event::{self, Event};
use std::{thread, time::Duration};
use tokio::sync::mpsc::UnboundedSender;

/// Anything that can wake the application loop.
///
/// Terminal input and background task results arrive through the same channel,
/// so an RPC completing redraws the UI exactly the way a keypress does.
#[derive(Debug)]
pub enum Msg {
    /// Raw input from the terminal
    Terminal(Event),
    /// A background task failed; surfaced in the error overlay
    Failed(String),
    /// The available disks came back from the backend
    Disks(Vec<Disk>),
}

/// What a screen tells the applicaiton after seeing a key.
///
/// Screens are offered every key first. Anything they return `Ignored` for
/// falls through to the application's own navigation, which is why a text
/// field can Tab on one screen without breaking Tab everywhere else.
pub enum Action {
    /// Not wanted; the applicaiton may use this key
    Ignored,
    /// Handled by the screen
    Consumed,
    /// Advance to the next step
    Next,
    /// Return to the previous step
    Back,
}

/// How long the input thread waits before checking whether the app is gone
const POLL: Duration = Duration::from_millis(100);

/// Read terminal input on a dedicated OS thread.
///
/// crossterm's `read()` blocks, which must never happen on a runtime worker.
/// A plain thread also keeps an async event-stream dependency out of the crate
/// entirely. It exits by itself once the reciever is dropped.
pub fn spawn_input(tx: UnboundedSender<Msg>) {
    thread::spawn(move || {
        while !tx.is_closed() {
            match event::poll(POLL) {
                Ok(true) => match event::read() {
                    Ok(event) => {
                        if tx.send(Msg::Terminal(event)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Failed(format!("terminal read failed: {e}")));
                        break;
                    }
                },
                Ok(false) => {}
                Err(e) => {
                    let _ = tx.send(Msg::Failed(format!("terminal poll failed: {e}")));
                    break;
                }
            }
        }
    });
}
