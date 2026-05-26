CREATE TABLE endpoints (
    id TEXT PRIMARY KEY NOT NULL,
    payload TEXT NOT NULL
);

CREATE TABLE replication_groups (
    id TEXT PRIMARY KEY NOT NULL,
    payload TEXT NOT NULL
);

CREATE TABLE messages (
    id TEXT PRIMARY KEY NOT NULL,
    payload TEXT NOT NULL
);

CREATE TABLE message_copies (
    message_id TEXT NOT NULL,
    endpoint_id TEXT NOT NULL,
    platform_message_id TEXT NOT NULL,
    sent INTEGER NOT NULL DEFAULT 0,
    payload TEXT NOT NULL,
    PRIMARY KEY (message_id, endpoint_id),
    UNIQUE (endpoint_id, platform_message_id)
);

CREATE TABLE seen_events (
    platform TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    platform_message_id TEXT NOT NULL,
    PRIMARY KEY (platform, chat_id, platform_message_id)
);

CREATE TABLE contacts (
    id TEXT PRIMARY KEY NOT NULL,
    payload TEXT NOT NULL
);

CREATE TABLE chat_settings (
    endpoint_id TEXT PRIMARY KEY NOT NULL,
    payload TEXT NOT NULL
);

CREATE TABLE dispatches (
    id TEXT PRIMARY KEY NOT NULL,
    target_endpoint TEXT NOT NULL,
    message_id TEXT NOT NULL,
    waiting_for TEXT,
    state TEXT NOT NULL CHECK (state IN ('pending', 'blocked', 'acknowledged')),
    payload TEXT NOT NULL
);

CREATE INDEX dispatches_pending_idx ON dispatches (state, target_endpoint);
CREATE INDEX dispatches_waiting_idx ON dispatches (state, waiting_for, target_endpoint);
