// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Embedded package selections for the target installation
//!
//! Selections are the KDL definitions in `data/selections/`, compiled into
//! the binary. Each names its required packages/providers and the other
//! selections it depends on.

use crate::install_model::prop;
use installer::StepError;
use kdl::KdlDocument;
use std::collections::{BTreeSet, HashMap, HashSet};

/// A selection definition loaded from data/selections
#[derive(Debug, Clone)]
pub struct Selection {
    pub name: String,
    pub summary: String,
    pub description: String,
    pub depends: Vec<String>,
    pub packages: Vec<String>,
}

/// Raw embedded selection documents
const RAW_SELECTIONS: &[&str] = &[
    include_str!("../../data/selections/base.kdl"),
    include_str!("../../data/selections/desktop-common.kdl"),
    include_str!("../../data/selections/cosmic.kdl"),
    include_str!("../../data/selections/develop.kdl"),
    include_str!("../../data/selections/gnome.kdl"),
    include_str!("../../data/selections/plasma.kdl"),
    include_str!("../../data/selections/windowmanager.kdl"),
    include_str!("../../data/selections/server.kdl"),
    include_str!("../../data/selections/kernel-common.kdl"),
    include_str!("../../data/selections/kernel-desktop.kdl"),
];

/// Selections that are always part of an installation and never offered
/// as a user-facing choice.
const IMPLICIT: &[&str] = &["kernel-common", "kernel-desktop"];

/// Selections that are structural rather than
/// user-facing.
const HIDDEN: &[&str] = &["base", "desktop-common", "develop", "kernel-common", "kernel-desktop"];

/// Parse all embedded selections
pub fn all() -> Vec<Selection> {
    RAW_SELECTIONS.iter().map(|raw| parse(raw)).collect()
}

/// The user-facing desktop choices
pub fn desktops() -> Vec<Selection> {
    all()
        .into_iter()
        .filter(|selection| !HIDDEN.contains(&selection.name.as_str()))
        .collect()
}

/// Resolve a chosen selection into the full, sorted package/provider list:
/// the selection itself, its dependency closure, and the implicit selections
pub fn resolve(name: &str) -> Result<Vec<String>, StepError> {
    let selections = all();
    let by_name: HashMap<&str, &Selection> = selections
        .iter()
        .map(|selection| (selection.name.as_str(), selection))
        .collect();
    let mut pending: Vec<String> = IMPLICIT.iter().map(|selection| selection.to_string()).collect();

    pending.push(name.to_string());

    let mut visited = HashSet::new();
    let mut packages = BTreeSet::new();

    while let Some(current) = pending.pop() {
        if !visited.insert(current.clone()) {
            continue;
        }

        let selection = by_name
            .get(current.as_str())
            .ok_or_else(|| StepError::Failed(format!("unknown selection: {current}")))?;

        packages.extend(selection.packages.iter().cloned());
        pending.extend(selection.depends.iter().cloned());
    }

    Ok(packages.into_iter().collect())
}

/// Best-effort identification of the selection from an imported package set
/// came from. A bare system-model records packages but no selection name, so the only
/// way to name the desktop is to find which selection's closure the set fully covers;
/// preferring the most specific where several nest.
pub fn identify(packages: &[String]) -> Option<String> {
    /// How much of a selection's closure must be present to be considered
    const MIN_COVERAGE: f64 = 0.9;

    let have: HashSet<&str> = packages.iter().map(String::as_str).collect();

    desktops()
        .into_iter()
        .filter_map(|selection| {
            let closure: Vec<String> = resolve(&selection.name)
                .ok()?
                .into_iter()
                .filter(|package| !package.contains('('))
                .collect();

            if closure.is_empty() {
                return None;
            }

            let matched = closure.iter().filter(|package| have.contains(package.as_str())).count();
            let coverage = matched as f64 / closure.len() as f64;

            // Tie-break on closure size so the result is stable rather than
            // dependent on selection order.
            (coverage >= MIN_COVERAGE).then_some((selection.name, matched, closure.len()))
        })
        .max_by_key(|(_, matched, size)| (*matched, *size))
        .map(|(name, _, _)| name)
}

/// Parse one embedded selection document
fn parse(raw: &str) -> Selection {
    let doc: KdlDocument = raw.parse().expect("embedded selection KDL must be valid");
    let node = doc
        .get("selection")
        .expect("embedded selection must have a selection node");
    let list = |key: &str| -> Vec<String> {
        node.children()
            .and_then(|children| children.get(key))
            .map(|list| {
                list.iter_children()
                    .map(|child| child.name().value().to_string())
                    .collect()
            })
            .unwrap_or_default()
    };

    Selection {
        name: prop(node, "name").expect("selection name is required").to_string(),
        summary: prop(node, "summary").unwrap_or_default().to_string(),
        description: prop(node, "description").unwrap_or_default().to_string(),
        depends: list("depends"),
        packages: list("packages"),
    }
}

/// The packages every installation must carry regardless of what an imported
/// model lists: the base system and kernel closures
pub fn mandatory(selection: &str) -> Result<Vec<String>, StepError> {
    match selection {
        // An absent selection means a bare system-model, whose floor is base.
        // Defaulting to desktop-common would force a desktop onto a headless
        // import.
        "server" | "" => resolve("base"),
        _ => resolve("desktop-common"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_selections_parse() {
        let selections = all();
        assert_eq!(selections.len(), 10);
        assert!(desktops().iter().any(|sel| sel.name == "cosmic"));
        assert!(desktops().iter().any(|sel| sel.name == "server"));
        assert!(desktops().iter().all(|sel| !HIDDEN.contains(&sel.name.as_str())));
    }

    #[test]
    fn resolve_includes_dependency_closure() {
        let packages = resolve("cosmic").expect("cosmic must resolve");

        assert!(packages.contains(&"cosmic-comp".to_string()));
        assert!(packages.contains(&"mesa-dri-drivers".to_string()));
        assert!(packages.contains(&"bash".to_string()));
        assert!(packages.contains(&"linux-stable".to_string()));
    }

    #[test]
    fn identify_tolerates_selection_drift() {
        let mut gnome = resolve("gnome").expect("gnome must resolve");
        gnome.retain(|package| package != "font-opensans");

        assert_eq!(identify(&gnome).as_deref(), Some("gnome"));
        assert_eq!(identify(&resolve("server").unwrap()).as_deref(), Some("server"));
        assert_eq!(identify(&[]), None);
    }

    #[test]
    fn mandatory_covers_boot_essentials() {
        let desktop = mandatory("cosmic").expect("desktop-common must resolve");
        assert!(desktop.contains(&"systemd-udev".to_string()));
        assert!(desktop.contains(&"linux-stable".to_string()));
        assert!(desktop.contains(&"mesa-dri-drivers".to_string()));

        let server = mandatory("server").expect("base must resolve");
        assert!(server.contains(&"systemd-udev".to_string()));
        assert!(
            !server.contains(&"mesa-dri-drivers".to_string()),
            "server stays headless"
        );
    }

    #[test]
    fn unknown_selection_fails() {
        assert!(resolve("no-such-selection").is_err())
    }
}
