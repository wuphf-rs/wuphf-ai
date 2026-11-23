# Releases

A tag `vX.Y.Z` publishes a release on GitHub
(`.github/workflows/release.yml`): every binary for linux `x86_64` and
`aarch64`, named `<binary>-linux-<arch>`, plus `SHA256SUMS` and its minisign
signature `SHA256SUMS.minisig`, whose trusted comment is `wuphf vX.Y.Z`.
`wuphf-server update` installs a download only when its SHA-256 is in a
`SHA256SUMS` that `minisign.pub` (built into `wuphf-server`) signed for that
very release.

## Making a release

```sh
nix develop -c scripts/release.sh 0.2.0    # sets the versions, commits, tags v0.2.0
git push origin HEAD v0.2.0                # the workflow builds and publishes
```

## The release key (once)

The workflow signs with the secret key; this directory holds the public one.
Until `minisign.pub` holds a key, no release can be installed by
`wuphf-server update` (it refuses what it cannot check).

```sh
# In the dev shell (minisign is in it). -W: no password; the secret lives in
# GitHub's secret store, which is what protects it.
minisign -G -W -p release/minisign.pub -s ~/.minisign/wuphf-release.key
```

Then:

1. Commit `release/minisign.pub`.
2. Add the secret key's contents as the repository secret
   `MINISIGN_SECRET_KEY` (Settings → Secrets and variables → Actions).
3. Keep a copy of the secret key offline: a lost key means a new one, and
   binaries built before the change keep trusting the old key only, so they
   need one manual install (or `[updates.wuphf] public_key`) to move on.
