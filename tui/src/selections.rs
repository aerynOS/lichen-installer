// SPDX-FileCopyrightText: Copyright © 2026 AerynOS Developers
//
// SPDX-License-Identifier: MPL-2.0

//! Embedded package selections for the target installation

use installer::StepError;
use kdl::{KdlDocument, KdlNode};
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

/// Always part of an installation, never offered as a choice
const IMPLICIT: &[&str] = &["kernel-common", "kernel-desktop"];
/// Structural rather than user-facing
const HIDDEN: &[&str] = &["base", "desktop-common", "develop", "kernel-common", "kernel-desktop"];

/// Parse all embedded selections
pub fn all() -> Vec<Selection> {
    RAW_SELECTIONS.iter().map(|raw| parse(raw)).collect()
}

/// The user facing desktop choices
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

/// The packages every installation must carry regardless of what an imported
/// model lists: the base system and kernel closures
pub fn mandatory(selection: &str) -> Result<Vec<String>, StepError> {
    if selection == "server" {
        resolve("base")
    } else {
        resolve("desktop-common")
    }
}

/// A string property of a KDL node.
pub(crate) fn prop<'a>(node: &'a KdlNode, key: &str) -> Option<&'a str> {
    node.get(key).and_then(|value| value.as_string())
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
        let packages = resolve("gnome").expect("gnome must resolve");
        assert!(packages.contains(&"gnome-desktop-defaults".to_string()));
        assert!(packages.contains(&"mesa-dri-drivers".to_string()));
        assert!(packages.contains(&"bash".to_string()));
        assert!(packages.contains(&"linux-stable".to_string()));
    }

    #[test]
    fn mandatory_covers_boot_essentials() {
        let desktop = mandatory("plasma").expect("desktop-common must resolve");
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
