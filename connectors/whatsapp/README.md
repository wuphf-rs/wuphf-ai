# WhatsApp Connector

This directory is reserved for the WhatsApp adapter. It must receive WhatsApp
events, normalize chat/group and participant information, execute dispatch
commands, and report resulting platform message identifiers.

Routing, replication membership, deduplication policy, and logical message
identity remain in the Rust core.
