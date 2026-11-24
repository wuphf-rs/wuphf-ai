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
      checks = nixpkgs.lib.genAttrs linuxSystems (system: {
        nixos-module =
          let
            machine = nixpkgs.lib.nixosSystem {
              inherit system;
              modules = [
                self.nixosModules.default
                {
                  boot.isContainer = true;
                  system.stateVersion = "25.05";
                  services.wuphf = {
                    secretsDirectory = "/run/secrets";
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
                      instagram-main = {
                        type = "instagram";
                        functions = [ "instagram.fetch_media" ];
                      };
                    };
                    providers.mediaconv.type = "mediaconv";
                    misc.enable = true;
                  };
                }
              ];
            };
            units = machine.config.systemd.units;
          in
          nixpkgs.legacyPackages.${system}.linkFarm "wuphf-nixos-module" (
            map
              (name: {
                inherit name;
                path = units.${name}.unit;
              })
              [
                "wuphf-core.service"
                "wuphf-telegram-main.service"
                "wuphf-whatsapp-jio.service"
                "wuphf-whatsapp-airtel.service"
                "wuphf-instagram-main.service"
                "wuphf-mediaconv.service"
                "wuphf-misc.service"
              ]
          );
      });

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
