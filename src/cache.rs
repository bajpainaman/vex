//===- cache.rs - Vx Package Tool ------------------------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// Keeps unpacked registry packages under `$VEX_HOME/packages`, so each version is downloaded once
// and shared by every project on this computer.
//
//===----------------------------------------------------------------------===//
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;

use anyhow::{Context, Result};

use crate::archive;
use crate::registry::{Registry, Release};

/// Set this to keep downloaded packages somewhere other than `~/.vex`.
const HOME_ENV_VAR: &str = "VEX_HOME";
const DEFAULT_HOME_DIR_NAME: &str = ".vex";
const PACKAGES_DIR_NAME: &str = "packages";
const UNPACKING_DIR_NAME: &str = "unpacking";

/// How much of the SHA-256 goes into a directory name: enough that two different archives of
/// one version never share a directory.
const SHA256_DIGITS_IN_DIR_NAME: usize = 16;

#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// `$VEX_HOME`, else `~/.vex`.
    ///
    /// # Errors
    ///
    /// If neither `VEX_HOME` nor `HOME` is set.
    pub fn from_env() -> Result<Self> {
        if let Some(home) = env::var_os(HOME_ENV_VAR) {
            return Ok(Self::new(PathBuf::from(home)));
        }
        let home = env::var_os("HOME")
            .context("set VEX_HOME or HOME, so vex knows where to keep downloaded packages")?;
        Ok(Self::new(PathBuf::from(home).join(DEFAULT_HOME_DIR_NAME)))
    }

    /// The directory holding `release`, downloading and unpacking it first if needed.
    ///
    /// # Errors
    ///
    /// If the archive cannot be downloaded, does not match its SHA-256, or cannot be unpacked.
    pub fn unpacked(&self, release: &Release, registry: &Registry) -> Result<PathBuf> {
        let dir = self.root.join(PACKAGES_DIR_NAME).join(format!(
            "{}-{}-{}",
            release.name,
            release.version,
            &release.sha256[..SHA256_DIGITS_IN_DIR_NAME]
        ));
        if dir.is_dir() {
            return Ok(dir);
        }

        let archive_bytes = registry.archive(release)?;
        // Unpack beside the final place and rename, so a stopped download never leaves half a
        // package where a later build would trust it.
        let unpacking = self.root.join(UNPACKING_DIR_NAME).join(format!(
            "{}-{}-{}",
            release.name,
            release.version,
            process::id()
        ));
        if unpacking.exists() {
            fs::remove_dir_all(&unpacking)
                .with_context(|| format!("cannot clear `{}`", unpacking.display()))?;
        }
        fs::create_dir_all(&unpacking)
            .with_context(|| format!("cannot create `{}`", unpacking.display()))?;
        archive::unpack(&archive_bytes, &unpacking)
            .with_context(|| format!("cannot unpack `{}` {}", release.name, release.version))?;
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create `{}`", parent.display()))?;
        }
        if let Err(error) = fs::rename(&unpacking, &dir) {
            // Another `vex` unpacked the same archive first. Its copy is just as good.
            if !dir.is_dir() {
                return Err(error).with_context(|| format!("cannot create `{}`", dir.display()));
            }
            fs::remove_dir_all(&unpacking)
                .with_context(|| format!("cannot remove `{}`", unpacking.display()))?;
        }
        Ok(dir)
    }
}
