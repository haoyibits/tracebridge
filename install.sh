#!/bin/sh
# Install tracebridge from GitHub releases.
#
#   curl -fsSL https://github.com/haoyibits/tracebridge/releases/latest/download/install.sh | sh
#
# Environment:
#   TRACEBRIDGE_VERSION      release tag to install (default: latest), e.g. v0.1.0
#   TRACEBRIDGE_INSTALL_DIR  target directory (default: ~/.local/bin)
#   TRACEBRIDGE_REPO         GitHub repository (default: haoyibits/tracebridge)
#   TRACEBRIDGE_BASE_URL     download from this URL instead of GitHub (mirrors, testing)
set -eu

repo="${TRACEBRIDGE_REPO:-haoyibits/tracebridge}"
version="${TRACEBRIDGE_VERSION:-latest}"
install_dir="${TRACEBRIDGE_INSTALL_DIR:-$HOME/.local/bin}"

fail() {
    echo "install.sh: $*" >&2
    exit 1
}

case "$(uname -s)" in
    Darwin) os=apple-darwin ;;
    Linux) os=unknown-linux-musl ;;
    *) fail "unsupported operating system: $(uname -s)" ;;
esac
case "$(uname -m)" in
    arm64 | aarch64) arch=aarch64 ;;
    x86_64 | amd64) arch=x86_64 ;;
    *) fail "unsupported architecture: $(uname -m)" ;;
esac
target="$arch-$os"
asset="tracebridge-$target.tar.gz"
if [ -n "${TRACEBRIDGE_BASE_URL:-}" ]; then
    base="$TRACEBRIDGE_BASE_URL"
elif [ "$version" = latest ]; then
    base="https://github.com/$repo/releases/latest/download"
else
    base="https://github.com/$repo/releases/download/$version"
fi

command -v curl >/dev/null 2>&1 || fail "curl is required"
temporary="$(mktemp -d)"
trap 'rm -rf "$temporary"' EXIT INT TERM

echo "Downloading $asset ($version)"
curl -fsSL "$base/$asset" -o "$temporary/$asset" || fail "cannot download $base/$asset"
curl -fsSL "$base/$asset.sha256" -o "$temporary/$asset.sha256" || fail "cannot download the checksum"

expected="$(cut -d ' ' -f 1 "$temporary/$asset.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$temporary/$asset" | cut -d ' ' -f 1)"
else
    actual="$(shasum -a 256 "$temporary/$asset" | cut -d ' ' -f 1)"
fi
[ "$expected" = "$actual" ] || fail "checksum mismatch for $asset"

tar -xzf "$temporary/$asset" -C "$temporary"
mkdir -p "$install_dir"
cp "$temporary/tracebridge-$target/tracebridge" "$install_dir/tracebridge.new"
chmod 755 "$install_dir/tracebridge.new"
if [ "$os" = apple-darwin ]; then
    xattr -d com.apple.quarantine "$install_dir/tracebridge.new" 2>/dev/null || true
fi
mv -f "$install_dir/tracebridge.new" "$install_dir/tracebridge"

echo "Installed $("$install_dir/tracebridge" --version) to $install_dir/tracebridge"
case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) echo "Note: $install_dir is not on PATH; add it to your shell profile, e.g. export PATH=\"$install_dir:\$PATH\"" ;;
esac
