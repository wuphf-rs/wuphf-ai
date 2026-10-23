{
  description = "wuphf development environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { nixpkgs, ... }:
    let
      supportedSystems = [
        "aarch64-linux"
        "x86_64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
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
      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            clippy
            go
            gopls
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
        };
      });

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
