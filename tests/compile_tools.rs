//===- compile_tools.rs - Vx Package Tool ----------------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// Runs `vex new`, `vex test`, `vex run` and `vex build` with a real `vxc`, including on
// `checksum`, a program that uses `zip`, a package that wraps the C library zlib.
//
// These need a `vxc` from `VXC` that can build a program (`-a emit-exe`) and link a C library
// (`-l`). Without one, each test is skipped with a note.
//
//===----------------------------------------------------------------------===//
//! End-to-end tests of the commands that compile, with a real `vxc` compiler.
#![allow(
    clippy::missing_panics_doc,
    reason = "a test reports a failure by panicking"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// crc32 of "123456789", the check value zlib's own tests use.
const CRC_OF_123456789: &str = "3421780262";

/// The compiler from `VXC`, if it can build programs and link C libraries.
fn compiler(test: &str) -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("VXC")?);
    let help = Command::new(&path).arg("--help").output().ok()?;
    let help = String::from_utf8_lossy(&help.stdout);
    if help.contains("emit-exe") && help.contains("--library-dir") {
        return Some(path);
    }
    eprintln!("skipped {test}: the vxc in VXC cannot build a program or link a C library");
    None
}

/// Copy `tests/packages/<name>` for each of `names` into `destination`, with subdirectories.
fn copy_packages(names: &[&str], destination: &Path) {
    fn copy_dir(from: &Path, to: &Path) {
        fs::create_dir_all(to).expect("create a directory");
        for entry in fs::read_dir(from).expect("list a test package") {
            let path = entry.expect("read a directory entry").path();
            let target = to.join(path.file_name().expect("a listed file has a name"));
            if path.is_dir() {
                copy_dir(&path, &target);
            } else {
                fs::copy(&path, &target).expect("copy a test package file");
            }
        }
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/packages");
    for name in names {
        copy_dir(&source.join(name), &destination.join(name));
    }
}

fn vex(dir: &Path, args: &[&str], compiler: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vex"))
        .args(args)
        .current_dir(dir)
        .env("VXC", compiler)
        .env_remove("VEX_REGISTRY")
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
fn new_makes_a_program_that_runs_and_passes_its_test() {
    let Some(compiler) = compiler("new_makes_a_program_that_runs_and_passes_its_test") else {
        return;
    };
    let root = tempfile::tempdir().expect("create a temporary directory");
    let output = vex(root.path(), &["new", "hello"], &compiler);
    assert!(output.status.success(), "{}", printed(&output));

    let package = root.path().join("hello");
    let output = vex(&package, &["test"], &compiler);
    let text = printed(&output);
    assert_eq!(output.status.code(), Some(0), "{text}");
    assert!(text.contains("test tests/add_one.vx ... ok"), "{text}");

    let output = vex(&package, &["run"], &compiler);
    assert_eq!(output.status.code(), Some(0), "{}", printed(&output));
    // The new program prints add_one(41).
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "42");
}

#[test]
fn run_exits_with_the_programs_own_exit_code() {
    let Some(compiler) = compiler("run_exits_with_the_programs_own_exit_code") else {
        return;
    };
    let root = tempfile::tempdir().expect("create a temporary directory");
    fs::write(
        root.path().join("vex.toml"),
        "[package]\nname = \"seven\"\nversion = \"1.0.0\"\n",
    )
    .expect("write vex.toml");
    fs::write(
        root.path().join("main.vx"),
        "fn main() -> i32 {\n  return 7;\n}\n",
    )
    .expect("write main.vx");

    let output = vex(root.path(), &["run"], &compiler);
    assert_eq!(output.status.code(), Some(7), "{}", printed(&output));
}

#[test]
fn a_package_that_wraps_a_c_library_links_it_for_the_program_that_uses_it() {
    let Some(compiler) =
        compiler("a_package_that_wraps_a_c_library_links_it_for_the_program_that_uses_it")
    else {
        return;
    };
    let root = tempfile::tempdir().expect("create a temporary directory");
    copy_packages(&["checksum", "zip"], root.path());
    let checksum = root.path().join("checksum");

    let output = vex(&checksum, &["run"], &compiler);
    assert_eq!(output.status.code(), Some(0), "{}", printed(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        CRC_OF_123456789
    );

    let output = vex(&checksum, &["build"], &compiler);
    assert_eq!(output.status.code(), Some(0), "{}", printed(&output));
    let program = checksum.join("target/checksum");
    let run = Command::new(&program)
        .output()
        .unwrap_or_else(|error| panic!("cannot run `{}`: {error}", program.display()));
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        CRC_OF_123456789
    );
}

#[test]
fn test_reports_each_test_and_fails_when_one_fails() {
    let Some(compiler) = compiler("test_reports_each_test_and_fails_when_one_fails") else {
        return;
    };
    let root = tempfile::tempdir().expect("create a temporary directory");
    copy_packages(&["checksum", "zip"], root.path());
    let checksum = root.path().join("checksum");

    let output = vex(&checksum, &["test"], &compiler);
    let text = printed(&output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(
        text.contains("test tests/crc_of_digits.vx ... ok"),
        "{text}"
    );
    // `wrong_crc.vx` returns 3, and the report shows that code, not the compiler's.
    assert!(
        text.contains("test tests/wrong_crc.vx ... FAILED (exit code 3)"),
        "{text}"
    );
    assert!(text.contains("1 passed, 1 failed"), "{text}");

    let output = vex(&checksum, &["test", "crc_of"], &compiler);
    let text = printed(&output);
    assert_eq!(output.status.code(), Some(0), "{text}");
    assert!(text.contains("1 passed, 0 failed"), "{text}");
}

#[test]
fn a_test_that_does_not_compile_is_reported() {
    let Some(compiler) = compiler("a_test_that_does_not_compile_is_reported") else {
        return;
    };
    let root = tempfile::tempdir().expect("create a temporary directory");
    copy_packages(&["checksum", "zip"], root.path());
    let checksum = root.path().join("checksum");
    fs::write(
        checksum.join("tests/no_such_function.vx"),
        "fn main() -> i32 {\n  return no_such_function();\n}\n",
    )
    .expect("write a broken test");

    let output = vex(&checksum, &["test", "no_such"], &compiler);
    let text = printed(&output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(
        text.contains("test tests/no_such_function.vx ... FAILED (does not compile)"),
        "{text}"
    );
}
