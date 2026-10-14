//===- build_and_run_packages.rs - Vx Package Tool -------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// Runs the real `vex` program with a real `vxc` compiler on the packages in `tests/packages`:
// `app` depends on `shapes`, and `shapes` depends on `numbers`.
//
// The compiler comes from `VXC`. It needs the `--package` flag, which a `vxc` built from Vx's
// `main` has and the v0.0.3 release does not. Without `VXC`, the tests that compile are
// skipped with a note, so `cargo test` passes on a machine with no Vx at all.
//
//===----------------------------------------------------------------------===//
//! End-to-end tests of `vex` with a real `vxc` compiler.
#![allow(
    clippy::missing_panics_doc,
    reason = "a test reports a failure by panicking"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The compiler from `VXC`, or `None` when it is not set.
fn compiler() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("VXC")?);
    assert!(
        path.is_file(),
        "VXC is set to `{}`, which is not a file",
        path.display()
    );
    Some(path)
}

/// Says why a test did nothing, so a skipped run is visible in the output.
fn skip(test: &str) {
    eprintln!("skipped {test}: set VXC to a vxc built from Vx's main to run it");
}

/// Copy the test packages to a fresh directory, so a test can write a `target` directory without
/// touching the repository.
fn copy_packages_to(destination: &Path) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/packages");
    for package in ["app", "shapes", "numbers"] {
        let package_destination = destination.join(package);
        fs::create_dir_all(&package_destination).expect("create a package directory");
        for entry in fs::read_dir(source.join(package)).expect("list a test package") {
            let file = entry.expect("read a directory entry").path();
            let name = file.file_name().expect("a listed file has a name");
            fs::copy(&file, package_destination.join(name)).expect("copy a test package file");
        }
    }
}

fn vex(args: &[&str], manifest: &Path, compiler: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vex"))
        .arg("--manifest-path")
        .arg(manifest)
        .args(args)
        .env("VXC", compiler)
        .output()
        .expect("start vex")
}

/// What the run printed, on either stream.
fn printed(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn run_compiles_a_package_with_the_packages_it_depends_on() {
    let Some(compiler) = compiler() else {
        return skip("run_compiles_a_package_with_the_packages_it_depends_on");
    };
    let packages = tempfile::tempdir().expect("create a temporary directory");
    copy_packages_to(packages.path());

    let output = vex(&["run"], &packages.path().join("app/vex.toml"), &compiler);
    let printed = printed(&output);
    assert_eq!(output.status.code(), Some(0), "{printed}");
    // 18 is twice(3 * 3): `shapes::area` ran and called `numbers::twice`.
    assert!(printed.contains("area 18"), "{printed}");
    // 4 comes from `app::sides`, a second file of the root package.
    assert!(printed.contains("sides 4"), "{printed}");
}

#[test]
fn build_writes_an_object_file_in_the_package() {
    let Some(compiler) = compiler() else {
        return skip("build_writes_an_object_file_in_the_package");
    };
    let packages = tempfile::tempdir().expect("create a temporary directory");
    copy_packages_to(packages.path());

    let output = vex(
        &["build", "--object"],
        &packages.path().join("app/vex.toml"),
        &compiler,
    );
    assert_eq!(output.status.code(), Some(0), "{}", printed(&output));
    let object_file = packages.path().join("app/target/app.o");
    let size = fs::metadata(&object_file)
        .unwrap_or_else(|error| panic!("no `{}`: {error}", object_file.display()))
        .len();
    assert!(size > 0, "`{}` is empty", object_file.display());
}

#[test]
fn a_library_package_cannot_be_run() {
    // This error comes before the compiler runs, so any path will do for `VXC`.
    let compiler = compiler().unwrap_or_else(|| PathBuf::from("vxc"));
    let packages = tempfile::tempdir().expect("create a temporary directory");
    copy_packages_to(packages.path());

    let output = vex(
        &["run"],
        &packages.path().join("numbers/vex.toml"),
        &compiler,
    );
    assert_eq!(output.status.code(), Some(1));
    let printed = printed(&output);
    assert!(
        printed.contains("error: package `numbers` has no `main.vx`"),
        "{printed}"
    );
}

#[test]
fn a_directory_without_vex_toml_is_an_error() {
    let empty = tempfile::tempdir().expect("create a temporary directory");

    let output = Command::new(env!("CARGO_BIN_EXE_vex"))
        .arg("run")
        .current_dir(empty.path())
        .output()
        .expect("start vex");
    assert_eq!(output.status.code(), Some(1));
    let printed = printed(&output);
    assert!(printed.contains("error: no `vex.toml` in"), "{printed}");
}
