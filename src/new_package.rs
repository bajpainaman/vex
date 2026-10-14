//===- new_package.rs - Vx Package Tool ------------------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// `vex new`: writes a small package that builds, runs and passes `vex test` as it is.
//
//===----------------------------------------------------------------------===//
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::manifest::{check_package_name, MANIFEST_FILE_NAME};

/// The version a new package starts at: below 1.0.0, so its API is not promised yet.
const FIRST_VERSION: &str = "0.1.0";

/// Create the package at `path`. Its directory name becomes the package name.
///
/// # Errors
///
/// If the name is not a package name, `path` exists and is not empty, or a file cannot be written.
pub fn create(path: &Path, lib: bool) -> Result<()> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        bail!(
            "`{}` does not end in a directory name to use as the package name",
            path.display()
        );
    };
    check_package_name(name)?;
    let is_empty_dir = path.is_dir()
        && fs::read_dir(path)
            .with_context(|| format!("cannot list `{}`", path.display()))?
            .next()
            .is_none();
    if path.exists() && !is_empty_dir {
        bail!(
            "`{}` already exists. vex new makes a new directory",
            path.display()
        );
    }

    let mut files = vec![
        (
            MANIFEST_FILE_NAME.to_string(),
            format!("[package]\nname = \"{name}\"\nversion = \"{FIRST_VERSION}\"\n\n[dependencies]\n"),
        ),
        (".gitignore".to_string(), "/target\n".to_string()),
        (
            "numbers.vx".to_string(),
            "fn add_one(n : i64) -> i64 {\n  return n + 1;\n}\n".to_string(),
        ),
        (
            "tests/add_one.vx".to_string(),
            format!(
                "// A test is a program: it passes when `main` returns 0.\n\
                 import {name}::numbers;\n\n\
                 fn main() -> i32 {{\n  if add_one(41) != 42 {{\n    return 1;\n  }}\n  return 0;\n}}\n"
            ),
        ),
    ];
    if !lib {
        files.push((
            "main.vx".to_string(),
            format!(
                "import {name}::numbers;\n\n\
                 fn main() -> i32 {{\n  print(add_one(41));\n  print!(\"\\n\");\n  return 0;\n}}\n"
            ),
        ));
    }

    for (relative, text) in &files {
        let file = path.join(relative);
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("cannot create `{}`", dir.display()))?;
        }
        fs::write(&file, text).with_context(|| format!("cannot write `{}`", file.display()))?;
    }
    println!(
        "created {} `{name}` in `{}`",
        if lib { "library" } else { "program" },
        path.display()
    );
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::missing_panics_doc,
    reason = "a test reports a failure by panicking"
)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    #[test]
    fn a_new_program_has_a_main_file_and_a_test() {
        let root = tempfile::tempdir().expect("create a temporary directory");
        let dir = root.path().join("hello");
        create(&dir, false).expect("create a program");
        let manifest = Manifest::load(&dir.join(MANIFEST_FILE_NAME)).expect("a valid vex.toml");
        assert_eq!(manifest.name, "hello");
        assert!(manifest.main.is_some());
        assert!(dir.join("tests/add_one.vx").is_file());
    }

    #[test]
    fn a_new_library_has_no_main_file() {
        let root = tempfile::tempdir().expect("create a temporary directory");
        let dir = root.path().join("helpers");
        create(&dir, true).expect("create a library");
        let manifest = Manifest::load(&dir.join(MANIFEST_FILE_NAME)).expect("a valid vex.toml");
        assert_eq!(manifest.main, None);
    }

    #[test]
    fn an_existing_directory_is_not_overwritten() {
        let root = tempfile::tempdir().expect("create a temporary directory");
        let dir = root.path().join("hello");
        fs::create_dir_all(&dir).expect("create hello");
        fs::write(dir.join("main.vx"), "mine").expect("write main.vx");
        let error = format!("{:#}", create(&dir, false).expect_err("hello is not empty"));
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(
            fs::read_to_string(dir.join("main.vx")).expect("read"),
            "mine"
        );
    }

    #[test]
    fn the_directory_name_must_be_a_package_name() {
        let root = tempfile::tempdir().expect("create a temporary directory");
        let error = format!(
            "{:#}",
            create(&root.path().join("my-app"), false).expect_err("a dash")
        );
        assert!(error.contains("is not a package name"), "{error}");
    }
}
