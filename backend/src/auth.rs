// SPDX-FileCopyrightText: Copyright © 2025 Serpent OS Developers
// SPDX-FileCopyrightText: Copyright © 2025 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

use nix::libc::gid_t;
use std::process::Command;
use tokio::net::unix::{pid_t, uid_t};
use tonic::{Request, Status, transport::server::UdsConnectInfo};
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub enum AuthInfo {
    /// Unix domain socket
    Unix { uid: uid_t, gid: gid_t, pid: Option<pid_t> },
}

/// Intercept to install our own specific helper type for PEERCRED
pub fn uds_interceptor(mut request: Request<()>) -> Result<Request<()>, Status> {
    let uds_creds = request.extensions().get::<UdsConnectInfo>();

    if let Some(peer_creds) = uds_creds.as_ref().and_then(|u| u.peer_cred) {
        let auth = AuthInfo::Unix {
            uid: peer_creds.uid(),
            gid: peer_creds.gid(),
            pid: peer_creds.pid(),
        };
        request.extensions_mut().insert(auth);
        Ok(request)
    } else {
        Err(Status::unauthenticated(
            "client socket did not share SO_PEERCRED, refusing connection",
        ))
    }
}

/// Check authorization for an action using pkcheck
fn check_authorization(action_id: &str, uid: u32, pid: u32) -> Result<bool, Status> {
    let output = Command::new("pkcheck")
        .args([
            "--action-id",
            action_id,
            "--process",
            &format!("{pid},{uid}"),
            "--allow-user-interaction",
        ])
        .output()
        .map_err(|err| Status::internal(format!("pkcheck failed to spawn: {err}")))?;

    if output.status.success() {
        Ok(true)
    } else {
        // pkcheck returns non-zero of denied/failed
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(action_id, uid, pid, "polkit denied: {}", stderr.trim());
        Ok(false)
    }
}

#[derive(Clone, Debug)]
pub struct AuthService {}

impl Default for AuthService {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthService {
    pub fn new() -> Self {
        Self {}
    }

    /// Verify the incoming request against polkit authorization
    pub async fn verify_request<T>(&self, request: Request<T>, action_id: &'static str) -> Result<Request<T>, Status> {
        let info = request.extensions().get::<AuthInfo>();
        match info {
            Some(AuthInfo::Unix { uid, gid: _, pid }) => {
                let pid = pid.unwrap_or(0) as u32;
                let uid = *uid as u32;

                // Check polkit authorization
                if !check_authorization(action_id, uid, pid)? {
                    return Err(Status::permission_denied(format!(
                        "not authorized for action: {}",
                        action_id
                    )));
                }

                info!(action_id, uid, pid, "autorized");
                Ok(request)
            }
            None => Err(Status::unauthenticated("client socket unsupported")),
        }
    }
}
