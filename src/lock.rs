//===- lock.rs - Vx Package Tool -------------------------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// `vex.lock` records the version and SHA-256 of every registry package a build used. Versions are
// already fixed by `vex.toml` (see resolve.rs); the lock file makes sure the bytes are too, so a
// registry that changes a published package is caught instead of trusted.
//
//===----------------------------------------------------------------------===//
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};

use crate::registry::Release;
use crate::version::Version;

const LOCK_FILE_NAME: &str = "vex.lock";

/// Raise this when the file's layout changes, so an old `vex` refuses a lock file it would
/// misread.
const LOCK_FORMAT_VERSION: u32 = 1;

const HEADER: &str = "# Written by vex. Commit this file, and do not edit it by hand.\n";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LockFile {
    version: u32,
    #[serde(default, rename = "package")]
    packages: Vec<LockedPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LockedPackage {
    name: String,
    version: Version,
    sha256: String,
}

/// What to do when the lock file would change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// Write the new lock file.
    Update,
    /// Refuse, as a CI build should: the lock file must already be right.
    Frozen,
}

/// Check `releases` against the `vex.lock` in `package_dir`, then write the file if it changed.
///
/// # Errors
///
/// If the lock file cannot be read, records a different SHA-256 for a version the build uses, or
/// would change in [`LockMode::Frozen`].
pub fn check_and_update(
    package_dir: &Path,
    releases: &BTreeMap<String, Release>,
    mode: LockMode,
) -> Result<()> {
    let path = lock_path(package_dir);
    let old_text = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read `{}`", path.display()))
        }
    };
    if let Some(text) = &old_text {
        check(&parse(text, &path)?, releases)?;
    }

    let new_text = render(releases)?;
    if old_text.as_deref() == Some(new_text.as_str()) {
        return Ok(());
    }
    if releases.is_empty() && old_text.is_none() {
        return Ok(());
    }
    ensure!(
        mode == LockMode::Update,
        "`{}` is {}, and --locked does not allow changing it. Run vex without --locked and \
         commit the new file",
        path.display(),
        if old_text.is_some() {
            "out of date"
        } else {
            "missing"
        }
    );
    fs::write(&path, new_text).with_context(|| format!("cannot write `{}`", path.display()))
}

/// # Errors
///
/// If `text` is not a lock file this `vex` can read.
fn parse(text: &str, path: &Path) -> Result<LockFile> {
    let lock: LockFile =
        toml::from_str(text).with_context(|| format!("`{}` is damaged", path.display()))?;
    ensure!(
        lock.version == LOCK_FORMAT_VERSION,
        "`{}` has format version {}, and this vex reads only version {LOCK_FORMAT_VERSION}",
        path.display(),
        lock.version
    );
    Ok(lock)
}

/// # Errors
///
/// If the lock file pins a different archive for a version the build uses.
fn check(lock: &LockFile, releases: &BTreeMap<String, Release>) -> Result<()> {
    for locked in &lock.packages {
        let Some(release) = releases.get(&locked.name) else {
            continue;
        };
        if release.version == locked.version && release.sha256 != locked.sha256 {
            bail!(
                "the registry's `{}` {} has SHA-256 {}, but vex.lock recorded {}. A published \
                 package must never change, so vex will not use it. If you trust the new archive, \
                 remove its entry from vex.lock",
                locked.name,
                locked.version,
                release.sha256,
                locked.sha256
            );
        }
    }
    Ok(())
}

/// # Errors
///
/// Never in practice; TOML serialization of these types cannot fail.
fn render(releases: &BTreeMap<String, Release>) -> Result<String> {
    let lock = LockFile {
        version: LOCK_FORMAT_VERSION,
        packages: releases
            .values()
            .map(|release| LockedPackage {
                name: release.name.clone(),
                version: release.version,
                sha256: release.sha256.clone(),
            })
            .collect(),
    };
    Ok(format!(
        "{HEADER}{}",
        toml::to_string(&lock).context("cannot write vex.lock")?
    ))
}

/// Where the lock file of the package in `dir` lives.
fn lock_path(dir: &Path) -> PathBuf {
    dir.join(LOCK_FILE_NAME)
}

#[cfg(test)]
#[allow(
    clippy::missing_panics_doc,
    reason = "a test reports a failure by panicking"
)]
mod tests {
    use super::*;

    fn releases(entries: &[(&str, &str, char)]) -> BTreeMap<String, Release> {
        entries
            .iter()
            .map(|(name, version, digit)| {
                (
                    (*name).to_string(),
                    Release {
                        name: (*name).to_string(),
                        version: Version::parse(version).expect("a valid version"),
                        dependencies: BTreeMap::new(),
                        sha256: digit.to_string().repeat(64),
                        description: None,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn the_lock_file_is_written_and_then_left_alone() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let used = releases(&[("shapes", "1.0.0", 'a')]);
        check_and_update(dir.path(), &used, LockMode::Update).expect("write vex.lock");
        let text = fs::read_to_string(lock_path(dir.path())).expect("read vex.lock");
        assert!(text.contains("name = \"shapes\""), "{text}");
        check_and_update(dir.path(), &used, LockMode::Frozen).expect("an unchanged lock file");
    }

    #[test]
    fn a_changed_archive_is_refused() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        check_and_update(
            dir.path(),
            &releases(&[("shapes", "1.0.0", 'a')]),
            LockMode::Update,
        )
        .expect("write vex.lock");
        let error = format!(
            "{:#}",
            check_and_update(
                dir.path(),
                &releases(&[("shapes", "1.0.0", 'b')]),
                LockMode::Update
            )
            .expect_err("the same version with other bytes")
        );
        assert!(error.contains("but vex.lock recorded aaaa"), "{error}");
    }

    #[test]
    fn a_new_version_replaces_the_old_entry() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        check_and_update(
            dir.path(),
            &releases(&[("shapes", "1.0.0", 'a')]),
            LockMode::Update,
        )
        .expect("write vex.lock");
        check_and_update(
            dir.path(),
            &releases(&[("shapes", "1.1.0", 'b')]),
            LockMode::Update,
        )
        .expect("move to 1.1.0");
        let text = fs::read_to_string(lock_path(dir.path())).expect("read vex.lock");
        assert!(text.contains("1.1.0") && !text.contains("1.0.0"), "{text}");
    }

    #[test]
    fn locked_refuses_a_missing_or_changed_lock_file() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let error = format!(
            "{:#}",
            check_and_update(
                dir.path(),
                &releases(&[("shapes", "1.0.0", 'a')]),
                LockMode::Frozen
            )
            .expect_err("no vex.lock yet")
        );
        assert!(error.contains("is missing, and --locked"), "{error}");

        check_and_update(
            dir.path(),
            &releases(&[("shapes", "1.0.0", 'a')]),
            LockMode::Update,
        )
        .expect("write vex.lock");
        let error = format!(
            "{:#}",
            check_and_update(
                dir.path(),
                &releases(&[("shapes", "1.1.0", 'b')]),
                LockMode::Frozen
            )
            .expect_err("a new version under --locked")
        );
        assert!(error.contains("is out of date, and --locked"), "{error}");
    }

    #[test]
    fn a_package_without_registry_dependencies_writes_no_lock_file() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        check_and_update(dir.path(), &BTreeMap::new(), LockMode::Frozen).expect("nothing to lock");
        assert!(!lock_path(dir.path()).exists());
    }
}
