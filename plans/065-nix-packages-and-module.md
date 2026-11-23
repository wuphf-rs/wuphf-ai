# Plan 065: Nix packages, a NixOS module, and a Cachix cache built by CI

> **Executor instructions**: as plan 058. Nix builds one at a time,
> `--max-jobs 1 --cores 2`.

## Status

- **Priority**: P2 — the owner asked for it (2026-11-24)
- **Effort**: M
- **Depends on**: 064 (versions; `scripts/build-release.sh` names)
- **Category**: packaging / ops

## Why this matters

The flake was a dev shell only. The owner runs NixOS and wants wuphf as a
module, with CI building a binary cache so machines download it instead of
compiling (2026-11-24).

## Design decisions (settled with the owner, 2026-11-24)

1. **Packages** (`nix/packages.nix`): `wuphf-server` (with `wuphf-tui`),
   `wuphf-whatsapp-rust`, the five Go binaries and `wuphf` (all of them),
   version-stamped from `Cargo.toml`, sources filtered to what each build
   reads. Rust git dependencies come from the lock files
   (`allowBuiltinFetchGit`); each Go module has its `vendorHash`.
   `wuphf-mediaconv` is wrapped with ffmpeg and ImageMagick.
2. **Module** (`nix/module.nix`, `nixosModules.default`):
   `services.wuphf.<part>` (core, telegram, whatsapp, whatsapp-rust,
   instagram, mediaconv, misc) with `enable`, `package`, `settings`
   (TOML) or `configFile`, `environmentFile`; units as `deploy/systemd`'s.
   The core's release check is off by default (the store is read-only).
3. **Cache: Cachix** (the owner's choice). `.github/workflows/nix.yml` builds
   `.#wuphf` and the module check on x86_64-linux and pushes to the cache
   named by the repository variable `CACHIX_CACHE` with the secret
   `CACHIX_AUTH_TOKEN`; without them it builds and checks only. aarch64 is
   not built (hosted arm runners are public-repository only).

## Verification

- Every package builds with Nix and reports the workspace version.
- `checks.<system>.nixos-module`: the module in a NixOS system, its units
  built.
- The workflow runs green on `v2`.

## Owner steps

- Create the Cachix cache, set `CACHIX_CACHE` (variable) and
  `CACHIX_AUTH_TOKEN` (secret) on the repository.
- Add the cache to `nix.settings.substituters` on the machines.
