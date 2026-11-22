# Sourced by the scripts/dev-bin launchers: the config file a launcher runs
# with when it gets no arguments.
#
# dev_config OVERRIDE EXAMPLE prints OVERRIDE (the launcher's WUPHF_*_CONFIG
# value) when it is set, else config.local.toml when the working directory has
# one (git-ignored, the owner's own), else EXAMPLE (the checked-in example), and
# says on stderr which one it picked.
dev_config() {
    local config
    if [ -n "${1:-}" ]; then
        config="$1"
    elif [ -f config.local.toml ]; then
        config=config.local.toml
    else
        config="$2"
    fi
    printf '%s: config %s\n' "$(basename "$0")" "$config" >&2
    printf '%s\n' "$config"
}
