-- Rename "replication group" to "replica set" across the schema. The table and
-- the conversations.group_id column are renamed in place; the JSON payload
-- columns embed the old "group_id" key (Conversation, Message, and the Message
-- inside each dispatch command), so those keys are rewritten too. The ":" anchor
-- keeps the rewrite to the serialized key, not arbitrary string values.
ALTER TABLE replication_groups RENAME TO replica_sets;
ALTER TABLE conversations RENAME COLUMN group_id TO replica_set_id;

UPDATE conversations SET payload = replace(payload, '"group_id":', '"replica_set_id":');
UPDATE messages SET payload = replace(payload, '"group_id":', '"replica_set_id":');
UPDATE dispatches SET payload = replace(payload, '"group_id":', '"replica_set_id":');
