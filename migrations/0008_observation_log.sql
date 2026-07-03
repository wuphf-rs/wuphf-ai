-- Observation log: a read model capturing every message a connector reports,
-- independent of routing, for read-only consumers such as the TUI. Rows hold a
-- JSON-encoded model value; native coordinates are denormalized into columns so
-- a chat's timeline can be paged.

CREATE TABLE observed_messages (
    connector_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    platform_message_id TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (connector_id, chat_id, platform_message_id)
);

CREATE INDEX observed_messages_chat_idx
    ON observed_messages (connector_id, chat_id, timestamp_ms);

CREATE TABLE observed_chats (
    connector_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    last_message_ts INTEGER NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (connector_id, chat_id)
);

CREATE INDEX observed_chats_recent_idx
    ON observed_chats (last_message_ts);
