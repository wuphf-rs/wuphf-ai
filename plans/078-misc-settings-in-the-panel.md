# Plan 078: wuphf-misc's settings in the panel (plugins declare their own settings)

> **Executor instructions**: as plan 058.

## Status

- **Priority**: P2
- **Effort**: M
- **Depends on**: 040 (connectors declare their own settings), 050 (wuphf-misc)
- **Category**: core / settings / misc

## Why

wuphf-misc reads only its own file, and a change needs a restart. On NixOS
the file is generated, so turning the Instagram downloader on for one more
chat means a rebuild. The owner wants to change it from `/wuphf` like the
connectors' settings.

## Design decisions (2026-11-24)

1. **A plugin is a client of the core that is neither a connector nor a
   provider** (wuphf-misc now; a custom one later). It declares its runtime
   settings the way a connector does (plan 040): a botconf descriptor, a
   JSON Schema of its section and its file values as the base. The core keeps
   them at `plugin_settings.<plugin id>`, and the plugin follows its section
   through the settings service's `Watch`.
2. **The declaration is one unary RPC**, `PluginTransport.DeclareSettings`,
   with the observer token (which wuphf-misc already presents, and which the
   settings service already treats as the owner). With no observer token set
   it is refused, as settings access is. Scoped plugin tokens and a
   `[[plugins]]` list in the core stay a later plan; this one adds no core
   config.
3. **The core does not know a plugin's features.** Connectors' keys are
   checked against `LIVE_SAFE`, which the core can know; a plugin's are not.
   The plugin declares only its `botconf`-tagged fields, and the core refuses
   only the client's connection keys (`core_address`, `plugin_id`, any
   `*_token_env`).
4. **Declarations share one table** with an owner kind
   (`settings_declarations(owner_kind, owner_id, …)`, migration 0020, the rows
   of 0018's table copied as `connector`). Every stored plugin declaration is
   restored at startup (there is no plugin list to check it against).
5. **wuphf-misc's runtime settings** are `log_level` and the whole
   `[instagram]` table: `enabled`, `chats` (picked by name in the panel,
   domain `wuphf.chat`), `caption`, `reply_on_failure`, `dedup_hours`,
   `max_per_minute`, `max_concurrent`, `call_timeout_seconds`. Each applies to
   the next message. `core_address`, the token variables,
   `reconnect_delay_ms` and `plugin_id` stay file only.
6. **A feature is built even when disabled**, and checks `enabled` per
   message, so it can be turned on from the panel. `enabled` with no chats
   is a warning, not a startup error (it crash-looped a NixOS unit). With no
   feature enabled wuphf-misc keeps running, waiting for the panel.
7. If the core refuses the declaration (an older core), wuphf-misc logs it
   and runs on its file values.

## Steps

1. `proto`: `plugin.proto` (`PluginTransport.DeclareSettings`), Rust build
   and Go generation.
2. Storage: `SettingsOwner { Connector, Plugin }` on `SettingsDeclaration`,
   migration 0020 and its `migration_tests` case, both backends.
3. Runtime: the declaration-to-section code shared by connectors and plugins;
   `plugin_settings` (restore, declare, the gRPC service); the core refuses a
   `[plugin_settings]` table in its file; `serve` and `wuphf-server settings`
   restore plugin sections. Tests: a declaration makes a section, a
   connection key is refused, it survives a restart, the RPC needs the
   observer token.
4. Go: the shared settings package takes a namespace (connectors' or
   plugins'); wuphf-misc's `config.Runtime`, declare at startup, follow, and
   the Instagram feature reading its live settings. Tests.
5. Nix module: misc's `plugin_id` is `misc`. Docs: misc README, Configuration,
   Components.
