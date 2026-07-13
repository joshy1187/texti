#!/usr/bin/env bash

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
DIST_ROOT="$REPO_ROOT/dist"
TMP_ROOT="$(mktemp -d /tmp/texti-dist-verify.XXXXXX)"

cleanup() {
    rm -rf -- "$TMP_ROOT"
}
trap cleanup EXIT

version="$(cargo pkgid --manifest-path "$REPO_ROOT/Cargo.toml" -p texti-app | sed -E 's/.*[@#]([0-9]+\.[0-9]+\.[0-9]+)$/\1/')"
deb_path="$DIST_ROOT/texti_${version}-1_amd64.deb"
appimage_path="$DIST_ROOT/Texti-${version}-x86_64.AppImage"

for path in "$deb_path" "$appimage_path" "$DIST_ROOT/SHA256SUMS"; do
    if [[ ! -f "$path" ]]; then
        printf 'error: missing release artifact: %s\n' "$path" >&2
        exit 1
    fi
done
if [[ ! -x "$appimage_path" ]]; then
    printf 'error: AppImage is not executable: %s\n' "$appimage_path" >&2
    exit 1
fi

(
    cd "$DIST_ROOT"
    sha256sum --check SHA256SUMS
)

dpkg-deb --info "$deb_path" > "$TMP_ROOT/deb-info.txt"
grep -Fx " Package: texti" "$TMP_ROOT/deb-info.txt"
grep -Fx " Version: ${version}-1" "$TMP_ROOT/deb-info.txt"
grep -Fx " Architecture: amd64" "$TMP_ROOT/deb-info.txt"
grep -F " Maintainer: Clairos Group LLC <connect@clairos.ai>" "$TMP_ROOT/deb-info.txt"
grep -E '^ Depends: .*libxkbcommon-x11-0' "$TMP_ROOT/deb-info.txt"

mkdir -p "$TMP_ROOT/deb" "$TMP_ROOT/appimage"
dpkg-deb --extract "$deb_path" "$TMP_ROOT/deb"
deb_binary="$TMP_ROOT/deb/usr/bin/texti"
[[ "$("$deb_binary" --version)" == "Texti $version" ]]

(
    cd "$TMP_ROOT/appimage"
    "$appimage_path" --appimage-extract >/dev/null
)
appimage_binary="$TMP_ROOT/appimage/squashfs-root/usr/bin/texti"
[[ "$("$appimage_binary" --version)" == "Texti $version" ]]
if [[ -z "$(find "$TMP_ROOT/appimage/squashfs-root/usr/lib" -maxdepth 1 \
    -name 'libxkbcommon-x11.so*' -print -quit)" ]]; then
    printf 'error: AppImage does not bundle libxkbcommon-x11\n' >&2
    exit 1
fi

deb_build_id="$(readelf -n "$deb_binary" | sed -n -E 's/^[[:space:]]*Build ID: ([[:xdigit:]]+)$/\1/p')"
appimage_build_id="$(readelf -n "$appimage_binary" | sed -n -E 's/^[[:space:]]*Build ID: ([[:xdigit:]]+)$/\1/p')"
if [[ -z "$deb_build_id" ]] || [[ "$deb_build_id" != "$appimage_build_id" ]]; then
    printf 'error: Debian and AppImage payload build IDs differ\n' >&2
    exit 1
fi

# linuxdeploy adds an AppImage-local RUNPATH, so the complete ELF files are not
# byte-identical. Compare the linked program sections to prove both packages
# contain the same release build without rejecting that intentional rewrite.
for section in .text .rodata .data; do
    deb_section="$TMP_ROOT/deb-${section#.}"
    appimage_section="$TMP_ROOT/appimage-${section#.}"
    objcopy --only-section="$section" -O binary "$deb_binary" "$deb_section"
    objcopy --only-section="$section" -O binary "$appimage_binary" "$appimage_section"
    if [[ ! -s "$deb_section" ]] || ! cmp -s "$deb_section" "$appimage_section"; then
        printf 'error: Debian and AppImage payload %s sections differ\n' "$section" >&2
        exit 1
    fi
done

max_glibc="$(
    objdump -T "$deb_binary" \
        | sed -n -E 's/.*\(GLIBC_([0-9]+\.[0-9]+)\).*/\1/p' \
        | sort -V \
        | tail -n 1
)"
if [[ -z "$max_glibc" ]] || dpkg --compare-versions "$max_glibc" gt 2.35; then
    printf 'error: release binary requires unsupported GLIBC_%s\n' "${max_glibc:-unknown}" >&2
    exit 1
fi

for relative in \
    usr/share/doc/texti/changelog.Debian.gz \
    usr/share/doc/texti/THIRD_PARTY_NOTICES.md \
    usr/share/applications/texti.desktop \
    usr/share/icons/hicolor/scalable/apps/texti.svg; do
    if [[ ! -f "$TMP_ROOT/deb/$relative" ]]; then
        printf 'error: Debian package is missing %s\n' "$relative" >&2
        exit 1
    fi
done
desktop-file-validate "$TMP_ROOT/deb/usr/share/applications/texti.desktop"

if [[ -n "${DISPLAY:-}" && "${TEXTI_SKIP_UI_SMOKE:-0}" != "1" ]]; then
    "$REPO_ROOT/scripts/smoke-x11.sh" "$deb_binary"
    APPIMAGE_EXTRACT_AND_RUN=1 "$REPO_ROOT/scripts/smoke-x11.sh" "$appimage_path"
fi

printf 'Verified Texti %s release artifacts (build %s, maximum GLIBC_%s)\n' \
    "$version" "$deb_build_id" "$max_glibc"
