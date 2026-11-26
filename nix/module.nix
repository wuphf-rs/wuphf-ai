# The NixOS module. A core (services.wuphf.core), any number of connectors
# and providers by id (services.wuphf.connectors.<id>, .providers.<id>), and
# wuphf-misc, each a unit as deploy/systemd's (one user, a state directory a
# unit, secrets from an environment file, the same hardening).
#
# A connector is described once: the module writes the core's [[connectors]]
# entry and the connector's own connector_id, core_address, token variable
# and the flags both sides must agree on. Its token lives in its own env file
# only; the core reads every connector's and provider's.
#
# Parts can run on several machines: the core's host describes a connector or
# provider that runs elsewhere with `remote = true` (the core gets its entry,
# no unit starts), and the other host describes the same id with the core off
# and `coreAddress` pointing at it (core.firewallInterfaces lets it in).
packages:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib)
    mkDefault
    mkEnableOption
    mkIf
    mkOption
    types
    ;
  cfg = config.services.wuphf;
  toml = pkgs.formats.toml { };
  ours = cfg.packages;

  # The kinds of connector: the platform the core knows them as, and which of
  # the flags both sides share their config has.
  connectorTypes = {
    telegram = {
      description = "Telegram connector";
      binary = "wuphf-telegram";
      platform = "telegram";
      flags = [
        "threads"
        "topicCreate"
      ];
    };
    whatsapp = {
      description = "WhatsApp connector";
      binary = "wuphf-whatsapp";
      platform = "whatsapp";
      flags = [
        "threads"
        "topicCreate"
        "disappearingTimers"
      ];
    };
    whatsapp-rust = {
      description = "WhatsApp connector (whatsapp-rust)";
      binary = "wuphf-whatsapp-rust";
      platform = "whatsapp";
      flags = [ "disappearingTimers" ];
    };
    instagram = {
      description = "Instagram connector";
      binary = "wuphf-instagram";
      platform = "instagram";
      flags = [
        "threads"
        "topicCreate"
      ];
      # Serves functions to the core (its [functions] section).
      provides = true;
    };
  };

  providerTypes = {
    mediaconv = {
      description = "sticker converter";
      binary = "wuphf-mediaconv";
      functions = [ "media.convert" ];
      # It runs ffmpeg and ImageMagick.
      writeExecute = true;
    };
  };

  # Each flag: its option's description and the key both configs use.
  flags = {
    threads = {
      key = "supports_threads";
      description = "Chats have threads (Telegram forum topics).";
    };
    topicCreate = {
      key = "supports_topic_create";
      description = "The connector can create threads (a topic per mirrored chat).";
    };
    disappearingTimers = {
      key = "supports_disappearing_timers";
      description = "Stamp chats' disappearing-message timers on what it sends (WhatsApp).";
    };
  };

  # `whatsapp-jio` → `WHATSAPP_JIO`, as the token variables are named.
  envName = id: lib.toUpper (builtins.replaceStrings [ "-" "." ] [ "_" "_" ] id);

  secretFile =
    id: if cfg.secretsDirectory == null then null else "${cfg.secretsDirectory}/wuphf-${id}.env";

  # The port of a `host:port` address (`0.0.0.0:50051`, `[::]:50051`).
  portOf = address: lib.toInt (lib.last (lib.splitString ":" address));

  # Whether only this machine can reach an address.
  isLoopback =
    address:
    lib.any (prefix: lib.hasPrefix prefix address) [
      "127."
      "localhost:"
      "[::1]:"
    ];

  # A connector or provider that runs on another machine.
  remoteOption =
    kind:
    mkOption {
      type = types.bool;
      default = false;
      description = ''
        It runs on another machine, which describes the same ${kind} (same
        id) with `core.enable = false` and `coreAddress` set to this core.
        Here the core still gets its entry and reads its tokens from
        `environmentFile` (the same values as on that machine), but no unit
        is started.
      '';
    };

  # The options every unit has: its package, its config (as Nix, or a file)
  # and its env file (by default in secretsDirectory, when the unit has
  # secrets of its own).
  unitOptions =
    {
      id,
      binary,
      settings,
      example,
      secrets ? true,
    }:
    {
      package = mkOption {
        type = types.package;
        default = ours.${binary};
        defaultText = lib.literalExpression "wuphf's ${binary}";
        description = "The package providing {command}`${binary}`.";
      };
      settings = mkOption {
        type = toml.type;
        default = { };
        description = ''
          Its configuration, written as its TOML file (every option is in
          ${example}); what the module derives is set already. Secrets never
          go here: name their variables and set them in `environmentFile`.
        '';
      };
      configFile = mkOption {
        type = types.path;
        default = toml.generate "wuphf-${id}.toml" settings;
        defaultText = lib.literalExpression "a file written from settings";
        description = "The config file, instead of `settings`.";
      };
      environmentFile = mkOption {
        type = types.nullOr types.path;
        default = if secrets then secretFile id else null;
        defaultText =
          if secrets then
            lib.literalExpression ''"''${secretsDirectory}/wuphf-${id}.env", or null''
          else
            lib.literalExpression "null";
        description = ''
          A file of `NAME=value` lines (its tokens), read by systemd. Keep it
          out of the Nix store.
        '';
      };
    };

  connectorType = types.submodule (
    { name, config, ... }:
    let
      kind = connectorTypes.${config.type};
    in
    {
      options = {
        enable = mkOption {
          type = types.bool;
          default = true;
          description = "Whether to run this connector.";
        };
        remote = remoteOption "connector";
        type = mkOption {
          type = types.enum (lib.attrNames connectorTypes);
          description = "The connector's kind.";
        };
        tokenEnv = mkOption {
          type = types.str;
          default = "WUPHF_CONNECTOR_${envName name}_TOKEN";
          defaultText = lib.literalExpression ''"WUPHF_CONNECTOR_<ID>_TOKEN"'';
          description = "The variable holding its token, set in its `environmentFile`.";
        };
        ownerIds = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "704338780" ];
          description = "The platform's user ids that may change settings through this connector (`/wuphf`).";
        };
        functions = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "instagram.fetch_media" ];
          description = ''
            Functions it serves to the core, under its own id (Instagram's
            downloads, for wuphf-misc); its token is
            `WUPHF_PROVIDER_<ID>_TOKEN`, in its `environmentFile`.
          '';
        };
        coreSettings = mkOption {
          type = toml.type;
          default = { };
          description = "More keys for its `[[connectors]]` entry in the core's config.";
        };
      }
      // lib.mapAttrs (
        _: flag:
        mkOption {
          type = types.bool;
          default = false;
          description = flag.description + " Set on both sides.";
        }
      ) flags
      // unitOptions {
        id = name;
        inherit (kind) binary;
        inherit (config) settings;
        example = "its `config.example.toml`";
      };

      config.settings = {
        connector_id = mkDefault name;
        core_address = mkDefault cfg.coreAddress;
        core_token_env = mkDefault config.tokenEnv;
      }
      // lib.listToAttrs (map (flag: lib.nameValuePair flags.${flag}.key config.${flag}) kind.flags)
      // lib.optionalAttrs (config.functions != [ ]) {
        functions = {
          provider_id = mkDefault name;
          token_env = mkDefault "WUPHF_PROVIDER_${envName name}_TOKEN";
        };
      };
    }
  );

  providerType = types.submodule (
    { name, config, ... }:
    let
      kind = providerTypes.${config.type};
    in
    {
      options = {
        enable = mkOption {
          type = types.bool;
          default = true;
          description = "Whether to run this provider.";
        };
        remote = remoteOption "provider";
        type = mkOption {
          type = types.enum (lib.attrNames providerTypes);
          description = "The provider's kind.";
        };
        tokenEnv = mkOption {
          type = types.str;
          default = "WUPHF_PROVIDER_${envName name}_TOKEN";
          defaultText = lib.literalExpression ''"WUPHF_PROVIDER_<ID>_TOKEN"'';
          description = "The variable holding its token, set in its `environmentFile`.";
        };
        functions = mkOption {
          type = types.listOf types.str;
          default = kind.functions;
          defaultText = lib.literalMD "what its kind serves (mediaconv: `media.convert`)";
          description = "The function names it may register.";
        };
      }
      // unitOptions {
        id = name;
        inherit (kind) binary;
        inherit (config) settings;
        example = "its `config.example.toml`";
      };

      config.settings = {
        provider_id = mkDefault name;
        core_address = mkDefault cfg.coreAddress;
        token_env = mkDefault config.tokenEnv;
      };
    }
  );

  # What the core knows of (every enabled one, here or remote); `local` keeps
  # those that run on this machine.
  connectors = lib.filterAttrs (_: connector: connector.enable) cfg.connectors;
  providers = lib.filterAttrs (_: provider: provider.enable) cfg.providers;
  local = lib.filterAttrs (_: part: !part.remote);
  remotes = lib.attrNames (lib.filterAttrs (_: part: part.remote) (connectors // providers));

  # Every running unit: its binary, arguments, env files and how it stops
  # (the connectors and providers save their sessions on SIGINT).
  units =
    lib.optionalAttrs cfg.core.enable {
      core = {
        description = "wuphf core server";
        inherit (cfg.core) package configFile;
        binary = "wuphf-server";
        args = [ "serve" ];
        # The tokens it checks live in each part's own file.
        environmentFiles = [
          cfg.core.environmentFile
        ]
        ++ lib.mapAttrsToList (_: unit: unit.environmentFile) (connectors // providers);
        sigint = false;
      };
    }
    // lib.mapAttrs (id: connector: {
      description = "wuphf ${connectorTypes.${connector.type}.description} (${id})";
      inherit (connector) package configFile;
      binary = connectorTypes.${connector.type}.binary;
      environmentFiles = [ connector.environmentFile ];
    }) (local connectors)
    // lib.mapAttrs (id: provider: {
      description = "wuphf ${providerTypes.${provider.type}.description} (${id})";
      inherit (provider) package configFile;
      binary = providerTypes.${provider.type}.binary;
      environmentFiles = [ provider.environmentFile ];
      writeExecute = providerTypes.${provider.type}.writeExecute or false;
    }) (local providers)
    // lib.optionalAttrs cfg.misc.enable {
      misc = {
        description = "wuphf features (wuphf-misc)";
        inherit (cfg.misc) package configFile;
        binary = "wuphf-misc";
        # The observer token is the core's.
        environmentFiles = [
          cfg.core.environmentFile
          cfg.misc.environmentFile
        ];
        stopTimeout = "90s";
      };
    };

  unit = id: spec: {
    description = spec.description;
    documentation = [ "https://github.com/wuphf-rs/wuphf-ai/blob/main/docs/wiki/Home.md" ];
    wantedBy = [ "multi-user.target" ];
    wants = [ "network-online.target" ];
    after = [
      "network-online.target"
    ]
    ++ lib.optional (id != "core" && cfg.core.enable) "wuphf-core.service";
    serviceConfig = {
      Type = "simple";
      User = cfg.user;
      Group = cfg.group;
      EnvironmentFile = lib.unique (lib.filter (file: file != null) spec.environmentFiles);
      StateDirectory = "wuphf/${id}";
      StateDirectoryMode = "0750";
      # Relative paths in the configs (sqlite://wuphf.sqlite, session files)
      # live here.
      WorkingDirectory = "%S/wuphf/${id}";
      ExecStart = lib.escapeShellArgs (
        [ (lib.getExe' spec.package spec.binary) ]
        ++ spec.args or [ ]
        ++ [
          "--config"
          spec.configFile
        ]
      );
      Restart = "on-failure";
      RestartSec = "5s";
      KillSignal = if spec.sigint or true then "SIGINT" else "SIGTERM";
      TimeoutStopSec = spec.stopTimeout or "30s";

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
      MemoryDenyWriteExecute = !(spec.writeExecute or false);
      SystemCallArchitectures = "native";
      RestrictAddressFamilies = [
        "AF_INET"
        "AF_INET6"
        "AF_UNIX"
      ];
      UMask = "0027";
    };
  };

  # Connectors setting a flag their kind's config does not have.
  badFlags = lib.concatLists (
    lib.mapAttrsToList (
      id: connector:
      map (flag: "${id}.${flag}") (
        lib.filter (flag: connector.${flag} && !(lib.elem flag connectorTypes.${connector.type}.flags)) (
          lib.attrNames flags
        )
      )
    ) cfg.connectors
  );
  badFunctions = lib.attrNames (
    lib.filterAttrs (
      _: connector: connector.functions != [ ] && !(connectorTypes.${connector.type}.provides or false)
    ) cfg.connectors
  );
  clashes =
    lib.intersectLists (lib.attrNames cfg.connectors) (lib.attrNames cfg.providers)
    ++ lib.intersectLists [
      "core"
      "misc"
    ] (lib.attrNames cfg.connectors ++ lib.attrNames cfg.providers);

  removed = name: replacement: lib.mkRemovedOptionModule [ "services" "wuphf" name ] replacement;
in
{
  imports = [
    (removed "telegram" "Use services.wuphf.connectors.<id> with type = \"telegram\".")
    (removed "whatsapp" "Use services.wuphf.connectors.<id> with type = \"whatsapp\".")
    (removed "whatsapp-rust" "Use services.wuphf.connectors.<id> with type = \"whatsapp-rust\".")
    (removed "instagram" "Use services.wuphf.connectors.<id> with type = \"instagram\".")
    (removed "mediaconv" "Use services.wuphf.providers.<id> with type = \"mediaconv\".")
  ];

  options.services.wuphf = {
    packages = mkOption {
      type = types.attrsOf types.anything;
      default = packages pkgs;
      defaultText = lib.literalMD "the flake's packages for the host (those its CI puts in the cache)";
      example = lib.literalExpression "pkgs.wuphf-packages";
      description = ''
        The package set every unit's `package` defaults to. The default is the
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
      description = "The user every unit runs as.";
    };
    group = mkOption {
      type = types.str;
      default = "wuphf";
      description = "The group every unit runs as.";
    };
    secretsDirectory = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "/etc/secrets";
      description = ''
        When set, the `environmentFile` of the core, every connector and every
        provider defaults to `<dir>/wuphf-<id>.env` (`wuphf-core.env`,
        `wuphf-telegram-main.env`, …).
      '';
    };
    coreAddress = mkOption {
      type = types.str;
      default = cfg.core.listen;
      defaultText = lib.literalExpression "config.services.wuphf.core.listen";
      description = "Where the connectors, providers and wuphf-misc reach the core.";
    };

    core = {
      enable = mkEnableOption "the wuphf core";
      listen = mkOption {
        type = types.str;
        default = "127.0.0.1:50051";
        example = "0.0.0.0:50051";
        description = ''
          The address the core listens on. For connectors on other machines,
          listen beyond loopback (`0.0.0.0:50051`) and open the port with
          `firewallInterfaces`. Then give the core an observer token (its env
          file's `WUPHF_CORE_OBSERVER_TOKEN`), or the observer is open to
          whatever reaches the port; `functions.invoke_token_env` likewise
          guards the function service.
        '';
      };
      firewallInterfaces = mkOption {
        type = types.listOf types.str;
        default = [ ];
        example = [ "tailscale0" ];
        description = ''
          The network interfaces on which the firewall lets in the port of
          `listen`, for connectors and providers on other machines (over
          Tailscale: `tailscale0`). They present their tokens, but the gRPC
          is not encrypted: use an interface that is (a VPN).
        '';
      };
    }
    // unitOptions {
      id = "core";
      binary = "wuphf-server";
      inherit (cfg.core) settings;
      example = "`config/example.toml`";
    };

    connectors = mkOption {
      type = types.attrsOf connectorType;
      default = { };
      example = lib.literalExpression ''
        {
          telegram-main = {
            type = "telegram";
            threads = true;
            topicCreate = true;
            ownerIds = [ "704338780" ];
            settings.session_file = "./telegram.session.json";
          };
          whatsapp-jio = {
            type = "whatsapp-rust";
            disappearingTimers = true;
          };
        }
      '';
      description = ''
        The connectors, by id: each runs as `wuphf-<id>` in
        `/var/lib/wuphf/<id>`, and the core gets its `[[connectors]]` entry.
      '';
    };

    providers = mkOption {
      type = types.attrsOf providerType;
      default = { };
      example = lib.literalExpression ''{ mediaconv.type = "mediaconv"; }'';
      description = ''
        The function providers, by id: each runs as `wuphf-<id>`, and the core
        gets its `[[functions.providers]]` entry.
      '';
    };

    misc = {
      enable = mkEnableOption "wuphf-misc (features that are not bridging)";
    }
    // unitOptions {
      id = "misc";
      binary = "wuphf-misc";
      inherit (cfg.misc) settings;
      example = "`misc/config.example.toml`";
      # It presents the core's observer token, from the core's env file.
      secrets = false;
    };
  };

  config = mkIf (units != { }) {
    assertions = [
      {
        assertion = badFlags == [ ];
        message = "services.wuphf.connectors: ${lib.concatStringsSep ", " badFlags}: that connector's kind has no such setting.";
      }
      {
        assertion = badFunctions == [ ];
        message = "services.wuphf.connectors: ${lib.concatStringsSep ", " badFunctions} cannot serve functions (only instagram does).";
      }
      {
        assertion = remotes == [ ] || cfg.core.enable;
        message = "services.wuphf: ${lib.concatStringsSep ", " remotes} is remote, which only means something on the core's machine (here services.wuphf.core is off). On the machine that runs it, leave remote off and set coreAddress.";
      }
      {
        assertion = cfg.core.firewallInterfaces == [ ] || !isLoopback cfg.core.listen;
        message = "services.wuphf.core.firewallInterfaces: the core listens on ${cfg.core.listen}, which other machines cannot reach; listen on 0.0.0.0:${toString (portOf cfg.core.listen)} (or this machine's address on that interface).";
      }
      {
        assertion = clashes == [ ];
        message = "services.wuphf: ${lib.concatStringsSep ", " clashes} names two units (ids are unique across connectors and providers, and core and misc are taken).";
      }
    ];

    warnings =
      lib.optional (!cfg.core.enable && isLoopback cfg.coreAddress)
        "services.wuphf: the core is off on this machine, but coreAddress is ${cfg.coreAddress}; set it to the core's machine (its Tailscale name, e.g. `core-host:50051`) unless a core runs here outside the module.";

    networking.firewall.interfaces = mkIf cfg.core.enable (
      lib.genAttrs cfg.core.firewallInterfaces (_: {
        allowedTCPPorts = [ (portOf cfg.core.listen) ];
      })
    );

    users.users = mkIf (cfg.user == "wuphf") {
      wuphf = {
        isSystemUser = true;
        group = cfg.group;
        home = "/var/lib/wuphf";
      };
    };
    users.groups = mkIf (cfg.group == "wuphf") { wuphf = { }; };

    services.wuphf.core.settings = {
      listen = mkDefault cfg.core.listen;
      database_url = mkDefault "sqlite://wuphf.sqlite";
      # The store is read-only: `wuphf-server update` cannot replace anything,
      # and its notice would tell the owner to run it. Update through the flake.
      updates.check_interval_seconds = mkDefault 0;
      connectors = lib.mapAttrsToList (
        id: connector:
        {
          inherit id;
          platform = connectorTypes.${connector.type}.platform;
          token_env = connector.tokenEnv;
          owner_ids = connector.ownerIds;
        }
        // lib.mapAttrs' (flag: spec: lib.nameValuePair spec.key connector.${flag}) flags
        // connector.coreSettings
      ) connectors;
      functions.providers =
        lib.mapAttrsToList (id: provider: {
          inherit id;
          token_env = provider.tokenEnv;
          inherit (provider) functions;
        }) providers
        ++ lib.mapAttrsToList (_: connector: {
          id = connector.settings.functions.provider_id;
          token_env = connector.settings.functions.token_env;
          inherit (connector) functions;
        }) (lib.filterAttrs (_: connector: connector.functions != [ ]) connectors);
    };

    services.wuphf.misc.settings = {
      core_address = mkDefault cfg.coreAddress;
      observer_token_env = mkDefault "WUPHF_CORE_OBSERVER_TOKEN";
    };

    # The binaries for setup, settings, export and the TUI.
    environment.systemPackages = lib.unique (lib.mapAttrsToList (_: spec: spec.package) units);

    systemd.services = lib.mapAttrs' (id: spec: lib.nameValuePair "wuphf-${id}" (unit id spec)) units;
  };
}
