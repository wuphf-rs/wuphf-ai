-- What the core knows about each platform chat apart from routing (plan 045):
-- a JSON ChatProfile keyed by the connector and the chat's native id. New
-- profile fields go in the JSON and need no schema change.
CREATE TABLE chat_profiles (
    connector_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY (connector_id, chat_id)
);
