// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Network configuration for the live system, over NetworkManager.

use crate::auth::AuthService;
use protocols::lichen::network::{
    AccessPoint, ConnectWifiRequest, ConnectWifiResponse, Device, NetworkStatus, ScanWifiResponse,
    network_server::{Network, NetworkServer},
};
use std::{cmp::Reverse, sync::Arc};
use tokio::{
    process::Command,
    time::{Duration, sleep},
};
use tonic::{Request, Response, Status};
use tracing::info;

pub struct Service {
    _auth: Arc<AuthService>,
}

/// Creates a new gRPC server instance using the default Service implementation
pub fn service(auth: Arc<AuthService>) -> NetworkServer<Service> {
    NetworkServer::new(Service { _auth: auth })
}

/// Run nmcli and return its stdout.
///
/// A non-zero exit carries nmcli's own stderr as it's already a very
/// good message to the user.
async fn nmcli(args: &[&str]) -> Result<String, Status> {
    run("nmcli", args).await
}

async fn iwctl(args: &[&str]) -> Result<String, Status> {
    run("iwctl", args).await
}

async fn run(program: &str, args: &[&str]) -> Result<String, Status> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|e| Status::unavailable(format!("could not run {program}: {e}")))?;

    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr).trim().to_string();

        return Err(Status::failed_precondition(if reason.is_empty() {
            format!("{program} {} failed", args.join(" "))
        } else {
            reason
        }));
    }

    String::from_utf8(output.stdout).map_err(|e| Status::internal(format!("{program} output was no utf-8: {e}")))
}

/// Split one terse (-t) nmcli record.
///
/// Values escape ':' as "\:" and '\' as "\\", so an SSID containing a colon
/// survives the round trip. Splitting natively would corrupt it.
pub(crate) fn split_terse(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut escaped = false;

    for character in line.chars() {
        if escaped {
            fields
                .last_mut()
                .expect("there is always a current field")
                .push(character);
            escaped = false;
            continue;
        }

        match character {
            '\\' => escaped = true,
            ':' => fields.push(String::new()),
            _ => fields
                .last_mut()
                .expect("there is always a current field")
                .push(character),
        }
    }
    fields
}

fn field(fields: &[String], index: usize) -> String {
    fields.get(index).cloned().unwrap_or_default()
}

#[tonic::async_trait]
impl Network for Service {
    /// Current link and connectivity state
    async fn status(&self, _request: Request<()>) -> Result<Response<NetworkStatus>, Status> {
        let general = nmcli(&["-t", "-f", "STATE,CONNECTIVITY", "general"]).await?;
        let connectivity = field(&split_terse(general.lines().next().unwrap_or_default()), 1);
        let listing = nmcli(&["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "device"]).await?;
        let devices: Vec<Device> = listing
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| {
                let fields = split_terse(line);

                Device {
                    name: field(&fields, 0),
                    kind: field(&fields, 1),
                    state: field(&fields, 2),
                    connection: Some(field(&fields, 3)).filter(|value| !value.is_empty()),
                }
            })
            .filter(|device| !matches!(device.kind.as_str(), "loopback" | "wifi-p2p"))
            .collect();
        let wifi_available = devices.iter().any(|device| device.kind == "wifi");

        Ok(Response::new(NetworkStatus {
            online: connectivity == "full",
            connectivity,
            devices,
            wifi_available,
        }))
    }

    /// Rescan and list visible access points, strongest first
    async fn scan_wifi(&self, _request: Request<()>) -> Result<Response<ScanWifiResponse>, Status> {
        let listing = nmcli(&[
            "-t",
            "-f",
            "IN-USE,SSID,SIGNAL,SECURITY",
            "device",
            "wifi",
            "list",
            "--rescan",
            "yes",
        ])
        .await?;
        let mut access_points: Vec<AccessPoint> = Vec::new();

        for line in listing.lines().filter(|line| !line.is_empty()) {
            let fields = split_terse(line);
            let ssid = field(&fields, 1);

            // A hidden network reports an empty SSID. It is reachable only by
            // typing the name, so a blank ssid is skipped.
            if ssid.is_empty() {
                continue;
            }

            let point = AccessPoint {
                ssid,
                signal: field(&fields, 2).parse().unwrap_or(0),
                security: field(&fields, 3),
                in_use: field(&fields, 0).trim() == "*",
            };

            // nmcli emits one row per BSSID; a mesh shows the same SSID several
            // times. Keep the strongest of each.
            match access_points.iter().position(|seen| seen.ssid == point.ssid) {
                Some(index) => {
                    if access_points[index].signal < point.signal {
                        access_points[index] = point;
                    }
                }
                None => access_points.push(point),
            }
        }

        access_points.sort_by_key(|b| Reverse(b.signal));

        Ok(Response::new(ScanWifiResponse { access_points }))
    }

    async fn connect_wifi(
        &self,
        request: Request<ConnectWifiRequest>,
    ) -> Result<Response<ConnectWifiResponse>, Status> {
        let request = request.into_inner();

        if request.ssid.is_empty() {
            return Err(Status::invalid_argument("an SSID is required"));
        }

        if request.hidden {
            connect_hidden(&request).await?;
        } else {
            connect_visible(&request).await?;
        }
        info!(ssid = %request.ssid, "Connected to access point");
        Ok(Response::new(ConnectWifiResponse { profile: request.ssid }))
    }
}

// Helpers

/// Connect to a visible network.
async fn connect_visible(request: &ConnectWifiRequest) -> Result<(), Status> {
    // The PSK is visible in this process's argv while nmcli runs. On a
    // single user live image that is acceptable; if this service ever runs
    // somewhere multi-user an update to write a keyfile will be required.
    let mut args = vec![
        "device".to_string(),
        "wifi".to_string(),
        "connect".to_string(),
        request.ssid.clone(),
    ];

    if let Some(psk) = request.psk.as_ref().filter(|psk| !psk.is_empty()) {
        args.push("password".to_string());
        args.push(psk.clone());
    }

    if let Some(device) = request.device.as_ref().filter(|device| !device.is_empty()) {
        args.push("ifname".to_string());
        args.push(device.clone());
    }

    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();

    nmcli(&borrowed).await.map(|_| ())
}

/// Connect to a hidden network
async fn connect_hidden(request: &ConnectWifiRequest) -> Result<(), Status> {
    // Delete any failed earlier attempts
    let _ = nmcli(&["connection", "delete", &request.ssid]).await;
    let station = request
        .device
        .as_deref()
        .filter(|device| !device.is_empty())
        .ok_or_else(|| Status::failed_precondition("no wireless device to connect with"))?;
    let mut args = Vec::new();

    // The passphrase is visible in argv while iwctl runs, exactly as it is for
    // nmcli on the visible path
    if let Some(psk) = request.psk.as_deref().filter(|psk| !psk.is_empty()) {
        args.push("--passphrase");
        args.push(psk);
    }

    args.extend(["station", station, "connect-hidden", &request.ssid]);

    iwctl(&args).await?;
    await_connection(&request.ssid, station).await
}

/// Wait for NetworkManager to report the wireless device on `ssid`.
async fn await_connection(ssid: &str, device: &str) -> Result<(), Status> {
    for _ in 0..30 {
        let listing = nmcli(&["-t", "-f", "DEVICE,STATE,CONNECTION", "device"]).await?;
        let connected = listing
            .lines()
            .map(split_terse)
            .any(|fields| field(&fields, 0) == device && field(&fields, 1) == "connected" && field(&fields, 2) == ssid);

        if connected {
            return Ok(());
        }

        sleep(Duration::from_millis(500)).await;
    }
    Err(Status::deadline_exceeded(format!("timed out connecting to {ssid}")))
}
