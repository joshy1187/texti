# Texti

[![CI](https://github.com/joshy1187/texti/actions/workflows/ci.yml/badge.svg)](https://github.com/joshy1187/texti/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/joshy1187/texti?display_name=tag)](https://github.com/joshy1187/texti/releases/latest)

Texti is a fast, focused Linux desktop text editor built with Rust and Slint. It
is designed for daily work on source code, configuration, notes, and logs without
growing into an IDE.

## Highlights

- A compact single-row header with visible, scrollable document tabs
- Automatic syntax detection and broad highlighting through `two-face`, Syntect,
  and a bundled Slint grammar
- A searchable command palette with configurable shortcuts and visibility
- Find and replace, go to line, undo and redo, recent files, and workspace search
- A focused dark interface with configurable font size, indentation, wrapping, line
  numbers, whitespace, and an optional minimap
- Session restoration and recovery snapshots for dirty drafts
- Save/discard/cancel prompts and external-file conflict handling
- LF/CRLF and UTF-8/UTF-16 round trips, atomic saves, and local-only data storage
- Read-only previews for binary-looking files and protective handling for large files

Texti intentionally has no permanent sidebar, toolbar, terminal, Git panel, LSP,
plugin system, or manual language picker. Workspace browsing, settings, and other
secondary workflows open as temporary overlays so the document remains central.

## Download

Texti 1.0 release packages target 64-bit Linux systems compatible with Ubuntu
22.04 or newer, Debian 12 or newer, and distributions with glibc 2.35 or newer.
Download the packages and `SHA256SUMS` from the
[latest GitHub release](https://github.com/joshy1187/texti/releases/latest).

Verify a downloaded package before running it:

```bash
sha256sum --check SHA256SUMS --ignore-missing
```

Install the Debian package system-wide:

```bash
sudo apt install ./texti_1.0.0-1_amd64.deb
```

Or run the portable AppImage without installation:

```bash
chmod +x Texti-1.0.0-x86_64.AppImage
./Texti-1.0.0-x86_64.AppImage
```

## Usage

Launch Texti with no arguments for a restored session, or pass any number of files
and folders. An already-open path activates its existing tab.

```bash
texti
texti src/main.rs Cargo.toml
texti ~/projects/texti
texti -- ./-filename-that-starts-with-a-dash
texti --help
texti --version
```

New documents use LF line endings. Existing LF or CRLF documents retain their line
ending style when edited and saved. UTF-8, UTF-8 with BOM, UTF-16 LE with BOM, and
UTF-16 BE with BOM are supported; unknown text formats fall back to plain text.

## Install From Source For The Current User

The default installation is per-user: it does not require `sudo`, replace another
editor, or change existing default-application associations. It installs the
binary to `~/.local/bin` and desktop integration under `$XDG_DATA_HOME` (normally
`~/.local/share`).

```bash
just install-local
```

`just install-local` performs a locked release build, validates the desktop entry,
installs the executable, icon, desktop entry, and licenses, then refreshes desktop
caches when the relevant utilities are available. Ensure `~/.local/bin` is on
`PATH`; log out and back in if the application launcher has not refreshed yet.

To remove only the per-user installation:

```bash
just uninstall-local
```

These recipes do not remove settings, sessions, or recovery data. The local Flatpak
manifest is under `packaging/flatpak`.

## Local Development

The workspace uses the Rust toolchain declared in `rust-toolchain.toml`.

```bash
cargo run -p texti-app
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --locked --release -p texti-app
```

Docker is required to produce distributable packages against the Ubuntu 22.04
compatibility baseline:

```bash
just package-linux
just verify-dist
```

The ignored `dist/` directory receives the `.deb`, `.AppImage`, and
`SHA256SUMS`; release binaries are never committed to Git.

Equivalent development tasks are available in the `justfile`. The complete release
gate is:

```bash
just release-check
```

It checks formatting, Clippy with warnings denied, workspace tests, dependency
policy, Cargo metadata/package contents, the desktop entry, and a locked release
build. `just`, `cargo-deny`, and `desktop-file-validate` must be installed to run
every local source gate.

## Workspace

- `texti-app`: Slint UI, input bridge, editor rendering, dialogs, and overlays
- `texti-core`: application state, document workflows, recovery, and sessions
- `texti-editor`: Ropey-backed buffers, editing, search, and undo/redo
- `texti-fs`: safe workspace and filesystem operations
- `texti-syntax`: automatic language detection and syntax spans
- `texti-settings`: persisted settings, recent files, recovery, and session data
- `texti-model`: shared application contracts

Texti stores settings and recovery data under the platform XDG directories resolved
by the `directories` crate. Document contents and editor state are not sent to a
network service.

On Linux, the usual locations are:

- `$XDG_CONFIG_HOME/texti/settings.json` for preferences
- `$XDG_DATA_HOME/texti/session.json` for the restorable session
- `$XDG_DATA_HOME/texti/recovery/` for dirty-document snapshots

## License

Texti is available under either the [MIT license](LICENSE-MIT) or the
[Apache License 2.0](LICENSE-APACHE), at your option. Redistributed assets are
documented in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

Copyright © 2026 The Clairos Group, LLC. Visit [clairos.ai](https://clairos.ai)
or contact [connect@clairos.ai](mailto:connect@clairos.ai).
