-- Plan 038 changed the config shape of `rules` and `notify`. Rewrite runtime
-- overrides stored in the old shapes, so they still load instead of being
-- skipped as invalid.
--
-- A chat object (`connector_id` or a separate connector, plus `chat_id` and an
-- optional `thread_id`) becomes one string:
--
--   <connector> ':' esc(chat_id) [ '#' esc(thread_id) ]
--   where esc(x) = replace(replace(x, '%', '%25'), '#', '%23')
--
-- The expression is written out inline below wherever a chat is converted.
-- A `notify` list of objects becomes a list of strings.

UPDATE settings
SET value = (
    SELECT json_group_array(
        json_extract(t.value, '$.connector_id') || ':' ||
        replace(replace(json_extract(t.value, '$.chat_id'), '%', '%25'), '#', '%23') ||
        coalesce('#' || replace(replace(json_extract(t.value, '$.thread_id'), '%', '%25'), '#', '%23'), '')
    )
    FROM json_each(settings.value) AS t
)
WHERE key = 'notify' AND json_type(value) = 'array';

-- A `rules` list of `{id, flow}` becomes a map keyed by id. The list position
-- becomes `priority`, so the first-match order is kept exactly. Each flow's
-- fields move up to the entry, with chats as strings:
--   one-to-one:  source, destination
--   many-to-one: source_connector, destination, plus only_chats (from an
--                `only` filter) or except_chats (from a `not` filter), whose
--                chats are on source_connector; an `all` filter or none adds
--                neither.
-- No `rules.<id>` rows exist yet: before this migration `rules` was not a
-- per-entry setting.

UPDATE settings
SET value = (
    SELECT json_group_object(
        json_extract(rule.value, '$.id'),
        json(CASE json_extract(rule.value, '$.flow.kind')
            WHEN 'one-to-one' THEN json_object(
                'kind', 'one-to-one',
                'source',
                    json_extract(rule.value, '$.flow.source_connector') || ':' ||
                    replace(replace(json_extract(rule.value, '$.flow.source_chat.chat_id'), '%', '%25'), '#', '%23') ||
                    coalesce('#' || replace(replace(json_extract(rule.value, '$.flow.source_chat.thread_id'), '%', '%25'), '#', '%23'), ''),
                'destination',
                    json_extract(rule.value, '$.flow.destination_connector') || ':' ||
                    replace(replace(json_extract(rule.value, '$.flow.destination_chat.chat_id'), '%', '%25'), '#', '%23') ||
                    coalesce('#' || replace(replace(json_extract(rule.value, '$.flow.destination_chat.thread_id'), '%', '%25'), '#', '%23'), ''),
                'priority', rule.key
            )
            ELSE (
                SELECT CASE json_extract(rule.value, '$.flow.source_chats_filter.kind')
                    WHEN 'only' THEN json_set(base.entry, '$.only_chats', json(base.chats))
                    WHEN 'not' THEN json_set(base.entry, '$.except_chats', json(base.chats))
                    ELSE base.entry
                END
                FROM (
                    SELECT
                        json_object(
                            'kind', 'many-to-one',
                            'source_connector', json_extract(rule.value, '$.flow.source_connector'),
                            'destination',
                                json_extract(rule.value, '$.flow.destination_connector') || ':' ||
                                replace(replace(json_extract(rule.value, '$.flow.destination_chat.chat_id'), '%', '%25'), '#', '%23') ||
                                coalesce('#' || replace(replace(json_extract(rule.value, '$.flow.destination_chat.thread_id'), '%', '%25'), '#', '%23'), ''),
                            'priority', rule.key
                        ) AS entry,
                        (
                            SELECT json_group_array(
                                json_extract(rule.value, '$.flow.source_connector') || ':' ||
                                replace(replace(json_extract(chat.value, '$.chat_id'), '%', '%25'), '#', '%23') ||
                                coalesce('#' || replace(replace(json_extract(chat.value, '$.thread_id'), '%', '%25'), '#', '%23'), '')
                            )
                            FROM json_each(rule.value, '$.flow.source_chats_filter.chats') AS chat
                        ) AS chats
                ) AS base
            )
        END)
    )
    FROM json_each(settings.value) AS rule
)
WHERE key = 'rules' AND json_type(value) = 'array';
