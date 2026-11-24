-- Health checks (plan 079): the open problems, the ones the owner ignores, and
-- the dispatches held while a direction of a rule is paused.

-- One row per open problem, keyed by its stable key. `payload` is the
-- model's Problem as JSON; `since_ms` is when it was first seen.
CREATE TABLE health_problems (
    key TEXT PRIMARY KEY NOT NULL,
    since_ms INTEGER NOT NULL,
    payload TEXT NOT NULL
);

-- Problems the owner chose to ignore until they pass once.
CREATE TABLE health_ignores (
    key TEXT PRIMARY KEY NOT NULL
);

-- Dispatches routed to a paused direction: not sent, kept in order until
-- the pause clears. `pause` is the model's HealthPause as JSON, `payload`
-- the Dispatch as JSON, and `held_at_ms` when it was held (a dispatch held
-- for too long is dropped on release).
CREATE TABLE held_dispatches (
    id TEXT PRIMARY KEY NOT NULL,
    pause TEXT NOT NULL,
    held_at_ms INTEGER NOT NULL,
    payload TEXT NOT NULL
);

CREATE INDEX held_dispatches_pause_idx ON held_dispatches (pause);
