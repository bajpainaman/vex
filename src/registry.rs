//===- registry.rs - Vx Package Tool ---------------------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// A registry is a set of plain files, so any directory or static web site can be one:
//
//   index/<name>.jsonl                     one line per published version of <name>
//   archives/<name>/<name>-<version>.tar.gz the package itself
//
// Each index line gives the package's registry dependencies and the SHA-256 of its archive.
// Lines are only ever added, so a published version never changes.
//
//===----------------------------------------------------------------------===//
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::check_package_name;
use crate::version::Version;

/// Set this to the registry `vex` uses when `--registry` is not given.
pub const REGISTRY_ENV_VAR: &str = "VEX_REGISTRY";

const INDEX_DIR: &str = "index";
const ARCHIVE_DIR: &str = "archives";
const INDEX_FILE_EXTENSION: &str = "jsonl";

/// A SHA-256 written in hexadecimal has 64 digits.
const SHA256_HEX_LENGTH: usize = 64;

/// The largest index file or archive `vex` downloads. A Vx package is source text, so anything
/// bigger is a mistake or an attack.
const MAX_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024;

/// One published version of a package: one line of its index file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub name: String,
    pub version: Version,
    /// The lowest version of each registry package this one needs.
    #[serde(default)]
    pub dependencies: BTreeMap<String, Version>,
    /// The SHA-256 of the archive, in hexadecimal.
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Where a registry's files are.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Location {
    Directory(PathBuf),
    /// The base URL, without a `/` at the end.
    Web(String),
}

/// A registry, and the index files read from it so far.
#[derive(Debug)]
pub struct Registry {
    location: Location,
    releases_by_name: HashMap<String, Vec<Release>>,
}

impl Registry {
    /// `place` is a directory, or an `https://` URL. Plain `http://` is accepted only for this
    /// computer, because a download over it can be changed on the way.
    ///
    /// # Errors
    ///
    /// If `place` is a URL `vex` will not use.
    pub fn open(place: &str) -> Result<Self> {
        let location = if let Some(rest) = place.strip_prefix("http://") {
            let host = rest.split(['/', ':']).next().unwrap_or_default();
            ensure!(
                host == "localhost" || host == "127.0.0.1",
                "registry `{place}` uses http://, which is allowed only for localhost. Use https://"
            );
            Location::Web(place.trim_end_matches('/').to_string())
        } else if place.starts_with("https://") {
            Location::Web(place.trim_end_matches('/').to_string())
        } else {
            let path = place.strip_prefix("file://").unwrap_or(place);
            Location::Directory(PathBuf::from(path))
        };
        Ok(Self {
            location,
            releases_by_name: HashMap::new(),
        })
    }

    /// Every published version of `name`, lowest first.
    ///
    /// # Errors
    ///
    /// If the registry has no package `name`, or its index file cannot be read or is damaged.
    pub fn releases(&mut self, name: &str) -> Result<&[Release]> {
        if !self.releases_by_name.contains_key(name) {
            check_package_name(name)?;
            let relative = format!("{INDEX_DIR}/{name}.{INDEX_FILE_EXTENSION}");
            let Some(bytes) = self.read(&relative)? else {
                bail!("the registry {} has no package `{name}`", self.describe());
            };
            let releases = parse_index(name, &bytes)
                .with_context(|| format!("in `{relative}` of the registry {}", self.describe()))?;
            self.releases_by_name.insert(name.to_string(), releases);
        }
        Ok(&self.releases_by_name[name])
    }

    /// The release `name` `version`.
    ///
    /// # Errors
    ///
    /// If the registry does not have that version.
    pub fn release(&mut self, name: &str, version: Version) -> Result<Release> {
        let describe = self.describe();
        let releases = self.releases(name)?;
        if let Some(release) = releases.iter().find(|release| release.version == version) {
            return Ok(release.clone());
        }
        let published: Vec<String> = releases.iter().map(|r| r.version.to_string()).collect();
        bail!(
            "the registry {describe} has no version {version} of `{name}`. Published versions: {}",
            published.join(", ")
        )
    }

    /// The archive of `release`, checked against the SHA-256 in the index.
    ///
    /// # Errors
    ///
    /// If the archive cannot be read, or its contents are not what the index says.
    pub fn archive(&self, release: &Release) -> Result<Vec<u8>> {
        let relative = archive_path(&release.name, release.version);
        let Some(bytes) = self.read(&relative)? else {
            bail!(
                "the registry {} lists `{}` {} but has no `{relative}`",
                self.describe(),
                release.name,
                release.version
            );
        };
        let actual = sha256_hex(&bytes);
        ensure!(
            actual == release.sha256,
            "the archive of `{}` {} from the registry {} has SHA-256 {actual}, but the index says \
             {}. The download was damaged or changed; nothing was installed",
            release.name,
            release.version,
            self.describe(),
            release.sha256
        );
        Ok(bytes)
    }

    /// Add `release` and its archive. Only a registry in a directory can be written to; to publish
    /// to a web registry, publish to its files and upload them.
    ///
    /// # Errors
    ///
    /// If the registry is on the web, already has this version, or cannot be written.
    pub fn publish(&mut self, release: &Release, archive: &[u8]) -> Result<()> {
        let Location::Directory(root) = &self.location else {
            bail!(
                "vex can publish only to a registry in a directory, not to {}. Publish to a copy of \
                 its files and upload them",
                self.describe()
            );
        };
        let root = root.clone();
        ensure!(
            sha256_hex(archive) == release.sha256,
            "the archive does not match its SHA-256"
        );
        let already_published = match self.releases(&release.name) {
            Ok(releases) => releases.iter().any(|r| r.version == release.version),
            Err(_) if !root.join(index_path(&release.name)).exists() => false,
            Err(error) => return Err(error),
        };
        ensure!(
            !already_published,
            "`{}` {} is already in the registry, and a published version never changes. Raise the \
             version in vex.toml",
            release.name,
            release.version
        );

        // The archive goes first, so an index line never points at a missing file.
        let archive_file = root.join(archive_path(&release.name, release.version));
        write_new_file(&archive_file, archive)?;
        let index_file = root.join(index_path(&release.name));
        let mut line = serde_json::to_string(release).context("cannot write the index line")?;
        line.push('\n');
        if let Some(dir) = index_file.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("cannot create `{}`", dir.display()))?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&index_file)
            .and_then(|mut file| file.write_all(line.as_bytes()))
            .with_context(|| format!("cannot add to `{}`", index_file.display()))?;
        self.releases_by_name.remove(&release.name);
        Ok(())
    }

    /// The registry as a person would write it.
    pub fn describe(&self) -> String {
        match &self.location {
            Location::Directory(path) => format!("`{}`", path.display()),
            Location::Web(url) => format!("`{url}`"),
        }
    }

    /// The bytes of the file at `relative`, or `None` if the registry does not have it.
    ///
    /// # Errors
    ///
    /// If the file exists but cannot be read.
    fn read(&self, relative: &str) -> Result<Option<Vec<u8>>> {
        match &self.location {
            Location::Directory(root) => {
                let path = root.join(relative);
                match fs::read(&path) {
                    Ok(bytes) => Ok(Some(bytes)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(error) => {
                        Err(error).with_context(|| format!("cannot read `{}`", path.display()))
                    }
                }
            }
            Location::Web(base) => download(&format!("{base}/{relative}")),
        }
    }
}

/// # Errors
///
/// If the server cannot be reached or answers with an error other than "not found".
fn download(url: &str) -> Result<Option<Vec<u8>>> {
    match ureq::get(url).call() {
        Ok(mut response) => response
            .body_mut()
            .with_config()
            .limit(MAX_DOWNLOAD_BYTES)
            .read_to_vec()
            .map(Some)
            .with_context(|| format!("cannot download `{url}`")),
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot download `{url}`")),
    }
}

/// # Errors
///
/// If a line is not a release of `name`, or a version is listed twice.
fn parse_index(name: &str, bytes: &[u8]) -> Result<Vec<Release>> {
    let text = std::str::from_utf8(bytes).context("the index is not UTF-8 text")?;
    let mut releases: Vec<Release> = Vec::new();
    for (number, line) in text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let line_number = number + 1;
        let release: Release = serde_json::from_str(line)
            .with_context(|| format!("line {line_number} is not a release"))?;
        ensure!(
            release.name == name,
            "line {line_number} is a release of `{}`, not of `{name}`",
            release.name
        );
        ensure!(
            release.sha256.len() == SHA256_HEX_LENGTH
                && release
                    .sha256
                    .chars()
                    .all(|c| matches!(c, '0'..='9' | 'a'..='f')),
            "line {line_number} has a SHA-256 that is not 64 lowercase hexadecimal digits"
        );
        for dependency in release.dependencies.keys() {
            check_package_name(dependency)
                .with_context(|| format!("line {line_number} has a bad dependency"))?;
        }
        ensure!(
            releases.iter().all(|r| r.version != release.version),
            "version {} is listed twice",
            release.version
        );
        releases.push(release);
    }
    releases.sort_by_key(|release| release.version);
    Ok(releases)
}

fn index_path(name: &str) -> String {
    format!("{INDEX_DIR}/{name}.{INDEX_FILE_EXTENSION}")
}

fn archive_path(name: &str, version: Version) -> String {
    format!("{ARCHIVE_DIR}/{name}/{name}-{version}.tar.gz")
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Write `bytes` to `path`, which must not exist yet.
///
/// # Errors
///
/// If `path` exists or cannot be written.
fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("cannot create `{}`", dir.display()))?;
    }
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .and_then(|mut file| file.write_all(bytes))
        .with_context(|| format!("cannot write `{}`", path.display()))
}

#[cfg(test)]
#[allow(
    clippy::missing_panics_doc,
    reason = "a test reports a failure by panicking"
)]
mod tests {
    use super::*;

    fn release(name: &str, version: &str, archive: &[u8]) -> Release {
        Release {
            name: name.to_string(),
            version: Version::parse(version).expect("a valid version"),
            dependencies: BTreeMap::new(),
            sha256: sha256_hex(archive),
            description: None,
        }
    }

    fn registry_in(dir: &Path) -> Registry {
        Registry::open(dir.to_str().expect("a UTF-8 path")).expect("a directory registry")
    }

    #[test]
    fn a_published_release_can_be_read_back() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let mut registry = registry_in(dir.path());
        registry
            .publish(&release("shapes", "1.0.0", b"one"), b"one")
            .expect("publish 1.0.0");
        registry
            .publish(&release("shapes", "0.9.0", b"old"), b"old")
            .expect("publish 0.9.0");

        let versions: Vec<String> = registry
            .releases("shapes")
            .expect("read the index")
            .iter()
            .map(|r| r.version.to_string())
            .collect();
        assert_eq!(versions, ["0.9.0", "1.0.0"]);
        let shapes = registry
            .release("shapes", Version::parse("1.0.0").expect("a valid version"))
            .expect("find 1.0.0");
        assert_eq!(registry.archive(&shapes).expect("read the archive"), b"one");
    }

    #[test]
    fn a_version_cannot_be_published_twice() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let mut registry = registry_in(dir.path());
        registry
            .publish(&release("shapes", "1.0.0", b"one"), b"one")
            .expect("publish 1.0.0");
        let error = format!(
            "{:#}",
            registry
                .publish(&release("shapes", "1.0.0", b"two"), b"two")
                .expect_err("the second publish should be refused")
        );
        assert!(
            error.contains("a published version never changes"),
            "{error}"
        );
    }

    #[test]
    fn a_changed_archive_is_refused() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let mut registry = registry_in(dir.path());
        let shapes = release("shapes", "1.0.0", b"one");
        registry.publish(&shapes, b"one").expect("publish 1.0.0");
        fs::write(
            dir.path().join(archive_path("shapes", shapes.version)),
            b"evil",
        )
        .expect("change the archive");

        let error = format!(
            "{:#}",
            registry.archive(&shapes).expect_err("a changed archive")
        );
        assert!(
            error.contains("The download was damaged or changed"),
            "{error}"
        );
    }

    #[test]
    fn a_missing_package_is_named() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let error = format!(
            "{:#}",
            registry_in(dir.path())
                .releases("shapes")
                .expect_err("an empty registry")
        );
        assert!(error.contains("has no package `shapes`"), "{error}");
    }

    #[test]
    fn a_missing_version_lists_the_published_ones() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let mut registry = registry_in(dir.path());
        registry
            .publish(&release("shapes", "1.0.0", b"one"), b"one")
            .expect("publish 1.0.0");
        let error = format!(
            "{:#}",
            registry
                .release("shapes", Version::parse("1.5.0").expect("a valid version"))
                .expect_err("1.5.0 was never published")
        );
        assert!(
            error.contains("has no version 1.5.0 of `shapes`. Published versions: 1.0.0"),
            "{error}"
        );
    }

    #[test]
    fn an_index_line_for_another_package_is_refused() {
        let line = serde_json::to_string(&release("numbers", "1.0.0", b"x")).expect("serialize");
        let error = format!(
            "{:#}",
            parse_index("shapes", line.as_bytes()).expect_err("a line for `numbers`")
        );
        assert!(
            error.contains("is a release of `numbers`, not of `shapes`"),
            "{error}"
        );
    }

    #[test]
    fn plain_http_is_only_for_this_computer() {
        assert!(Registry::open("http://localhost:8080/registry").is_ok());
        assert!(Registry::open("http://127.0.0.1:8080").is_ok());
        let error = format!(
            "{:#}",
            Registry::open("http://example.com").expect_err("plain http to another computer")
        );
        assert!(error.contains("allowed only for localhost"), "{error}");
    }
}
