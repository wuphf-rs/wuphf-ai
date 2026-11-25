-- Story expiries (plan 084): when the mirror copies of each story are due to
-- be deleted (24 hours after it was posted), if the owner turns
-- `stories.expire_copies` on. Recorded for every story as it arrives, so
-- turning the setting on also expires the stories already mirrored.
CREATE TABLE message_expiries (
    message_id TEXT PRIMARY KEY NOT NULL,
    expires_at_ms INTEGER NOT NULL
);

CREATE INDEX message_expiries_due ON message_expiries (expires_at_ms);
