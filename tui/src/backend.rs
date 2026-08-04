// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Making sure the privileged backend is up before the interface starts.

use color_eyre::{
    Result,
    eyre::{bail, eyre},
};
use nix::unistd::Uid;
use protocols::lichen::system::system_client::SystemClient;
use std::{
    env,
    fs::File,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    process::{Child, Command},
    time,
};
use tonic::transport::Channel;

/// The privileged half of the installer
const BACKEND: &str = "lichen_backend";
/// Where backend's raw stderr is parked while the TUI owns the screen
const BACKEND_STDERR: &str = "/tmp/lichen-backend.stderr";

/// Generous, because pkexec may be waiting on a typed password. A backend
/// that dies is detected immediately regardless, so this is not a stall.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const RETRY_DELAY: Duration = Duration::from_millis(100);

/// A backend this process started and is responsible for stopping.
pub struct Spawned(Child);

/// Connect to the backend, starting it first if nothing is listening.
///
/// Returns the channel, and a handle only when starting the backend
/// from the TUI; one that was already started belongs to whoever started it.
pub async fn connect(socket: &str) -> Result<(Channel, Option<Spawned>)> {
    if let Ok(channel) = protocols::unix_channel(socket).await {
        return Ok((channel, None));
    }

    let program = locate()?;
    println!("Starting {}...", program.display());

    let mut child = spawn(&program)?;
    let channel = wait_for_socket(socket, &mut child).await?;

    Ok((channel, Some(Spawned(child))))
}

/// An explicit override first, then alongside this binary, which covers both
/// the dev tree and an installed /usr/bin, then $PATH. Always absolute:
/// pkexec is given a full path or nothing.
fn locate() -> Result<PathBuf> {
    if let Some(path) = env::var_os("LICHEN_BACKEND") {
        let path = PathBuf::from(path);

        return path
            .canonicalize()
            .map_err(|e| eyre!("LICHEN_BACKEND={} cannot be used: {e}", path.display()));
    }

    if let Ok(exe) = env::current_exe()
        && let Some(sibling) = exe.parent().map(|dir| dir.join(BACKEND))
        && sibling.is_file()
    {
        return Ok(sibling);
    }

    if let Some(paths) = env::var_os("PATH")
        && let Some(found) = env::split_paths(&paths)
            .map(|dir| dir.join(BACKEND))
            .find(|candidate| candidate.is_file())
    {
        return Ok(found);
    }

    bail!("could not find {BACKEND}; set LICHEN_BACKEND to its path")
}

fn spawn(program: &Path) -> Result<Child> {
    let mut command = if Uid::effective().is_root() {
        Command::new(program)
    } else {
        // The backend refuses to run as anything but root. pkexec is the
        // supported escalation path.
        let mut command = Command::new("pkexec");

        command.arg(program);
        command
    };

    // The backend logs to stdout as well as /tmp/lichen-backend.log, and
    // inheriting that would nuke the interface once the TUI takes the
    // screen.
    //
    // stderr has to go the same way, and not because of the backend itself:
    // disks-rs prints straight to stderr from BlockDevice::discover and from
    // partitioning's writer, so probing partitions or applying a strategy
    // paints hundres of lines over the alternate screen. ratatui diffs against
    // what it drew, so cells someone else overwrote a never repainted until a
    // window resize forces a full redraw. pkexec is unaffected: its text agent talks
    // to /dev/tty rather than stderr, and the prompt happens before ratatui::init
    command.stdout(Stdio::null());
    match File::create(BACKEND_STDERR) {
        Ok(file) => command.stderr(Stdio::from(file)),
        // Losing that output is survivable; painting over the interface is not.
        Err(_) => command.stderr(Stdio::null()),
    };

    command
        .spawn()
        .map_err(|e| eyre!("failed to start {}: {e}", program.display()))
}

/// Poll until the socket answers. A backend that exits due to refused auth,
/// already-bound socket, or missing privs is reported right away instead
/// of after the full timeout.
async fn wait_for_socket(socket: &str, child: &mut Child) -> Result<Channel> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;

    loop {
        if let Some(status) = child.try_wait()? {
            bail!("{BACKEND} exited before it began service ({status}); see /tmp/lichen-backend.log");
        }

        if let Ok(channel) = protocols::unix_channel(socket).await {
            return Ok(channel);
        }

        if Instant::now() >= deadline {
            bail!("{BACKEND} did not start listening on {socket}; see /tmp/lichen-backend.log");
        }

        time::sleep(RETRY_DELAY).await;
    }
}

impl Spawned {
    /// Stop the backend the TUI started, leaving any other alone.
    ///
    /// The RPC is what acutally works: under pkexec the child held is the
    /// pkexec process, and an unprivileged signal to a root process goes
    /// nowhere. Killing is the fallback for when RPC cannot be delivered.
    pub async fn stop(mut self, channel: Channel) {
        if SystemClient::new(channel).shutdown(()).await.is_err() {
            let _ = self.0.start_kill();
        }

        let _ = self.0.wait().await;
    }
}
