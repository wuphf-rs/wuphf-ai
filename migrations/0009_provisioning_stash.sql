-- Durable provisioning stash: messages received while their conversation's
-- mirror thread is still being provisioned are held here rather than only in
-- memory, so a crash cannot lose them. Each row is replayed once the provision
-- settles (acknowledged or failed). The message body lives in `messages` and is
-- joined back on replay; only the replay coordinates are denormalized here.

CREATE TABLE provisioning_stash (
    replica_set_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    origin_endpoint TEXT NOT NULL,
    provision_title TEXT NOT NULL,
    PRIMARY KEY (replica_set_id, message_id)
);
