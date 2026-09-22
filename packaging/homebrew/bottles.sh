#!/bin/sh
# Repackage the release archives as Homebrew bottles.
#
#   packaging/homebrew/bottles.sh <version> <dist dir>
#
# For every tracebridge-<target>.tar.gz in <dist dir>, writes
# tracebridge-<version>.<tag>.bottle.tar.gz and its .sha256 next to it. A
# bottle is the installed keg as a tarball: tracebridge/<version>/bin/...
# Homebrew pours a bottle without building anything, so installing from the
# tap does not need Xcode or the Command Line Tools.
#
# Bottle tags: a bottle serves its macOS version and every later one, so the
# macOS bottles use the version of the release runner (macos-14, Sonoma); the
# binaries themselves run on macOS 11 and later.
set -eu

version="${1:?version, e.g. 0.1.0}"
dist="${2:?directory with tracebridge-<target>.tar.gz}"

tag_of() {
    case "$1" in
        aarch64-apple-darwin) echo arm64_sonoma ;;
        x86_64-apple-darwin) echo sonoma ;;
        aarch64-unknown-linux-musl) echo arm64_linux ;;
        x86_64-unknown-linux-musl) echo x86_64_linux ;;
        *) echo "bottles.sh: no bottle tag for $1" >&2; exit 1 ;;
    esac
}

checksum() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi
}

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
for archive in "$dist"/tracebridge-*.tar.gz; do
    base="$(basename "$archive" .tar.gz)"
    case "$base" in *.bottle) continue ;; esac
    target="${base#tracebridge-}"
    tag="$(tag_of "$target")"
    rm -rf "$work/unpack" "$work/tracebridge"
    mkdir -p "$work/unpack" "$work/tracebridge/$version/bin"
    tar -xzf "$archive" -C "$work/unpack"
    cp "$work/unpack/$base/tracebridge" "$work/tracebridge/$version/bin/"
    cp "$work/unpack/$base/LICENSE" "$work/unpack/$base/NOTICE" "$work/unpack/$base/README.md" \
        "$work/tracebridge/$version/"
    chmod 755 "$work/tracebridge/$version/bin/tracebridge"
    bottle="tracebridge-$version.$tag.bottle.tar.gz"
    tar -C "$work" -czf "$dist/$bottle" "tracebridge"
    (cd "$dist" && checksum "$bottle" > "$bottle.sha256")
    echo "$bottle"
done
