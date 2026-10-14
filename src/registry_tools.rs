//===- registry_tools.rs - Vx Package Tool ---------------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// Tools for the people who run a registry: `vex registry verify` checks every published version,
// as a registry's CI does before it accepts a new one, and `vex registry json` writes the
// `packages.json` that the registry's web pages and `vex search` read.
//
//===----------------------------------------------------------------------===//
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::archive;
use crate::graph::check_matches_index;
use crate::manifest::{Manifest, MANIFEST_FILE_NAME};
use crate::registry::{Registry, Release};
use crate::version::Version;

/// The longest README `packages.json` carries for one package. The web page shows the start of a
/// longer one, and the archive has all of it.
const MAX_README_BYTES: usize = 64 * 1024;

/// The directory of a package's test programs, as `vex test` names it.
const TESTS_DIR: &str = "tests";

/// Where `vex registry json` output goes in a registry, and where `vex search` looks for it.
pub const PACKAGES_FILE_NAME: &str = "packages.json";

/// Unpack `release` into a fresh temporary directory, call `inspect` on it, and remove it.
///
/// # Errors
///
/// If the archive cannot be fetched or unpacked, or `inspect` fails.
fn with_unpacked<T>(
    registry: &Registry,
    release: &Release,
    inspect: impl FnOnce(&Path) -> Result<T>,
) -> Result<T> {
    let dir = env::temp_dir().join(format!(
        "vex-registry-{}-{}-{}",
        process::id(),
        release.name,
        release.version
    ));
    if dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("cannot clear `{}`", dir.display()))?;
    }
    fs::create_dir_all(&dir).with_context(|| format!("cannot create `{}`", dir.display()))?;
    let result = registry
        .archive(release)
        .and_then(|bytes| archive::unpack(&bytes, &dir))
        .and_then(|()| inspect(&dir));
    fs::remove_dir_all(&dir).with_context(|| format!("cannot remove `{}`", dir.display()))?;
    result
}

/// Check every version of every package. Returns how many versions were checked.
///
/// # Errors
///
/// If any version is damaged; the error lists every problem found, not just the first.
pub fn verify(registry: &mut Registry) -> Result<usize> {
    let mut problems: Vec<String> = Vec::new();
    let mut checked = 0;
    for name in registry.package_names()? {
        let releases = match registry.releases(&name) {
            Ok(releases) => releases.to_vec(),
            Err(error) => {
                problems.push(format!("{error:#}"));
                continue;
            }
        };
        for release in &releases {
            checked += 1;
            if let Err(error) = verify_release(registry, release) {
                problems.push(format!("{} {}: {error:#}", release.name, release.version));
            }
        }
    }
    if !problems.is_empty() {
        bail!(
            "{} problem(s) in the registry:\n  {}",
            problems.len(),
            problems.join("\n  ")
        );
    }
    Ok(checked)
}

/// # Errors
///
/// If the archive does not match the index, holds a package that does not match its index line,
/// names a directory under `[link]`, or needs a version the registry does not have.
fn verify_release(registry: &mut Registry, release: &Release) -> Result<()> {
    let link_dirs = with_unpacked(registry, release, |dir| {
        check_matches_index(dir, release)?;
        let manifest = Manifest::load(&dir.join(MANIFEST_FILE_NAME))?;
        Ok(manifest.link.dirs.len())
    })?;
    if link_dirs > 0 {
        bail!("it names `dirs` under `[link]`, which only exist on the computer that published it");
    }
    for (dependency, minimum) in &release.dependencies {
        registry.release(dependency, *minimum)?;
    }
    Ok(())
}

#[derive(Serialize)]
struct PackagesFile {
    packages: Vec<PackageSummary>,
}

#[derive(Serialize)]
struct PackageSummary {
    name: String,
    description: Option<String>,
    latest: Version,
    /// Newest first.
    versions: Vec<VersionSummary>,
    /// The C libraries the latest version links.
    links: Vec<String>,
    /// The `.vx` files of the latest version, which are what `import name::..` can name.
    modules: Vec<String>,
    /// The packages whose latest version depends on this one.
    used_by: Vec<String>,
    readme: Option<String>,
}

#[derive(Serialize)]
struct VersionSummary {
    version: Version,
    dependencies: BTreeMap<String, Version>,
    sha256: String,
}

/// What `packages.json` shows of a package's latest version, read from its archive.
struct LatestContents {
    links: Vec<String>,
    modules: Vec<String>,
    readme: Option<String>,
}

/// # Errors
///
/// If the package files cannot be read.
fn read_latest_contents(dir: &Path) -> Result<LatestContents> {
    let manifest = Manifest::load(&dir.join(MANIFEST_FILE_NAME))?;
    let files = archive::package_files(dir)?;
    let modules = files
        .iter()
        // Test programs are packed so a registry can run them, but nothing imports them.
        .filter(|path| path.extension().is_some_and(|e| e == "vx") && !path.starts_with(TESTS_DIR))
        .map(|path| {
            path.with_extension("")
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("::")
        })
        .collect();
    let readme_file: Option<&PathBuf> = files.iter().find(|path| {
        path.components().count() == 1
            && path
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("README")
    });
    let readme = match readme_file {
        Some(file) => {
            let mut text = fs::read_to_string(dir.join(file))
                .with_context(|| format!("cannot read `{}`", file.display()))?;
            if text.len() > MAX_README_BYTES {
                let mut end = MAX_README_BYTES;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
            }
            Some(text)
        }
        None => None,
    };
    Ok(LatestContents {
        links: manifest.link.libraries,
        modules,
        readme,
    })
}

/// Write `packages.json` for every package in the registry to `output`.
///
/// # Errors
///
/// If the registry cannot be read, or `output` cannot be written.
pub fn write_json(registry: &mut Registry, output: &Path) -> Result<usize> {
    let mut packages = Vec::new();
    let mut used_by: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for name in registry.package_names()? {
        let releases = registry.releases(&name)?.to_vec();
        let Some(latest) = releases.last().cloned() else {
            continue;
        };
        for dependency in latest.dependencies.keys() {
            used_by
                .entry(dependency.clone())
                .or_default()
                .insert(name.clone());
        }
        let contents = with_unpacked(registry, &latest, read_latest_contents)
            .with_context(|| format!("while reading `{name}` {}", latest.version))?;
        packages.push(PackageSummary {
            name: name.clone(),
            description: latest.description.clone(),
            latest: latest.version,
            versions: releases
                .iter()
                .rev()
                .map(|release| VersionSummary {
                    version: release.version,
                    dependencies: release.dependencies.clone(),
                    sha256: release.sha256.clone(),
                })
                .collect(),
            links: contents.links,
            modules: contents.modules,
            used_by: Vec::new(),
            readme: contents.readme,
        });
    }
    for package in &mut packages {
        if let Some(users) = used_by.remove(&package.name) {
            package.used_by = users.into_iter().collect();
        }
    }
    let count = packages.len();
    let text = serde_json::to_string_pretty(&PackagesFile { packages })
        .context("cannot write packages.json")?;
    if let Some(dir) = output.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir).with_context(|| format!("cannot create `{}`", dir.display()))?;
    }
    fs::write(output, text).with_context(|| format!("cannot write `{}`", output.display()))?;
    Ok(count)
}

/// The packages whose name or description contains `text`, ignoring case, as lines of
/// `name version  description`.
///
/// # Errors
///
/// If the registry has no `packages.json` or it cannot be read.
pub fn search(registry: &Registry, text: &str) -> Result<Vec<String>> {
    let Some(bytes) = registry.read(PACKAGES_FILE_NAME)? else {
        bail!(
            "the registry {} has no `{PACKAGES_FILE_NAME}`, so it cannot be searched. Its owner \
             makes one with `vex registry json`",
            registry.describe()
        );
    };
    let file: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("`{PACKAGES_FILE_NAME}` is damaged"))?;
    let wanted = text.to_lowercase();
    let mut lines = Vec::new();
    for package in file["packages"].as_array().into_iter().flatten() {
        let name = package["name"].as_str().unwrap_or_default();
        let description = package["description"].as_str().unwrap_or_default();
        if name.to_lowercase().contains(&wanted) || description.to_lowercase().contains(&wanted) {
            let latest = package["latest"].as_str().unwrap_or_default();
            lines.push(
                format!("{name} {latest}  {description}")
                    .trim_end()
                    .to_string(),
            );
        }
    }
    Ok(lines)
}
