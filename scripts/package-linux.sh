#!/usr/bin/env bash

set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
IMAGE_NAME="texti-release:ubuntu-22.04-rust-1.97.0"
UBUNTU_IMAGE="ubuntu:22.04@sha256:0d779ea97881505f5ef0039336ee85edba27519bdba968c284c86ee066a973c8"

if ! command -v docker >/dev/null 2>&1; then
    printf 'error: Docker is required to build distributable Linux packages\n' >&2
    exit 2
fi
if [[ "$(uname -m)" != "x86_64" ]]; then
    printf 'error: this release recipe currently supports x86_64 hosts only\n' >&2
    exit 2
fi

mkdir -p "$REPO_ROOT/dist"

docker build \
    --file "$REPO_ROOT/packaging/linux/Dockerfile.release" \
    --tag "$IMAGE_NAME" \
    "$REPO_ROOT"

docker run --rm \
    --env "HOST_UID=$(id -u)" \
    --env "HOST_GID=$(id -g)" \
    --volume "$REPO_ROOT:/source:ro" \
    --volume "$REPO_ROOT/dist:/dist" \
    --volume texti-release-cargo-registry-v1:/usr/local/cargo/registry \
    --volume texti-release-target-rust-1-97:/build/target \
    "$IMAGE_NAME" \
    bash /source/scripts/package-linux-container.sh

docker run --rm \
    --env DEBIAN_FRONTEND=noninteractive \
    --volume "$REPO_ROOT/dist:/dist:ro" \
    "$UBUNTU_IMAGE" \
    bash -c 'apt-get update >/dev/null && apt-get install --yes /dist/texti_*_amd64.deb >/dev/null && texti --version'

printf 'Verified clean Ubuntu 22.04 installation from %s/dist\n' "$REPO_ROOT"
