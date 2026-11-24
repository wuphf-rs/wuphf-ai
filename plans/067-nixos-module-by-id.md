# Plan 067: The NixOS module runs connectors and providers by id

> **Executor instructions**: as plan 058. Check the module with `nix eval`.
> Never build on battery.

## Status

- **Priority**: P2. The owner asked for it on 2026-11-24: two WhatsApp
  accounts, jio and airtel, on oracleamperehyd, and "a better way to manage
  multiple instances of all connectors".
- **Effort**: M
- **Depends on**: 065
- **Category**: packaging / ops

## Why this matters

`services.wuphf.<part>` ran each part once. The same facts were also written
twice, in the core's `[[connectors]]` entry and in the connector's own config,
and a mismatch made the core refuse the connector:
- the id (`connector_id`);
- the token variable;
- `core_address`;
- the flags both sides share.

## Design decisions (settled with the owner, 2026-11-24)

1. **Connectors by id.** `services.wuphf.connectors.<id>` takes:
   - `type`: `telegram`, `whatsapp`, `whatsapp-rust` or `instagram`;
   - the both-sides flags `threads`, `topicCreate` and `disappearingTimers`.
     Each is accepted only by the kinds whose config has it; an assertion
     enforces this;
   - `ownerIds`;
   - `functions`, which serve functions to the core under its own id
     (instagram only);
   - `tokenEnv` (default `WUPHF_CONNECTOR_<ID>_TOKEN`) and `coreSettings`;
   - `settings`, `configFile`, `environmentFile` and `package`.

   The module derives the core's entry and the connector's `connector_id`,
   `core_address`, `core_token_env` and flags.
2. **Providers by id.** `services.wuphf.providers.<id>` takes `type`
   (`mediaconv`), `functions` and `tokenEnv`
   (`WUPHF_PROVIDER_<ID>_TOKEN`), and feeds the core's
   `[[functions.providers]]`.
3. **One core** (`core.listen`, `coreAddress`) and **one wuphf-misc**. misc
   presents the core's observer token, read from the core's env file.
4. **Each token lives in one file**, its unit's `environmentFile`. The core's
   unit loads every connector's and provider's file. `secretsDirectory`
   defaults each file to `<dir>/wuphf-<id>.env`.
5. **Units are `wuphf-<id>`**, with state in `/var/lib/wuphf/<id>`. Ids are
   unique across connectors and providers, and `core` and `misc` are taken;
   an assertion enforces this.
6. **The v0.2.0 options are removed.** `services.wuphf.telegram`, `whatsapp`,
   `whatsapp-rust`, `instagram` and `mediaconv` fail with a message naming
   the replacement (`mkRemovedOptionModule`). The owner is the only user.

## Verification

- `checks.<system>.nixos-module` uses the new shape: two whatsapp-rust
  connectors, telegram with flags and owners, instagram serving functions,
  mediaconv and misc.
- With `nix eval`:
  - the derived core and connector settings, each unit's env files, state
    directory and stop signal;
  - every assertion, and the message for a removed option.
- The derived core config, written as TOML, loads in
  `wuphf-server settings list`.
