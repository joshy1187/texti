#!/usr/bin/env bash

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
TMP_ROOT="$(mktemp -d /tmp/texti-local-install.XXXXXX)"

cleanup() {
    rm -rf -- "$TMP_ROOT"
}
trap cleanup EXIT

version="$(cargo pkgid --manifest-path "$REPO_ROOT/Cargo.toml" -p texti-app | sed -E 's/.*[@#]([0-9]+\.[0-9]+\.[0-9]+)$/\1/')"
deb_path="$REPO_ROOT/dist/texti_${version}-1_amd64.deb"
if [[ ! -f "$deb_path" ]]; then
    printf 'error: build and verify %s first\n' "$deb_path" >&2
    exit 1
fi

dpkg-deb --extract "$deb_path" "$TMP_ROOT/deb"
bin_home="$HOME/.local/bin"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"

install -Dm755 "$TMP_ROOT/deb/usr/bin/texti" "$bin_home/texti"
install -Dm644 "$REPO_ROOT/packaging/linux/texti.desktop" "$data_home/applications/texti.desktop"
install -Dm644 "$REPO_ROOT/packaging/linux/texti.svg" "$data_home/icons/hicolor/scalable/apps/texti.svg"
install -Dm644 "$REPO_ROOT/LICENSE-MIT" "$data_home/doc/texti/LICENSE-MIT"
install -Dm644 "$REPO_ROOT/LICENSE-APACHE" "$data_home/doc/texti/LICENSE-APACHE"
install -Dm644 "$REPO_ROOT/THIRD_PARTY_NOTICES.md" "$data_home/doc/texti/THIRD_PARTY_NOTICES.md"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$data_home/applications"
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache --force --ignore-theme-index "$data_home/icons/hicolor" >/dev/null
fi

cmp "$TMP_ROOT/deb/usr/bin/texti" "$bin_home/texti"
[[ "$("$bin_home/texti" --version)" == "Texti $version" ]]
printf 'Installed verified Texti %s release to %s\n' "$version" "$bin_home/texti"
