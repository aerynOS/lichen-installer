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
    filesystems, plan,
    selections::packages_for,
    theme::*,
};
use installer::Model;
use protocols::lichen::storage::provisioner::{
    StrategyDefinition, StrategyPlan, TryStrategyRequest, provisioner_client::ProvisionerClient,
};
use ratatui::{
    Frame,
    crossterm::event::{KeyCode, KeyEvent},
    layout::{Constraint, Layout, Rect},
    text::Line,
    widgets::{Block, Borders, List, ListItem, ListState, Padding, Paragraph, Wrap},
};

/// A strategy and the plan it produced for the chosen disk
type Viable = (StrategyDefinition, StrategyPlan);

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
}

impl Strategy {
    pub fn new() -> Self {
        Self {
            state: State::Loading,
            stage: Stage::Approach,
            approach_list: ListState::default(),
            filesystem_list: ListState::default(),
            probed: None,
        }
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
                let index = *self.approaches().get(position)?;

                self.viable()[index].0.id.clone()
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
            Stage::Approach => self.approaches().len(),
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
    }

    fn on_message(&mut self, msg: &Msg, model: &mut Model) {
        let Msg::Strategies(viable) = msg else {
            return;
        };

        self.state = State::Ready(viable.clone());
        self.stage = Stage::Approach;

        let approaches = self.approaches();
        let wanted = filesystems::base(&model.storage.strategy_id).to_string();
        let selected = approaches
            .iter()
            .position(|&index| filesystems::base(&self.viable()[index].0.id) == wanted)
            .unwrap_or(0);

        self.approach_list.select((!approaches.is_empty()).then_some(selected));
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
                let items: Vec<ListItem<'_>> = self
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
            List::new(items).highlight_style(SELECTED).highlight_symbol("▸ "),
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
