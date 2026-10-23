-- The contacts and chat_settings tables were created in 0001 for features that
-- never shipped; nothing has ever written to them. Dropping them keeps the
-- schema in step with the Storage trait.
DROP TABLE IF EXISTS contacts;
DROP TABLE IF EXISTS chat_settings;
