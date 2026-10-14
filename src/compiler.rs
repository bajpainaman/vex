//===- compiler.rs - Vx Package Tool ---------------------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// Finds the `vxc` compiler and works out the command line that compiles a package.
//
//===----------------------------------------------------------------------===//
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, ensure, Context, Result};

use crate::graph::PackageGraph;

const COMPILER_NAME: &str = "vxc";

/// Set this to the path of a `vxc` to use it instead of the one `vex` would find.
const COMPILER_ENV_VAR: &str = "VXC";

/// Where `vex build` writes, inside the package directory.
pub const OUTPUT_DIR_NAME: &str = "target";

/// Where `vex package` writes archives, inside the output directory.
const PACKAGE_DIR_NAME: &str = "package";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Compile the package to an object file.
    BuildObject,
    /// Compile the package to a program that runs by itself.
    BuildExecutable,
    /// Compile the package and run it.
    Run,
}

/// Compiler flags that `vex` hands on unchanged.
#[derive(Debug, Default)]
pub struct CompileOptions {
    pub machine: Option<PathBuf>,
    pub host: Option<String>,
    /// Arguments for the program itself. Only used by `Action::Run`.
    pub program_args: Vec<String>,
    /// The file to compile instead of the package's main file, such as a test.
    pub entry: Option<PathBuf>,
    /// Where to write a program or object file instead of `target/<name>`.
    pub output: Option<PathBuf>,
}

/// How one run of the compiler went.
#[derive(Debug)]
pub enum Compiled {
    Ok,
    /// The compiler refused the program. `printed` is everything it wrote, for the user.
    Failed {
        exit_code: i32,
        printed: String,
    },
}

/// What the `vxc` in use can do. Released compilers and ones built from `main` differ, so `vex`
/// asks rather than assumes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompilerFeatures {
    /// `-a emit-exe` builds a program that runs by itself.
    pub executables: bool,
    /// `-l` and `-L` link C libraries.
    pub c_libraries: bool,
}

impl CompilerFeatures {
    /// Read the features from `vxc --help`. A compiler that cannot be run has none; running it
    /// for the build reports why.
    pub fn of(compiler: &Path) -> Self {
        let Ok(output) = Command::new(compiler).arg("--help").output() else {
            return Self::default();
        };
        let help = String::from_utf8_lossy(&output.stdout);
        Self {
            executables: help.contains("emit-exe"),
            c_libraries: help.contains("--library-dir"),
        }
    }
}

/// `$VXC`, else the `vxc` installed beside this program, else whichever `vxc` is on `PATH`.
pub fn locate_compiler() -> PathBuf {
    if let Some(path) = env::var_os(COMPILER_ENV_VAR) {
        return PathBuf::from(path);
    }
    let beside_this_program = env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(COMPILER_NAME)))
        .filter(|path| path.is_file());
    beside_this_program.unwrap_or_else(|| PathBuf::from(COMPILER_NAME))
}

/// The file `vex build` writes for this package: `target/<name>`, or `target/<name>.o` for an
/// object file.
pub fn output_path(graph: &PackageGraph, action: Action) -> PathBuf {
    let name = match action {
        Action::BuildObject => format!("{}.o", graph.root.name),
        Action::BuildExecutable | Action::Run => graph.root.name.clone(),
    };
    output_dir(graph).join(name)
}

/// Run `compiler` with `args`, keeping its progress lines off the screen: they are shown only if
/// it fails. What it writes to stderr, such as warnings, is always shown.
///
/// # Errors
///
/// If the compiler cannot be started or is stopped by a signal.
pub fn compile(compiler: &Path, args: &[OsString]) -> Result<Compiled> {
    let output = Command::new(compiler)
        .args(args)
        .output()
        .with_context(|| {
            format!(
                "cannot run the compiler `{}`. Install Vx, or set VXC to the path of `vxc`",
                compiler.display()
            )
        })?;
    let Some(exit_code) = output.status.code() else {
        bail!("the compiler was stopped by a signal: {}", output.status);
    };
    if exit_code == 0 {
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        return Ok(Compiled::Ok);
    }
    Ok(Compiled::Failed {
        exit_code,
        printed: format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    })
}

/// The directory `vex package` writes archives into.
pub fn package_output_dir(package_dir: &Path) -> PathBuf {
    package_dir.join(OUTPUT_DIR_NAME).join(PACKAGE_DIR_NAME)
}

/// The directory `vex build` writes into.
pub fn output_dir(graph: &PackageGraph) -> PathBuf {
    graph.root.dir.join(OUTPUT_DIR_NAME)
}

/// The arguments to give `vxc` so that it compiles the root package of `graph`.
///
/// # Errors
///
/// If the root package has no main file, or it links C libraries and the compiler cannot.
pub fn compiler_args(
    graph: &PackageGraph,
    action: Action,
    options: &CompileOptions,
    features: CompilerFeatures,
) -> Result<Vec<OsString>> {
    let Some(main) = options.entry.as_ref().or(graph.root.main.as_ref()) else {
        bail!(
            "package `{}` has no `main.vx`, so there is nothing to build or run. Add one, or name \
             the file with `main = \"..\"` under `[package]`",
            graph.root.name
        );
    };

    let mut args: Vec<OsString> = Vec::new();
    // Every package in the build is named, not only the root's direct dependencies: the compiler
    // also has to find what those dependencies import.
    for (name, dir) in &graph.packages {
        let mut package = OsString::from(format!("{name}="));
        package.push(dir);
        args.push("--package".into());
        args.push(package);
    }
    if let Some(machine) = &options.machine {
        args.push("--machine".into());
        args.push(machine.into());
    }
    if let Some(host) = &options.host {
        args.push("--host".into());
        args.push(host.into());
    }
    // An object file is not linked, so its C libraries are for whoever links it.
    let links = action != Action::BuildObject
        && !(graph.link.libraries.is_empty() && graph.link.dirs.is_empty());
    if links {
        ensure!(
            features.c_libraries,
            "the build links the C libraries {}, but this vxc has no `-l` flag to link them. Use \
             a vxc built from Vx's `main`",
            graph.link.libraries.join(", ")
        );
        for library in &graph.link.libraries {
            args.push("-l".into());
            args.push(library.into());
        }
        for dir in &graph.link.dirs {
            args.push("-L".into());
            args.push(dir.into());
        }
    }
    match action {
        Action::BuildObject => {
            args.push("-c".into());
            args.push(main.into());
            args.push("-o".into());
            args.push(
                options
                    .output
                    .clone()
                    .unwrap_or_else(|| output_path(graph, action))
                    .into(),
            );
        }
        Action::BuildExecutable => {
            args.push("-a".into());
            args.push("emit-exe".into());
            args.push(main.into());
            args.push("-o".into());
            args.push(
                options
                    .output
                    .clone()
                    .unwrap_or_else(|| output_path(graph, action))
                    .into(),
            );
        }
        Action::Run => {
            args.push("--run".into());
            args.push(main.into());
            if !options.program_args.is_empty() {
                args.push("--".into());
                args.extend(options.program_args.iter().map(OsString::from));
            }
        }
    }
    Ok(args)
}

#[cfg(test)]
#[allow(
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    reason = "a test reports a failure by panicking, and its helpers pass errors on"
)]
mod tests {
    use super::*;
    use crate::manifest::{Dependency, Link, Manifest};
    use crate::version::Version;
    use std::collections::BTreeMap;

    fn graph_of_app_and_shapes(main: Option<&str>) -> PackageGraph {
        let root = Manifest {
            name: "app".to_string(),
            version: Version {
                major: 1,
                minor: 0,
                patch: 0,
            },
            dir: PathBuf::from("/work/app"),
            main: main.map(PathBuf::from),
            description: None,
            dependencies: BTreeMap::from([(
                "shapes".to_string(),
                Dependency::Path(PathBuf::from("/work/shapes")),
            )]),
            link: Link::default(),
        };
        let packages = BTreeMap::from([
            ("app".to_string(), PathBuf::from("/work/app")),
            ("shapes".to_string(), PathBuf::from("/work/shapes")),
        ]);
        PackageGraph {
            root,
            packages,
            releases: BTreeMap::new(),
            link: Link::default(),
        }
    }

    const EVERY_FEATURE: CompilerFeatures = CompilerFeatures {
        executables: true,
        c_libraries: true,
    };

    fn args_as_text(graph: &PackageGraph, action: Action, options: &CompileOptions) -> Vec<String> {
        compiler_args(graph, action, options, EVERY_FEATURE)
            .expect("a package with a main file")
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn run_names_every_package_then_the_main_file() {
        let graph = graph_of_app_and_shapes(Some("/work/app/main.vx"));
        assert_eq!(
            args_as_text(&graph, Action::Run, &CompileOptions::default()),
            [
                "--package",
                "app=/work/app",
                "--package",
                "shapes=/work/shapes",
                "--run",
                "/work/app/main.vx"
            ]
        );
    }

    #[test]
    fn build_writes_an_object_file_named_after_the_package() {
        let graph = graph_of_app_and_shapes(Some("/work/app/main.vx"));
        let args = args_as_text(&graph, Action::BuildObject, &CompileOptions::default());
        assert_eq!(
            args[4..],
            ["-c", "/work/app/main.vx", "-o", "/work/app/target/app.o"]
        );
    }

    #[test]
    fn machine_host_and_program_arguments_are_handed_on() {
        let graph = graph_of_app_and_shapes(Some("/work/app/main.vx"));
        let options = CompileOptions {
            machine: Some(PathBuf::from("fleet/h100-sxm.vx")),
            host: Some("default".to_string()),
            program_args: vec!["--steps".to_string(), "3".to_string()],
            entry: None,
            output: None,
        };
        let args = args_as_text(&graph, Action::Run, &options);
        assert_eq!(
            args[4..],
            [
                "--machine",
                "fleet/h100-sxm.vx",
                "--host",
                "default",
                "--run",
                "/work/app/main.vx",
                "--",
                "--steps",
                "3"
            ]
        );
    }

    #[test]
    fn a_library_cannot_be_built_or_run() {
        let graph = graph_of_app_and_shapes(None);
        let error = compiler_args(
            &graph,
            Action::Run,
            &CompileOptions::default(),
            EVERY_FEATURE,
        )
        .expect_err("a package with no main file");
        assert!(error.to_string().contains("has no `main.vx`"), "{error}");
    }

    #[test]
    fn build_can_write_an_executable() {
        let graph = graph_of_app_and_shapes(Some("/work/app/main.vx"));
        let args = args_as_text(&graph, Action::BuildExecutable, &CompileOptions::default());
        assert_eq!(
            args[4..],
            [
                "-a",
                "emit-exe",
                "/work/app/main.vx",
                "-o",
                "/work/app/target/app"
            ]
        );
    }

    #[test]
    fn an_entry_file_replaces_the_main_file() {
        let graph = graph_of_app_and_shapes(None);
        let options = CompileOptions {
            entry: Some(PathBuf::from("/work/app/tests/sum.vx")),
            ..CompileOptions::default()
        };
        let args = args_as_text(&graph, Action::Run, &options);
        assert_eq!(args[4..], ["--run", "/work/app/tests/sum.vx"]);
    }

    fn graph_that_links_zlib() -> PackageGraph {
        let mut graph = graph_of_app_and_shapes(Some("/work/app/main.vx"));
        graph.link = Link {
            libraries: vec!["z".to_string()],
            dirs: vec![PathBuf::from("/work/shapes/native")],
        };
        graph
    }

    #[test]
    fn c_libraries_are_linked_into_a_program_but_not_an_object_file() {
        let graph = graph_that_links_zlib();
        let run = args_as_text(&graph, Action::Run, &CompileOptions::default());
        assert_eq!(
            run[4..],
            [
                "-l",
                "z",
                "-L",
                "/work/shapes/native",
                "--run",
                "/work/app/main.vx"
            ]
        );
        let object = args_as_text(&graph, Action::BuildObject, &CompileOptions::default());
        assert!(!object.contains(&"-l".to_string()), "{object:?}");
    }

    #[test]
    fn c_libraries_need_a_compiler_that_links_them() {
        let error = compiler_args(
            &graph_that_links_zlib(),
            Action::Run,
            &CompileOptions::default(),
            CompilerFeatures::default(),
        )
        .expect_err("a vxc without -l");
        assert!(
            error.to_string().contains("this vxc has no `-l` flag"),
            "{error}"
        );
    }
}
