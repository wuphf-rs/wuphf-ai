-- Allow the dead-letter terminal state. SQLite cannot alter a CHECK
-- constraint in place, so the table is rebuilt with the widened constraint.
CREATE TABLE dispatches_new (
    id TEXT PRIMARY KEY NOT NULL,
    target_endpoint TEXT NOT NULL,
    message_id TEXT NOT NULL,
    waiting_for TEXT,
    state TEXT NOT NULL CHECK (state IN ('pending', 'blocked', 'acknowledged', 'failed')),
    payload TEXT NOT NULL
);

INSERT INTO dispatches_new (id, target_endpoint, message_id, waiting_for, state, payload)
SELECT id, target_endpoint, message_id, waiting_for, state, payload FROM dispatches;

DROP TABLE dispatches;
ALTER TABLE dispatches_new RENAME TO dispatches;

CREATE INDEX dispatches_pending_idx ON dispatches (state, target_endpoint);
CREATE INDEX dispatches_waiting_idx ON dispatches (state, waiting_for, target_endpoint);
