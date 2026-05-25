PRAGMA foreign_keys = ON;

CREATE TABLE configured_connectors (
    id TEXT PRIMARY KEY
);

CREATE TABLE configured_endpoints (
    id TEXT PRIMARY KEY,
    connector_id TEXT NOT NULL REFERENCES configured_connectors(id),
    address TEXT NOT NULL
);

CREATE TABLE configured_replication_groups (
    id TEXT PRIMARY KEY
);

CREATE TABLE configured_group_endpoints (
    group_id TEXT NOT NULL REFERENCES configured_replication_groups(id),
    endpoint_id TEXT NOT NULL REFERENCES configured_endpoints(id),
    PRIMARY KEY (group_id, endpoint_id)
);

CREATE TABLE logical_messages (
    id TEXT PRIMARY KEY,
    origin_endpoint_id TEXT NOT NULL REFERENCES configured_endpoints(id),
    parent_logical_message_id TEXT REFERENCES logical_messages(id),
    sender_id TEXT NOT NULL,
    sender_display_name TEXT NOT NULL,
    sent_at_unix_ms INTEGER NOT NULL,
    text TEXT NOT NULL,
    attachments_json TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE message_copies (
    id TEXT PRIMARY KEY,
    logical_message_id TEXT NOT NULL REFERENCES logical_messages(id),
    endpoint_id TEXT NOT NULL REFERENCES configured_endpoints(id),
    platform_message_id TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('origin', 'dispatch')),
    dispatch_id TEXT UNIQUE,
    acknowledged_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (endpoint_id, platform_message_id),
    UNIQUE (logical_message_id, endpoint_id)
);

CREATE TABLE ingested_events (
    connector_id TEXT NOT NULL REFERENCES configured_connectors(id),
    event_id TEXT NOT NULL,
    logical_message_id TEXT NOT NULL REFERENCES logical_messages(id),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (connector_id, event_id)
);

CREATE TABLE dispatch_commands (
    id TEXT PRIMARY KEY,
    logical_message_id TEXT NOT NULL REFERENCES logical_messages(id),
    target_endpoint_id TEXT NOT NULL REFERENCES configured_endpoints(id),
    target_connector_id TEXT NOT NULL REFERENCES configured_connectors(id),
    parent_logical_message_id TEXT REFERENCES logical_messages(id),
    endpoint_sequence INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'failed', 'completed')),
    last_error TEXT,
    acknowledged_platform_message_id TEXT,
    acknowledged_at TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (logical_message_id, target_endpoint_id),
    UNIQUE (target_endpoint_id, endpoint_sequence)
);

CREATE INDEX dispatch_delivery_order
    ON dispatch_commands(target_connector_id, target_endpoint_id, endpoint_sequence);

