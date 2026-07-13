#!/usr/bin/env bash

set -Eeuo pipefail

SOURCE_ROOT=/source
WORK_ROOT=/build/work
TARGET_ROOT=/build/target
APPDIR=/build/AppDir
DIST_ROOT=/dist

finish() {
    if [[ "${HOST_UID:-}" =~ ^[0-9]+$ && "${HOST_GID:-}" =~ ^[0-9]+$ ]]; then
        chown -R "${HOST_UID}:${HOST_GID}" "$DIST_ROOT" 2>/dev/null || true
    fi
}
trap finish EXIT

cd /
rm -rf "$WORK_ROOT" "$APPDIR"
mkdir -p "$WORK_ROOT" "$APPDIR/usr/share/doc/texti" "$DIST_ROOT"
find "$DIST_ROOT" -mindepth 1 -maxdepth 1 -delete

rsync --archive \
    --exclude .appimage-work \
    --exclude .cargo-home \
    --exclude .git \
    --exclude dist \
    --exclude target \
    "$SOURCE_ROOT/" "$WORK_ROOT/"
ln -s "$TARGET_ROOT" "$WORK_ROOT/target"

cd "$WORK_ROOT"

version="$(cargo pkgid -p texti-app | sed -E 's/.*[@#]([0-9]+\.[0-9]+\.[0-9]+)$/\1/')"
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    printf 'error: could not resolve Texti version: %s\n' "$version" >&2
    exit 1
fi

deb_path="$DIST_ROOT/texti_${version}-1_amd64.deb"
appimage_path="$DIST_ROOT/Texti-${version}-x86_64.AppImage"

desktop-file-validate packaging/linux/texti.desktop
cargo build --locked --release -p texti-app -j 1
[[ "$(target/release/texti --version)" == "Texti $version" ]]

cargo deb --locked -p texti-app --no-build --output "$deb_path"
lintian --fail-on error "$deb_path"

install -Dm644 LICENSE-MIT "$APPDIR/usr/share/doc/texti/LICENSE-MIT"
install -Dm644 LICENSE-APACHE "$APPDIR/usr/share/doc/texti/LICENSE-APACHE"
install -Dm644 THIRD_PARTY_NOTICES.md "$APPDIR/usr/share/doc/texti/THIRD_PARTY_NOTICES.md"

export ARCH=x86_64
export VERSION="$version"
export OUTPUT="$appimage_path"
linuxdeploy --appimage-extract-and-run \
    --appdir "$APPDIR" \
    --executable target/release/texti \
    --library /lib/x86_64-linux-gnu/libxkbcommon-x11.so.0 \
    --desktop-file packaging/linux/texti.desktop \
    --icon-file packaging/linux/texti.svg \
    --output appimage

chmod 0755 "$appimage_path"
APPIMAGE_EXTRACT_AND_RUN=1 "$appimage_path" --version | grep -Fx "Texti $version"

(
    cd "$DIST_ROOT"
    sha256sum "${deb_path##*/}" "${appimage_path##*/}" > SHA256SUMS
)

printf 'Created Texti %s Linux artifacts in %s\n' "$version" "$DIST_ROOT"
