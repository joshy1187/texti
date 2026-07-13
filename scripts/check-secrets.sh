#!/usr/bin/env bash

set -Eeuo pipefail

GITLEAKS_VERSION=8.30.1
GITLEAKS_SHA256=551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb
GITLEAKS_BINARY_SHA256=88f91962aa2f93ac6ab281d553b9e125f5197bbbce38f9f2437f7299c32e5509
GITLEAKS_ARCHIVE="gitleaks_${GITLEAKS_VERSION}_linux_x64.tar.gz"
GITLEAKS_URL="https://github.com/gitleaks/gitleaks/releases/download/v${GITLEAKS_VERSION}/${GITLEAKS_ARCHIVE}"
CACHE_ROOT="${XDG_CACHE_HOME:-$HOME/.cache}/texti-release-tools/gitleaks-${GITLEAKS_VERSION}"
GITLEAKS="$CACHE_ROOT/gitleaks"

usage() {
    printf 'Usage: %s [--staged|--directory PATH]\n' "${0##*/}" >&2
}

install_gitleaks() {
    local tmp_root=""

    if [[ -x "$GITLEAKS" ]] \
        && printf '%s  %s\n' "$GITLEAKS_BINARY_SHA256" "$GITLEAKS" | sha256sum --check --status; then
        return
    fi

    if ! command -v curl >/dev/null 2>&1; then
        printf 'error: curl is required to download the pinned Gitleaks scanner\n' >&2
        exit 2
    fi

    tmp_root="$(mktemp -d /tmp/texti-gitleaks.XXXXXX)"
    trap 'rm -rf -- "$tmp_root"' RETURN
    curl --fail --location --silent --show-error --output "$tmp_root/$GITLEAKS_ARCHIVE" "$GITLEAKS_URL"
    printf '%s  %s\n' "$GITLEAKS_SHA256" "$tmp_root/$GITLEAKS_ARCHIVE" | sha256sum --check
    tar -xzf "$tmp_root/$GITLEAKS_ARCHIVE" -C "$tmp_root" gitleaks
    printf '%s  %s\n' "$GITLEAKS_BINARY_SHA256" "$tmp_root/gitleaks" | sha256sum --check
    mkdir -p "$CACHE_ROOT"
    install -m755 "$tmp_root/gitleaks" "$GITLEAKS"
    rm -rf -- "$tmp_root"
    trap - RETURN
}

install_gitleaks

case "${1:-}" in
    "")
        git rev-parse --is-inside-work-tree >/dev/null
        "$GITLEAKS" git --redact=100 --no-banner .
        ;;
    --staged)
        if (( $# != 1 )); then
            usage
            exit 2
        fi
        git rev-parse --is-inside-work-tree >/dev/null
        "$GITLEAKS" git --staged --redact=100 --no-banner .
        ;;
    --directory)
        if (( $# != 2 )); then
            usage
            exit 2
        fi
        "$GITLEAKS" dir --redact=100 --no-banner "$2"
        ;;
    *)
        usage
        exit 2
        ;;
esac
