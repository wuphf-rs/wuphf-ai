# Short aliases for the wuphf dev session, sourced by every tab's shell.
#
# The commands themselves are real scripts in scripts/dev-bin, which the
# launcher puts on PATH — they work in any shell, and outside zellij too. These
# aliases are only shorthand on top of them.
#
# Source this by hand in a shell the launcher did not set up:
#     source scripts/dev-aliases.sh

alias ws='wuphf-server'
alias wtg='wuphf-telegram'
alias wwa='wuphf-whatsapp'
alias wwr='wuphf-whatsapp-rust'
alias wig='wuphf-instagram'
alias wmc='wuphf-mediaconv'
alias wtui='wuphf-tui'

# wuphf-help lists what this session provides. Kept as a function so it is
# available to re-run after the start-up banner scrolls away.
wuphf-help() {
    cat <<'HELP'
wuphf dev session

  ws     wuphf-server       core server      (config/example.toml)
  wtg    wuphf-telegram     Telegram connector
  wwa    wuphf-whatsapp     WhatsApp connector
  wwr    wuphf-whatsapp-rust  WhatsApp connector (whatsapp-rust)
  wig    wuphf-instagram    Instagram connector
  wmc    wuphf-mediaconv    sticker converter (media.convert@1 provider)
  wtui   wuphf-tui          observer TUI

Each command runs with no arguments for the usual dev config. Pass any
argument to drive it yourself, e.g. `wtg reset -chat telegram:channel:123`
or `ws serve --config config.local.toml`.

Override a config without arguments: WUPHF_SERVER_CONFIG,
WUPHF_TELEGRAM_CONFIG, WUPHF_WHATSAPP_CONFIG, WUPHF_INSTAGRAM_CONFIG,
WUPHF_MEDIACONV_CONFIG, WUPHF_WHATSAPP_RUST_CONFIG.

scripts/install-dev-bins.sh builds release binaries the commands then run
instead of building on every start; re-run it after changing code, or delete
scripts/dev-bin/.bin/ to build on start again.

wmc needs WUPHF_PROVIDER_MEDIACONV_TOKEN set, and the server config needs the
matching [[functions.providers]] entry (see providers/mediaconv/README.md).

Tabs: Alt+<number> to jump, Alt+h / Alt+l to cycle.
Start the server before the connectors: they connect to it on startup.
HELP
}

# Show the banner once per tab. The marker is exported, so subshells started
# inside a tab stay quiet.
if [ -n "${WUPHF_DEV_SESSION:-}" ] && [ -z "${WUPHF_DEV_BANNER:-}" ]; then
    export WUPHF_DEV_BANNER=1
    wuphf-help
fi
