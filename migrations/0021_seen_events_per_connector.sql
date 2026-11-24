-- An inbound event was claimed once per platform chat: (platform, chat_id,
-- native id). Two accounts in one group (two WhatsApp connectors) report every
-- message under the same native id, so whichever connector reported first took
-- the claim and the other account's conversation never received the message.
-- Claims are now per connector.
--
-- Each existing claim is attributed to the connector whose received copy it
-- produced: the core stores that copy right after claiming, at the origin
-- endpoint, under the same chat and native id. A claim with no received copy (a
-- crash between the two writes) is dropped; the event is simply ingested again
-- if it is ever replayed.

CREATE TABLE seen_events_new (
    connector_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    platform_message_id TEXT NOT NULL,
    PRIMARY KEY (connector_id, chat_id, platform_message_id)
);

INSERT OR IGNORE INTO seen_events_new (connector_id, chat_id, platform_message_id)
SELECT
    json_extract(e.payload, '$.connector_id'),
    s.chat_id,
    s.platform_message_id
FROM message_copies c
JOIN endpoints e ON e.id = c.endpoint_id
JOIN seen_events s
    ON s.platform = json_quote(json_extract(e.payload, '$.platform'))
    AND s.chat_id = json_extract(e.payload, '$.chat_id')
    AND s.platform_message_id = c.platform_message_id
WHERE c.sent = 0;

DROP TABLE seen_events;
ALTER TABLE seen_events_new RENAME TO seen_events;
