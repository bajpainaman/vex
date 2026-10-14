//===- main.rs - Vx Package Tool -------------------------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// `vex` builds a Vx package. It reads `vex.toml`, finds the packages that one depends on, and
// runs the `vxc` compiler with a `--package` flag for each of them.
//
//===----------------------------------------------------------------------===//
//! `vex` builds a Vx package and the packages it depends on.

mod archive;
mod cache;
mod compiler;
mod graph;
mod lock;
mod manifest;
mod registry;
mod resolve;
mod version;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command};

use anyhow::{bail, ensure, Context, Result};
use clap::{Args, Parser, Subcommand};

use cache::Cache;
use compiler::{Action, CompileOptions};
use graph::PackageGraph;
use lock::LockMode;
use manifest::{Dependency, Manifest, MANIFEST_FILE_NAME};
use registry::{sha256_hex, Registry, Release, REGISTRY_ENV_VAR};
use resolve::{select_versions, Requirement};
use version::Version;

#[derive(Parser)]
#[command(version, about = "Builds a Vx package and the packages it depends on")]
struct Cli {
    /// The vex.toml to use. Without it, vex looks in this directory and then in each one above it
    #[arg(long = "manifest-path", value_name = "FILE", global = true)]
    manifest_path: Option<PathBuf>,

    /// The registry: a directory, or an `https://` URL. Without it, vex uses `$VEX_REGISTRY`
    #[arg(long = "registry", value_name = "DIR|URL", global = true)]
    registry: Option<String>,

    /// Fail instead of writing vex.lock, as a CI build should
    #[arg(long = "locked", global = true)]
    locked: bool,

    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Compile the package to an object file in its `target` directory
    Build(CompileFlags),
    /// Compile the package and run it. The exit code is the program's own
    Run {
        #[command(flatten)]
        flags: CompileFlags,

        /// Arguments for the program, after `--`
        #[arg(last = true)]
        program_args: Vec<String>,
    },
    /// Download the registry packages the build needs and write vex.lock, without compiling
    Fetch,
    /// Add a registry dependency to vex.toml: `vex add shapes` or `vex add shapes@1.2.0`
    Add {
        /// The package, and optionally the lowest version to accept. Without a version, vex takes
        /// the latest one in the registry
        #[arg(value_name = "NAME[@VERSION]")]
        package: String,
    },
    /// Write the archive that `vex publish` would publish, into target/package
    Package,
    /// Put this version of the package into a registry in a directory
    Publish,
}

#[derive(Args)]
struct CompileFlags {
    /// Compile against this machine file, as `vxc --machine` does
    #[arg(long = "machine", value_name = "FILE")]
    machine: Option<PathBuf>,

    /// The host the program runs on, as `vxc --host` takes it: a file, or `default`
    #[arg(long = "host", value_name = "FILE|default")]
    host: Option<String>,
}

fn main() {
    match run(Cli::parse()) {
        Ok(exit_code) => process::exit(exit_code),
        Err(error) => {
            eprintln!("error: {error:#}");
            process::exit(1);
        }
    }
}

/// Returns the exit code of the compiler, which for `vex run` is the program's own.
///
/// # Errors
///
/// If the package or one of its dependencies is not usable, or the compiler cannot be run.
fn run(cli: Cli) -> Result<i32> {
    let manifest_path = match cli.manifest_path {
        Some(path) => path,
        None => find_manifest()?,
    };
    let manifest = Manifest::load(&manifest_path)?;
    let mut registry = open_registry(cli.registry.as_deref())?;
    let lock_mode = if cli.locked {
        LockMode::Frozen
    } else {
        LockMode::Update
    };

    let (action, options) = match cli.command {
        CliCommand::Build(flags) => (
            Action::Build,
            CompileOptions {
                machine: flags.machine,
                host: flags.host,
                program_args: Vec::new(),
            },
        ),
        CliCommand::Run {
            flags,
            program_args,
        } => (
            Action::Run,
            CompileOptions {
                machine: flags.machine,
                host: flags.host,
                program_args,
            },
        ),
        CliCommand::Fetch => {
            let graph = resolve_and_lock(manifest, registry.as_mut(), lock_mode)?;
            for release in graph.releases.values() {
                println!("{} {}", release.name, release.version);
            }
            return Ok(0);
        }
        CliCommand::Add { package } => {
            add(&manifest, &manifest_path, &package, registry.as_mut())?;
            return Ok(0);
        }
        CliCommand::Package => {
            let (path, release) = package(&manifest, registry.as_mut())?;
            println!("{}", path.display());
            println!("sha256 {}", release.sha256);
            return Ok(0);
        }
        CliCommand::Publish => {
            let Some(registry) = registry.as_mut() else {
                bail!(
                    "vex publish needs a registry. Pass --registry DIR, or set {REGISTRY_ENV_VAR}"
                );
            };
            let (path, release) = package(&manifest, Some(registry))?;
            let archive =
                fs::read(&path).with_context(|| format!("cannot read `{}`", path.display()))?;
            registry.publish(&release, &archive)?;
            println!(
                "published {} {} to {}",
                release.name,
                release.version,
                registry.describe()
            );
            return Ok(0);
        }
    };

    let graph = resolve_and_lock(manifest, registry.as_mut(), lock_mode)?;
    let args = compiler::compiler_args(&graph, action, &options)?;
    if action == Action::Build {
        let output_dir = compiler::output_dir(&graph);
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("cannot create `{}`", output_dir.display()))?;
    }

    let compiler_path = compiler::locate_compiler();
    let status = Command::new(&compiler_path)
        .args(&args)
        .status()
        .with_context(|| {
            format!(
                "cannot run the compiler `{}`. Install Vx, or set VXC to the path of `vxc`",
                compiler_path.display()
            )
        })?;
    match status.code() {
        Some(code) => Ok(code),
        None => bail!("the compiler was stopped by a signal: {status}"),
    }
}

/// The registry from `--registry`, else from `$VEX_REGISTRY`, else none.
///
/// # Errors
///
/// If the registry named is not one vex will use.
fn open_registry(flag: Option<&str>) -> Result<Option<Registry>> {
    let place = match flag {
        Some(place) => place.to_string(),
        None => match env::var(REGISTRY_ENV_VAR) {
            Ok(place) if !place.is_empty() => place,
            _ => return Ok(None),
        },
    };
    Registry::open(&place).map(Some)
}

/// # Errors
///
/// The same as [`PackageGraph::resolve`] and [`lock::check_and_update`].
fn resolve_and_lock(
    manifest: Manifest,
    registry: Option<&mut Registry>,
    lock_mode: LockMode,
) -> Result<PackageGraph> {
    let graph = PackageGraph::resolve(manifest, registry, &Cache::from_env()?)?;
    lock::check_and_update(&graph.root.dir, &graph.releases, lock_mode)?;
    Ok(graph)
}

/// Write the package's archive to `target/package` and return its path and index entry.
///
/// # Errors
///
/// If the package has a path dependency, a registry dependency that does not resolve, or its
/// files cannot be packed.
fn package(manifest: &Manifest, registry: Option<&mut Registry>) -> Result<(PathBuf, Release)> {
    let mut dependencies = std::collections::BTreeMap::new();
    for (name, dependency) in &manifest.dependencies {
        match dependency {
            Dependency::Registry(minimum) => {
                dependencies.insert(name.clone(), *minimum);
            }
            Dependency::Path(dir) => bail!(
                "`{}` depends on `{name}` by path (`{}`). A published package can depend only on \
                 registry versions, because the path will not exist on other computers. Publish \
                 `{name}` first and write `{name} = \"<version>\"`",
                manifest.name,
                dir.display()
            ),
        }
    }
    if !dependencies.is_empty() {
        let Some(registry) = registry else {
            bail!(
                "`{}` has registry dependencies, so packing it needs the registry to check them. \
                 Pass --registry, or set {REGISTRY_ENV_VAR}",
                manifest.name
            );
        };
        let requirements: Vec<Requirement> = dependencies
            .iter()
            .map(|(name, minimum)| Requirement {
                name: name.clone(),
                minimum: *minimum,
                wanted_by: manifest.name.clone(),
            })
            .collect();
        select_versions(&requirements, registry)
            .with_context(|| format!("the dependencies of `{}` do not resolve", manifest.name))?;
    }

    let archive = archive::pack(&manifest.dir)?;
    let output_dir = compiler::package_output_dir(&manifest.dir);
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("cannot create `{}`", output_dir.display()))?;
    let path = output_dir.join(format!("{}-{}.tar.gz", manifest.name, manifest.version));
    fs::write(&path, &archive).with_context(|| format!("cannot write `{}`", path.display()))?;
    let release = Release {
        name: manifest.name.clone(),
        version: manifest.version,
        dependencies,
        sha256: sha256_hex(&archive),
        description: manifest.description.clone(),
    };
    Ok((path, release))
}

/// Add `name = "<version>"` to `[dependencies]`, keeping the rest of vex.toml as it was written.
///
/// # Errors
///
/// If `spec` is not `NAME` or `NAME@VERSION`, the version is not published, or vex.toml cannot be
/// rewritten.
fn add(
    manifest: &Manifest,
    manifest_path: &Path,
    spec: &str,
    registry: Option<&mut Registry>,
) -> Result<()> {
    let (name, wanted) = match spec.split_once('@') {
        Some((name, version)) => (name, Some(Version::parse(version)?)),
        None => (spec, None),
    };
    manifest::check_package_name(name)?;
    ensure!(
        name != manifest.name,
        "package `{name}` cannot depend on itself"
    );
    let Some(registry) = registry else {
        bail!("vex add needs a registry. Pass --registry, or set {REGISTRY_ENV_VAR}");
    };
    let version = if let Some(version) = wanted {
        registry.release(name, version)?.version
    } else {
        let releases = registry.releases(name)?;
        let Some(latest) = releases.last() else {
            bail!("the registry has no versions of `{name}`");
        };
        latest.version
    };

    let text = fs::read_to_string(manifest_path)
        .with_context(|| format!("cannot read `{}`", manifest_path.display()))?;
    let mut document: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("`{}` is not valid TOML", manifest_path.display()))?;
    let dependencies = document
        .entry("dependencies")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_like_mut()
        .context("`dependencies` in vex.toml is not a table")?;
    dependencies.insert(name, toml_edit::value(version.to_string()));
    fs::write(manifest_path, document.to_string())
        .with_context(|| format!("cannot write `{}`", manifest_path.display()))?;
    println!("added {name} = \"{version}\"");
    Ok(())
}

/// The `vex.toml` in the current directory, or in the nearest directory above it.
///
/// # Errors
///
/// If the current directory cannot be read, or no `vex.toml` is found.
fn find_manifest() -> Result<PathBuf> {
    let start = env::current_dir().context("cannot read the current directory")?;
    for dir in start.ancestors() {
        let candidate = dir.join(MANIFEST_FILE_NAME);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!(
        "no `{MANIFEST_FILE_NAME}` in `{}` or in any directory above it",
        start.display()
    )
}
