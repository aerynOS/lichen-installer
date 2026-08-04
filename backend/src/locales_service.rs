// SPDX-FileCopyrightText: Copyright © 2025 Serpent OS Developers
// SPDX-FileCopyrightText: Copyright © 2025 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

use crate::auth::AuthService;
use locales_rs::Registry;
use protocols::lichen::locales::{
    GetKeymapsRequest, GetLocaleRequest, Keymap, ListKeymapsResponse, ListLocalesResponse, Locale, SetKeymapRequest,
    SetKeymapResponse, locales_server,
};
use std::{collections::HashMap, env, sync::Arc};
use tokio::{fs, process::Command};
use tonic::{Request, Response, Status};
use tracing::info;

/// The only human-readable source of layout names on the system
const XKB_RULES: [&str; 2] = [
    "/usr/share/X11/xkb/rules/base.lst",
    "/usr/share/X11/xkb/rules/evdev.lst",
];
/// systemd's console-keymap -> X11-layout conversion table
const KBD_MODEL_MAP: &str = "/usr/share/systemd/kbd-model-map";

/// System service for queries and shutdown
pub struct Service {
    _auth: Arc<AuthService>,

    // The locales registry
    registry: Registry,

    // Known locales
    locale_codes: Vec<String>,

    // Keyboard layouts, read once at startup
    keymaps: Vec<Keymap>,
}

/// Creates a new gRPC server instance using the default Service implementation
pub async fn service(auth: Arc<AuthService>) -> color_eyre::Result<locales_server::LocalesServer<Service>> {
    let registry = Registry::new()?;

    let output = Command::new("localectl").arg("list-locales").output().await?;
    let text = String::from_utf8(output.stdout)?;
    let locale_codes = text.lines().map(|l| l.to_string()).collect::<Vec<_>>();
    let keymaps = load_keymaps().await;

    info!(num_locales = locale_codes.len(), "Loaded system locale codes");
    info!(num_keymaps = keymaps.len(), "Loaded keyboard layouts");

    let current_lang = env::var("LANG").unwrap_or("en_US.UTF-8".to_string());
    let current_locale = registry.locale(&current_lang);
    if let Some(locale) = current_locale {
        info!(lang = current_lang, "Current system locale is {}", locale.display_name);
    } else {
        info!("No current system locale found");
    }

    let server = locales_server::LocalesServer::new(Service {
        _auth: auth,
        registry,
        locale_codes,
        keymaps,
    });

    Ok(server)
}

#[tonic::async_trait]
impl locales_server::Locales for Service {
    /// Lists all available locales on the system
    async fn list_locales(&self, _request: Request<()>) -> Result<Response<ListLocalesResponse>, Status> {
        let locales = self
            .locale_codes
            .iter()
            .filter_map(|code| self.registry.locale(code))
            .map(|locale| locale.into())
            .collect();

        let response = ListLocalesResponse { locales };
        Ok(Response::new(response))
    }

    /// Gets the locale details for a specific locale
    async fn get_locale(&self, _request: Request<GetLocaleRequest>) -> Result<Response<Locale>, Status> {
        let request = _request.into_inner();
        let locale_code = request.name;

        match self.registry.locale(&locale_code) {
            Some(locale) => {
                let conv = locale.into();
                Ok(Response::new(conv))
            }
            None => Err(Status::not_found(format!("Locale code {} not found", locale_code))),
        }
    }

    /// Keyboard layouts the target can be configured with
    async fn list_keymaps(&self, _reqeust: Request<()>) -> Result<Response<ListKeymapsResponse>, Status> {
        Ok(Response::new(ListKeymapsResponse {
            keymaps: self.keymaps.clone(),
        }))
    }

    /// Reslove a single layout code
    async fn get_keymap(&self, request: Request<GetKeymapsRequest>) -> Result<Response<Keymap>, Status> {
        let layout = request.into_inner().layout;

        self.keymaps
            .iter()
            .find(|keymap| keymap.layout == layout)
            .cloned()
            .map(Response::new)
            .ok_or_else(|| Status::not_found(format!("keyboard layout {layout} not found")))
    }

    /// Apply a layout to the running system
    async fn set_keymap(&self, request: Request<SetKeymapRequest>) -> Result<Response<SetKeymapResponse>, Status> {
        let request = request.into_inner();

        if request.layout.is_empty() {
            return Err(Status::invalid_argument("a layout is required"));
        }

        // --no-convert on both: localectl would otherwise derive the other half
        // itself and could quietly disagree with what the client was shown
        localectl(&["--no-convert", "set-x11-keymap", &request.layout]).await?;

        let mut console = String::new();

        if !request.console.is_empty() {
            localectl(&["--no-convert", "set-keymap", &request.console]).await?;
            console = request.console;
        }

        info!(layout = %request.layout, console = %console, "Applied keyboard layout");

        let mut applied = self
            .keymaps
            .iter()
            .find(|keymap| keymap.layout == request.layout)
            .cloned()
            .unwrap_or_else(|| Keymap {
                layout: request.layout.clone(),
                description: String::new(),
                console: String::new(),
            });

        applied.console = console;

        Ok(Response::new(SetKeymapResponse { applied: Some(applied) }))
    }
}

// Helpers

/// Keyboard layouts, from the X11 rules file.
///
/// The names come from there because it is the only place they exist in a form
/// worth showing a human. `localectl list-keymaps` gives 249 codes like `trq`
/// and `la-latin1` with no descriptions at all. The console keymap is then
/// reverse-mapped through systemd's own table, which is what laclectl uses in
/// the other direction.
async fn load_keymaps() -> Vec<Keymap> {
    let consoles = console_map().await;
    let mut text = String::new();
    let mut keymaps = Vec::new();
    let mut inside = false;

    for path in XKB_RULES {
        if let Ok(contents) = fs::read_to_string(path).await {
            text = contents;
            break;
        }
    }

    if text.is_empty() {
        return Vec::new();
    }

    for line in text.lines() {
        if let Some(section) = line.strip_prefix('!') {
            inside = section.trim() == "layout";
            continue;
        }

        if !inside {
            continue;
        }

        let mut parts = line.trim().splitn(2, char::is_whitespace);
        let Some(layout) = parts.next().filter(|layout| !layout.is_empty()) else {
            continue;
        };

        keymaps.push(Keymap {
            console: consoles.get(layout).cloned().unwrap_or_default(),
            layout: layout.to_string(),
            description: parts.next().unwrap_or_default().trim().to_string(),
        });
    }

    keymaps.sort_by(|a, b| a.description.cmp(&b.description));
    keymaps
}

/// X11 -> console keymap
async fn console_map() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(text) = fs::read_to_string(KBD_MODEL_MAP).await else {
        return map;
    };

    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }

        let mut fields = line.split_whitespace();
        let (Some(console), Some(layouts), Some(_model), Some(variant)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };

        // Variant rows describe dvoak and friends, which are a separate
        // choice this screen does not offer yet
        if variant != "-" {
            continue;
        }

        // A row may list several layouts ("mk,us"); the first is the subject
        if let Some(layout) = layouts.split(',').next() {
            map.entry(layout.to_string()).or_insert_with(|| console.to_string());
        }
    }

    map
}

/// Run localectl, turning a non-zero exit inot its own stderr.
///
/// This only changes what the user actually types on a real console, where
/// systemd-localed calls loadkeys on the installer's behalf. Inside a Wayland session
/// the compositor owns the keymap and will not pick the change up until it restarts;
/// the config written for the installed system is correct either way.
async fn localectl(args: &[&str]) -> Result<(), Status> {
    let output = Command::new("localectl")
        .args(args)
        .output()
        .await
        .map_err(|e| Status::unavailable(format!("could not run localectl: {e}")))?;

    if !output.status.success() {
        let reason = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(Status::failed_precondition(if reason.is_empty() {
            format!("localectl {} failed", args.join(" "))
        } else {
            reason
        }));
    }

    Ok(())
}
