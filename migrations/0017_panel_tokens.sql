-- Settings-panel button handles: actions too long for a platform's button
-- data, stored under a short key derived from the action (botconf-panel's
-- hybrid tokens). Rows expire after 30 days unless a redraw renews them;
-- botconf-panel purges expired rows itself, at most hourly.
CREATE TABLE panel_tokens (
    key TEXT PRIMARY KEY NOT NULL,
    action TEXT NOT NULL,
    expires_at INTEGER NOT NULL -- milliseconds since the Unix epoch
);
CREATE INDEX panel_tokens_expiry_idx ON panel_tokens (expires_at);
