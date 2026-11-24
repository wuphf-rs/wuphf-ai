-- Reactions (plan 080): the last reaction each reactor has on each copy of a
-- message. A platform that reports a message's reactions as a whole (a
-- Telegram user session) is diffed against these rows, and a reaction equal
-- to the stored one is ignored, so an echo or a replay does nothing.
--
-- `reactor` is the platform's id for the reactor, or '' for the connector's
-- own session account: there the row is the reaction the bridge applied as
-- the owner, or the owner's own reported one. A cleared reaction has no row.
CREATE TABLE reactions (
    message_id TEXT NOT NULL,
    endpoint_id TEXT NOT NULL,
    reactor TEXT NOT NULL,
    emoji TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (message_id, endpoint_id, reactor)
);
