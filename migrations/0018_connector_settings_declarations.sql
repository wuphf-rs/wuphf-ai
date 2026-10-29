-- What each connector last declared about its runtime settings when it
-- registered (plan 040): the botconf descriptor of its settings, the JSON
-- Schema of its section and its own file values as the base. Kept so the
-- connector's section exists at startup, before the connector reconnects,
-- and so `wuphf settings` can edit it while connectors are down.
CREATE TABLE connector_settings_declarations (
    connector_id TEXT PRIMARY KEY NOT NULL,
    descriptor BLOB NOT NULL,
    json_schema TEXT NOT NULL,
    base_json TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
