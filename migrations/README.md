# Database Migrations

This directory contains ordered SQLite migrations owned by the Rust core
server. Persistent model changes must be represented by migrations so
durable dispatch and message copy state remains reproducible.
