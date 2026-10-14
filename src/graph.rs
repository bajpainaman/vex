//===- graph.rs - Vx Package Tool ------------------------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// Follows `[dependencies]` from a package to every package it needs, directly or through
// another package.
//
//===----------------------------------------------------------------------===//
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use crate::cache::Cache;
use crate::manifest::{Dependency, Link, Manifest, MANIFEST_FILE_NAME};
use crate::registry::{Registry, Release, REGISTRY_ENV_VAR};
use crate::resolve::{select_versions, Requirement};

/// A package and everything it needs to compile.
#[derive(Debug)]
pub struct PackageGraph {
    pub root: Manifest,
    /// The directory of every package in the build, the root included, by package name.
    ///
    /// One name maps to one directory because the compiler's `--package name=dir` does, so a build
    /// holds one copy of each package.
    pub packages: BTreeMap<String, PathBuf>,
    /// The registry packages in the build, by name. These go into `vex.lock`.
    pub releases: BTreeMap<String, Release>,
    /// The C libraries of every package in the build, each listed once.
    pub link: Link,
}

impl PackageGraph {
    /// Find every package `root` needs. `registry` is needed only when some package in the build
    /// asks for a version from a registry.
    ///
    /// # Errors
    ///
    /// If a dependency's `vex.toml` cannot be read, two packages share a name, packages depend on
    /// each other in a circle, or a registry package cannot be found or fetched.
    pub fn resolve(root: Manifest, registry: Option<&mut Registry>, cache: &Cache) -> Result<Self> {
        let requirements = registry_requirements(&root)?;
        let mut releases = BTreeMap::new();
        let mut registry_dirs = BTreeMap::new();
        if !requirements.is_empty() {
            let Some(registry) = registry else {
                let first = &requirements[0];
                bail!(
                    "`{}` asks for `{}` {} from a registry, but no registry is set. Pass \
                     --registry, or set {REGISTRY_ENV_VAR}",
                    first.wanted_by,
                    first.name,
                    first.minimum
                );
            };
            releases = select_versions(&requirements, registry)?;
            for release in releases.values() {
                let dir = cache.unpacked(release, registry)?;
                check_matches_index(&dir, release)?;
                registry_dirs.insert(release.name.clone(), dir);
            }
        }

        let mut packages = BTreeMap::new();
        packages.insert(root.name.clone(), root.dir.clone());
        let mut chain = vec![root.name.clone()];
        let mut link = Link::default();
        add_link(&mut link, &root.link);
        visit(&root, &registry_dirs, &mut chain, &mut packages, &mut link)?;
        Ok(Self {
            root,
            packages,
            releases,
            link,
        })
    }
}

/// The registry versions asked for by `root` and by every local package it reaches through
/// `{ path = ".." }` dependencies. Registry packages are not followed: their own needs come from
/// the registry's index.
///
/// # Errors
///
/// If a local dependency's `vex.toml` cannot be read.
fn registry_requirements(root: &Manifest) -> Result<Vec<Requirement>> {
    let mut requirements = Vec::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    let mut work = vec![root.clone()];
    seen.insert(root.dir.clone());
    while let Some(manifest) = work.pop() {
        for (name, dependency) in &manifest.dependencies {
            match dependency {
                Dependency::Registry(minimum) => requirements.push(Requirement {
                    name: name.clone(),
                    minimum: *minimum,
                    wanted_by: manifest.name.clone(),
                }),
                Dependency::Path(dir) => {
                    if seen.insert(dir.clone()) {
                        work.push(Manifest::load(&dir.join(MANIFEST_FILE_NAME)).with_context(
                            || format!("while reading dependency `{name}` of `{}`", manifest.name),
                        )?);
                    }
                }
            }
        }
    }
    Ok(requirements)
}

/// A registry package's own `vex.toml` must say what its index line says, because the index is
/// what versions were picked from.
///
/// # Errors
///
/// If the two disagree, or the `vex.toml` cannot be read.
fn check_matches_index(dir: &std::path::Path, release: &Release) -> Result<()> {
    let manifest = Manifest::load(&dir.join(MANIFEST_FILE_NAME)).with_context(|| {
        format!(
            "in the registry package `{}` {}",
            release.name, release.version
        )
    })?;
    let dependencies: Option<BTreeMap<String, crate::version::Version>> = manifest
        .dependencies
        .iter()
        .map(|(name, dependency)| match dependency {
            Dependency::Registry(minimum) => Some((name.clone(), *minimum)),
            Dependency::Path(_) => None,
        })
        .collect();
    if manifest.name != release.name
        || manifest.version != release.version
        || dependencies.as_ref() != Some(&release.dependencies)
    {
        bail!(
            "the registry package `{}` {} does not match its index entry: its vex.toml names \
             `{}` {} and different dependencies. The registry is damaged",
            release.name,
            release.version,
            manifest.name,
            manifest.version
        );
    }
    Ok(())
}

/// `chain` is the list of packages from the root down to `manifest`, used to report a cycle.
///
/// # Errors
///
/// The same as [`PackageGraph::resolve`].
fn visit(
    manifest: &Manifest,
    registry_dirs: &BTreeMap<String, PathBuf>,
    chain: &mut Vec<String>,
    packages: &mut BTreeMap<String, PathBuf>,
    link: &mut Link,
) -> Result<()> {
    for (name, dependency) in &manifest.dependencies {
        let dir = match dependency {
            Dependency::Path(dir) => dir,
            // Every registry dependency was picked in `resolve`, or its index check failed.
            Dependency::Registry(_) => registry_dirs.get(name).with_context(|| {
                format!(
                    "internal error: no version of `{name}` was picked for `{}`",
                    manifest.name
                )
            })?,
        };
        if let Some(known_dir) = packages.get(name) {
            if known_dir != dir {
                bail!(
                    "two different packages are both called `{name}`: `{}` and `{}`. A build can \
                     hold only one package of each name",
                    known_dir.display(),
                    dir.display()
                );
            }
            if let Some(start) = chain.iter().position(|seen| seen == name) {
                bail!(
                    "packages depend on each other in a circle: {} -> {name}",
                    chain[start..].join(" -> ")
                );
            }
            continue;
        }

        let dependency = Manifest::load(&dir.join(MANIFEST_FILE_NAME))
            .with_context(|| format!("while reading dependency `{name}` of `{}`", manifest.name))?;
        if dependency.name != *name {
            bail!(
                "`{}` depends on `{name}`, but the package in `{}` is named `{}`. The name in \
                 `[dependencies]` must be the package's own name, because that is the name `import` \
                 uses",
                manifest.name,
                dir.display(),
                dependency.name
            );
        }

        packages.insert(name.clone(), dir.clone());
        add_link(link, &dependency.link);
        chain.push(name.clone());
        visit(&dependency, registry_dirs, chain, packages, link)?;
        chain.pop();
    }
    Ok(())
}

/// Add the libraries and directories of `package` that `all` does not have yet, keeping their
/// order: a C linker can care which library comes first.
fn add_link(all: &mut Link, package: &Link) {
    for library in &package.libraries {
        if !all.libraries.contains(library) {
            all.libraries.push(library.clone());
        }
    }
    for dir in &package.dirs {
        if !all.dirs.contains(dir) {
            all.dirs.push(dir.clone());
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    reason = "a test reports a failure by panicking, and its helpers pass errors on"
)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::fs;
    use std::path::Path;

    /// Write `<root>/<name>/vex.toml` for a package that depends on `dependencies`, each given as
    /// (name in `[dependencies]`, directory next to this package).
    fn write_package(
        root: &Path,
        dir_name: &str,
        package_name: &str,
        dependencies: &[(&str, &str)],
    ) {
        let dir = root.join(dir_name);
        fs::create_dir_all(&dir).expect("create the package directory");
        let mut text = format!(
            "[package]\nname = \"{package_name}\"\nversion = \"1.0.0\"\n\n[dependencies]\n"
        );
        for (name, sibling_dir) in dependencies {
            writeln!(text, "{name} = {{ path = \"../{sibling_dir}\" }}")
                .expect("writing to a String cannot fail");
        }
        fs::write(dir.join(MANIFEST_FILE_NAME), text).expect("write vex.toml");
    }

    fn resolve_package(root: &Path, dir_name: &str) -> Result<PackageGraph> {
        PackageGraph::resolve(
            Manifest::load(&root.join(dir_name).join(MANIFEST_FILE_NAME))?,
            None,
            &Cache::new(root.join("cache")),
        )
    }

    fn error_of(root: &Path, dir_name: &str) -> String {
        format!(
            "{:#}",
            resolve_package(root, dir_name).expect_err("this build should be refused")
        )
    }

    #[test]
    fn a_dependency_of_a_dependency_is_part_of_the_build() {
        let root = tempfile::tempdir().expect("create a temporary directory");
        write_package(root.path(), "app", "app", &[("shapes", "shapes")]);
        write_package(root.path(), "shapes", "shapes", &[("numbers", "numbers")]);
        write_package(root.path(), "numbers", "numbers", &[]);

        let graph = resolve_package(root.path(), "app").expect("three packages in a line");
        let names: Vec<&str> = graph.packages.keys().map(String::as_str).collect();
        assert_eq!(names, ["app", "numbers", "shapes"]);
        assert_eq!(
            graph.packages["numbers"],
            root.path()
                .join("numbers")
                .canonicalize()
                .expect("resolve numbers")
        );
    }

    #[test]
    fn the_c_libraries_of_every_package_are_linked_once() {
        let root = tempfile::tempdir().expect("create a temporary directory");
        write_package(root.path(), "app", "app", &[("zip", "zip"), ("png", "png")]);
        for (name, libraries) in [("zip", "\"z\""), ("png", "\"png\", \"z\"")] {
            fs::create_dir_all(root.path().join(name)).expect("create a package directory");
            fs::write(
                root.path().join(name).join(MANIFEST_FILE_NAME),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"1.0.0\"\n\n[link]\nlibraries = [{libraries}]\n"
                ),
            )
            .expect("write vex.toml");
        }

        let graph = resolve_package(root.path(), "app").expect("two packages that link C");
        // `png` comes first because `[dependencies]` is read in name order.
        assert_eq!(graph.link.libraries, ["png", "z"]);
    }

    #[test]
    fn two_packages_may_share_a_dependency() {
        let root = tempfile::tempdir().expect("create a temporary directory");
        write_package(
            root.path(),
            "app",
            "app",
            &[("shapes", "shapes"), ("text", "text")],
        );
        write_package(root.path(), "shapes", "shapes", &[("numbers", "numbers")]);
        write_package(root.path(), "text", "text", &[("numbers", "numbers")]);
        write_package(root.path(), "numbers", "numbers", &[]);

        let graph = resolve_package(root.path(), "app").expect("a shared dependency");
        assert_eq!(graph.packages.len(), 4);
    }

    #[test]
    fn two_packages_with_one_name_are_refused() {
        let root = tempfile::tempdir().expect("create a temporary directory");
        write_package(
            root.path(),
            "app",
            "app",
            &[("shapes", "shapes"), ("numbers", "numbers_a")],
        );
        write_package(root.path(), "shapes", "shapes", &[("numbers", "numbers_b")]);
        write_package(root.path(), "numbers_a", "numbers", &[]);
        write_package(root.path(), "numbers_b", "numbers", &[]);

        let error = error_of(root.path(), "app");
        assert!(
            error.contains("two different packages are both called `numbers`"),
            "{error}"
        );
    }

    #[test]
    fn packages_that_depend_on_each_other_are_refused() {
        let root = tempfile::tempdir().expect("create a temporary directory");
        write_package(root.path(), "app", "app", &[("shapes", "shapes")]);
        write_package(root.path(), "shapes", "shapes", &[("numbers", "numbers")]);
        write_package(root.path(), "numbers", "numbers", &[("shapes", "shapes")]);

        let error = error_of(root.path(), "app");
        assert!(
            error.contains("in a circle: shapes -> numbers -> shapes"),
            "{error}"
        );
    }

    #[test]
    fn a_dependency_must_be_listed_under_its_own_name() {
        let root = tempfile::tempdir().expect("create a temporary directory");
        write_package(root.path(), "app", "app", &[("geometry", "shapes")]);
        write_package(root.path(), "shapes", "shapes", &[]);

        let error = error_of(root.path(), "app");
        assert!(error.contains("is named `shapes`"), "{error}");
    }

    #[test]
    fn a_dependency_without_a_vx_toml_is_refused() {
        let root = tempfile::tempdir().expect("create a temporary directory");
        write_package(root.path(), "app", "app", &[("shapes", "shapes")]);
        fs::create_dir_all(root.path().join("shapes")).expect("create shapes");

        let error = error_of(root.path(), "app");
        assert!(
            error.contains("while reading dependency `shapes` of `app`"),
            "{error}"
        );
    }
}
