CREATE TABLE aliases (
    connector_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    thread_key TEXT NOT NULL,
    endpoint_id TEXT NOT NULL,
    is_canonical INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (connector_id, chat_id, thread_key)
);

CREATE INDEX aliases_endpoint_idx ON aliases (endpoint_id);

CREATE TABLE conversations (
    id TEXT PRIMARY KEY NOT NULL,
    rule_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    source_connector TEXT NOT NULL,
    source_chat_id TEXT NOT NULL,
    source_thread_key TEXT NOT NULL,
    payload TEXT NOT NULL,
    UNIQUE (rule_id, source_connector, source_chat_id, source_thread_key)
);

CREATE TABLE thread_mappings (
    conversation_id TEXT NOT NULL,
    connector_id TEXT NOT NULL,
    container TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    PRIMARY KEY (conversation_id, connector_id),
    UNIQUE (connector_id, container, thread_id)
);
