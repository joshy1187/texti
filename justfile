set shell := ["bash", "-eu", "-o", "pipefail", "-c"]

dev:
    cargo run -p texti-app

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets -- -D warnings

test:
    cargo test --workspace

nextest:
    cargo nextest run --workspace

deny:
    cargo deny check

secrets:
    scripts/check-secrets.sh

secrets-staged:
    scripts/check-secrets.sh --staged

release:
    cargo build --locked --release -p texti-app

validate-desktop:
    desktop-file-validate packaging/linux/texti.desktop

package-check:
    cargo metadata --no-deps --format-version 1 > /dev/null
    cargo package -p texti-app --list --allow-dirty > /dev/null

check: fmt-check clippy test deny validate-desktop package-check

pkg-deb: release
    cargo deb --locked -p texti-app --no-build

package-linux:
    scripts/package-linux.sh

verify-dist: package-linux
    scripts/verify-dist.sh

release-check: check verify-dist

install-local: release validate-desktop
    #!/usr/bin/env bash
    set -euo pipefail
    bin_home="${HOME}/.local/bin"
    data_home="${XDG_DATA_HOME:-${HOME}/.local/share}"
    install -Dm755 target/release/texti "${bin_home}/texti"
    install -Dm644 packaging/linux/texti.desktop "${data_home}/applications/texti.desktop"
    install -Dm644 packaging/linux/texti.svg "${data_home}/icons/hicolor/scalable/apps/texti.svg"
    install -Dm644 LICENSE-MIT "${data_home}/doc/texti/LICENSE-MIT"
    install -Dm644 LICENSE-APACHE "${data_home}/doc/texti/LICENSE-APACHE"
    install -Dm644 THIRD_PARTY_NOTICES.md "${data_home}/doc/texti/THIRD_PARTY_NOTICES.md"
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "${data_home}/applications"
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1; then
        gtk-update-icon-cache --force --ignore-theme-index "${data_home}/icons/hicolor" >/dev/null
    fi
    printf 'Installed Texti to %s\n' "${bin_home}/texti"

install-dist-local: verify-dist
    scripts/install-dist-local.sh

uninstall-local:
    #!/usr/bin/env bash
    set -euo pipefail
    data_home="${XDG_DATA_HOME:-${HOME}/.local/share}"
    rm -f "${HOME}/.local/bin/texti"
    rm -f "${data_home}/applications/texti.desktop"
    rm -f "${data_home}/icons/hicolor/scalable/apps/texti.svg"
    rm -rf "${data_home}/doc/texti"
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "${data_home}/applications"
    fi
