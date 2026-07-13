# Texti 1.0 Product Notes

## Product Summary

Texti is a Linux-native desktop text editor built as a single Rust process with a
compiled Slint interface. It is designed for fast editing of code, configuration,
notes, and logs while keeping the interface quiet and the file workflows safe.

The document remains the primary surface. Texti has a compact tabs-first header,
but no permanent sidebar, toolbar, search field, terminal, or file-control row.
Commands are available through keyboard shortcuts, a searchable command palette,
one application menu, a focused editor context menu, native file dialogs, and
temporary overlays.

Texti accepts multiple file and folder paths from the command line, including paths
after `--`, and exposes `--help` and `--version` without opening the UI. This keeps
shell use and desktop `%F` multi-file launching predictable.

## Interface And Commands

The single-row header contains window controls, visible document tabs, and the
application menu. Tabs adapt between compact and readable widths, scroll when they
overflow, and show filename, active/dirty/readonly state, and a close control. Tabs
can be activated directly, closed with the close control or middle click, and cycled
with `Ctrl+Tab` and `Ctrl+Shift+Tab`.

The command palette opens with `Ctrl+Shift+P` or the permanent `F1` fallback and
searches file, edit, view, workspace, and settings commands. Palette visibility and
one primary shortcut per command can be customized in Settings. The editor context
menu stays limited to editing, search, and command-palette actions. Workspace
browsing and recent files remain temporary searchable overlays instead of occupying
a persistent sidebar.

The custom editor surface provides:

- Cursor and selection handling, word and document navigation, and caret visibility
- Undo and redo with range-based edit transactions and typing/deletion coalescing
- Copy, cut, paste, select all, word deletion, and selection replacement
- Configurable spaces or tabs, tab width, indent/outdent, and newline auto-indent
- Word wrap, line numbers, whitespace display, and per-tab cursor/selection/scroll state
- Find next/previous with wrapping, clickable results, replace next/all, and go to line
- Double-click word selection and triple-click line selection
- Vertical scrolling, horizontal scrolling for unwrapped text, and middle-click auto-scroll

The optional minimap is off by default. When enabled in View, Settings, or the
command palette, it samples the document at the right edge and supports click/drag
scrolling without adding a permanent toolbar button.

## Settings

Settings apply immediately and are persisted atomically. Available preferences
include:

- A fixed dark interface and dark syntax-highlighting palette
- Editor font size
- Tab width of 2, 4, or 8 and spaces-versus-tabs indentation
- Word wrap, line numbers, minimap, and whitespace visibility
- Recovery, hidden-file visibility, and trash confirmation
- Command-palette visibility and keyboard shortcuts, with conflict validation and reset

Existing settings files remain compatible through serde defaults and runtime
normalization. Automatic syntax detection is retained as the only language mode.

## Syntax Highlighting

Texti automatically detects the active language from filenames, extensions,
shebangs, Syntect metadata, and lightweight content heuristics. `two-face` and
Syntect provide the bundled grammar catalog and render-ready spans for a broad set
of programming, markup, configuration, query, and scripting languages. Slint files
use the bundled official Slint Sublime syntax definition and produce highlighted
spans rather than only a language label. Unknown formats fall back to plain text
rather than being mislabeled.

Texti uses a fixed dark highlighting palette. Syntax results are cached by document
revision, and highlighting is skipped for binary previews and protective
readonly/degraded large-file modes.

## Documents, Sessions, And Recovery

Texti supports untitled documents, native Open and Save As dialogs, recent files,
workspace file search, reload, and canonical-path deduplication. Opening an already
open path activates its existing tab.

Each tab preserves its cursor, selection, preferred column, and scroll position.
Texti persists a versioned session manifest containing the open documents, active
tab, workspace, and view state. Dirty buffers use one stable atomic recovery
snapshot per document, written after a short debounce and flushed when the app
exits. On restart, saved files and recoverable drafts are restored; saving or
discarding a document removes obsolete recovery data.

Closing a dirty tab presents Save, Discard, and Cancel. When recovery is disabled,
quitting with dirty documents uses the same explicit decision flow. With recovery
enabled, normal exit records the session and drafts without requiring every file to
be saved first.

Texti checks file fingerprints when focus returns and before saving. Clean buffers
can reload changed files, while dirty conflicts present Reload, Overwrite, Save As,
and Cancel choices.

## File And Workspace Safety

Texti uses Rust filesystem APIs for normal operations and keeps document data local.

- UTF-8, UTF-8 BOM, UTF-16 LE BOM, and UTF-16 BE BOM text files round-trip their
  encoding; existing LF or CRLF style is preserved, and new documents default to LF.
- Binary-looking files open as readonly byte/hex previews.
- Very large files use explicit opportunistic, degraded, or readonly modes.
- Workspace roots are canonicalized and unsafe child names are rejected.
- Save writes a temporary sibling file, syncs it, and atomically replaces the target.
- Create and rename operations detect destination conflicts.
- Trash uses the XDG Trash layout and can require confirmation.
- Save failures keep the dirty buffer open instead of losing the edit.

Settings, recent files, recovery snapshots, and sessions live in the user's XDG
application directories. Texti does not send document contents, paths, or editor
state to a network service.

## Distribution And Release

The supported default installation is per-user. It places `texti` in
`~/.local/bin`, installs the desktop entry and icon under `$XDG_DATA_HOME`, and
registers common source-code, markup, data, and configuration MIME types without
changing the user's existing default editor. The desktop launcher retains
`Exec=texti %F` so multiple selected files open in one Texti process.

Debian, AppImage, and local Flatpak packaging are maintained. Package metadata uses
The Clairos Group, LLC as the maintainer, links to the public GitHub repository, and
ships the project's real license and required third-party notices.
The release version comes from Cargo package metadata so the CLI, About screen,
binary, and packages report the same value.

The 1.0 release gate requires formatting, workspace tests, Clippy with warnings
denied, dependency-policy checks, Cargo metadata/package validation, desktop-entry
validation, a locked release build, and Debian package creation. The installed
binary is then checked with `texti --version` and smoke-tested from both the shell
and desktop launcher.

## Architecture

- `texti-app`: Slint window, tabs, overlays, input bridge, and visible-row rendering
- `texti-core`: authoritative application state and document/file workflows
- `texti-editor`: Ropey-backed buffers, range edits, search, and undo/redo
- `texti-fs`: canonicalized workspace operations, atomic save, and trash support
- `texti-syntax`: automatic detection and cached highlighted spans
- `texti-settings`: preferences, recent files, recovery snapshots, and sessions
- `texti-model`: shared serializable UI and core contracts

The renderer builds visible rows plus overscan rather than materializing the whole
document for every cursor movement. Layout and syntax work are keyed to document
revision and relevant view settings.

## Product Boundaries

Texti is a focused editor rather than a full IDE. It does not include split views,
LSP integration, an embedded terminal, Git UI, plugins, collaboration, semantic
refactoring, Markdown preview, regex search, or multi-cursor editing. Its custom
editor currently uses stable monospace layout metrics, and the bundled syntax set
falls back to plain text for unsupported niche formats.
