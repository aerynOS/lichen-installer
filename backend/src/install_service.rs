// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Install service: privileged operations for installing the target system

pub mod btrfs;
mod fetch;

use crate::{auth::AuthService, install_service::btrfs::is_btrfs, network_service::split_terse};
use disks::BlockDevice;
use lichen_macros::authorized;
use protocols::lichen::install::{
    DiscoverSystemModelsResponse, DiscoveredModel, FetchModelRequest, FetchModelResponse, InstallProgress,
    InstallSystemRequest, TargetMount, WriteSystemModelRequest, WriteSystemModelResponse,
    install_server::{Install, InstallServer},
};
use std::{
    collections::VecDeque,
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{self, fs::PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::{info, warn};

/// Where the target root is temp mounted while writing
const TARGET_MOUNT: &str = "/run/lichen/target";
/// Where candidate partitions are briefly mounted read-only while probing
/// for a previous installation's system-model
const PROBE_MOUNT: &str = "/run/lichen/probe";
/// Location of the system-model inside the target root; moss reads and
/// rewrites this exact path on the installed system
const SYSTEM_MODEL_PATH: &str = "usr/lib/system-model.kdl";
/// The installer's permanent record on the target: the install-model superset
/// wrapping the system-model
const INSTALL_MODEL_PATH: &str = "etc/moss/install-model.kdl";
/// NetworkManager's system keyfile directory, on the live system and target alike
const NM_CONNECTIONS: &str = "/etc/NetworkManager/system-connections";
/// iwd's credential store, where a hidden network's key actually lives
const IWD_STORE: &str = "/var/lib/iwd";
/// Repo config directory inside the target root
const REPO_DIR: &str = "etc/moss/repo.d";
/// A progress callback: the phase now running, and a line for the log.
///
/// An empty phase means "still whatever was last announced", which is what
/// every line captured from a subprocess is.
type Progress<'a> = &'a (dyn Fn(&str, String) + Sync);
/// The unstable repo kdl entry
const UNSTABLE_REPO: &str = r#"unstable {
    description "AerynOS unstable package stream"
    base-uri "https://cdn.aerynos.dev/"
    channel main
    version "stream/unstable"
    arch x86_64
    priority 0
    active #true
}
"#;

/// Service represents the install service implementation
#[derive(Debug)]
pub struct Service {
    auth: Arc<AuthService>,
}

/// A target mount resolved to its on-disk filesystem type, with a btrfs root
/// expanded into its @ (root) and @home subvolumes.
struct ResolvedMount {
    device: String,
    mountpoint: String,
    fstype: String,
    subvol: Option<String>,
}

/// Creates a new Install gRPC server instance using the default Service implementation
pub fn service(auth: Arc<AuthService>) -> InstallServer<Service> {
    InstallServer::new(Service { auth })
}

#[tonic::async_trait]
impl Install for Service {
    type InstallSystemStream = ReceiverStream<Result<InstallProgress, Status>>;

    #[authorized("com.aerynos.lichen.install.write-model")]
    async fn write_system_model(
        &self,
        request: Request<WriteSystemModelRequest>,
    ) -> Result<Response<WriteSystemModelResponse>, tonic::Status> {
        let request = request.into_inner();
        info!(root_device = %request.root_device, "Writing system-model target");

        if request.root_device.is_empty() {
            return Err(Status::invalid_argument("no root device provided"));
        }
        if !Path::new(&request.root_device).exists() {
            return Err(Status::not_found(format!("no such device: {}", request.root_device)));
        }

        tokio::task::block_in_place(|| {
            write_to_target(&request.root_device, &request.system_model, &request.install_model)
        })?;

        Ok(Response::new(WriteSystemModelResponse {}))
    }

    #[authorized("com.aerynos.lichen.install.discover")]
    async fn discover_system_models(
        &self,
        request: Request<()>,
    ) -> Result<Response<DiscoverSystemModelsResponse>, tonic::Status> {
        let _ = request;
        info!("Probing disks for previous installation system-models");
        let models = tokio::task::block_in_place(discover_models)?;

        Ok(Response::new(DiscoverSystemModelsResponse { models }))
    }

    #[authorized("com.aerynos.lichen.install.fetch-model")]
    async fn fetch_model(
        &self,
        request: Request<FetchModelRequest>,
    ) -> Result<Response<FetchModelResponse>, tonic::Status> {
        let request = request.into_inner();
        if request.uri.is_empty() {
            return Err(Status::invalid_argument("no URI provided"));
        }

        info!(uri = %request.uri, "Fetching a model document");

        let contents = fetch::fetch(&request.uri).await?;

        Ok(Response::new(FetchModelResponse { contents }))
    }

    #[authorized("com.aerynos.lichen.install.system")]
    async fn install_system(
        &self,
        request: Request<InstallSystemRequest>,
    ) -> Result<Response<Self::InstallSystemStream>, tonic::Status> {
        let request = request.into_inner();
        // Without a root mount, moss would sync the whole OS into the empty
        // directory on the live medium's tmpfs: an install that reports
        // success and leaves the disk untouched.
        if !request.mounts.iter().any(|mount| mount.mountpoint == "/") {
            return Err(Status::invalid_argument("no root mount provided"));
        }

        info!("Installing system to target");

        let (tx, rx) = mpsc::channel(64);
        let done = Arc::new(AtomicBool::new(false));

        // Keep-alive ticks so the stream never idles, even while moss is quiet
        {
            let tx = tx.clone();
            let done = done.clone();

            thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_secs(10));

                    let update = InstallProgress {
                        message: String::new(),
                        finished: false,
                        phase: String::new(),
                    };

                    if tx.blocking_send(Ok(update)).is_err() {
                        break;
                    }
                }
            });
        }

        thread::spawn(move || {
            let progress = |phase: &str, message: String| {
                let _ = tx.blocking_send(Ok(InstallProgress {
                    message,
                    finished: false,
                    phase: phase.to_string(),
                }));
            };
            let result = install_target(&request, &progress);

            done.store(true, Ordering::Relaxed);
            match result {
                Ok(()) => {
                    let _ = tx.blocking_send(Ok(InstallProgress {
                        message: "Installation complete".to_string(),
                        finished: true,
                        phase: String::new(),
                    }));
                }
                Err(status) => {
                    let _ = tx.blocking_send(Err(status));
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(rx)))
    }
}

/// Mount the target root, write the model, and always unmount again,
/// even when the write fails
fn write_to_target(root_device: &str, system_model: &str, install_model: &str) -> Result<(), Status> {
    let target = Path::new(TARGET_MOUNT);

    fs::create_dir_all(target)?;

    // The model must land where the OS will: for btrfs that's the @ subvolume,
    // not the top level, which the installed system never mounts.
    let mut mount = Command::new("mount");

    if is_btrfs(root_device)? {
        btrfs::create_subvolumes(target, root_device)?;
        mount.args(["-o", &format!("subvol={}", btrfs::ROOT_SUBVOL)]);
    }

    mount.arg(root_device).arg(target);
    run(&mut mount)?;

    let result = (|| -> Result<(), Status> {
        let system_model_path = target.join(SYSTEM_MODEL_PATH);
        let install_model_path = target.join(INSTALL_MODEL_PATH);

        if let Some(parent) = system_model_path.parent() {
            fs::create_dir_all(parent)?;
        }

        if let Some(parent) = install_model_path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(&system_model_path, system_model)?;
        fs::write(&install_model_path, install_model)?;

        Ok(())
    })();

    let unmounted = run(Command::new("umount").arg(target));

    result.and(unmounted)
}

/// Probe every unmounted partition read-only for a system-model from a previous
/// installation. Unmountable partitions are auto skipped.
fn discover_models() -> Result<Vec<DiscoveredModel>, Status> {
    let probe = Path::new(PROBE_MOUNT);
    fs::create_dir_all(probe)?;

    let mounted = fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let devices = BlockDevice::discover()?;
    let mut models = Vec::new();

    for device in &devices {
        for partition in device.partitions() {
            let node = partition.device.display().to_string();

            // Never touch partitions that are already mounted somewhere
            if mounted.lines().any(|line| line.starts_with(&format!("{node} "))) {
                continue;
            }

            // Read-only, because these are the user's existing partitions and
            // may hold a foreign OS. A rw mount would replay journals and bump
            // mount counts on filesystems that do not need to be touched. No
            // mountable filesystem means not a candidate.
            if run(Command::new("mount").args(["-o", "ro", &node, &probe.to_string_lossy()])).is_err() {
                continue;
            }

            let model_contents = fs::read_to_string(probe.join(INSTALL_MODEL_PATH))
                .or_else(|_| fs::read_to_string(probe.join(SYSTEM_MODEL_PATH)))
                .ok();
            let _ = run(Command::new("umount").arg(probe));

            if let Some(model_contents) = model_contents {
                info!(device = %node, "Found system or install-model from a previous installation");
                models.push(DiscoveredModel {
                    device: node,
                    contents: model_contents,
                });
            }
        }
    }

    Ok(models)
}

/// Mount the target filesystems, install the OS via moss from the system
/// model written earlier, configure the target, and always unmount again
fn install_target(request: &InstallSystemRequest, progress: Progress<'_>) -> Result<(), Status> {
    let target = Path::new(TARGET_MOUNT);
    fs::create_dir_all(target)?;

    // Sort by path length so a parent is always mounted before its children:
    // mounting /boot after /boot/efi would shadow the ESP, and blsforme would
    // write boot entries into a directory nothing ever reads.
    let mounts = resolve_mounts(&request.mounts)?;

    // If this is a btrfs root, create the @/@home subvolumes
    if let Some(root) = mounts
        .iter()
        .find(|mount| mount.mountpoint == "/" && mount.subvol.is_some())
    {
        btrfs::create_subvolumes(target, &root.device)?;
    }

    let mut mounted: Vec<PathBuf> = Vec::new();
    let result = (|| -> Result<(), Status> {
        progress("mount", "Mounting target filesystems".to_string());
        for mount in &mounts {
            let mountpoint = target.join(mount.mountpoint.trim_start_matches('/'));
            fs::create_dir_all(&mountpoint)?;

            let mut cmd = Command::new("mount");

            if let Some(subvol) = &mount.subvol {
                cmd.args(["-o", &format!("subvol={subvol}")]);
            }
            cmd.arg(&mount.device).arg(&mountpoint);
            run(&mut cmd)?;
            mounted.push(mountpoint);
        }

        // Virtual filesystems needed by moss triggers and chroot commands
        for (source, dest) in [
            ("/dev", "dev"),
            ("/dev/shm", "dev/shm"),
            ("/dev/pts", "dev/pts"),
            ("/proc", "proc"),
            ("/sys", "sys"),
        ] {
            let mountpoint = target.join(dest);
            fs::create_dir_all(&mountpoint)?;
            run(Command::new("mount").args(["--bind", source, &mountpoint.to_string_lossy()]))?;
            mounted.push(mountpoint);
        }

        // `moss sync --import` does not bootstrap repos on an empty root
        if request.repositories.is_empty() {
            warn!("no repos to prime; sync will fail unless moss bootstraps them itself");
        }
        configure_repos(target)?;
        progress("index", "Refreshing package index".to_string());
        run(Command::new("moss").arg("-D").arg(target).args(["repo", "update"]))?;

        // moss materializes the system from the model, including populating
        // the mounted ESP/XBOOTLDR with boot entries via its blsforme
        // integration, which is why the boot mounts must be live first
        progress("packages", "Installing packages".to_string());
        info!("Running moss sync against the target (this can take a while)");
        let sync_result = run_streaming(
            Command::new("moss")
                .args(["sync", "--import"])
                .arg(target.join(SYSTEM_MODEL_PATH))
                .arg("-D")
                .arg(target)
                .arg("-u")
                .arg("-y"),
            progress,
        );

        if let Err(ref err) = sync_result {
            warn!("moss sync failed; attempting to clean vfat boot partitions before unmount: {err}");
            fsck_vfat_mounts(target, &mounts)?;
        }

        sync_result?;

        progress("configure", "Configuring target system".to_string());
        configure_target(target, request)
    })();

    // Unwind in reverse: the bind mounts and nested boot mounts sit under the
    // target root, so unmounting the root first fails with EBUSY and pins it
    // for the rest of the session. sync last, because the user is told they
    // may reboot the moment this returns.
    progress("unmount", "Unmounting target filesystems".to_string());
    for mountpoint in mounted.iter().rev() {
        let _ = run(Command::new("umount").args(["-l", mountpoint.to_str().expect("should have had a mountpoint")]));
    }
    let _ = run(&mut Command::new("sync"));

    if result.is_err() {
        warn!("Installation failed: the target disk may be in a partial or unbootable state; do not reboot!");
    }

    result
}

/// Make the unstable stream the only configured repo on the installed system,
/// regardless of which stream the live media primed. Inheriting the live
/// medium's volatile repo would have the installed system self-upgrade onto a
/// stream it should never track. Both extensions are cleared because a stale
/// .yaml would coexist with the .kdl written, giving moss two answers.
fn configure_repos(target: &Path) -> Result<(), Status> {
    let repo_dir = target.join(REPO_DIR);
    fs::create_dir_all(&repo_dir)?;

    for entry in fs::read_dir(&repo_dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "yaml" || ext == "kdl") {
            fs::remove_file(&path)?;
        }
    }

    fs::write(repo_dir.join("unstable.kdl"), UNSTABLE_REPO)?;
    Ok(())
}

/// Apply the installer-owned config to the installed target
fn configure_target(target: &Path, req: &InstallSystemRequest) -> Result<(), Status> {
    if !req.locale.is_empty() {
        fs::write(target.join("etc/locale.conf"), format!("LANG={}\n", req.locale))?;
    }

    if !req.timezone.is_empty() {
        let localtime = target.join("etc/localtime");
        let _ = fs::remove_file(&localtime);
        unix::fs::symlink(format!("../usr/share/zoneinfo/{}", req.timezone), &localtime)?;
    }

    if !req.keymap.is_empty() {
        fs::write(target.join("etc/vtconsole.conf"), format!("KEYMAP={}\n", req.keymap))?;
    }

    if !req.x11_layout.is_empty() {
        let directory = target.join("etc/X11/xorg.conf.d");
        fs::create_dir_all(&directory)?;
        fs::write(
            directory.join("00-keyboard.conf"),
            format!(
                "Section \"InputClass\"\n\
                 \x20       Identifier \"system-keyboard\"\n\
                 \x20       MatchIsKeyboard \"on\"\n\
                 \x20       Option \"XkbLayout\" \"{}\"\n\
                 EndSection\n",
                req.x11_layout
            ),
        )?;
    }

    // moss installs systemd's /etc/machine-id from the package set, so the
    // target would inherit the live medium's id. Every machine installed from
    // that medium would then share a DHCP DUID and journal id, and systemd
    // would treat the first boot as an nth boot and skip ConditionFirstBoot
    // units. Absent is the expected case, hence the ignored result.
    let _ = fs::remove_file(target.join("etc/machine-id"));
    run(Command::new("chroot").arg(target).arg("systemd-machine-id-setup"))?;

    if let Some(user) = &req.user {
        run(Command::new("chroot")
            .arg(target)
            .args([
                "useradd",
                "-m",
                "-U",
                "-G",
                "audio,adm,wheel,render,kvm,input,users",
                "-c",
            ])
            .args([&user.real_name, &user.username]))?;
    }

    let mut entries = String::new();
    if !req.root_password_hash.is_empty() {
        entries.push_str(&format!("root:{}\n", req.root_password_hash));
    }
    if let Some(user) = &req.user {
        entries.push_str(&format!("{}:{}\n", user.username, user.password_hash));
    }
    if !entries.is_empty() {
        set_passwords(target, &entries)?;
    }

    write_fstab(target, req)?;
    if let Some(profile) = req.network_profile.as_deref().filter(|profile| !profile.is_empty()) {
        copy_network_profile(target, profile)?;
    }

    Ok(())
}

/// Carry the live system's wireless credentials onto the target.
///
/// Two carriers, because AerynOS runst NetworkManager on the iwd backend. A
/// visible network gets a NM keyfile under /etc holding the psk. A hidden one
/// is connected by iwctl, no NM only write a volatile stub under /run with
/// no `psk=` in it at all; copying that would install a profile that can never
/// authenticate. They key for those lives in iwd's own store, which iwd on the
/// target reads regardless of which profile NM holds.
fn copy_network_profile(target: &Path, profile: &str) -> Result<(), Status> {
    if let Some(source) = nm_keyfile(profile)?
        && source.starts_with(NM_CONNECTIONS)
        && source.is_file()
        && let Some(name) = source.file_name().and_then(|name| name.to_str())
    {
        copy_secret(&source, &target.join(NM_CONNECTIONS.trim_start_matches('/')), name)?;
    }

    for suffix in ["psk", "open", "8021x"] {
        let name = format!("{profile}.{suffix}");
        let source = PathBuf::from(IWD_STORE).join(&name);

        if source.is_file() {
            copy_secret(&source, &target.join(IWD_STORE.trim_start_matches('/')), &name)?;
        }
    }
    Ok(())
}

/// Ask NetworkManager which keyfile backs a profile
///
/// A missing answer is not fatal: the install continues, the target simply
/// comes up without a connection.
fn nm_keyfile(profile: &str) -> Result<Option<PathBuf>, Status> {
    let output = Command::new("nmcli")
        .args(["-t", "-f", "NAME,FILENAME", "connection", "show"])
        .output()
        .map_err(|e| Status::internal(format!("failed to spawn nmcli: {e}")))?;

    if !output.status.success() {
        warn!("nmcli could not list connections; the target will carry no network profile");
        return Ok(None);
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(split_terse)
        .find(|fields| fields.first().is_some_and(|name| name == profile))
        .and_then(|fields| fields.get(1).map(PathBuf::from)))
}

/// Copy a credential into the target as 0600 root:root, in a 0700 directory
fn copy_secret(source: &Path, directory: &Path, name: &str) -> Result<(), Status> {
    fs::create_dir_all(directory)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;

    let destination = directory.join(name);
    fs::copy(source, &destination)?;
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))?;

    info!("carried {} onto the target", source.display());
    Ok(())
}

/// Set account passwords from pre-computed crypt(3) hashes via chpasswd -e
fn set_passwords(target: &Path, entries: &str) -> Result<(), Status> {
    let mut child = Command::new("chroot")
        .arg(target)
        .args(["chpasswd", "-e"])
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Status::internal(format!("failed to spawn chpasswd: {e}")))?;

    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(entries.as_bytes())?;

    let output = child
        .wait_with_output()
        .map_err(|e| Status::internal(format!("chpasswd did not complete: {e}")))?;

    if !output.status.success() {
        return Err(Status::internal(format!(
            "chpasswd failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(())
}

/// Read a single blkid tag value from a device, bypassing the cache: the
/// partition was created and formatted moments ago, and a stale entry would
/// put the previous layout's PARTUUID into the target's fstab.
fn blkid(device: &str, tag: &str) -> Result<String, Status> {
    let output = Command::new("blkid")
        .args(["-c", "/dev/null", "-s", tag, "-o", "value"])
        .arg(device)
        .output()
        .map_err(|e| Status::internal(format!("failed to spawn blkid: {e}")))?;

    if !output.status.success() {
        return Err(Status::internal(format!("blkid {tag} failed for {device}")));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Run a long command forwarding its output lines as progress, keeping
/// tail of recent lines for error reporting
fn run_streaming(command: &mut Command, progress: Progress<'_>) -> Result<(), Status> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Status::internal(format!("failed to spawn {:?}: {e}", command.get_program())))?;
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    let record = |line: &str| {
        let mut tail = tail.lock().unwrap();
        if tail.len() >= 30 {
            tail.pop_front();
        }
        tail.push_back(line.to_string());
    };

    thread::scope(|scope| {
        scope.spawn(|| {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                record(&line);
                warn!("stderr: {line}");
            }
        });

        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            record(&line);

            let cleaned = clean_line(&line);
            if !cleaned.is_empty() {
                progress("", cleaned);
            }
        }
    });

    let status = child
        .wait()
        .map_err(|e| Status::internal(format!("{:?} did not complete: {e}", command.get_program())))?;

    if !status.success() {
        let detail = tail.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n");
        return Err(Status::internal(format!(
            "{:?} failed ({}): {}",
            command.get_program(),
            status,
            detail,
        )));
    }

    Ok(())
}

/// Reduce a raw output line to something fit for a one-line progress display
fn clean_line(line: &str) -> String {
    let last = line.rsplit('\r').next().unwrap_or(line);
    let mut cleaned = String::with_capacity(last.len());
    let mut chars = last.chars();

    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            if chars.next() == Some('[') {
                for end in chars.by_ref() {
                    if end.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if !character.is_control() {
            cleaned.push(character);
        }
    }
    cleaned.trim().to_string()
}

/// Run a command to completion, mapping failure to a gRPC status carrying
/// the command's stderr
fn run(command: &mut Command) -> Result<(), Status> {
    let output = command
        .output()
        .map_err(|e| Status::internal(format!("failed to spawn {:?}: {e}", command.get_program())))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if stderr.trim().is_empty() { stdout } else { stderr };

        return Err(Status::internal(format!(
            "{:?} failed ({}): {}",
            command.get_program(),
            output.status,
            detail.trim()
        )));
    }
    Ok(())
}

/// Run fsck -a on all vfat boot partitions before unmounting after a failed
/// install. This reduces the change a partial write leave the ESP or
/// XBOOTLDR in a dirty state that the user then reboots into.
fn fsck_vfat_mounts(target: &Path, mounts: &[ResolvedMount]) -> Result<(), Status> {
    for mount in mounts.iter().filter(|mount| mount.fstype == "vfat") {
        let mountpoint = target.join(mount.mountpoint.trim_start_matches('/'));
        let _ = run(Command::new("fsck.vfat").args(["-a", "-w", &mountpoint.to_string_lossy()]));
    }

    // Non-fatal: if fsck cannot repair it, the install has already failed
    // and the user must be told not to reboot.
    Ok(())
}

/// Mount options and fsck pass for the target filesystem.
///
/// vfat carries no UNIX permissions, so without an explicit
/// umask the ESP and XBOOTLDR contents are world readable.
fn fstab_params(mountpoint: &str, fstype: &str, subvol: Option<&str>) -> (String, u8) {
    let (base, pass) = match (mountpoint, fstype) {
        (_, "btrfs") => ("defaults", 0u8),
        ("/", _) => ("defaults", 1),
        // ESP and XBOOTLDR are vfat. Run fsck at boot: ESP first, XBOOTLDR
        // second. `flush` added so metadata is pushed out more aggressively,
        // reducing the dirty-state window across unclean shutdowns.
        ("/efi", "vfat") => ("defaults,unmask=0077,flush", 1),
        (_, "vfat") => ("defaults,umask=0077,flush", 2),
        _ => ("defaults", 2),
    };
    match subvol {
        Some(subvol) => (format!("{base},subvol={subvol}"), pass),
        None => (base.to_string(), pass),
    }
}

fn write_fstab(target: &Path, request: &InstallSystemRequest) -> Result<(), Status> {
    let mounts = resolve_mounts(&request.mounts)?;

    // A rootless fstable is worse than none: the initrd hands off to a system
    // that can neither remount / nor find /boot.
    if !mounts.iter().any(|mount| mount.mountpoint == "/") {
        return Err(Status::internal("target has no root mount; refusing to write fstab"));
    }

    let mut fstab = String::from("# /etc/fstab: static filesystem information.\n");

    for mount in mounts {
        let partuuid = blkid(&mount.device, "PARTUUID")?;
        let (options, pass) = fstab_params(&mount.mountpoint, &mount.fstype, mount.subvol.as_deref());

        fstab.push_str(&format!(
            "PARTUUID={partuuid} {} {} {options} 0 {pass}\n",
            mount.mountpoint, mount.fstype
        ));
    }

    fs::write(target.join("etc/fstab"), fstab)?;
    Ok(())
}

/// Probe each requested mount's filesystem and expand a btrfs root into the
/// default @/@home subvolume layout. Sorted so parents precede children.
fn resolve_mounts(mounts: &[TargetMount]) -> Result<Vec<ResolvedMount>, Status> {
    let mut resolved = Vec::new();

    for mount in mounts {
        if !mount.mountpoint.starts_with('/') {
            continue;
        }
        resolved.push(ResolvedMount {
            device: mount.device.clone(),
            mountpoint: mount.mountpoint.clone(),
            fstype: blkid(&mount.device, "TYPE")?,
            subvol: None,
        });
    }

    // Checks to see if it's a btrfs filesystem. If it is, it creates @/@home, if it
    // isn't it's a no-op function.
    btrfs::expand_subvolumes(&mut resolved);
    resolved.sort_by_key(|mount| mount.mountpoint.len());
    Ok(resolved)
}
