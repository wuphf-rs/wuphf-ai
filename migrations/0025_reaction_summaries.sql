-- Reaction counts (plan 085): the anonymous counts last read for a post (a
-- WhatsApp channel post), one row per logical message, with the endpoint of
-- the copy they were read on. The core compares a new reading with this one
-- and edits the footer line of the post's other copies only when they differ.
-- `counts` is a JSON list of {"emoji", "count"}.
CREATE TABLE reaction_summaries (
    message_id TEXT PRIMARY KEY NOT NULL,
    endpoint_id TEXT NOT NULL,
    counts TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
