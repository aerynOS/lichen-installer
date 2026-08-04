// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! How the disk should be partitioned, and what the root filesystem should be.
//!
//! Two questions on one screen. The probe returns a plan for every applicable
//! strategy, so the consequences of both are previewed live rather than after
//! the fact.

use super::{Context, Screen};
use crate::{
    events::{Action, Msg},
    filesystems,
    install_model::{from_kdl, parse_error_detail},
    plan,
    selections::{mandatory, packages_for},
    theme::*,
};
use installer::Model;
use protocols::lichen::{
    install::install_client::InstallClient,
    storage::{
        disks::{ListDisksRequest, disks_client::DisksClient},
        provisioner::{StrategyDefinition, StrategyPlan, TryStrategyRequest, provisioner_client::ProvisionerClient},
    },
};
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Padding, Paragraph, Wrap},
};
use std::collections::BTreeSet;

/// A strategy and the plan it produced for the chosen disk
type Viable = (StrategyDefinition, StrategyPlan);

/// A previous installation found on the chosen disk.
///
/// The text is kept rather than the parsed model: `installer::Model` is not
/// `Clone`, and re-parsing one small document at the moment of choice is
/// cheaper than keeping a second whole model alive. `strategy` is lifted out
/// once on arrival so the live preview has something to show without parsing
/// on every frame.
struct Discovered {
    /// Partition the model was found on
    device: String,
    /// Full text of the discovered system-model
    contents: String,
    /// Strategy the model records, empty when it records mone
    strategy: String,
}

/// Which of the two questions is currently being asked
enum Stage {
    Approach,
    Filesystem,
}

enum State {
    Loading,
    Ready(Vec<Viable>),
}

pub struct Strategy {
    state: State,
    stage: Stage,
    approach_list: ListState,
    filesystem_list: ListState,
    /// Disk the current probe was run against; a different one re-probes
    probed: Option<String>,
    /// An existing installation on that disk, offered as "Refresh OS"
    discovered: Option<Discovered>,
}

impl Strategy {
    pub fn new() -> Self {
        Self {
            state: State::Loading,
            stage: Stage::Approach,
            approach_list: ListState::default(),
            filesystem_list: ListState::default(),
            probed: None,
            discovered: None,
        }
    }

    /// Row index of the "Refresh OS" entry, when a previous installation was found
    fn refresh_row(&self) -> Option<usize> {
        self.discovered.as_ref().map(|_| self.approaches().len())
    }

    /// Rows in the approach list, "Refresh OS" included
    fn approach_rows(&self) -> usize {
        self.approaches().len() + usize::from(self.discovered.is_some())
    }

    fn viable(&self) -> &[Viable] {
        match &self.state {
            State::Ready(viable) => viable,
            State::Loading => &[],
        }
    }

    /// One index into `viable` per distinct partitioning approach; the
    /// filesystem variants of an approach collapse into a single entry.
    fn approaches(&self) -> Vec<usize> {
        let viable = self.viable();
        let mut chosen: Vec<usize> = Vec::new();

        for (index, (definition, _)) in viable.iter().enumerate() {
            let base = filesystems::base(&definition.id);

            if !chosen.iter().any(|&seen| filesystems::base(&viable[seen].0.id) == base) {
                chosen.push(index);
            }
        }
        chosen
    }

    /// Root filesystems the highlighted approach offers, as
    /// (strategy id, filesystem, hint)
    fn variants(&self) -> Vec<(String, &str, &str)> {
        let Some(position) = self.approach_list.selected() else {
            return Vec::new();
        };
        let Some(&index) = self.approaches().get(position) else {
            return Vec::new();
        };
        let base = filesystems::base(&self.viable()[index].0.id);
        let all: Vec<_> = filesystems::CHOICES
            .iter()
            .map(|(suffix, name, hint)| (format!("{base}{suffix}"), *name, *hint))
            .filter(|(id, _, _)| self.viable().iter().any(|(definition, _)| &definition.id == id))
            .collect();

        // Never hide everything: finding no mkfs helper at all says more about
        // the probe than about the media.
        let creatable: Vec<_> = all
            .iter()
            .filter(|(_, name, _)| filesystems::mkfs_available(name))
            .cloned()
            .collect();

        if creatable.is_empty() { all } else { creatable }
    }

    /// The plan that would be applied if the current highlights were accepted
    fn preview(&self) -> Option<&StrategyPlan> {
        let id = match self.stage {
            Stage::Approach => {
                let position = self.approach_list.selected()?;

                match self.approaches().get(position) {
                    Some(&index) => self.viable()[index].0.id.clone(),
                    // The refresh row: preview what the recored strategy would do
                    None => self.discovered.as_ref()?.strategy.clone(),
                }
            }
            Stage::Filesystem => self.variants().get(self.filesystem_list.selected()?)?.0.clone(),
        };

        self.viable()
            .iter()
            .find(|(definition, _)| definition.id == id)
            .map(|(_, plan)| plan)
    }

    fn move_selection(&mut self, delta: isize) {
        let count = match self.stage {
            Stage::Approach => self.approach_rows(),
            Stage::Filesystem => self.variants().len(),
        };

        if count == 0 {
            return;
        }

        let list = match self.stage {
            Stage::Approach => &mut self.approach_list,
            Stage::Filesystem => &mut self.filesystem_list,
        };
        let current = list.selected().unwrap_or(0) as isize;
        let next = current.saturating_add(delta).clamp(0, count as isize - 1);

        list.select(Some(next as usize));
    }

    /// Settle the highlight after either half of the probe lands.
    ///
    /// The two messages race, so this runs for both rather than living inside
    /// one arm. Reinstalling over an existing AerynOS is the likeliest intent
    /// when one is found, so it starts highlighted, as the CLI does.
    fn select_default(&mut self, model: &Model) {
        if let Some(row) = self.refresh_row()
            && !model.imported
        {
            self.approach_list.select(Some(row));
            return;
        }

        let approaches = self.approaches();
        let wanted = filesystems::base(&model.storage.strategy_id).to_string();
        let selected = approaches
            .iter()
            .position(|&index| filesystems::base(&self.viable()[index].0.id) == wanted)
            .unwrap_or(0);

        self.approach_list.select((!approaches.is_empty()).then_some(selected));
    }

    fn advance(&mut self, model: &mut Model) -> Action {
        match self.stage {
            Stage::Filesystem => {
                let Some(position) = self.filesystem_list.selected() else {
                    return Action::Consumed;
                };
                let Some((id, _, _)) = self.variants().get(position).cloned() else {
                    return Action::Consumed;
                };

                self.commit(&id, model)
            }
            Stage::Approach => {
                if let Some(row) = self.refresh_row()
                    && self.approach_list.selected() == Some(row)
                {
                    return self.refresh(model);
                }

                let variants = self.variants();

                match variants.len() {
                    // An approach with a single filesystem, or none that can be
                    // named, has nothing to ask about
                    0 => {
                        let Some(index) = self
                            .approach_list
                            .selected()
                            .and_then(|position| self.approaches().get(position).copied())
                        else {
                            return Action::Consumed;
                        };
                        let id = self.viable()[index].0.id.clone();

                        self.commit(&id, model)
                    }
                    1 => {
                        let id = variants[0].0.clone();
                        self.commit(&id, model)
                    }
                    _ => {
                        let selected = variants
                            .iter()
                            .position(|(id, _, _)| *id == model.storage.strategy_id)
                            .unwrap_or(0);

                        self.filesystem_list.select(Some(selected));
                        self.stage = Stage::Filesystem;
                        Action::Consumed
                    }
                }
            }
        }
    }

    /// Adopt the settings and package set of the installation already on the
    /// disk, the partition with strategy it recorded.
    ///
    /// The package set is unioned with `mandatory` rather than taken as it
    /// stands: a model written by an older installer, or hand-edited since,
    /// must still produce something that boots.
    fn refresh(&mut self, model: &mut Model) -> Action {
        let Some(discovered) = &self.discovered else {
            return Action::Consumed;
        };

        match from_kdl(&discovered.contents) {
            Ok(parsed) => *model = parsed,
            Err(err) => {
                return Action::Failed(format!(
                    "failed to parse the model on {}: {}",
                    discovered.device,
                    parse_error_detail(&err)
                ));
            }
        }
        model.imported = true;

        let mut packages: BTreeSet<String> = model.software.packages.iter().cloned().collect();

        match mandatory(&model.software.selection) {
            Ok(required) => packages.extend(required),
            Err(err) => return Action::Failed(err.to_string()),
        }

        model.software.packages = packages.into_iter().collect();

        // Partition with the strategy the discovered model names, falling back
        // to the first that applies to this disk.
        let id = match self
            .viable()
            .iter()
            .find(|(definition, _)| definition.id == model.storage.strategy_id)
        {
            Some((definition, _)) => definition.id.clone(),
            None => match self.viable().first() {
                Some((definition, _)) => definition.id.clone(),
                None => return Action::Consumed,
            },
        };

        self.commit(&id, model)
    }

    fn commit(&mut self, id: &str, model: &mut Model) -> Action {
        let Some((definition, plan)) = self.viable().iter().find(|(definition, _)| definition.id == id) else {
            return Action::Consumed;
        };

        model.storage.strategy_id = definition.id.clone();
        model.storage.strategy_name = definition.name.clone();
        model.storage.plan = Some(plan.clone());

        // The root filesystem just changed, so its packages have to be re-derived.
        // A no-op unless a desktop has already been chosen. This is basically a
        // guard if someone changes their mind after chosing a desktop environment
        // so packages that aren't needed aren't installed and the ones that are
        // needed are not accidentally removed.
        if let Err(error) = packages_for(model) {
            return Action::Failed(error.to_string());
        }
        Action::Next
    }
}

impl Screen for Strategy {
    fn title(&self) -> &str {
        "Strategy"
    }

    fn hints(&self) -> &[(&str, &str)] {
        match self.stage {
            Stage::Approach => &[("↑↓", "choose"), ("⏎", "select")],
            Stage::Filesystem => &[("↑↓", "choose"), ("⏎", "select"), ("Esc", "back")],
        }
    }

    fn is_complete(&self, model: &Model) -> bool {
        model.storage.plan.is_some()
    }

    fn handle_key(&mut self, key: KeyEvent, model: &mut Model) -> Action {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                Action::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                Action::Consumed
            }
            KeyCode::Home => {
                self.move_selection(isize::MIN);
                Action::Consumed
            }
            KeyCode::End => {
                self.move_selection(isize::MAX);
                Action::Consumed
            }
            KeyCode::Enter => self.advance(model),
            KeyCode::Esc | KeyCode::Left if matches!(self.stage, Stage::Filesystem) => {
                self.stage = Stage::Approach;
                Action::Consumed
            }
            _ => Action::Ignored,
        }
    }

    fn on_enter(&mut self, ctx: &Context, model: &Model) {
        // A plan computed for a different disk is worthless, so a changed
        // disk re-probes rather than showing stale answers.
        if model.storage.disk.is_empty() || self.probed.as_deref() == Some(model.storage.disk.as_str()) {
            return;
        }

        self.probed = Some(model.storage.disk.clone());
        self.state = State::Loading;
        self.stage = Stage::Approach;
        self.discovered = None;

        let channel = ctx.channel.clone();
        let disk = model.storage.disk.clone();

        ctx.spawn(async move {
            let mut provisioner = ProvisionerClient::new(channel);
            let strategies = provisioner.list_strategies(()).await?.into_inner().strategies;
            let mut viable = Vec::new();

            // One probe per strategy, sequentially, but on a background task so the
            // interface stays live for however long the backend takes.
            for definition in strategies {
                let plans = provisioner
                    .try_strategy(TryStrategyRequest {
                        strategy: definition.id.clone(),
                        disks: vec![disk.clone()],
                    })
                    .await?
                    .into_inner()
                    .plans;

                if let Some(plan) = plans.into_iter().next() {
                    viable.push((definition, plan));
                }
            }

            Ok(Msg::Strategies(viable))
        });

        // Independently of the probe, and far faster: an installation already
        // on this disk can supply its own settings and package set.
        let channel = ctx.channel.clone();
        let disk = model.storage.disk.clone();

        ctx.spawn(async move {
            // Matched by partition rather than by name prefix, so /dev/sda
            // cannot claim a model found on /dev/sdaa.
            let partitions: Vec<String> = DisksClient::new(channel.clone())
                .list_disks(ListDisksRequest {
                    exclude_loopback: false,
                })
                .await?
                .into_inner()
                .disks
                .into_iter()
                .find(|candidate| candidate.device == disk)
                .map(|candidate| candidate.partitions.into_iter().map(|part| part.device).collect())
                .unwrap_or_default();
            let discovered = InstallClient::new(channel)
                .discover_system_models(())
                .await?
                .into_inner()
                .models
                .into_iter()
                .find(|found| partitions.iter().any(|device| device == &found.device));

            Ok(Msg::Discovered(discovered))
        });
    }

    fn on_message(&mut self, msg: &Msg, model: &mut Model) {
        match msg {
            Msg::Strategies(viable) => {
                self.state = State::Ready(viable.clone());
                self.stage = Stage::Approach;
            }
            Msg::Discovered(discovered) => {
                self.discovered = discovered.as_ref().map(|found| Discovered {
                    device: found.device.clone(),
                    contents: found.contents.clone(),
                    strategy: from_kdl(&found.contents)
                        .map(|parsed| parsed.storage.strategy_id)
                        .unwrap_or_default(),
                });
            }
            _ => return,
        }

        self.select_default(model);
    }

    fn render(&mut self, frame: &mut Frame<'_>, area: Rect, model: &Model) {
        let [heading, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(area);
        let question = match self.stage {
            Stage::Approach => "How should the disk be partitioned?",
            Stage::Filesystem => "Which filesystem should the root partition use?",
        };

        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(question, HEADING),
                Line::styled(model.storage.disk_display.clone(), HINT),
            ]),
            heading,
        );

        if matches!(self.state, State::Loading) {
            frame.render_widget(
                Paragraph::new(Line::styled("Working out what can be done with this disk...", HINT)),
                body,
            );
            return;
        }

        if self.viable().is_empty() {
            frame.render_widget(
                Paragraph::new(format!(
                    "No partitioning strategy applies to {}.\n\n\
                     It may be too small, or already laid out in a way no strategy can work with.",
                    model.storage.disk
                ))
                .style(WARNING)
                .wrap(Wrap { trim: false }),
                body,
            );
            return;
        }

        let [choices, preview] =
            Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(body);

        self.render_choices(frame, choices);
        self.render_preview(frame, preview);
    }
}

impl Strategy {
    fn render_choices(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let (items, list) = match self.stage {
            Stage::Approach => {
                let mut items: Vec<ListItem<'_>> = self
                    .approaches()
                    .iter()
                    .map(|&index| {
                        let definition = &self.viable()[index].0;

                        ListItem::new(vec![
                            Line::styled(filesystems::base(&definition.name).to_string(), BODY),
                            Line::styled(format!("  {}", definition.description), HINT),
                        ])
                    })
                    .collect();

                if let Some(discovered) = &self.discovered {
                    items.push(ListItem::new(vec![
                        Line::styled("Refresh OS", BODY),
                        Line::styled(
                            format!("  Reinstall with the settings found on {}", discovered.device),
                            HINT,
                        ),
                    ]));
                }
                (items, &mut self.approach_list)
            }
            Stage::Filesystem => {
                let items: Vec<ListItem<'_>> = self
                    .variants()
                    .iter()
                    .map(|(_, name, hint)| {
                        ListItem::new(vec![
                            Line::styled(name.to_string(), BODY),
                            Line::styled(format!("  {hint}"), HINT),
                        ])
                    })
                    .collect();
                (items, &mut self.filesystem_list)
            }
        };

        frame.render_stateful_widget(
            List::new(items).highlight_style(SELECTED).highlight_symbol(CURSOR),
            area,
            list,
        );
    }

    /// The consequences of the current highlight, updated as it moves.
    fn render_preview(&self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(FRAME)
            .padding(Padding::left(2));
        let inner = block.inner(area);

        frame.render_widget(block, area);

        let mut lines = vec![Line::styled("Planned changes", HINT), Line::raw("")];

        match self.preview() {
            Some(plan) => lines.extend(plan::describe(plan)),
            None => lines.push(Line::styled("Nothing to preview", HINT)),
        }

        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}
