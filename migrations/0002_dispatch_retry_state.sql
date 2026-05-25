ALTER TABLE dispatch_commands ADD COLUMN attempt_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE dispatch_commands ADD COLUMN last_attempted_at_unix_ms INTEGER;
ALTER TABLE dispatch_commands ADD COLUMN next_attempt_at_unix_ms INTEGER;
ALTER TABLE dispatch_commands ADD COLUMN terminal_failure INTEGER NOT NULL DEFAULT 0
    CHECK (terminal_failure IN (0, 1));

