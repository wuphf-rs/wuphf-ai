CREATE TABLE media_blobs (
    id TEXT PRIMARY KEY NOT NULL,
    metadata TEXT NOT NULL,
    contents BLOB NOT NULL
);
