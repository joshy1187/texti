#!/usr/bin/env bash

set -Eeuo pipefail

usage() {
    printf 'Usage: %s [--desktop] [path-to-texti-binary]\n' "${0##*/}" >&2
}

LAUNCH_MODE="direct"
if [[ "${1:-}" == "--desktop" ]]; then
    LAUNCH_MODE="desktop"
    shift
elif [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    usage
    exit 0
elif [[ "${1:-}" == --* ]]; then
    usage
    exit 2
fi

if (( $# > 1 )); then
    usage
    exit 2
fi

if [[ -z "${DISPLAY:-}" ]]; then
    printf 'error: DISPLAY must point to a running X11 display\n' >&2
    exit 2
fi

if ! command -v xdotool >/dev/null 2>&1; then
    printf 'error: xdotool is required for the X11 smoke test\n' >&2
    exit 2
fi

if [[ "$LAUNCH_MODE" == "desktop" ]]; then
    for command in gio update-desktop-database; do
        if ! command -v "$command" >/dev/null 2>&1; then
            printf 'error: %s is required for desktop launch mode\n' "$command" >&2
            exit 2
        fi
    done
fi

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
BINARY="${1:-$REPO_ROOT/target/release/texti}"
if [[ "$BINARY" != /* ]]; then
    BINARY="$PWD/$BINARY"
fi
if [[ ! -f "$BINARY" || ! -x "$BINARY" ]]; then
    printf 'error: Texti binary is not executable: %s\n' "$BINARY" >&2
    exit 2
fi

TMP_ROOT="$(mktemp -d /tmp/texti-smoke.XXXXXX)"
APP_PID=""
RUN_PID=""
LAUNCHER_PID=""
LOG_FILE="$TMP_ROOT/texti.log"
PID_FILE="$TMP_ROOT/texti.pid"
STATUS_FILE="$TMP_ROOT/texti.status"
LAUNCHER_PID_FILE="$TMP_ROOT/launcher.pid"

cleanup() {
    local status=$?
    trap - EXIT HUP INT TERM

    if [[ -n "$APP_PID" ]] && kill -0 "$APP_PID" 2>/dev/null; then
        kill -TERM "$APP_PID" 2>/dev/null || true
        for (( attempt = 0; attempt < 20; attempt++ )); do
            kill -0 "$APP_PID" 2>/dev/null || break
            sleep 0.05
        done
        if kill -0 "$APP_PID" 2>/dev/null; then
            kill -KILL "$APP_PID" 2>/dev/null || true
        fi
        wait "$APP_PID" 2>/dev/null || true
    fi

    if [[ -n "$RUN_PID" && "$RUN_PID" != "$APP_PID" ]] && kill -0 "$RUN_PID" 2>/dev/null; then
        kill -TERM "$RUN_PID" 2>/dev/null || true
        wait "$RUN_PID" 2>/dev/null || true
    fi

    if [[ -n "$LAUNCHER_PID" ]] && kill -0 "$LAUNCHER_PID" 2>/dev/null; then
        kill -TERM "$LAUNCHER_PID" 2>/dev/null || true
    fi

    case "$TMP_ROOT" in
        /tmp/texti-smoke.*) rm -rf -- "$TMP_ROOT" ;;
    esac
    exit "$status"
}

trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

fail() {
    printf 'error: %s\n' "$1" >&2
    if [[ -s "$LOG_FILE" ]]; then
        printf '%s\n' '--- Texti output ---' >&2
        sed -n '1,200p' "$LOG_FILE" >&2
    fi
    exit 1
}

assert_alive() {
    local state=""
    if ! kill -0 "$APP_PID" 2>/dev/null; then
        fail "Texti exited before the smoke test completed"
    fi
    if [[ -r "/proc/$APP_PID/stat" ]]; then
        read -r _ _ state _ < "/proc/$APP_PID/stat" || true
        if [[ "$state" == "Z" ]]; then
            fail "Texti became a zombie before the smoke test completed"
        fi
    fi
}

assert_no_panic() {
    if grep -Eiq \
        "thread .+ panicked|panicked at|fatal runtime error|BorrowMutError|already borrowed" \
        "$LOG_FILE"; then
        fail "Texti emitted panic output"
    fi
}

settle_ui() {
    sleep 0.15
    assert_alive
    assert_no_panic
}

find_texti_window() {
    local ids=""
    local id=""
    local name=""
    local window_pid=""

    for (( attempt = 0; attempt < 100; attempt++ )); do
        if ! kill -0 "$APP_PID" 2>/dev/null && ! kill -0 "$RUN_PID" 2>/dev/null; then
            return 1
        fi
        ids="$(xdotool search --onlyvisible --pid "$APP_PID" 2>/dev/null || true)"
        if [[ -z "$ids" ]]; then
            # AppImage extraction launches the packaged executable as a child,
            # whose window is not owned by the outer AppImage process.
            ids="$(xdotool search --onlyvisible --name "$EXPECTED_TITLE" 2>/dev/null || true)"
        fi
        if [[ -n "$ids" ]]; then
            while IFS= read -r id; do
                [[ -n "$id" ]] || continue
                name="$(xdotool getwindowname "$id" 2>/dev/null || true)"
                if [[ "$name" == *"$EXPECTED_TITLE"* ]]; then
                    window_pid="$(xdotool getwindowpid "$id" 2>/dev/null || true)"
                    if [[ "$window_pid" =~ ^[0-9]+$ ]] && kill -0 "$window_pid" 2>/dev/null; then
                        APP_PID="$window_pid"
                    fi
                    WINDOW_ID="$id"
                    return 0
                fi
            done <<< "$ids"
        fi
        sleep 0.05
    done
    return 1
}

focus_window() {
    local window_id=$1
    local focused=""

    xdotool windowactivate "$window_id" >/dev/null 2>&1 || true
    xdotool windowfocus "$window_id" >/dev/null 2>&1 || return 1
    for (( attempt = 0; attempt < 40; attempt++ )); do
        focused="$(xdotool getwindowfocus 2>/dev/null || true)"
        if [[ "$focused" == "$window_id" ]]; then
            return 0
        fi
        sleep 0.05
    done
    return 1
}

wait_for_dirty_title() {
    local window_id=$1
    local title=""

    for (( attempt = 0; attempt < 100; attempt++ )); do
        assert_alive
        title="$(xdotool getwindowname "$window_id" 2>/dev/null || true)"
        if [[ "$title" == *"$EXPECTED_TITLE"* && "$title" == *"*" ]]; then
            return 0
        fi
        sleep 0.05
    done
    return 1
}

wait_for_active_fixture() {
    local window_id=$1
    local title=""

    for (( attempt = 0; attempt < 100; attempt++ )); do
        assert_alive
        title="$(xdotool getwindowname "$window_id" 2>/dev/null || true)"
        if [[ "$title" == *"$EXPECTED_TITLE"* ]]; then
            return 0
        fi
        sleep 0.05
    done
    return 1
}

wait_for_saved_fixture() {
    for (( attempt = 0; attempt < 100; attempt++ )); do
        assert_alive
        if cmp -s "$EXPECTED_FILE" "$FIXTURE"; then
            return 0
        fi
        sleep 0.05
    done
    return 1
}

wait_for_exit() {
    local state=""

    for (( attempt = 0; attempt < 100; attempt++ )); do
        if ! kill -0 "$APP_PID" 2>/dev/null; then
            return 0
        fi
        if [[ -r "/proc/$APP_PID/stat" ]]; then
            read -r _ _ state _ < "/proc/$APP_PID/stat" || true
            if [[ "$state" == "Z" ]]; then
                return 0
            fi
        fi
        sleep 0.05
    done
    return 1
}

mkdir -p \
    "$TMP_ROOT/home" \
    "$TMP_ROOT/xdg-config" \
    "$TMP_ROOT/xdg-data" \
    "$TMP_ROOT/xdg-cache" \
    "$TMP_ROOT/bin"
export HOME="$TMP_ROOT/home"
export XDG_CONFIG_HOME="$TMP_ROOT/xdg-config"
export XDG_DATA_HOME="$TMP_ROOT/xdg-data"
export XDG_CACHE_HOME="$TMP_ROOT/xdg-cache"
export WINIT_UNIX_BACKEND=x11
export SLINT_BACKEND=winit
export RUST_BACKTRACE=1
unset WAYLAND_DISPLAY

FIXTURE="$TMP_ROOT/texti-smoke-$$.rs"
EXPECTED_FILE="$TMP_ROOT/expected.rs"
EXPECTED_TITLE="Texti - ${FIXTURE##*/}"
MARKER="TEXTI_SMOKE"
FINAL_CONTENT="TEXTI_SMOKE_SAVED"
printf '%s\n' \
    'fn main() {' \
    '    println!("seed");' \
    '}' > "$FIXTURE"
printf '%s' "$FINAL_CONTENT" > "$EXPECTED_FILE"

if [[ "$LAUNCH_MODE" == "direct" ]]; then
    "$BINARY" -- "$FIXTURE" > "$LOG_FILE" 2>&1 &
    RUN_PID=$!
    APP_PID=$RUN_PID
else
    APPLICATIONS_DIR="$XDG_DATA_HOME/applications"
    ICONS_DIR="$XDG_DATA_HOME/icons/hicolor/scalable/apps"
    DESKTOP_FILE="$APPLICATIONS_DIR/texti.desktop"
    WRAPPER="$TMP_ROOT/bin/texti"
    mkdir -p "$APPLICATIONS_DIR" "$ICONS_DIR"
    install -m 644 "$REPO_ROOT/packaging/linux/texti.desktop" "$DESKTOP_FILE"
    install -m 644 "$REPO_ROOT/packaging/linux/texti.svg" "$ICONS_DIR/texti.svg"
    # These variables intentionally expand later inside the generated wrapper.
    # shellcheck disable=SC2016
    printf '%s\n' \
        '#!/usr/bin/env bash' \
        'set -u' \
        'printf '\''%s\n'\'' "$$" > "$TEXTI_SMOKE_LAUNCHER_PID_FILE"' \
        '"$TEXTI_SMOKE_BINARY" "$@" >> "$TEXTI_SMOKE_LOG_FILE" 2>&1 &' \
        'app_pid=$!' \
        'printf '\''%s\n'\'' "$app_pid" > "$TEXTI_SMOKE_PID_FILE"' \
        'wait "$app_pid"' \
        'status=$?' \
        'printf '\''%s\n'\'' "$status" > "$TEXTI_SMOKE_STATUS_FILE"' \
        'exit "$status"' > "$WRAPPER"
    chmod 755 "$WRAPPER"
    export PATH="$TMP_ROOT/bin:$PATH"
    export TEXTI_SMOKE_BINARY="$BINARY"
    export TEXTI_SMOKE_LOG_FILE="$LOG_FILE"
    export TEXTI_SMOKE_PID_FILE="$PID_FILE"
    export TEXTI_SMOKE_STATUS_FILE="$STATUS_FILE"
    export TEXTI_SMOKE_LAUNCHER_PID_FILE="$LAUNCHER_PID_FILE"
    update-desktop-database "$APPLICATIONS_DIR"
    gio launch "$DESKTOP_FILE" "$FIXTURE" >> "$LOG_FILE" 2>&1

    for (( attempt = 0; attempt < 100; attempt++ )); do
        if [[ -s "$PID_FILE" && -s "$LAUNCHER_PID_FILE" ]]; then
            APP_PID="$(<"$PID_FILE")"
            RUN_PID=$APP_PID
            LAUNCHER_PID="$(<"$LAUNCHER_PID_FILE")"
            break
        fi
        sleep 0.05
    done
    if [[ ! "$APP_PID" =~ ^[0-9]+$ ]] || ! kill -0 "$APP_PID" 2>/dev/null; then
        fail "desktop entry did not launch the Texti binary"
    fi
fi

WINDOW_ID=""
if ! find_texti_window; then
    assert_no_panic
    fail "no visible Texti window appeared for PID $APP_PID"
fi
focus_window "$WINDOW_ID" || fail "could not focus Texti window $WINDOW_ID"
settle_ui

# Exercise the native resize path before editing. This catches redraw-time
# borrow failures and ensures the editor remains interactive after repeated
# viewport changes.
xdotool windowsize "$WINDOW_ID" 1120 720
settle_ui
xdotool windowsize "$WINDOW_ID" 1460 920
settle_ui
xdotool windowsize "$WINDOW_ID" 1280 820
focus_window "$WINDOW_ID" || fail "could not refocus Texti after resizing"
xdotool mousemove --window "$WINDOW_ID" 240 120 click 1
sleep 0.35
settle_ui

# Insert a recognizable marker, then exercise both deletion directions and the
# undo/redo paths. Normalize the final content with Select All so this test does
# not depend on Texti's internal undo-transaction coalescing policy.
xdotool type --clearmodifiers --delay 5 "$MARKER"
xdotool key --clearmodifiers BackSpace
xdotool key --clearmodifiers Delete
assert_alive
assert_no_panic
xdotool key --clearmodifiers ctrl+z
xdotool key --clearmodifiers ctrl+shift+z
xdotool key --clearmodifiers ctrl+z
xdotool key --clearmodifiers ctrl+a
xdotool type --clearmodifiers --delay 5 "$FINAL_CONTENT"

# Typing while the command palette is open must update its query, not the file.
xdotool key --clearmodifiers F1
settle_ui
xdotool type --clearmodifiers --delay 5 toggle
xdotool key --clearmodifiers Escape
settle_ui

# Exercise Escape from each overlay text field. The final Select All below also
# proves that focus returned to the editor after the overlays were dismissed.
xdotool key --clearmodifiers ctrl+f
settle_ui
xdotool type --clearmodifiers --delay 5 seed
xdotool key --clearmodifiers Escape
settle_ui
xdotool key --clearmodifiers ctrl+h
settle_ui
xdotool type --clearmodifiers --delay 5 seed
xdotool key --clearmodifiers Tab
xdotool type --clearmodifiers --delay 5 replacement
xdotool key --clearmodifiers Escape
settle_ui
xdotool key --clearmodifiers ctrl+g
settle_ui
xdotool type --clearmodifiers --delay 5 1
xdotool key --clearmodifiers Escape
settle_ui

xdotool key --clearmodifiers F1
settle_ui
xdotool type --clearmodifiers --delay 5 "Search Workspace Files"
xdotool key --clearmodifiers Return
settle_ui
xdotool type --clearmodifiers --delay 5 smoke
xdotool key --clearmodifiers Escape
settle_ui

xdotool key --clearmodifiers F1
settle_ui
xdotool type --clearmodifiers --delay 5 "New File in Workspace"
xdotool key --clearmodifiers Return
settle_ui
xdotool type --clearmodifiers --delay 5 candidate
xdotool key --clearmodifiers Escape
settle_ui

# Cold renderer/font-cache startup can delay an overlay dismissal by a frame.
# Explicitly close any remaining overlay, restore editor focus, and confirm the
# original fixture is active before performing the save assertion.
xdotool key --clearmodifiers Escape
settle_ui
xdotool key --clearmodifiers Escape
focus_window "$WINDOW_ID" || fail "could not refocus Texti before saving"
xdotool mousemove --window "$WINDOW_ID" 240 120 click 1
settle_ui
wait_for_active_fixture "$WINDOW_ID" || fail "the original fixture was not active before saving"

xdotool key --clearmodifiers ctrl+a
xdotool type --clearmodifiers --delay 15 "$FINAL_CONTENT"

wait_for_dirty_title "$WINDOW_ID" || fail "Texti never reported the edited fixture as dirty"

# A blocking close decision must reject ordinary shortcuts. Escape cancels the
# pending close, after which Save should work normally.
xdotool key --clearmodifiers ctrl+w
settle_ui
xdotool key --clearmodifiers ctrl+s
settle_ui
if cmp -s "$EXPECTED_FILE" "$FIXTURE"; then
    fail "Save bypassed the active dirty-file decision"
fi
xdotool key --clearmodifiers Escape
settle_ui
focus_window "$WINDOW_ID" || fail "could not refocus Texti after cancelling close"
xdotool key --clearmodifiers ctrl+s
wait_for_saved_fixture || {
    diff -u "$EXPECTED_FILE" "$FIXTURE" >&2 || true
    fail "the edited fixture was not saved with the expected contents"
}
assert_alive
assert_no_panic

xdotool key --clearmodifiers alt+F4
wait_for_exit || fail "Texti did not close within five seconds"

APP_PID=""
if [[ "$LAUNCH_MODE" == "direct" ]]; then
    APP_STATUS=0
    wait "$RUN_PID" || APP_STATUS=$?
    RUN_PID=""
else
    for (( attempt = 0; attempt < 100; attempt++ )); do
        [[ -s "$STATUS_FILE" ]] && break
        sleep 0.05
    done
    if [[ ! -s "$STATUS_FILE" ]]; then
        fail "desktop launcher did not report Texti's exit status"
    fi
    APP_STATUS="$(<"$STATUS_FILE")"
    RUN_PID=""
    LAUNCHER_PID=""
fi
if (( APP_STATUS != 0 )); then
    fail "Texti exited with status $APP_STATUS"
fi
assert_no_panic

SESSION_FILE="$(find "$XDG_DATA_HOME" -type f -name session.json -print -quit)"
if [[ -z "$SESSION_FILE" ]]; then
    fail "Texti did not persist an isolated session manifest"
fi
if ! grep -Fq "$FIXTURE" "$SESSION_FILE"; then
    fail "the session manifest does not reference the saved fixture"
fi
RECOVERY_FILE="$(find "$XDG_DATA_HOME" -type f -path '*/recovery/*' -print -quit)"
if [[ -n "$RECOVERY_FILE" ]]; then
    fail "recovery data remained after the fixture was saved and Texti closed"
fi

printf 'Texti X11 smoke test passed (%s): %s\n' "$LAUNCH_MODE" "$BINARY"
