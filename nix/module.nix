# The NixOS module: services.wuphf.<part> runs each part as deploy/systemd's
# units do (one user, a state directory a part, secrets from an environment
# file, the same hardening), its config written from `settings` or given as a
# file.
packages:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib)
    mkEnableOption
    mkIf
    mkOption
    types
    ;
  cfg = config.services.wuphf;
  toml = pkgs.formats.toml { };
  ours = cfg.packages;

  # The parts: their binary, package, how they take a config, and how they
  # stop (the connectors and the provider save their session on SIGINT).
  parts = {
    core = {
      description = "wuphf core server";
      package = ours.wuphf-server;
      binary = "wuphf-server";
      args = config: [
        "serve"
        "--config"
        config
      ];
      sigint = false;
    };
    telegram = connector "Telegram connector" ours.wuphf-telegram "wuphf-telegram";
    whatsapp = connector "WhatsApp connector" ours.wuphf-whatsapp "wuphf-whatsapp";
    whatsapp-rust =
      connector "WhatsApp connector (whatsapp-rust)" ours.wuphf-whatsapp-rust
        "wuphf-whatsapp-rust";
    instagram = connector "Instagram connector" ours.wuphf-instagram "wuphf-instagram";
    mediaconv = connector "sticker converter" ours.wuphf-mediaconv "wuphf-mediaconv" // {
      # It runs ffmpeg and ImageMagick.
      writeExecute = true;
    };
    misc = connector "features (wuphf-misc)" ours.wuphf-misc "wuphf-misc" // {
      stopTimeout = "90s";
    };
  };
  connector = description: package: binary: {
    description = "wuphf ${description}";
    inherit package binary;
    args = config: [
      "--config"
      config
    ];
    sigint = true;
  };

  partOptions = name: part: {
    enable = mkEnableOption part.description;
    package = mkOption {
      type = types.package;
      default = part.package;
      defaultText = lib.literalExpression "wuphf's ${part.binary}";
      description = "The package providing {command}`${part.binary}`.";
    };
    settings = mkOption {
      type = toml.type;
      default = { };
      description = ''
        The part's configuration, written as its TOML file. Every option is
        described in its example file (the core's `config/example.toml`, the
        others' `config.example.toml`). Secrets never go here: name their
        environment variables (`token_env`, `core_token_env`, …) and set
        them in `environmentFile`.
      '';
    };
    configFile = mkOption {
      type = types.path;
      default = toml.generate "wuphf-${name}.toml" cfg.${name}.settings;
      defaultText = lib.literalExpression "a file written from settings";
      description = "The config file, instead of `settings`.";
    };
    environmentFile = mkOption {
      type = types.nullOr types.path;
      default = null;
      example = "/run/secrets/wuphf-${name}.env";
      description = ''
        A file of `NAME=value` lines (the tokens the config names), read by
        systemd. Keep it out of the Nix store.
      '';
    };
  };

  unit =
    name: part:
    let
      settings = cfg.${name};
    in
    {
      description = part.description;
      documentation = [ "https://github.com/wuphf-rs/wuphf-ai/blob/v2/docs/wiki/Home.md" ];
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [
        "network-online.target"
      ]
      ++ lib.optional (name != "core" && cfg.core.enable) "wuphf-core.service";
      serviceConfig = {
        Type = "simple";
        User = cfg.user;
        Group = cfg.group;
        EnvironmentFile = lib.optional (settings.environmentFile != null) settings.environmentFile;
        StateDirectory = "wuphf/${name}";
        StateDirectoryMode = "0750";
        # Relative paths in the configs (sqlite://wuphf.sqlite, session files)
        # live here.
        WorkingDirectory = "%S/wuphf/${name}";
        ExecStart = lib.escapeShellArgs (
          [ (lib.getExe' settings.package part.binary) ] ++ part.args settings.configFile
        );
        Restart = "on-failure";
        RestartSec = "5s";
        KillSignal = if part.sigint then "SIGINT" else "SIGTERM";
        TimeoutStopSec = part.stopTimeout or "30s";

        NoNewPrivileges = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        PrivateTmp = true;
        PrivateDevices = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectKernelLogs = true;
        ProtectControlGroups = true;
        ProtectClock = true;
        ProtectHostname = true;
        RestrictSUIDSGID = true;
        RestrictRealtime = true;
        RestrictNamespaces = true;
        LockPersonality = true;
        MemoryDenyWriteExecute = !(part.writeExecute or false);
        SystemCallArchitectures = "native";
        RestrictAddressFamilies = [
          "AF_INET"
          "AF_INET6"
          "AF_UNIX"
        ];
        UMask = "0027";
      };
    };

  enabled = lib.filterAttrs (name: _: cfg.${name}.enable) parts;
in
{
  options.services.wuphf = {
    packages = mkOption {
      type = types.attrsOf types.anything;
      default = packages pkgs;
      defaultText = lib.literalMD "the flake's packages for the host (those its CI puts in the cache)";
      example = lib.literalExpression "pkgs.wuphf-packages";
      description = ''
        The package set every part's `package` defaults to. The default is the
        flake's own builds, which the binary cache holds, so a machine
        (or a laptop deploying to it, cross or not) downloads them. To build
        them with this system's nixpkgs instead (natively, or cross-compiled
        for a cross-built system), add the flake's `overlays.default` and set
        this to `pkgs.wuphf-packages`.
      '';
    };
    user = mkOption {
      type = types.str;
      default = "wuphf";
      description = "The user every part runs as.";
    };
    group = mkOption {
      type = types.str;
      default = "wuphf";
      description = "The group every part runs as.";
    };
  }
  // lib.mapAttrs (name: part: partOptions name part) parts;

  config = mkIf (enabled != { }) {
    users.users = mkIf (cfg.user == "wuphf") {
      wuphf = {
        isSystemUser = true;
        group = cfg.group;
        home = "/var/lib/wuphf";
      };
    };
    users.groups = mkIf (cfg.group == "wuphf") { wuphf = { }; };

    # The store is read-only: `wuphf-server update` cannot replace anything,
    # and its notice would tell the owner to run it. Update through the flake.
    services.wuphf.core.settings.updates.check_interval_seconds = lib.mkDefault 0;

    # The binaries for setup, settings, export and the TUI.
    environment.systemPackages = lib.mapAttrsToList (name: _: cfg.${name}.package) enabled;

    systemd.services = lib.mapAttrs' (
      name: part: lib.nameValuePair "wuphf-${name}" (unit name part)
    ) enabled;
  };
}
