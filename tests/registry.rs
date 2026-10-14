//===- registry.rs - Vx Package Tool ---------------------------*- Rust -*-===//
//
// Part of the Vx Project, under the Apache License v2.0 with LLVM Exceptions.
// See LICENSE for license information.
// SPDX-License-Identifier: Apache-2.0 WITH LLVM-exception
//
//===----------------------------------------------------------------------===//
//
// Runs the real `vex` program against a registry: `numbers` and `shapes` are published, and `app`
// gets them from the registry instead of from local directories. Only the last test needs a
// compiler; the others stop before `vxc` would run.
//
//===----------------------------------------------------------------------===//
//! End-to-end tests of publishing to a registry and building from one.
#![allow(
    clippy::missing_panics_doc,
    reason = "a test reports a failure by panicking"
)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;

/// A registry, a download cache, and copies of the test packages, all in one temporary directory.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    /// The test packages, with every `{ path = ".." }` dependency changed to `"1.0.0"` so they
    /// come from the registry.
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("create a temporary directory");
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/packages");
        for package in ["app", "shapes", "numbers"] {
            let destination = dir.path().join(package);
            fs::create_dir_all(&destination).expect("create a package directory");
            for entry in fs::read_dir(source.join(package)).expect("list a test package") {
                let file = entry.expect("read a directory entry").path();
                let name = file.file_name().expect("a listed file has a name");
                fs::copy(&file, destination.join(name)).expect("copy a test package file");
            }
            let manifest = destination.join("vex.toml");
            let text = fs::read_to_string(&manifest).expect("read vex.toml");
            let text = text
                .replace("shapes = { path = \"../shapes\" }", "shapes = \"1.0.0\"")
                .replace("numbers = { path = \"../numbers\" }", "numbers = \"1.0.0\"");
            fs::write(&manifest, text).expect("write vex.toml");
        }
        Self { dir }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.path().join(relative)
    }

    fn registry(&self) -> String {
        self.path("registry").display().to_string()
    }

    /// Run `vex` on `package` with this world's cache and no registry from the environment.
    fn vex(&self, package: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vex"))
            .arg("--manifest-path")
            .arg(self.path(package).join("vex.toml"))
            .args(args)
            .env("VEX_HOME", self.path("cache"))
            .env_remove("VEX_REGISTRY")
            .output()
            .expect("start vex")
    }

    fn publish(&self, package: &str) {
        let output = self.vex(package, &["publish", "--registry", &self.registry()]);
        assert!(output.status.success(), "{}", printed(&output));
    }
}

/// What the run printed, on either stream.
fn printed(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_fails_with(output: &Output, message: &str) {
    let printed = printed(output);
    assert_eq!(output.status.code(), Some(1), "{printed}");
    assert!(printed.contains(message), "{printed}");
}

#[test]
fn fetch_downloads_the_packages_and_writes_vex_lock() {
    let world = World::new();
    world.publish("numbers");
    world.publish("shapes");

    let output = world.vex("app", &["fetch", "--registry", &world.registry()]);
    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "numbers 1.0.0\nshapes 1.0.0\n"
    );
    let lock = fs::read_to_string(world.path("app/vex.lock")).expect("vex.lock was written");
    assert!(lock.contains("name = \"numbers\""), "{lock}");
    assert!(lock.contains("name = \"shapes\""), "{lock}");
    let cached = fs::read_dir(world.path("cache/packages"))
        .expect("the cache has packages")
        .count();
    assert_eq!(cached, 2);

    // The second run uses the lock file and the cache, and changes nothing.
    let output = world.vex(
        "app",
        &["fetch", "--locked", "--registry", &world.registry()],
    );
    assert!(output.status.success(), "{}", printed(&output));
}

#[test]
fn a_package_must_publish_its_dependencies_first() {
    let world = World::new();
    let output = world.vex("shapes", &["publish", "--registry", &world.registry()]);
    assert_fails_with(&output, "the dependencies of `shapes` do not resolve");
    assert_fails_with(&output, "has no package `numbers`");
}

#[test]
fn a_package_with_a_path_dependency_cannot_be_published() {
    let world = World::new();
    fs::write(
        world.path("shapes/vex.toml"),
        "[package]\nname = \"shapes\"\nversion = \"1.0.0\"\n\n[dependencies]\nnumbers = { path = \"../numbers\" }\n",
    )
    .expect("write vex.toml");
    let output = world.vex("shapes", &["publish", "--registry", &world.registry()]);
    assert_fails_with(&output, "`shapes` depends on `numbers` by path");
}

#[test]
fn a_package_that_names_link_directories_cannot_be_published() {
    let world = World::new();
    fs::create_dir_all(world.path("numbers/native")).expect("create native");
    fs::write(
        world.path("numbers/vex.toml"),
        "[package]\nname = \"numbers\"\nversion = \"1.0.0\"\n\n[link]\nlibraries = [\"z\"]\ndirs = [\"native\"]\n",
    )
    .expect("write vex.toml");
    let output = world.vex("numbers", &["publish", "--registry", &world.registry()]);
    assert_fails_with(&output, "`numbers` names `dirs` under `[link]`");
}

#[test]
fn a_published_version_cannot_be_published_again() {
    let world = World::new();
    world.publish("numbers");
    let output = world.vex("numbers", &["publish", "--registry", &world.registry()]);
    assert_fails_with(&output, "a published version never changes");
}

#[test]
fn an_archive_changed_in_the_registry_is_not_installed() {
    let world = World::new();
    world.publish("numbers");
    let archive = world.path("registry/archives/numbers/numbers-1.0.0.tar.gz");
    let mut bytes = fs::read(&archive).expect("read the archive");
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(&archive, bytes).expect("change the archive");

    let output = world.vex("shapes", &["fetch", "--registry", &world.registry()]);
    assert_fails_with(
        &output,
        "The download was damaged or changed; nothing was installed",
    );
    assert!(!world.path("cache/packages").exists());
}

#[test]
fn an_index_that_disagrees_with_its_package_is_refused() {
    let world = World::new();
    world.publish("numbers");
    world.publish("shapes");
    // The index now says `shapes` needs nothing, but its archive's vex.toml still needs `numbers`.
    let index = world.path("registry/index/shapes.jsonl");
    let text = fs::read_to_string(&index).expect("read the index");
    fs::write(
        &index,
        text.replace(
            "\"dependencies\":{\"numbers\":\"1.0.0\"}",
            "\"dependencies\":{}",
        ),
    )
    .expect("change the index");

    let output = world.vex("app", &["fetch", "--registry", &world.registry()]);
    assert_fails_with(
        &output,
        "the registry package `shapes` 1.0.0 does not match its index entry",
    );
}

#[test]
fn locked_refuses_to_write_a_missing_lock_file() {
    let world = World::new();
    world.publish("numbers");
    let output = world.vex(
        "shapes",
        &["fetch", "--locked", "--registry", &world.registry()],
    );
    assert_fails_with(
        &output,
        "is missing, and --locked does not allow changing it",
    );
}

#[test]
fn a_registry_dependency_without_a_registry_is_an_error() {
    let world = World::new();
    let output = world.vex("shapes", &["fetch"]);
    assert_fails_with(
        &output,
        "`shapes` asks for `numbers` 1.0.0 from a registry, but no registry is set",
    );
}

#[test]
fn add_writes_the_latest_version_into_vex_toml() {
    let world = World::new();
    world.publish("numbers");
    fs::write(
        world.path("numbers/vex.toml"),
        "[package]\nname = \"numbers\"\nversion = \"1.3.0\"\n",
    )
    .expect("raise the version");
    world.publish("numbers");
    let manifest = world.path("shapes/vex.toml");
    fs::write(
        &manifest,
        "# shapes keeps this comment\n[package]\nname = \"shapes\"\nversion = \"1.0.0\"\n",
    )
    .expect("write vex.toml");

    let output = world.vex(
        "shapes",
        &["add", "numbers", "--registry", &world.registry()],
    );
    assert!(output.status.success(), "{}", printed(&output));
    let text = fs::read_to_string(&manifest).expect("read vex.toml");
    assert!(text.contains("numbers = \"1.3.0\""), "{text}");
    assert!(text.contains("# shapes keeps this comment"), "{text}");

    let output = world.vex(
        "shapes",
        &["add", "numbers@1.0.0", "--registry", &world.registry()],
    );
    assert!(output.status.success(), "{}", printed(&output));
    let text = fs::read_to_string(&manifest).expect("read vex.toml");
    assert!(text.contains("numbers = \"1.0.0\""), "{text}");

    let output = world.vex(
        "shapes",
        &["add", "numbers@2.0.0", "--registry", &world.registry()],
    );
    assert_fails_with(&output, "has no version 2.0.0 of `numbers`");
}

/// Serve the files under `root` over plain HTTP on this computer, the way a static web host
/// would, for as many requests as come.
fn serve(root: PathBuf) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listen on a free port");
    let address = listener.local_addr().expect("the listening address");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.expect("accept a connection");
            let mut request_line = String::new();
            BufReader::new(&stream)
                .read_line(&mut request_line)
                .expect("read the request");
            let path = request_line.split(' ').nth(1).unwrap_or("/");
            let file = root.join(path.trim_start_matches('/'));
            let response = match fs::read(&file) {
                Ok(body) => {
                    let mut head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    head.extend(body);
                    head
                }
                Err(_) => {
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_vec()
                }
            };
            stream.write_all(&response).expect("send the response");
        }
    });
    format!("http://{address}")
}

#[test]
fn a_registry_can_be_a_static_web_site() {
    let world = World::new();
    world.publish("numbers");
    world.publish("shapes");
    let url = serve(world.path("registry"));

    let output = world.vex("app", &["fetch", "--registry", &url]);
    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "numbers 1.0.0\nshapes 1.0.0\n"
    );

    let output = world.vex("app", &["add", "nothing_here", "--registry", &url]);
    assert_fails_with(&output, "has no package `nothing_here`");
}

#[test]
fn run_compiles_a_program_whose_packages_come_from_the_registry() {
    let Some(compiler) = std::env::var_os("VXC") else {
        eprintln!(
            "skipped run_compiles_a_program_whose_packages_come_from_the_registry: set VXC to a \
             vxc built from Vx's main to run it"
        );
        return;
    };
    let world = World::new();
    world.publish("numbers");
    world.publish("shapes");

    let output = Command::new(env!("CARGO_BIN_EXE_vex"))
        .arg("--manifest-path")
        .arg(world.path("app/vex.toml"))
        .args(["run", "--registry", &world.registry()])
        .env("VEX_HOME", world.path("cache"))
        .env("VXC", compiler)
        .output()
        .expect("start vex");
    let printed = printed(&output);
    assert_eq!(output.status.code(), Some(0), "{printed}");
    // 18 is twice(3 * 3): `shapes::area` from the registry called `numbers::twice` from the
    // registry.
    assert!(printed.contains("area 18"), "{printed}");
}

#[test]
fn verify_accepts_a_good_registry_and_lists_every_problem_in_a_bad_one() {
    let world = World::new();
    world.publish("numbers");
    world.publish("shapes");
    let output = world.vex(
        "app",
        &["registry", "verify", "--registry", &world.registry()],
    );
    assert!(output.status.success(), "{}", printed(&output));
    assert!(
        printed(&output).contains("2 published versions checked, no problems"),
        "{}",
        printed(&output)
    );

    // Two separate faults: one archive changed, one index line that disagrees with its package.
    let archive = world.path("registry/archives/numbers/numbers-1.0.0.tar.gz");
    let mut bytes = fs::read(&archive).expect("read the archive");
    bytes.push(0);
    fs::write(&archive, bytes).expect("change the archive");
    let index = world.path("registry/index/shapes.jsonl");
    let text = fs::read_to_string(&index).expect("read the index");
    fs::write(
        &index,
        text.replace(
            "\"dependencies\":{\"numbers\":\"1.0.0\"}",
            "\"dependencies\":{}",
        ),
    )
    .expect("change the index");

    let output = world.vex(
        "app",
        &["registry", "verify", "--registry", &world.registry()],
    );
    assert_fails_with(&output, "2 problem(s) in the registry");
    assert_fails_with(&output, "numbers 1.0.0: the archive of `numbers` 1.0.0");
    assert_fails_with(
        &output,
        "shapes 1.0.0: the registry package `shapes` 1.0.0 does not match",
    );
}

#[test]
fn json_describes_each_package_and_search_reads_it() {
    let world = World::new();
    fs::write(
        world.path("numbers/vex.toml"),
        "[package]\nname = \"numbers\"\nversion = \"1.0.0\"\ndescription = \"Small helpers for integers\"\n",
    )
    .expect("write vex.toml");
    fs::write(
        world.path("numbers/README.md"),
        "# numbers\n\nDoubles things.\n",
    )
    .expect("write README.md");
    fs::create_dir_all(world.path("numbers/tests")).expect("create numbers/tests");
    fs::write(
        world.path("numbers/tests/twice_works.vx"),
        "fn main() -> i32 { return 0; }\n",
    )
    .expect("write a test");
    world.publish("numbers");
    world.publish("shapes");

    let output = world.vex(
        "app",
        &["registry", "json", "--registry", &world.registry()],
    );
    assert!(output.status.success(), "{}", printed(&output));
    let json: serde_json::Value = serde_json::from_slice(
        &fs::read(world.path("registry/packages.json")).expect("packages.json was written"),
    )
    .expect("packages.json is JSON");
    let numbers = &json["packages"][0];
    assert_eq!(numbers["name"], "numbers");
    assert_eq!(numbers["latest"], "1.0.0");
    assert_eq!(numbers["description"], "Small helpers for integers");
    assert_eq!(numbers["readme"], "# numbers\n\nDoubles things.\n");
    assert_eq!(numbers["modules"], serde_json::json!(["twice"]));
    assert_eq!(numbers["used_by"], serde_json::json!(["shapes"]));
    assert_eq!(
        json["packages"][1]["versions"][0]["dependencies"]["numbers"],
        "1.0.0"
    );

    let output = world.vex(
        "app",
        &["search", "INTEGERS", "--registry", &world.registry()],
    );
    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "numbers 1.0.0  Small helpers for integers\n"
    );
}
