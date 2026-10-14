The package tool for [Vx](https://vxlang.org). For macOS (Apple Silicon and Intel) and Linux (x86_64 and arm64).

## Install

On macOS (Apple Silicon or Intel) and Linux (x86_64 or arm64):

```sh
curl -fsSL https://raw.githubusercontent.com/bajpainaman/vex/main/install.sh | sh
```

It picks the right build, checks it against `SHA256SUMS`, and installs `vex` in `~/.local/bin`.
Or download a build below:

| File | For |
|---|---|
| `vex-v0.1.0-macos-universal.tar.gz` | macOS, Apple Silicon and Intel |
| `vex-v0.1.0-linux-x86_64.tar.gz` | Linux on x86_64, any distribution (static) |
| `vex-v0.1.0-linux-arm64.tar.gz` | Linux on arm64, any distribution (static) |

The macOS binary is not signed. A download made with `curl` runs as it is. If you download it
in a browser, macOS blocks it; run `xattr -d com.apple.quarantine vex` once.

## Try it

```sh
export VEX_REGISTRY=https://bajpainaman.github.io/vex-registry
vex new hello && cd hello
vex add intmath
vex test
vex run
```

`vex` runs the `vxc` compiler, so it needs Vx installed. `vex run` and `vex build` make a program
only with a `vxc` that has `-a emit-exe`; otherwise `vex build` writes an object file.

## In this release

- `vex new`, `build`, `run` and `test`.
- A registry: `vex add`, `fetch`, `search`, `package` and `publish`, with `vex.lock`.
  Versions are picked by minimal version selection, as in Go modules.
- `[link]` for packages that wrap a C library.
- `vex registry verify` and `vex registry json` for people who run a registry.
