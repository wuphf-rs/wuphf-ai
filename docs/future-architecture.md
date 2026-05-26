# Future Architecture

## Purpose

The first MVP now separates durable state, routing indexes, delivery policy,
connector sessions, transport handling, and an inbound processing pipeline.
This document records likely next refinements without moving deferred platform
features into the current implementation prematurely.

## Pipeline Evolution

The core should grow around typed pipelines for normalized protocol operations,
not around platform SDK objects. Connectors remain responsible for translating
WhatsApp, Telegram, or other platform events into the canonical protocol.

The current create-message path is:

```text
connector stream
-> normalized CreateMessage
-> inbound pipeline authorization
-> durable ingestion/deduplication and message-copy resolution
-> indexed replication routing
-> durable dispatch claiming
-> connector stream
```

Future core stages may be added when behavior requires them:

- Policy filtering for configured replication rules.
- Mention/reference rewriting after an identity model is defined.
- Structured diagnostic or metrics hooks.
- Message lifecycle stages for edits, reactions, deletes, or revokes after the
  protocol supports them.
- Media descriptor policy after a media-transfer design is approved.

## Middleware Guidance

A mandatory middleware or staged pipeline can be useful as message lifecycle
behavior grows. It should not be introduced as an open-ended plugin system in
the MVP.

Suitable middleware characteristics:

- Operates on normalized core event types.
- Has explicit ordering and typed outcomes.
- Is configured and owned by the core.
- Does not bypass persistence or dispatch-ordering rules.

Unsuitable middleware responsibilities:

- Platform-specific normalization; that belongs in connectors.
- Deduplication as an in-memory or detachable filter.
- Message-copy mapping outside the durable repository transaction.
- Retry, acknowledgement, or dispatch ordering decisions outside core-owned
  delivery state.

Deduplication, origin message-copy creation, and dispatch persistence must
remain one durable operation. Any later pipeline API must preserve this
transaction boundary even if it exposes before-ingestion validation or
after-ingestion enrichment hooks.

## Potential Internal Shape

When multiple normalized operation types exist, consider replacing the
create-specific pipeline with typed operation handlers:

```text
InboundOperationPipeline<CreateMessage>
InboundOperationPipeline<EditMessage>
InboundOperationPipeline<Reaction>
InboundOperationPipeline<DeleteMessage>
```

Shared stages should be introduced only after two or more operations require
the same behavior. This avoids building generic abstractions around behavior
that does not exist yet.

## Platform Milestone

Before real WhatsApp and Telegram connectors are added, specify:

- Endpoint identity for WhatsApp chats and Telegram chats/topics.
- WhatsApp conversation to Telegram topic mapping.
- Reply mapping and origin identification in both directions.
- Credential storage and connector deployment.
- Failure classification for platform API responses.

Telegram topic behavior and production connector logic remain outside the
mock-backed MVP until this design is agreed.

## Operations And Administration

Terminal dispatch failures deliberately preserve endpoint ordering by blocking
later commands. The MVP local operator path now supports:

- Listing pending, retrying, and terminal dispatch commands.
- Inspecting attempts, retry eligibility, and failure reasons.
- Explicitly retrying a terminal failure.

A future authenticated live operational surface should add:

- Inspecting message-copy mappings alongside dispatch state.
- Explicitly resolving a terminal failure without retrying it.
- Reporting connector session health and dispatch backlog metrics.
- Waking connected connectors immediately after operator recovery.

This surface should call core-owned operations; it must not mutate SQLite
state directly.
