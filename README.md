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
shapes = { path = "../shapes" }
```

`name` is what `import` uses: a file `sides.vx` in this package is `import app::sides`. A
package with a `main.vx` is a program; one without is a library. Dependencies are local paths
for now.

## Commands

```sh
vex run              # compile the package and run it; the exit code is the program's
vex build            # compile to target/<name>.o
vex run -- --steps 3 # arguments after -- go to the program
```

Both take `--machine FILE` and `--host FILE|default`, which go to `vxc` unchanged, and
`--manifest-path FILE` to name a `vex.toml` that is not in this directory or above it.

`vex` finds the compiler through `$VXC`, then a `vxc` beside itself, then `PATH`. It needs the
`--package` flag, which a `vxc` built from Vx's `main` has; the v0.0.3 release does not.

## Rules it enforces

- One copy of each package in a build: two different packages with one name are an error.
- Packages that depend on each other in a circle are an error.
- A dependency is listed under the package's own name, because that is the name `import` uses.

## Not done

Git dependencies, a lock file, version ranges, a registry, and linking a C library a package
wraps. A package can also import another it does not list, because `vxc` must see every package
in the build.

## Building and testing

```sh
cargo build
cargo test                       # unit tests; the end-to-end tests skip without VXC
VXC=/path/to/vxc cargo test      # also runs three packages through a real compiler
```

A `vxc` built from source needs the LLVM tools on `PATH` (Vx's `config.local` sets that); the
installed toolchain's wrapper does it by itself.

Licensed under the Apache License 2.0 with LLVM Exceptions, like Vx.
