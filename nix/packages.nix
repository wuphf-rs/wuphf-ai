# Every wuphf binary as a Nix package, each stamped with the workspace version
# (`<binary> --version`), named as releases and scripts/build-release.sh name
# them. `wuphf` holds them all.
{
  lib,
  rustPlatform,
  buildGoModule,
  symlinkJoin,
  makeWrapper,
  protobuf,
  ffmpeg,
  imagemagick,
}:
let
  version = (lib.importTOML ../Cargo.toml).workspace.package.version;

  # Only what the builds read, so a change to docs or scripts rebuilds nothing.
  source =
    paths:
    lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions (map (path: ../. + "/${path}") paths);
    };
  rustSource = source [
    "Cargo.toml"
    "Cargo.lock"
    "crates"
    "migrations"
    "release/minisign.pub"
  ];
  goSource = source [
    "connectors/shared"
    "connectors/telegram"
    "connectors/whatsapp"
    "connectors/instagram"
    "providers/mediaconv"
    "misc"
    "proto/go"
  ];

  meta = {
    homepage = "https://github.com/wuphf-rs/wuphf-ai";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
  };

  # The core: wuphf-server (the wuphf-cli crate) and wuphf-tui.
  wuphf-server = rustPlatform.buildRustPackage {
    pname = "wuphf-server";
    inherit version;
    src = rustSource;
    cargoLock = {
      lockFile = ../Cargo.lock;
      # botconf comes from git, pinned by the lock file.
      allowBuiltinFetchGit = true;
    };
    cargoBuildFlags = [
      "-p"
      "wuphf-cli"
      "-p"
      "wuphf-tui"
    ];
    nativeBuildInputs = [ protobuf ];
    # The workspace's tests run in CI (cargo test), not in every build.
    doCheck = false;
    postInstall = ''
      mv $out/bin/wuphf-cli $out/bin/wuphf-server
    '';
    meta = meta // {
      description = "wuphf core: routing, identity, storage and the TUI";
      mainProgram = "wuphf-server";
    };
  };

  # Its own Cargo workspace (its SQLite cannot share the core's graph); it
  # builds the core's crates by path.
  wuphf-whatsapp-rust = rustPlatform.buildRustPackage {
    pname = "wuphf-whatsapp-rust";
    inherit version;
    src = source [
      "Cargo.toml"
      "crates"
      "migrations"
      "release/minisign.pub"
      "connectors/whatsapp-rust"
    ];
    cargoRoot = "connectors/whatsapp-rust";
    buildAndTestSubdir = "connectors/whatsapp-rust";
    cargoLock = {
      lockFile = ../connectors/whatsapp-rust/Cargo.lock;
      allowBuiltinFetchGit = true;
    };
    nativeBuildInputs = [ protobuf ];
    # whatsapp-rust's default "simd" feature is nightly-only (portable_simd).
    env.RUSTC_BOOTSTRAP = 1;
    doCheck = false;
    postInstall = ''
      mv $out/bin/wuphf-connector-whatsapp $out/bin/wuphf-whatsapp-rust
    '';
    meta = meta // {
      description = "wuphf WhatsApp connector (whatsapp-rust)";
      mainProgram = "wuphf-whatsapp-rust";
    };
  };

  go =
    {
      binary,
      module,
      vendorHash,
      cgo ? false,
      description,
      extra ? { },
    }:
    buildGoModule (
      {
        pname = binary;
        inherit version vendorHash;
        src = goSource;
        modRoot = module;
        subPackages = [ "cmd/${binary}" ];
        env.CGO_ENABLED = if cgo then 1 else 0;
        ldflags = [
          "-s"
          "-w"
          "-X github.com/wuphf-rs/wuphf-ai/connectors/shared/version.Version=${version}"
        ];
        doCheck = false;
        meta = meta // {
          inherit description;
          mainProgram = binary;
        };
      }
      // extra
    );

  wuphf-telegram = go {
    binary = "wuphf-telegram";
    module = "connectors/telegram";
    vendorHash = "sha256-Ffvn3Sh9Lue2kQcAQaBr3UDc7UMDgJLr62Li5IRodTs=";
    description = "wuphf Telegram connector";
  };
  wuphf-whatsapp = go {
    binary = "wuphf-whatsapp";
    module = "connectors/whatsapp";
    vendorHash = "sha256-ynXAp5PoHP4ZzC4kekxJn/qwuCr2oZ2IYL4q8phQSGE=";
    cgo = true;
    description = "wuphf WhatsApp connector (whatsmeow)";
  };
  wuphf-instagram = go {
    binary = "wuphf-instagram";
    module = "connectors/instagram";
    vendorHash = "sha256-5sd7J2FuKwM5wGioFlKbBEIigdoVypLuvKrnidifBRM=";
    description = "wuphf Instagram connector";
  };
  wuphf-mediaconv = go {
    binary = "wuphf-mediaconv";
    module = "providers/mediaconv";
    vendorHash = "sha256-Oymf5bosmwcx8feUrn1p1GqfqGQtPR+H0GCWwwjK0pM=";
    cgo = true;
    description = "wuphf sticker converter (media.convert@1)";
    # It runs ffmpeg and ImageMagick.
    extra = {
      nativeBuildInputs = [ makeWrapper ];
      postInstall = ''
        wrapProgram $out/bin/wuphf-mediaconv \
          --prefix PATH : ${
            lib.makeBinPath [
              ffmpeg
              imagemagick
            ]
          }
      '';
    };
  };
  wuphf-misc = go {
    binary = "wuphf-misc";
    module = "misc";
    vendorHash = "sha256-0+yoR4LjeBUNK8To95xWam4HGrvFJnsykGA6ejJV+BQ=";
    description = "wuphf-misc: features that are not bridging";
  };
in
{
  inherit
    wuphf-server
    wuphf-whatsapp-rust
    wuphf-telegram
    wuphf-whatsapp
    wuphf-instagram
    wuphf-mediaconv
    wuphf-misc
    ;
  wuphf = symlinkJoin {
    name = "wuphf-${version}";
    paths = [
      wuphf-server
      wuphf-whatsapp-rust
      wuphf-telegram
      wuphf-whatsapp
      wuphf-instagram
      wuphf-mediaconv
      wuphf-misc
    ];
    meta = meta // {
      description = "Every wuphf binary";
      mainProgram = "wuphf-server";
    };
  };
}
