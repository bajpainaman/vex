//===- resolve.rs - Vx Package Tool ----------------------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// Picks one version of each registry package with minimal version selection, as Go modules do:
// every package names the lowest version of a dependency it works with, and the build takes the
// highest of those lowest versions. It never takes a version nobody asked for, so publishing a new
// version does not change anyone's build until they ask for it.
//
//===----------------------------------------------------------------------===//
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};

use crate::registry::{Registry, Release};
use crate::version::Version;

/// "`wanted_by` needs `name` at `minimum` or a later compatible version."
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    pub name: String,
    pub minimum: Version,
    /// The package that asks, as a person reads it: `app`, or `shapes 1.2.0`.
    pub wanted_by: String,
}

/// The release of each registry package the build uses, by name.
///
/// # Errors
///
/// If the registry lacks a version someone asks for, or two packages need versions of one package
/// that cannot replace each other, such as 1.x and 2.x.
pub fn select_versions(
    root_requirements: &[Requirement],
    registry: &mut Registry,
) -> Result<BTreeMap<String, Release>> {
    // Raise each package to the highest version anything reachable asks for. Versions only go up,
    // so this stops.
    let mut selected: BTreeMap<String, Version> = BTreeMap::new();
    let mut seen: BTreeSet<(String, Version)> = BTreeSet::new();
    let mut work: Vec<Requirement> = root_requirements.to_vec();
    while let Some(requirement) = work.pop() {
        if !seen.insert((requirement.name.clone(), requirement.minimum)) {
            continue;
        }
        let release = registry.release(&requirement.name, requirement.minimum)?;
        let highest = selected
            .entry(requirement.name.clone())
            .or_insert(requirement.minimum);
        *highest = (*highest).max(requirement.minimum);
        for (name, minimum) in &release.dependencies {
            work.push(Requirement {
                name: name.clone(),
                minimum: *minimum,
                wanted_by: format!("{} {}", release.name, release.version),
            });
        }
    }

    // Keep only what the selected versions still need: a requirement of a version that was
    // replaced by a later one is not part of the build.
    let mut releases: BTreeMap<String, Release> = BTreeMap::new();
    let mut wanted: BTreeMap<String, Vec<Requirement>> = BTreeMap::new();
    let mut work: Vec<Requirement> = root_requirements.to_vec();
    while let Some(requirement) = work.pop() {
        wanted
            .entry(requirement.name.clone())
            .or_default()
            .push(requirement.clone());
        if releases.contains_key(&requirement.name) {
            continue;
        }
        let release = registry.release(&requirement.name, selected[&requirement.name])?;
        for (name, minimum) in &release.dependencies {
            work.push(Requirement {
                name: name.clone(),
                minimum: *minimum,
                wanted_by: format!("{} {}", release.name, release.version),
            });
        }
        releases.insert(requirement.name.clone(), release);
    }

    for (name, requirements) in &wanted {
        let chosen = releases[name].version;
        let incompatible: Vec<&Requirement> = requirements
            .iter()
            .filter(|r| r.minimum.compatibility_line() != chosen.compatibility_line())
            .collect();
        if incompatible.is_empty() {
            continue;
        }
        let mut lines: Vec<String> = requirements
            .iter()
            .map(|r| {
                format!(
                    "  {} needs {} ({})",
                    r.wanted_by,
                    r.minimum,
                    r.minimum.compatibility_line_name()
                )
            })
            .collect();
        lines.sort();
        lines.dedup();
        bail!(
            "packages need versions of `{name}` that cannot replace each other, and a build holds \
             one copy of each package:\n{}",
            lines.join("\n")
        );
    }
    Ok(releases)
}

#[cfg(test)]
#[allow(
    clippy::missing_panics_doc,
    reason = "a test reports a failure by panicking"
)]
mod tests {
    use super::*;
    use crate::registry::sha256_hex;

    /// A release written `("name", "version", &[("dependency", "minimum version")])`.
    type TestRelease<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

    /// A registry in a temporary directory with the given releases.
    fn registry_with(dir: &std::path::Path, releases: &[TestRelease<'_>]) -> Registry {
        let mut registry =
            Registry::open(dir.to_str().expect("a UTF-8 path")).expect("a directory registry");
        for (name, version, dependencies) in releases {
            let archive = format!("{name} {version}");
            let release = Release {
                name: (*name).to_string(),
                version: Version::parse(version).expect("a valid version"),
                dependencies: dependencies
                    .iter()
                    .map(|(n, v)| {
                        (
                            (*n).to_string(),
                            Version::parse(v).expect("a valid version"),
                        )
                    })
                    .collect(),
                sha256: sha256_hex(archive.as_bytes()),
                description: None,
            };
            registry
                .publish(&release, archive.as_bytes())
                .expect("publish a test release");
        }
        registry
    }

    fn app_needs(dependencies: &[(&str, &str)]) -> Vec<Requirement> {
        dependencies
            .iter()
            .map(|(name, version)| Requirement {
                name: (*name).to_string(),
                minimum: Version::parse(version).expect("a valid version"),
                wanted_by: "app".to_string(),
            })
            .collect()
    }

    fn chosen(releases: &BTreeMap<String, Release>) -> Vec<String> {
        releases
            .values()
            .map(|r| format!("{} {}", r.name, r.version))
            .collect()
    }

    #[test]
    fn the_lowest_version_asked_for_is_used_even_when_a_later_one_exists() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let mut registry = registry_with(
            dir.path(),
            &[("shapes", "1.0.0", &[]), ("shapes", "1.3.0", &[])],
        );
        let releases =
            select_versions(&app_needs(&[("shapes", "1.0.0")]), &mut registry).expect("resolve");
        assert_eq!(chosen(&releases), ["shapes 1.0.0"]);
    }

    #[test]
    fn the_highest_of_the_versions_asked_for_wins() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let mut registry = registry_with(
            dir.path(),
            &[
                ("numbers", "1.1.0", &[]),
                ("numbers", "1.4.0", &[]),
                ("numbers", "1.9.0", &[]),
                ("shapes", "2.0.0", &[("numbers", "1.4.0")]),
            ],
        );
        let releases = select_versions(
            &app_needs(&[("shapes", "2.0.0"), ("numbers", "1.1.0")]),
            &mut registry,
        )
        .expect("resolve");
        assert_eq!(chosen(&releases), ["numbers 1.4.0", "shapes 2.0.0"]);
    }

    #[test]
    fn a_dependency_of_a_replaced_version_is_dropped() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let mut registry = registry_with(
            dir.path(),
            &[
                ("old_helper", "1.0.0", &[]),
                ("shapes", "1.0.0", &[("old_helper", "1.0.0")]),
                ("shapes", "1.1.0", &[]),
                ("text", "1.0.0", &[("shapes", "1.1.0")]),
            ],
        );
        let releases = select_versions(
            &app_needs(&[("shapes", "1.0.0"), ("text", "1.0.0")]),
            &mut registry,
        )
        .expect("resolve");
        assert_eq!(chosen(&releases), ["shapes 1.1.0", "text 1.0.0"]);
    }

    #[test]
    fn two_major_versions_of_one_package_are_refused() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let mut registry = registry_with(
            dir.path(),
            &[
                ("numbers", "1.0.0", &[]),
                ("numbers", "2.0.0", &[]),
                ("shapes", "1.0.0", &[("numbers", "2.0.0")]),
            ],
        );
        let error = format!(
            "{:#}",
            select_versions(
                &app_needs(&[("shapes", "1.0.0"), ("numbers", "1.0.0")]),
                &mut registry
            )
            .expect_err("numbers 1.x and 2.x")
        );
        assert!(
            error.contains("versions of `numbers` that cannot replace each other"),
            "{error}"
        );
        assert!(error.contains("app needs 1.0.0 (1.x)"), "{error}");
        assert!(error.contains("shapes 1.0.0 needs 2.0.0 (2.x)"), "{error}");
    }

    #[test]
    fn below_1_0_0_a_new_minor_version_is_a_different_package() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let mut registry = registry_with(
            dir.path(),
            &[
                ("numbers", "0.3.0", &[]),
                ("numbers", "0.4.0", &[]),
                ("shapes", "1.0.0", &[("numbers", "0.4.0")]),
            ],
        );
        let error = format!(
            "{:#}",
            select_versions(
                &app_needs(&[("shapes", "1.0.0"), ("numbers", "0.3.0")]),
                &mut registry
            )
            .expect_err("numbers 0.3.x and 0.4.x")
        );
        assert!(error.contains("app needs 0.3.0 (0.3.x)"), "{error}");
    }

    #[test]
    fn a_version_that_was_never_published_is_an_error() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let mut registry = registry_with(dir.path(), &[("shapes", "1.0.0", &[])]);
        let error = format!(
            "{:#}",
            select_versions(&app_needs(&[("shapes", "1.2.0")]), &mut registry)
                .expect_err("shapes 1.2.0 does not exist")
        );
        assert!(
            error.contains("has no version 1.2.0 of `shapes`"),
            "{error}"
        );
    }

    #[test]
    fn packages_that_depend_on_each_other_still_resolve() {
        // A circle between registry packages is caught later, by the package graph, with the
        // same message as for local packages. Picking versions must not loop forever on it.
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let mut registry = registry_with(
            dir.path(),
            &[
                ("ping", "1.0.0", &[("pong", "1.0.0")]),
                ("pong", "1.0.0", &[("ping", "1.0.0")]),
            ],
        );
        let releases =
            select_versions(&app_needs(&[("ping", "1.0.0")]), &mut registry).expect("resolve");
        assert_eq!(chosen(&releases), ["ping 1.0.0", "pong 1.0.0"]);
    }
}
