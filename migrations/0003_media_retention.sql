ALTER TABLE media_blobs ADD COLUMN stored_at_ms INTEGER;

CREATE INDEX media_blobs_stored_at_idx ON media_blobs (stored_at_ms);

CREATE TABLE message_media (
    message_id TEXT NOT NULL,
    media_id TEXT NOT NULL,
    PRIMARY KEY (message_id, media_id)
);

CREATE INDEX message_media_media_idx ON message_media (media_id);
