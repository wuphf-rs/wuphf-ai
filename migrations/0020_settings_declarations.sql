-- Plugins declare their runtime settings too (plan 078): the declarations of
-- connectors (0018) and of plugins share one table, keyed by owner kind and
-- id. Each connector's row is kept as a `connector` declaration.
CREATE TABLE settings_declarations (
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('connector', 'plugin')),
    owner_id TEXT NOT NULL,
    descriptor BLOB NOT NULL,
    json_schema TEXT NOT NULL,
    base_json TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (owner_kind, owner_id)
);

INSERT INTO settings_declarations
    (owner_kind, owner_id, descriptor, json_schema, base_json, updated_at_ms)
SELECT 'connector', connector_id, descriptor, json_schema, base_json, updated_at_ms
FROM connector_settings_declarations;

DROP TABLE connector_settings_declarations;
