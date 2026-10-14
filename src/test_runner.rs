//===- test_runner.rs - Vx Package Tool ------------------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// `vex test`: each `.vx` file directly in a package's `tests` directory is a program with its own
// `fn main`, and it passes when it exits with 0. Vx has no test attribute yet, so a test is just a
// program; it can `import` the package it tests by the package's name.
//
//===----------------------------------------------------------------------===//
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::compiler::{self, Action, CompileOptions, Compiled, CompilerFeatures};
use crate::graph::PackageGraph;

const TESTS_DIR_NAME: &str = "tests";

/// The exit code of `vex test` when a test failed.
const FAILED_EXIT_CODE: i32 = 1;

/// The test programs of the package in `dir`, sorted. Files in directories below `tests` are not
/// tests, so tests can share helper files kept there.
///
/// # Errors
///
/// If `tests` exists but cannot be listed.
fn test_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let tests_dir = dir.join(TESTS_DIR_NAME);
    if !tests_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(&tests_dir)
        .with_context(|| format!("cannot list `{}`", tests_dir.display()))?
    {
        let path = entry
            .with_context(|| format!("cannot list `{}`", tests_dir.display()))?
            .path();
        if path.is_file() && path.extension().is_some_and(|e| e == "vx") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// Run the tests of the root package of `graph` whose file name contains `filter`, and return the
/// exit code for `vex test`.
///
/// # Errors
///
/// If the tests cannot be listed, `filter` matches none, or the compiler cannot be run.
pub fn run_tests(
    graph: &PackageGraph,
    options: &CompileOptions,
    filter: Option<&str>,
    compiler: &Path,
) -> Result<i32> {
    let all = test_files(&graph.root.dir)?;
    if all.is_empty() {
        println!(
            "no tests: `{}` has no `.vx` files in `{TESTS_DIR_NAME}/`",
            graph.root.name
        );
        return Ok(0);
    }
    let selected: Vec<&PathBuf> = all
        .iter()
        .filter(|path| {
            filter.is_none_or(|text| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(text))
            })
        })
        .collect();
    if selected.is_empty() {
        bail!(
            "no test file name contains `{}`. The tests are: {}",
            filter.unwrap_or_default(),
            all.iter()
                .map(|path| relative(&graph.root.dir, path))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let features = CompilerFeatures::of(compiler);
    let mut failures: Vec<(String, String)> = Vec::new();
    for path in &selected {
        let name = relative(&graph.root.dir, path);
        match run_one(graph, options, path, compiler, features)? {
            Outcome::Passed => println!("test {name} ... ok"),
            Outcome::Failed { why, printed } => {
                println!("test {name} ... FAILED ({why})");
                failures.push((name, printed));
            }
        }
    }

    for (name, printed) in &failures {
        println!("\n---- {name} printed:\n{}", printed.trim_end());
    }
    let passed = selected.len() - failures.len();
    println!("\n{passed} passed, {} failed", failures.len());
    Ok(if failures.is_empty() {
        0
    } else {
        FAILED_EXIT_CODE
    })
}

enum Outcome {
    Passed,
    Failed { why: String, printed: String },
}

/// Compile and run the test program at `path`.
///
/// # Errors
///
/// If the compiler or the test program cannot be started.
fn run_one(
    graph: &PackageGraph,
    options: &CompileOptions,
    path: &Path,
    compiler: &Path,
    features: CompilerFeatures,
) -> Result<Outcome> {
    let mut test_options = CompileOptions {
        machine: options.machine.clone(),
        host: options.host.clone(),
        program_args: Vec::new(),
        entry: Some(path.to_path_buf()),
        output: None,
    };
    let output = if features.executables {
        // A test built as a program reports its own exit code; one run inside `vxc` exits with 1
        // whatever the program returned.
        let stem = path.file_stem().unwrap_or_default();
        let program = compiler::output_dir(graph).join(TESTS_DIR_NAME).join(stem);
        if let Some(dir) = program.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("cannot create `{}`", dir.display()))?;
        }
        test_options.output = Some(program.clone());
        let args =
            compiler::compiler_args(graph, Action::BuildExecutable, &test_options, features)?;
        if let Compiled::Failed { printed, .. } = compiler::compile(compiler, &args)? {
            return Ok(Outcome::Failed {
                why: "does not compile".to_string(),
                printed,
            });
        }
        Command::new(&program)
            .output()
            .with_context(|| format!("cannot run `{}`", program.display()))?
    } else {
        let args = compiler::compiler_args(graph, Action::Run, &test_options, features)?;
        Command::new(compiler)
            .args(&args)
            .output()
            .with_context(|| {
                format!(
                    "cannot run the compiler `{}`. Install Vx, or set VXC to the path of `vxc`",
                    compiler.display()
                )
            })?
    };
    if output.status.success() {
        return Ok(Outcome::Passed);
    }
    Ok(Outcome::Failed {
        why: output.status.code().map_or_else(
            || "stopped by a signal".to_string(),
            |code| format!("exit code {code}"),
        ),
        printed: format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    })
}

/// `path` relative to the package directory, with `/` between parts, for messages.
fn relative(package_dir: &Path, path: &Path) -> String {
    path.strip_prefix(package_dir)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
#[allow(
    clippy::missing_panics_doc,
    reason = "a test reports a failure by panicking"
)]
mod tests {
    use super::*;

    #[test]
    fn only_vx_files_directly_in_tests_are_tests() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let tests = dir.path().join(TESTS_DIR_NAME);
        fs::create_dir_all(tests.join("helpers")).expect("create tests/helpers");
        for file in ["b_sum.vx", "a_area.vx", "notes.md", "helpers/shared.vx"] {
            fs::write(tests.join(file), "").expect("write a file");
        }
        let names: Vec<String> = test_files(dir.path())
            .expect("list the tests")
            .iter()
            .map(|path| relative(dir.path(), path))
            .collect();
        assert_eq!(names, ["tests/a_area.vx", "tests/b_sum.vx"]);
    }

    #[test]
    fn a_package_without_a_tests_directory_has_no_tests() {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        assert_eq!(
            test_files(dir.path()).expect("list the tests"),
            Vec::<PathBuf>::new()
        );
    }
}
