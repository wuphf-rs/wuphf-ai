-- Platform address equivalences: two addresses on one connector that name the
-- same chat (e.g. a WhatsApp phone JID and the contact's LID). Unlike `aliases`
-- these are recorded even when neither address has an endpoint yet, so a rule's
-- chat list can match a chat under any of its forms from the first message.
-- Links are symmetric: both directions are stored so a lookup from either side
-- finds the other.

CREATE TABLE address_links (
    connector_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    thread_key TEXT NOT NULL,
    linked_chat_id TEXT NOT NULL,
    linked_thread_key TEXT NOT NULL,
    PRIMARY KEY (connector_id, chat_id, thread_key, linked_chat_id, linked_thread_key)
);

CREATE INDEX address_links_lookup
    ON address_links (connector_id, chat_id, thread_key);
