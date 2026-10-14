//===- archive.rs - Vx Package Tool ----------------------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// Packs a package into a `.tar.gz` and unpacks one. Packing the same files always gives the same
// bytes, so anyone can rebuild an archive from the source and compare its SHA-256 with the
// registry's.
//
//===----------------------------------------------------------------------===//
use std::fs;
use std::io::{Read, Write as _};
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, ensure, Context, Result};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;

use crate::compiler::OUTPUT_DIR_NAME;
use crate::manifest::MANIFEST_FILE_NAME;

/// Every file in an archive gets these, so the archive does not depend on who packed it or when.
const FILE_MODE: u32 = 0o644;
const FILE_MTIME: u64 = 0;

/// A package is source text. More than this, unpacked, is a mistake or a compression bomb.
const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;

/// The files of the package in `dir` that go into its archive, as paths relative to `dir`, sorted:
/// `vex.toml`, every `.vx` file, and `README*` and `LICENSE*` files at the top.
///
/// # Errors
///
/// If a directory cannot be read, or the package holds a symbolic link.
pub fn package_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect(dir, Path::new(""), &mut files)?;
    files.sort();
    Ok(files)
}

/// # Errors
///
/// The same as [`package_files`].
fn collect(root: &Path, relative_dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let dir = root.join(relative_dir);
    let entries = fs::read_dir(&dir).with_context(|| format!("cannot list `{}`", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot list `{}`", dir.display()))?;
        let name = entry.file_name();
        let Some(name_text) = name.to_str() else {
            bail!("`{}` has a name that is not UTF-8", entry.path().display());
        };
        let at_top = relative_dir.as_os_str().is_empty();
        if name_text.starts_with('.') || (at_top && name_text == OUTPUT_DIR_NAME) {
            continue;
        }
        let relative = relative_dir.join(&name);
        let file_type = entry
            .file_type()
            .with_context(|| format!("cannot read `{}`", entry.path().display()))?;
        if file_type.is_symlink() {
            bail!(
                "`{}` is a symbolic link. A package must hold the files themselves",
                entry.path().display()
            );
        }
        if file_type.is_dir() {
            collect(root, &relative, files)?;
            continue;
        }
        let upper = name_text.to_ascii_uppercase();
        let wanted = Path::new(name_text).extension().is_some_and(|e| e == "vx")
            || (at_top
                && (name_text == MANIFEST_FILE_NAME
                    || upper.starts_with("README")
                    || upper.starts_with("LICENSE")));
        if wanted {
            files.push(relative);
        }
    }
    Ok(())
}

/// The `.tar.gz` of the package in `dir`.
///
/// # Errors
///
/// If a file cannot be read.
pub fn pack(dir: &Path) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::best()));
    for relative in package_files(dir)? {
        let path = dir.join(&relative);
        let contents =
            fs::read(&path).with_context(|| format!("cannot read `{}`", path.display()))?;
        let mut header = tar::Header::new_ustar();
        header.set_size(contents.len() as u64);
        header.set_mode(FILE_MODE);
        header.set_mtime(FILE_MTIME);
        header.set_uid(0);
        header.set_gid(0);
        header.set_entry_type(tar::EntryType::Regular);
        // Always `/`, so an archive packed on Windows is the same as one packed elsewhere.
        let archive_path = relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        builder
            .append_data(&mut header, &archive_path, contents.as_slice())
            .with_context(|| format!("cannot add `{archive_path}` to the archive"))?;
    }
    let encoder = builder.into_inner().context("cannot finish the archive")?;
    encoder.finish().context("cannot compress the archive")
}

/// Unpack `archive` into `dest`, which must be empty. Only plain files with plain relative paths
/// are accepted: no links, no absolute paths and no `..`, so an archive cannot write outside
/// `dest`.
///
/// # Errors
///
/// If the archive is damaged, holds something other than plain files, or is too large.
pub fn unpack(archive: &[u8], dest: &Path) -> Result<()> {
    let mut tar = tar::Archive::new(GzDecoder::new(archive));
    let mut unpacked: u64 = 0;
    for entry in tar.entries().context("the archive is damaged")? {
        let mut entry = entry.context("the archive is damaged")?;
        let path = entry
            .path()
            .context("the archive has a path that cannot be read")?
            .into_owned();
        ensure!(
            entry.header().entry_type() == tar::EntryType::Regular,
            "the archive holds `{}`, which is not a plain file",
            path.display()
        );
        ensure!(
            path.components().all(|c| matches!(c, Component::Normal(_))),
            "the archive holds `{}`, which would be written outside the package",
            path.display()
        );
        unpacked = unpacked.saturating_add(entry.size());
        ensure!(
            unpacked <= MAX_UNPACKED_BYTES,
            "the archive unpacks to more than {MAX_UNPACKED_BYTES} bytes"
        );
        let mut contents = Vec::new();
        entry
            .by_ref()
            .take(MAX_UNPACKED_BYTES)
            .read_to_end(&mut contents)
            .with_context(|| format!("cannot unpack `{}`", path.display()))?;
        let target = dest.join(&path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("cannot create `{}`", parent.display()))?;
        }
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .and_then(|mut file| file.write_all(&contents))
            .with_context(|| format!("cannot write `{}`", target.display()))?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::missing_panics_doc,
    reason = "a test reports a failure by panicking"
)]
mod tests {
    use super::*;

    fn write(dir: &Path, relative: &str, text: &str) {
        let path = dir.join(relative);
        fs::create_dir_all(path.parent().expect("a file has a parent")).expect("create a dir");
        fs::write(path, text).expect("write a file");
    }

    fn sample_package(dir: &Path) {
        write(
            dir,
            "vex.toml",
            "[package]\nname = \"shapes\"\nversion = \"1.0.0\"\n",
        );
        write(dir, "area.vx", "fn area() -> i32 { return 1; }\n");
        write(
            dir,
            "geometry/circle.vx",
            "fn circle() -> i32 { return 2; }\n",
        );
        write(dir, "README.md", "shapes\n");
        write(dir, "target/shapes.o", "object file");
        write(dir, ".git/HEAD", "ref");
        write(dir, "notes.txt", "not part of the package");
        write(dir, "geometry/README.md", "only the top README is packed");
    }

    #[test]
    fn only_package_files_are_packed() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        sample_package(dir.path());
        let files: Vec<String> = package_files(dir.path())
            .expect("list the package")
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert_eq!(
            files,
            ["README.md", "area.vx", "geometry/circle.vx", "vex.toml"]
        );
    }

    #[test]
    fn packing_the_same_files_twice_gives_the_same_bytes() {
        let first = tempfile::tempdir().expect("create a temporary directory");
        let second = tempfile::tempdir().expect("create a temporary directory");
        sample_package(first.path());
        sample_package(second.path());
        assert_eq!(
            pack(first.path()).expect("pack the first copy"),
            pack(second.path()).expect("pack the second copy")
        );
    }

    #[test]
    fn an_unpacked_archive_has_the_package_files() {
        let source = tempfile::tempdir().expect("create a temporary directory");
        let dest = tempfile::tempdir().expect("create a temporary directory");
        sample_package(source.path());
        unpack(&pack(source.path()).expect("pack"), dest.path()).expect("unpack");
        assert_eq!(
            fs::read_to_string(dest.path().join("geometry/circle.vx")).expect("read circle.vx"),
            "fn circle() -> i32 { return 2; }\n"
        );
        assert!(!dest.path().join("target").exists());
    }

    #[test]
    fn a_symbolic_link_cannot_be_packed() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        sample_package(dir.path());
        std::os::unix::fs::symlink("/etc/passwd", dir.path().join("secret.vx"))
            .expect("create a symbolic link");
        let error = format!("{:#}", pack(dir.path()).expect_err("a link in the package"));
        assert!(error.contains("is a symbolic link"), "{error}");
    }

    /// A `.tar.gz` holding one file whose header names `path`, written without the checks
    /// `tar::Builder` makes, the way an attacker would.
    fn archive_with_raw_path(path: &str) -> Vec<u8> {
        let mut header = tar::Header::new_gnu();
        header.as_gnu_mut().expect("a GNU header").name[..path.len()]
            .copy_from_slice(path.as_bytes());
        header.set_size(4);
        header.set_mode(FILE_MODE);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        builder.append(&header, &b"evil"[..]).expect("append");
        builder
            .into_inner()
            .expect("finish")
            .finish()
            .expect("compress")
    }

    #[test]
    fn an_archive_cannot_write_outside_its_directory() {
        for path in ["../escape.vx", "/tmp/escape.vx", "a/../../escape.vx"] {
            let dest = tempfile::tempdir().expect("create a temporary directory");
            let error = format!(
                "{:#}",
                unpack(&archive_with_raw_path(path), dest.path()).expect_err(path)
            );
            assert!(
                error.contains("would be written outside the package"),
                "{path}: {error}"
            );
        }
    }

    #[test]
    fn an_archive_with_a_link_is_refused() {
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_entry_type(tar::EntryType::Symlink);
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        builder
            .append_link(&mut header, "link.vx", "/etc/passwd")
            .expect("append a link");
        let archive = builder
            .into_inner()
            .expect("finish")
            .finish()
            .expect("compress");
        let dest = tempfile::tempdir().expect("create a temporary directory");
        let error = format!("{:#}", unpack(&archive, dest.path()).expect_err("a link"));
        assert!(error.contains("which is not a plain file"), "{error}");
    }
}
