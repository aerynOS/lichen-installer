// SPDX-FileCopyrightText: Copyright © 2026 aerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Writing the installation to disk.
//!
//! Three RPCs in sequence — apply the partitioning strategy, write the model
//! documents into the fresh rootfs, then run the install itself, which is
//! server-streaming and reports as it goes. Everything the task needs is
//! captured up front so it never reaches back into the model.

use super::{Context, Screen};
use crate::{
    events::{Action, Msg},
    install_model::{repositories, system_model_kdl, to_kdl},
    theme::*,
};
use installer::Model;
use protocols::lichen::{
    install::{
        InstallSystemRequest, RepoSpec, TargetMount, UserSpec, WriteSystemModelRequest, install_client::InstallClient,
    },
    storage::provisioner::{ApplyStrategyRequest, provisioner_client::ProvisionerClient},
    system::system_client::SystemClient,
};
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};
use tokio::sync::mpsc::UnboundedSender;
use tonic::{Status, transport::Channel};

/// The install, phase by phase, with each on'es rough share of the wall clock.
///
/// The first two run here rahter than in the backend, which is why this table
/// cannot live there. The weights are what make the lower bar mean elapsed time
/// rather than step count: packages is most of the install, and a bar treating
/// it as 1/7th would sit at 43% for minutes and then leap forward. They sum to
/// 100, so the running total is already a percentage.
const PHASES: [(&str, &str, u16); 7] = [
    ("partition", "Partitioning the disk", 10),
    ("model", "Writing the system model", 2),
    ("mount", "Mounting target filesystems", 3),
    ("index", "Refreshing package index", 5),
    ("packages", "Installing packages", 70),
    ("configure", "Configuring target system", 5),
    ("unmount", "Unmounting target filesystems", 5),
];

enum State {
    Working,
    Done,
    Rebooting,
    Failed,
}

/// The two ways out, once the install has finished
#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Reboot,
    Quit,
}

/// Everything the install task needs, captured before it starts
struct Job {
    channel: Channel,
    strategy: String,
    disk: String,
    system_model: String,
    record: String,
    locale: String,
    timezone: String,
    root_password_hash: String,
    user: Option<UserSpec>,
    keymap: String,
    x11_layout: String,
    network_profile: Option<String>,
}

pub struct Install {
    state: State,
    log: Vec<String>,
    started: bool,
    /// Index into `PHASES`
    phase: usize,
    /// How far through the running phase of the tool doing the work states
    fraction: f32,
    /// The current package in heartbeat
    package: Option<String>,
    /// Animation clock, counted from `Msg::Tick`
    tick: usize,
    /// Which of the two finished-install buttson has focus
    choice: Choice,
    /// Cloned on entry: the reboot RPC is fired from `handle_key`
    ctx: Option<Context>,
}

impl Install {
    pub fn new() -> Self {
        Self {
            state: State::Working,
            log: Vec::new(),
            started: false,
            phase: 0,
            fraction: 0.0,
            package: None,
            tick: 0,
            choice: Choice::Reboot,
            ctx: None,
        }
    }

    /// Two bars: how far through the phases, and how far through the work.
    ///
    /// They disagree on purpose. The phases are wildly unequal in length, and
    /// seeing "5 of 7" sat above "20%" is what tells the user the long phase
    /// is still ahead of them.
    fn render_progress(&self, frame: &mut Frame<'_>, area: Rect) {
        let (beat, title, style) = match self.state {
            State::Working => (HEARTBEAT[self.tick % HEARTBEAT.len()], self.working_title(), HEADING),
            State::Done => (
                COMPLETE,
                "All phases complete. It is safe to reboot.".to_string(),
                SUCCESS,
            ),
            State::Rebooting => (
                HEARTBEAT[self.tick % HEARTBEAT.len()],
                "Rebooting...".to_string(),
                SUCCESS,
            ),
            State::Failed => ("!", PHASES[self.phase].1.to_string(), ERROR),
        };
        let [label, bars] = Layout::vertical([Constraint::Length(1), Constraint::Length(2)]).areas(area);

        frame.render_widget(Paragraph::new(Line::styled(format!("{beat} {title}"), style)), label);

        let [phase, total] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(bars);

        bar_row(
            frame,
            phase,
            "Phase",
            (self.phase as u16 + 1) * 100 / PHASES.len() as u16,
            format!("{} of {}", self.phase + 1, PHASES.len()),
        );
        bar_row(
            frame,
            total,
            "Total",
            self.completed(),
            format!("{}%", self.completed()),
        );
    }

    /// What the heatbeat syas while work is running.
    fn working_title(&self) -> String {
        match &self.package {
            Some(package) if PHASES[self.phase].0 == "packages" => format!("Installing package: {package}"),
            _ => PHASES[self.phase].1.to_string(),
        }
    }

    /// Share of the work already completed.
    ///
    /// A phase counts only once it has been completed, so the bar never takes credit
    /// for work still in-progress.
    fn completed(&self) -> u16 {
        match self.state {
            State::Done | State::Rebooting => 100,
            _ => {
                let finished: u16 = PHASES.iter().take(self.phase).map(|(_, _, weight)| weight).sum();
                let running = (PHASES[self.phase].2 as f32 * self.fraction) as u16;

                finished + running
            }
        }
    }

    /// Ask the backend to reboot the machine.
    fn reboot(&mut self) -> Action {
        let Some(ctx) = self.ctx.clone() else {
            return Action::Failed("not connected to the backend".to_string());
        };
        let channel = ctx.channel.clone();

        self.state = State::Rebooting;

        ctx.spawn(async move {
            SystemClient::new(channel).reboot(()).await?;

            Ok(Msg::RebootAccepted)
        });

        Action::Consumed
    }

    /// The tail is what matters in a live log, so scroll by dropping the head
    fn render_log(&self, frame: &mut Frame<'_>, area: Rect) {
        let start = self.log.len().saturating_sub(area.height as usize);
        let lines: Vec<Line<'static>> = self.log[start..]
            .iter()
            .map(|line| Line::styled(line.clone(), BODY))
            .collect();

        frame.render_widget(Paragraph::new(lines), area);
    }

    /// The two ways out of a finished install
    fn render_choices(&self, frame: &mut Frame<'_>, area: Rect) {
        let row = Rect {
            y: area.y + 1,
            height: 1,
            ..area
        };
        let line = Line::from(vec![
            choice("Reboot now", self.choice == Choice::Reboot),
            Span::raw("  "),
            choice("Quit", self.choice == Choice::Quit),
        ]);

        frame.render_widget(Paragraph::new(line).right_aligned(), row);
    }
}

impl Screen for Install {
    fn title(&self) -> &str {
        "Install"
    }

    fn is_complete(&self, _model: &Model) -> bool {
        matches!(self.state, State::Done)
    }

    fn hints(&self) -> &[(&str, &str)] {
        match self.state {
            State::Done => &[("←→", "choose"), ("Enter", "select")],
            _ => &[],
        }
    }

    fn handle_key(&mut self, key: KeyEvent, _model: &mut Model) -> Action {
        // Nothing here is cancellable and navigation is locked by the phase.
        // Only the finished state has anything to process
        if !matches!(self.state, State::Done) {
            return Action::Consumed;
        }

        match key.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                self.choice = match self.choice {
                    Choice::Reboot => Choice::Quit,
                    Choice::Quit => Choice::Reboot,
                };
                Action::Consumed
            }
            KeyCode::Enter => match self.choice {
                Choice::Reboot => self.reboot(),
                Choice::Quit => Action::Quit,
            },
            _ => Action::Consumed,
        }
    }

    fn on_enter(&mut self, ctx: &Context, model: &Model) {
        self.ctx = Some(ctx.clone());
        if self.started {
            return;
        }

        self.started = true;

        let job = Job {
            channel: ctx.channel.clone(),
            strategy: model.storage.strategy_id.clone(),
            disk: model.storage.disk.clone(),
            system_model: system_model_kdl(model),
            record: to_kdl(model),
            locale: model.region.language.clone(),
            timezone: model.region.timezone.clone(),
            root_password_hash: model.accounts.root_password_hash.clone().unwrap_or_default(),
            user: model.accounts.user.as_ref().map(|user| UserSpec {
                username: user.username.clone(),
                real_name: user.real_name.clone(),
                password_hash: user.password_hash.clone(),
            }),
            keymap: model.region.keymap.clone(),
            x11_layout: model.region.layout.clone(),
            network_profile: model.network.profile.clone(),
        };
        let tx = ctx.tx.clone();

        // Not `ctx.spawn`: that delivers a single Msg; this needs to report live
        // the live install progress.
        tokio::spawn(async move {
            let _ = match run(&job, &tx).await {
                Ok(()) => tx.send(Msg::InstallFinished),
                Err(status) => tx.send(Msg::InstallFailed(status.message().to_string())),
            };
        });
    }

    fn on_message(&mut self, msg: &Msg, _model: &mut Model) {
        match msg {
            Msg::Tick => self.tick = self.tick.wrapping_add(1),
            Msg::InstallProgress { phase, line, fraction } => {
                if let Some(index) = PHASES.iter().position(|(phas, _, _)| phas == phase) {
                    if index != self.phase {
                        self.fraction = 0.0;
                    }
                    self.phase = index;
                }

                if let Some(completed) = fraction {
                    self.fraction = self.fraction.max(*completed);
                }

                if let Some(name) = line.strip_prefix("Installed ") {
                    let name = name.strip_suffix(" (cached)").unwrap_or(name);
                    self.package = Some(name.to_string());
                } else if !line.is_empty()
                    && PHASES[self.phase].0 != "packages"
                    && line.as_str() != PHASES[self.phase].1
                {
                    self.log.push(line.clone());
                }
            }
            Msg::InstallFinished => {
                self.state = State::Done;
                self.log.push("Installation complete".to_string());
            }
            Msg::InstallFailed(reason) => {
                self.state = State::Failed;
                self.log.push(reason.clone());
            }
            _ => {}
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, model: &Model) {
        let [heading, bars, body] =
            Layout::vertical([Constraint::Length(3), Constraint::Length(3), Constraint::Min(1)]).areas(area);
        let (title, style) = match self.state {
            State::Working => ("Installing aerynOS", HEADING),
            State::Done => (
                "Get ready for the aerynOS, experience! It's now installed on your device!!!",
                SUCCESS,
            ),
            State::Rebooting => ("Rebooting", SUCCESS),
            State::Failed => ("The installation failed", ERROR),
        };
        let note = match self.state {
            State::Working => format!("Writing to {}. Do not power off.", model.storage.disk),
            State::Done => "Choose Reboot now to start boot into your newly installed system, or Quit to stay in the live environment".to_string(),
            State::Rebooting => "Leaving the installer so the system can reboot.".to_string(),
            State::Failed => "The disk may be in a partial state".to_string(),
        };

        frame.render_widget(
            Paragraph::new(vec![Line::styled(title, style), Line::styled(note, HINT)]).wrap(Wrap { trim: false }),
            heading,
        );

        self.render_progress(frame, bars);

        // Once it is finished, the way out matter more than the log tail
        if matches!(self.state, State::Done) {
            let [log, buttons] = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(body);

            self.render_log(frame, log);
            self.render_choices(frame, buttons);
            return;
        }

        self.render_log(frame, body);
    }
}

// Helpers

/// Apply, write, install. Progress goes out as it happens; the return value is
/// only the final verdict.
async fn run(job: &Job, tx: &UnboundedSender<Msg>) -> Result<(), Status> {
    let progress = |phase: &str, message: String| {
        let _ = tx.send(Msg::InstallProgress {
            phase: phase.to_string(),
            line: message,
            fraction: None,
        });
    };

    progress("partition", format!("Applying {} to {}", job.strategy, job.disk));

    let applied = ProvisionerClient::new(job.channel.clone())
        .apply_strategy(ApplyStrategyRequest {
            strategy: job.strategy.clone(),
            disks: vec![job.disk.clone()],
        })
        .await?
        .into_inner();
    let plan = applied
        .plan
        .ok_or_else(|| Status::internal("the backend returned no applied plan"))?;
    let root_device = plan
        .role_mounts
        .iter()
        .find(|mount| mount.mountpoint == "/")
        .map(|mount| mount.device.clone())
        .ok_or_else(|| Status::internal("the applied plan has no root mount"))?;
    let repositories = repositories(&job.system_model)
        .map_err(|e| Status::internal(format!("the generated system-model failed to parse: {e}")))?
        .into_iter()
        .map(|repo| RepoSpec {
            id: repo.id,
            uri: repo.uri,
        })
        .collect();
    let mut install = InstallClient::new(job.channel.clone());

    progress("model", format!("Writing the system-model to {root_device}"));

    install
        .write_system_model(WriteSystemModelRequest {
            root_device,
            system_model: job.system_model.clone(),
            install_model: job.record.clone(),
        })
        .await?;

    let mounts = plan
        .role_mounts
        .iter()
        .filter(|mount| mount.mountpoint.starts_with('/'))
        .map(|mount| TargetMount {
            device: mount.device.clone(),
            mountpoint: mount.mountpoint.clone(),
        })
        .collect();

    progress("", "Installing aerynOS; this can take several minutes...".to_string());

    let mut stream = install
        .install_system(InstallSystemRequest {
            mounts,
            locale: job.locale.clone(),
            timezone: job.timezone.clone(),
            root_password_hash: job.root_password_hash.clone(),
            user: job.user.clone(),
            repositories,
            keymap: job.keymap.clone(),
            x11_layout: job.x11_layout.clone(),
            network_profile: job.network_profile.clone(),
        })
        .await?
        .into_inner();

    while let Some(update) = stream.message().await? {
        if update.finished {
            return Ok(());
        }

        if !update.phase.is_empty() || !update.message.is_empty() || update.fraction.is_some() {
            let _ = tx.send(Msg::InstallProgress {
                phase: update.phase,
                line: update.message,
                fraction: update.fraction,
            });
        }
    }

    Err(Status::aborted("the install stream ended without completing"))
}

/// One labelled gauge bar: caption left, bar center, value right.
fn bar_row(frame: &mut Frame<'_>, area: Rect, caption: &str, percent: u16, value: String) {
    let [caption_area, bar, value_area] =
        Layout::horizontal([Constraint::Length(7), Constraint::Min(10), Constraint::Length(9)]).areas(area);
    let cells = bar.width.saturating_sub(2);
    let steps = percent.min(100) as u32 * cells as u32 * 4 / 100;
    let full = (steps / 4) as u16;
    let partial = (steps % 4) as usize;
    let mut drawn = BAR[0].repeat(full as usize);

    if full < cells && partial > 0 {
        drawn.push_str(BAR[4 - partial]);
    }

    let used = full + u16::from(full < cells && partial > 0);

    drawn.push_str(&BAR[4].repeat(cells.saturating_sub(used) as usize));

    frame.render_widget(Paragraph::new(Line::styled(caption.to_string(), HINT)), caption_area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("|", FRAME),
            Span::styled(drawn, STEP_ACTIVE),
            Span::styled("|", FRAME),
        ])),
        bar,
    );
    frame.render_widget(Paragraph::new(Line::styled(value, BODY)).right_aligned(), value_area);
}

/// One, button padded so the focus highlight has some body to it
fn choice(label: &str, focused: bool) -> Span<'static> {
    let style = if focused { SELECTED } else { BUTTON };
    Span::styled(format!("    {label}    "), style)
}
