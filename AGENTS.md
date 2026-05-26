# wuphf Contributor Instructions

- Keep the Rust core authoritative for logical identity, routing, deduplication,
  sent-message loop prevention, and reply mapping.
- Keep connectors thin: normalize incoming platform objects and execute
  dispatch commands; do not implement bridge decisions in connectors.
- Preserve the invariant that one logical message can have many
  platform-specific message copies.
- Keep crates small and aligned to their named responsibility.
- Add tests when changing identity, routing, storage, or runtime behavior.
- Keep persistence behind the `Storage` trait. SQLite schema changes require
  migrations; do not leak database concerns into routing or connectors.
