# vex

Builds a [Vx](https://vxlang.org) package and the packages it depends on.

`vex` reads a `vex.toml`, finds every package in the build, and runs `vxc` with one
`--package name=dir` flag for each. It runs the compiler as a program, so it needs no LLVM to
build and is not part of the compiler.

This is a prototype, written as a data point for
[vx-lang/Vx#1275](https://github.com/vx-lang/Vx/issues/1275), where the package manager is being
designed. Expect it to change or be replaced.

## A package

```toml
# vex.toml
[package]
name = "app"
version = "1.0.0"

[dependencies]
shapes = { path = "../shapes" }  # a package in a local directory
numbers = "1.2.0"                # version 1.2.0 or later of `numbers`, from a registry
```

`name` is what `import` uses: a file `sides.vx` in this package is `import app::sides`. A
package with a `main.vx` is a program; one without is a library. `description = ".."` under
`[package]` is optional and goes into the registry.

A package that calls a C library through an `extern` block names it, and every program that
uses the package links it:

```toml
[link]
libraries = ["z"]   # links zlib, as `-l z` does
dirs = ["native"]   # optional: where to look besides the system's usual places
```

## Commands

```sh
vex new hello        # a new program in hello/, with a test; `--lib` for a library
vex run              # build the program and run it; the exit code is the program's
vex run -- --steps 3 # arguments after -- go to the program
vex build            # build the program, target/<name>
vex build --object   # write an object file, target/<name>.o, instead
vex test             # run each program in tests/; a test passes when it exits with 0
vex test parse       # run only the tests whose file name contains `parse`
```

A test is a program with its own `fn main` in `tests/`, and it can `import` the package it tests
by the package's name. Vx has no test attribute yet, so this needs nothing from the language.

`run`, `build` and `test` take `--machine FILE` and `--host FILE|default`, which go to `vxc`
unchanged. Every command takes `--manifest-path FILE` to name a `vex.toml` that is not in this
directory or above it.

Registry commands:

```sh
vex add numbers           # add the latest version of `numbers` to [dependencies]
vex add numbers@1.2.0     # add a given version
vex fetch                 # download what the build needs and write vex.lock
vex package               # write the archive `publish` would publish, to target/package
vex publish               # put this version into the registry
vex search integers       # find packages by name or description
```

For the people who run a registry:

```sh
vex registry verify       # check every published version, as the registry's CI does
vex registry json         # write packages.json, which the web pages and `vex search` read
```

Every command takes `--registry DIR|URL` (or `$VEX_REGISTRY`) and `--locked`, which fails
instead of changing `vex.lock`, as a CI build should. Downloads are kept in `$VEX_HOME`
(default `~/.vex`) and shared by every project.

`vex` finds the compiler through `$VXC`, then a `vxc` beside itself, then `PATH`. It needs the
`--package` flag, which a `vxc` built from Vx's `main` has; the v0.0.3 release does not.
Building a program needs `vxc -a emit-exe`, and `[link]` needs `vxc -l`; `vex` asks the
compiler (`vxc --help`) whether it has them. Without `emit-exe`, `vex build` writes an object
file, and `vex run` and `vex test` run the program inside `vxc`, which exits with 1 whatever
the program returned.

## The registry

A registry is plain files, so a directory, a git repository or any static web host can be one:

```
index/<name>.jsonl                       one line per published version
archives/<name>/<name>-<version>.tar.gz  the package
```

An index line holds the version, the package's registry dependencies and the SHA-256 of its
archive. `vex publish` writes to a registry in a directory; to run a public one, publish into a
git repository and serve it with GitHub Pages or any other static host. `https://` is required,
except for `localhost`.

How versions are picked: each package names the lowest version of a dependency it works with,
and the build takes the highest of those lowest versions (minimal version selection, as in Go
modules). A version nobody asked for is never used, so publishing a new version cannot change
anyone's build until they ask for it with `vex add` or by editing `vex.toml`. Versions with
different major numbers (or different minor numbers below 1.0.0) cannot both be in one build.

What protects a build:

- A published version never changes: publishing the same version twice is refused.
- An archive is checked against the SHA-256 in the index before it is unpacked, and against
  `vex.lock`, so a registry that changes a package later is caught.
- No package code runs when a package is downloaded or built.
- An archive holds only plain files with relative paths: no links, no `..`.
- Packing is reproducible: the same files always give the same archive bytes, so anyone can
  rebuild an archive from the source and compare its SHA-256.
- A published package cannot have path dependencies, and its registry dependencies must
  already be in the registry.

## Rules it enforces

- One copy of each package in a build: two different packages with one name are an error.
- Packages that depend on each other in a circle are an error.
- A dependency is listed under the package's own name, because that is the name `import` uses.

## Not done

Git dependencies, yanking a bad version, and authentication for publishing to a shared
registry. A package can also import another it does not list, because `vxc` must see every package
in the build.

## Building and testing

```sh
cargo build
cargo test                       # unit tests; the end-to-end tests skip without VXC
VXC=/path/to/vxc cargo test      # also compiles and runs the packages in tests/packages
```

A `vxc` built from source needs the LLVM tools on `PATH` (Vx's `config.local` sets that); the
installed toolchain's wrapper does it by itself.

Licensed under the Apache License 2.0 with LLVM Exceptions, like Vx.
