{
  description = "wuphf: a multi-platform messaging bridge (packages, a NixOS module, the dev shell)";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs, ... }:
    let
      supportedSystems = [
        "aarch64-linux"
        "x86_64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];
      linuxSystems = [
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems =
        function:
        nixpkgs.lib.genAttrs supportedSystems (
          system:
          function (
            import nixpkgs {
              inherit system;
            }
          )
        );
    in
    {
      packages = nixpkgs.lib.genAttrs linuxSystems (
        system:
        let
          # callPackage adds override functions; packages holds derivations only.
          packages = nixpkgs.lib.filterAttrs (_: nixpkgs.lib.isDerivation) (
            nixpkgs.legacyPackages.${system}.callPackage ./nix/packages.nix { }
          );
        in
        packages // { default = packages.wuphf; }
      );

      # services.wuphf.<part>: every part as a systemd service (nix/module.nix).
      nixosModules.default = import ./nix/module.nix (
        pkgs: self.packages.${pkgs.stdenv.hostPlatform.system}
      );

      # The module in a whole NixOS system: its units are built, so a broken
      # option or unit fails here.
      checks = nixpkgs.lib.genAttrs linuxSystems (
        system:
        let
          inherit (nixpkgs) lib;
          # A container system with services.wuphf set to `wuphf`.
          machine =
            wuphf:
            nixpkgs.lib.nixosSystem {
              inherit system;
              modules = [
                self.nixosModules.default
                {
                  boot.isContainer = true;
                  system.stateVersion = "25.05";
                  services.wuphf = {
                    secretsDirectory = "/run/secrets";
                  }
                  // wuphf;
                }
              ];
            };
          # Its units, by name, as a store tree; a missing one fails. Its
          # failed assertions fail too, as a rebuild would.
          unitFiles =
            name: system': units:
            let
              failed = lib.filter (assertion: !assertion.assertion) system'.config.assertions;
              all = system'.config.systemd.units;
            in
            assert lib.assertMsg (failed == [ ]) (lib.concatMapStringsSep "\n" (a: a.message) failed);
            nixpkgs.legacyPackages.${system}.linkFarm name (
              map (unit: {
                name = unit;
                path = all.${unit}.unit;
              }) units
            );
          instagram = {
            type = "instagram";
            threads = true;
            topicCreate = true;
            functions = [ "instagram.fetch_media" ];
          };
        in
        {
          nixos-module =
            unitFiles "wuphf-nixos-module"
              (machine {
                core.enable = true;
                connectors = {
                  telegram-main = {
                    type = "telegram";
                    threads = true;
                    topicCreate = true;
                    ownerIds = [ "704338780" ];
                  };
                  # Two copies of one kind.
                  whatsapp-jio = {
                    type = "whatsapp-rust";
                    disappearingTimers = true;
                  };
                  whatsapp-airtel = {
                    type = "whatsapp-rust";
                    disappearingTimers = true;
                  };
                  instagram-main = instagram;
                };
                providers.mediaconv.type = "mediaconv";
                misc.enable = true;
              })
              [
                "wuphf-core.service"
                "wuphf-telegram-main.service"
                "wuphf-whatsapp-jio.service"
                "wuphf-whatsapp-airtel.service"
                "wuphf-instagram-main.service"
                "wuphf-mediaconv.service"
                "wuphf-misc.service"
              ];

          # Two machines over a VPN: the core's runs everything but Instagram,
          # which the other runs against it.
          nixos-module-split =
            let
              coreHost = machine {
                core = {
                  enable = true;
                  listen = "0.0.0.0:50051";
                  firewallInterfaces = [ "tailscale0" ];
                };
                connectors = {
                  telegram-main = {
                    type = "telegram";
                    threads = true;
                    topicCreate = true;
                  };
                  instagram-main = instagram // {
                    remote = true;
                  };
                };
                misc.enable = true;
              };
              instagramHost = machine {
                coreAddress = "core-host:50051";
                connectors.instagram-main = instagram;
              };
              core = coreHost.config.services.wuphf.core.settings;
              coreUnits = coreHost.config.systemd.units;
              instagramUnits = instagramHost.config.systemd.units;
              coreService = coreHost.config.systemd.services.wuphf-core.serviceConfig;
            in
            assert lib.assertMsg (
              lib.elem "instagram-main" (map (c: c.id) core.connectors)
              && lib.elem "instagram-main" (map (p: p.id) core.functions.providers)
            ) "the core's host lost the remote connector's [[connectors]] or [[functions.providers]] entry";
            assert lib.assertMsg (lib.elem "/run/secrets/wuphf-instagram-main.env" coreService.EnvironmentFile)
              "the core does not read the remote connector's tokens";
            assert lib.assertMsg (
              !(coreUnits ? "wuphf-instagram-main.service")
            ) "the core's host runs the remote connector";
            assert lib.assertMsg (
              coreHost.config.networking.firewall.interfaces.tailscale0.allowedTCPPorts == [ 50051 ]
            ) "the core's port is not open on tailscale0";
            assert lib.assertMsg (
              !(instagramUnits ? "wuphf-core.service") && !(instagramUnits ? "wuphf-misc.service")
            ) "the Instagram host runs more than Instagram";
            assert lib.assertMsg (
              instagramHost.config.services.wuphf.connectors.instagram-main.settings.core_address
              == "core-host:50051"
            ) "the Instagram connector does not dial the core's host";
            nixpkgs.legacyPackages.${system}.symlinkJoin {
              name = "wuphf-nixos-module-split";
              paths = [
                (unitFiles "core-host" coreHost [
                  "wuphf-core.service"
                  "wuphf-telegram-main.service"
                  "wuphf-misc.service"
                ])
                (unitFiles "instagram-host" instagramHost [ "wuphf-instagram-main.service" ])
              ];
            };
        }
      );

      overlays.default = final: _: {
        wuphf-packages = final.callPackage ./nix/packages.nix { };
      };

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            clippy
            go
            gopls
            # Signs releases (release/README.md).
            minisign
            pkg-config
            protobuf
            protoc-gen-go
            protoc-gen-go-grpc
            rust-analyzer
            rustc
            rustfmt
            sqlite
            # ffmpeg and ImageMagick are used by the providers/mediaconv sticker
            # converter. ImageMagick decodes animated WebP (which ffmpeg cannot)
            # so WhatsApp animated stickers can be re-encoded by ffmpeg into
            # Telegram WebM video stickers.
            ffmpeg
            imagemagick
            # libwebp provides webpmux, used to embed WhatsApp sticker-pack
            # metadata (pack name, author, project link) into bridged stickers.
            libwebp
          ];

          env.RUST_BACKTRACE = "1";

          # The repo's pre-commit checks (scripts/git-hooks/pre-commit).
          shellHook = ''
            if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
              git config core.hooksPath scripts/git-hooks
            fi
          '';
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
