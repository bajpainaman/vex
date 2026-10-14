#!/bin/sh
# Installs vex, the package tool for Vx, from its GitHub releases.
#
#   curl -fsSL https://raw.githubusercontent.com/bajpainaman/vex/main/install.sh | sh
#
# VEX_VERSION picks a release (default: the latest), and VEX_INSTALL_DIR where vex goes
# (default: ~/.local/bin). The download is checked against the release's SHA256SUMS.
set -eu

repo="bajpainaman/vex"
install_dir="${VEX_INSTALL_DIR:-$HOME/.local/bin}"

fail() {
    echo "error: $*" >&2
    exit 1
}

case "$(uname -s)/$(uname -m)" in
    Darwin/*) build="macos-universal" ;;
    Linux/x86_64 | Linux/amd64) build="linux-x86_64" ;;
    Linux/aarch64 | Linux/arm64) build="linux-arm64" ;;
    *) fail "vex has no build for $(uname -s) on $(uname -m) yet. Build it from source: cargo install --git https://github.com/$repo" ;;
esac

version="${VEX_VERSION:-}"
if [ -z "$version" ]; then
    # github.com redirects releases/latest to the latest tag. The GitHub API would say the same,
    # but it allows only 60 requests an hour per address, which a shared network uses up.
    latest=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest") ||
        fail "cannot reach https://github.com/$repo/releases/latest"
    version=${latest##*/tag/}
    case "$version" in
        v[0-9]*) ;;
        *) fail "cannot find the latest release of $repo (github.com sent $latest)" ;;
    esac
fi

name="vex-$version-$build"
url="https://github.com/$repo/releases/download/$version"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

echo "Downloading $name"
curl -fsSL -o "$work/$name.tar.gz" "$url/$name.tar.gz" || fail "cannot download $url/$name.tar.gz"
curl -fsSL -o "$work/SHA256SUMS" "$url/SHA256SUMS" || fail "cannot download $url/SHA256SUMS"

expected=$(grep " $name.tar.gz\$" "$work/SHA256SUMS" | cut -d ' ' -f 1)
[ -n "$expected" ] || fail "SHA256SUMS has no line for $name.tar.gz"
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$work/$name.tar.gz" | cut -d ' ' -f 1)
else
    actual=$(shasum -a 256 "$work/$name.tar.gz" | cut -d ' ' -f 1)
fi
[ "$actual" = "$expected" ] || fail "$name.tar.gz has SHA-256 $actual, but SHA256SUMS says $expected. Nothing was installed"

tar -xzf "$work/$name.tar.gz" -C "$work"
mkdir -p "$install_dir"
install -m 755 "$work/$name/vex" "$install_dir/vex"
echo "Installed $("$install_dir/vex" --version) to $install_dir/vex"

case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) echo "Add $install_dir to your PATH to run vex: export PATH=\"$install_dir:\$PATH\"" ;;
esac
