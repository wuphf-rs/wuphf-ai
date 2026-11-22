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
alias wmi='wuphf-misc'
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
  wmi    wuphf-misc         non-bridging features (Instagram downloader)
  wtui   wuphf-tui          observer TUI

Each command runs with no arguments on its component's config.local.toml
(git-ignored; copy the component's config.example.toml to start one), or on
the example when there is none, and says which on stderr. Pass any argument
to drive it yourself, e.g. `wtg reset -config config.local.toml -chat
telegram:channel:123`; arguments replace the config flag too.

Point a command at another config without arguments: WUPHF_SERVER_CONFIG,
WUPHF_TELEGRAM_CONFIG, WUPHF_WHATSAPP_CONFIG, WUPHF_INSTAGRAM_CONFIG,
WUPHF_MEDIACONV_CONFIG, WUPHF_WHATSAPP_RUST_CONFIG, WUPHF_MISC_CONFIG.

scripts/install-dev-bins.sh builds release binaries the commands then run
instead of building on every start; re-run it after changing code, or delete
scripts/dev-bin/.bin/ to build on start again.

Every connector authenticates to the server with a token, and the server
refuses to start until all of them are set. Put them in the git-ignored
.env.local at the repo root, which dev-session.sh exports into every tab
(elsewhere: set -a; . ./.env.local; set +a):
  WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN, WUPHF_CONNECTOR_WHATSAPP_MAIN_TOKEN,
  WUPHF_CONNECTOR_INSTAGRAM_MAIN_TOKEN. wwr also needs
  WUPHF_CONNECTOR_WHATSAPP_RUST_TOKEN, and its [[connectors]] entry uncommented
  in the server config.

wmc needs WUPHF_PROVIDER_MEDIACONV_TOKEN set, and the server config needs the
matching [[functions.providers]] entry (see providers/mediaconv/README.md).

wmi needs WUPHF_OBSERVER_TOKEN (the server's observer_token) and a config
with a feature enabled; the Instagram downloader also needs the Instagram
connector's [functions] section (see misc/README.md).

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
