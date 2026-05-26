# Telegram Connector

This directory is reserved for the Telegram adapter. It must receive Telegram
events, normalize them into core events including topic/thread identifiers,
execute dispatch commands, and report resulting platform message identifiers.

Routing, replication membership, deduplication policy, and logical message
identity remain in the Rust core.
