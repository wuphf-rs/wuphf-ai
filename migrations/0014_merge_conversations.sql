-- A conversation and its replica set were always one-to-one, and a thread
-- mapping repeated the thread id each mirror endpoint already stores. Merge
-- them: a conversation now lists its own endpoints.
--
-- Each merged conversation takes its replica set's id. Messages, dispatches and
-- the provisioning stash already store replica-set ids, so they keep pointing at
-- the right record and only their JSON key or column is renamed. The old
-- conversation ids were referenced only by thread_mappings, which is dropped.
--
-- Replica sets created for owner messages to chats outside every rule had no
-- conversation; they become conversations with no rule, named after their one
-- endpoint.
--
-- Payload keys are written in the order serde writes `Conversation`, so a
-- migrated row is byte-identical to one the current model would write.

CREATE TABLE conversations_new (
    id TEXT PRIMARY KEY NOT NULL,
    rule_id TEXT,
    source_connector TEXT NOT NULL,
    source_chat_id TEXT NOT NULL,
    source_thread_key TEXT NOT NULL,
    payload TEXT NOT NULL
);

INSERT INTO conversations_new
    (id, rule_id, source_connector, source_chat_id, source_thread_key, payload)
SELECT
    rs.id,
    c.rule_id,
    c.source_connector,
    c.source_chat_id,
    c.source_thread_key,
    json_object(
        'id', rs.id,
        'rule_id', c.rule_id,
        'source', json_object(
            'connector_id', c.source_connector,
            'chat_id', c.source_chat_id,
            'thread_id', NULLIF(c.source_thread_key, '')
        ),
        'endpoints', rs.payload -> '$.endpoints',
        'title', c.payload ->> '$.title'
    )
FROM conversations AS c
JOIN replica_sets AS rs ON rs.id = c.replica_set_id;

INSERT INTO conversations_new
    (id, rule_id, source_connector, source_chat_id, source_thread_key, payload)
SELECT
    rs.id,
    NULL,
    e.payload ->> '$.connector_id',
    e.payload ->> '$.chat_id',
    coalesce(e.payload ->> '$.thread_id', ''),
    json_object(
        'id', rs.id,
        'rule_id', NULL,
        'source', json_object(
            'connector_id', e.payload ->> '$.connector_id',
            'chat_id', e.payload ->> '$.chat_id',
            'thread_id', e.payload ->> '$.thread_id'
        ),
        'endpoints', rs.payload -> '$.endpoints',
        'title', NULL
    )
FROM replica_sets AS rs
JOIN endpoints AS e ON e.id = rs.payload ->> '$.endpoints[0]'
WHERE NOT EXISTS (SELECT 1 FROM conversations AS c WHERE c.replica_set_id = rs.id);

DROP TABLE conversations;
ALTER TABLE conversations_new RENAME TO conversations;
CREATE UNIQUE INDEX conversations_source_idx
    ON conversations (rule_id, source_connector, source_chat_id, source_thread_key);

-- Which conversation an endpoint belongs to, indexed. An endpoint belongs to
-- at most one conversation.
CREATE TABLE conversation_endpoints (
    endpoint_id TEXT PRIMARY KEY NOT NULL,
    conversation_id TEXT NOT NULL
);
INSERT OR IGNORE INTO conversation_endpoints (endpoint_id, conversation_id)
SELECT member.value, c.id
FROM conversations AS c, json_each(c.payload, '$.endpoints') AS member;
CREATE INDEX conversation_endpoints_conversation_idx
    ON conversation_endpoints (conversation_id);

DROP TABLE replica_sets;
DROP TABLE thread_mappings;

UPDATE messages SET payload = replace(payload, '"replica_set_id":', '"conversation_id":');
UPDATE dispatches SET payload = replace(payload, '"replica_set_id":', '"conversation_id":');

ALTER TABLE provisioning_stash RENAME COLUMN replica_set_id TO conversation_id;
