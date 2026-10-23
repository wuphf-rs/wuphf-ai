-- Rename the model's stored JSON keys to one vocabulary: `native_id` for an
-- app's own message id, `author_id`/`author_name` for who wrote a message, and
-- short action verbs for the dispatch variants. Only JSON keys change; SQL
-- column names (e.g. `message_copies.platform_message_id`) stay as they are.
--
-- As in 0006, each key is matched with its quotes and the trailing ":", so only
-- serialized keys are rewritten: a string value is never followed by ":", and
-- quotes inside message text are escaped (\"), so user content cannot match.

-- Message: the row itself, and every Message embedded in a dispatch.
UPDATE messages SET payload =
    replace(replace(payload,
        '"sender":', '"author_id":'),
        '"sender_name":', '"author_name":');

-- MessageCopy.
UPDATE message_copies SET payload =
    replace(payload, '"platform_message_id":', '"native_id":');

-- Dispatch: the embedded Message and ReplyContext, the Acknowledged state, the
-- target's thread to create, and the Action variants and their fields.
UPDATE dispatches SET payload =
    replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(payload,
        '"sender":', '"author_id":'),
        '"sender_name":', '"author_name":'),
        '"platform_message_id":', '"native_id":'),
        '"target_platform_message_id":', '"native_id":'),
        '"reply_context":', '"reply":'),
        '"provision_thread":', '"create_thread":'),
        '"MessageCreate":', '"Send":'),
        '"MessageEdit":', '"Edit":'),
        '"MessageDelete":', '"Delete":'),
        '"RetitleThread":', '"RenameThread":'),
        '"Reaction":', '"React":');

-- ObservedMessage.
UPDATE observed_messages SET payload =
    replace(replace(replace(replace(payload,
        '"platform_message_id":', '"native_id":'),
        '"sender":', '"author_id":'),
        '"sender_name":', '"author_name":'),
        '"reply_to_platform_message_id":', '"reply_to_native_id":');
