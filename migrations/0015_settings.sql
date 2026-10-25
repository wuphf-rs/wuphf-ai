-- Runtime settings overrides (botconf): configuration keys changed at runtime,
-- layered over the config file and the environment. One row per dotted key;
-- `value` is the JSON-encoded value. Written only through the settings store,
-- which validates every change against the whole configuration first.
-- `updated_by` names who changed it (a platform user, or NULL for the CLI).

CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL,
    updated_by TEXT,
    updated_at_ms INTEGER NOT NULL
);
