// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
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
};
use ratatui::{
    Frame,
    crossterm::event::KeyEvent,
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::Paragraph,
};
use tokio::sync::mpsc::UnboundedSender;
use tonic::{Status, transport::Channel};

enum State {
    Working,
    Done,
    Failed,
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
}

pub struct Install {
    state: State,
    log: Vec<String>,
    started: bool,
}

impl Install {
    pub fn new() -> Self {
        Self {
            state: State::Working,
            log: Vec::new(),
            started: false,
        }
    }
}

impl Screen for Install {
    fn title(&self) -> &str {
        "Install"
    }

    fn is_complete(&self, _model: &Model) -> bool {
        matches!(self.state, State::Done)
    }

    fn handle_key(&mut self, _key: KeyEvent, _model: &mut Model) -> Action {
        // Nothing here is cancellable and navigation is locked by the phase
        Action::Consumed
    }

    fn on_enter(&mut self, ctx: &Context, model: &Model) {
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
        };
        let tx = ctx.tx.clone();

        // Not `ctx.spawn`: that delivers a single Msg; this needs to report live
        // the live install progress.
        tokio::spawn(async move {
            let _ = match run(&job, &tx).await {
                Ok(()) => tx.send(Msg::InstallFinished),
                Err(status) => tx.send(Msg::Failed(status.message().to_string())),
            };
        });
    }

    fn on_message(&mut self, msg: &Msg, _model: &mut Model) {
        match msg {
            Msg::InstallProgress(line) => self.log.push(line.clone()),
            Msg::InstallFinished => {
                self.state = State::Done;
                self.log.push("Installation complete".to_string());
            }
            Msg::Failed(reason) => {
                if matches!(self.state, State::Working) {
                    self.state = State::Failed;
                    self.log.push(reason.clone());
                }
            }
            _ => {}
        }
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, model: &Model) {
        let [heading, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
        let (title, style) = match self.state {
            State::Working => ("Installing AerynOS", HEADING),
            State::Done => (
                "Get ready for the AerynOS, experience! It's now installed on your device!!!",
                SUCCESS,
            ),
            State::Failed => ("The installation failed", ERROR),
        };
        let note = match self.state {
            State::Working => format!("Writing to {}. Do not power off.", model.storage.disk),
            State::Done => "You can now reboot into your new AerynOS installation!".to_string(),
            State::Failed => "The disk may be in a partial state".to_string(),
        };

        frame.render_widget(
            Paragraph::new(vec![Line::styled(title, style), Line::styled(note, HINT)]),
            heading,
        );

        // The tail is what matters in the live log, so scroll by dropping the head
        let start = self.log.len().saturating_sub(body.height as usize);
        let lines: Vec<Line<'static>> = self.log[start..]
            .iter()
            .map(|line| Line::styled(line.clone(), BODY))
            .collect();
        frame.render_widget(Paragraph::new(lines), body);
    }
}

// Helpers

/// Apply, write, install. Progress goes out as it happens; the return value is
/// only the final verdict.
async fn run(job: &Job, tx: &UnboundedSender<Msg>) -> Result<(), Status> {
    let progress = |message: &str| {
        let _ = tx.send(Msg::InstallProgress(message.to_string()));
    };

    progress(&format!("Applying {} to {}", job.strategy, job.disk));

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

    progress(&format!("Writing the sytem model to {root_device}"));

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

    progress("Installing AerynOS; this can take several minutes...");

    let mut stream = install
        .install_system(InstallSystemRequest {
            mounts,
            locale: job.locale.clone(),
            timezone: job.timezone.clone(),
            root_password_hash: job.root_password_hash.clone(),
            user: job.user.clone(),
            repositories,
        })
        .await?
        .into_inner();

    while let Some(update) = stream.message().await? {
        if !update.message.is_empty() {
            progress(&update.message);
        }

        if update.finished {
            return Ok(());
        }
    }

    Err(Status::aborted("the install stream ended without completing"))
}
