#!/usr/bin/env bash
# Launch (or re-attach to) the wuphf dev session in zellij: one tab per
# component — server, each connector, the observer TUI — plus a spare shell.
#
# Tabs are shells, not running processes — see scripts/dev-layout.kdl. Each one
# starts its component by name (wuphf-server, wuphf-telegram, …) or by the short
# aliases in scripts/dev-aliases.sh; run `wuphf-help` in any tab for the list.
set -euo pipefail

readonly REPO="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
readonly LAYOUT="$REPO/scripts/dev-layout.kdl"
SESSION="${WUPHF_DEV_SESSION:-wuphf}"

usage() {
    cat <<'USAGE'
Usage: scripts/dev-session.sh [options]

  -s, --session NAME   session name (default: wuphf, or $WUPHF_DEV_SESSION)
  -k, --kill           kill the existing session instead of attaching
  -h, --help           show this help

Attaches to the session when it already exists, so running this twice reuses
the running components rather than starting a second copy of them.
USAGE
}

kill_session=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        -s | --session)
            [ "$#" -ge 2 ] || { echo "error: $1 needs a session name" >&2; exit 2; }
            SESSION="$2"
            shift 2
            ;;
        -k | --kill)
            kill_session=true
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "error: unknown option $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

command -v zellij >/dev/null 2>&1 || {
    echo "error: zellij is not installed or not on PATH" >&2
    exit 1
}

session_exists() {
    zellij list-sessions --short --no-formatting 2>/dev/null |
        grep -qxF "$SESSION"
}

if [ "$kill_session" = true ]; then
    if session_exists; then
        zellij kill-session "$SESSION"
    else
        echo "no session named $SESSION"
    fi
    exit 0
fi

# Attaching first keeps the session single-instance: the components already
# running in it stay running, and their scrollback is still there.
if session_exists; then
    exec zellij attach "$SESSION"
fi

[ -f "$LAYOUT" ] || {
    echo "error: layout not found at $LAYOUT" >&2
    exit 1
}

# Every tab inherits this environment from the zellij server, which is what
# makes the commands and aliases available without touching the user's shell
# configuration.
export PATH="$REPO/scripts/dev-bin:$PATH"
export WUPHF_DEV_SESSION="$SESSION"

# Aliases need the shell's own rc file, which only zsh lets us redirect through
# the environment. Everything still works without this — the dev-bin commands
# are on PATH regardless — so a different shell just misses the shorthand.
if [ "$(basename "${SHELL:-}")" = "zsh" ]; then
    rc_dir="${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}/wuphf-dev-$(id -u)"
    mkdir -p "$rc_dir"
    # The real rc file is sourced first so each tab is the user's normal shell,
    # with the session's aliases layered on top.
    cat > "$rc_dir/.zshrc" <<RC
[ -f "\${WUPHF_DEV_USER_ZDOTDIR:-\$HOME}/.zshrc" ] && . "\${WUPHF_DEV_USER_ZDOTDIR:-\$HOME}/.zshrc"
. "$REPO/scripts/dev-aliases.sh"
RC
    export WUPHF_DEV_USER_ZDOTDIR="${ZDOTDIR:-$HOME}"
    export ZDOTDIR="$rc_dir"
fi

# Layout pane cwds are relative to where zellij starts.
cd "$REPO"

# --new-session-with-layout, not --layout: with a --session flag the latter means
# "add these tabs to that running session", which fails when it does not exist.
exec zellij --session "$SESSION" --new-session-with-layout "$LAYOUT"
