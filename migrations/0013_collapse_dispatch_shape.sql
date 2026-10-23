-- Rewrite stored dispatches into the collapsed Dispatch shape:
--
--   old: {"id", "command": {"target_endpoint", "target_connector",
--          "target": {"address", "create_thread": {"container", "title"}},
--          "payload": {<Action>: {...}}}, "state"}
--   new: {"id", "state", "to": {"endpoint_id", "connector_id", "address",
--          "create_thread": <title>}, "replica_set_id", "action": {<Action>: {...}}}
--
-- The replica set id moves from inside the message or action onto the
-- dispatch. A thread to create shrinks to its title, because its container is
-- always the target chat. Delete keeps only the message id, and the
-- DeliveryAck/SeenReceipt actions become one Receipt with a kind. Keys are
-- written in the order serde writes them, so a migrated row is byte-identical
-- to one the current model would write. The table's columns do not change.

UPDATE dispatches SET payload = json_object(
    'id', payload ->> '$.id',
    'state', payload -> '$.state',
    'to', json_object(
        'endpoint_id', payload ->> '$.command.target_endpoint',
        'connector_id', payload ->> '$.command.target_connector',
        'address', payload -> '$.command.target.address',
        'create_thread', payload ->> '$.command.target.create_thread.title'
    ),
    'replica_set_id', coalesce(
        payload ->> '$.command.payload.Send.message.replica_set_id',
        payload ->> '$.command.payload.Edit.message.replica_set_id',
        payload ->> '$.command.payload.Delete.message.replica_set_id',
        payload ->> '$.command.payload.React.replica_set_id',
        payload ->> '$.command.payload.DeliveryAck.replica_set_id',
        payload ->> '$.command.payload.SeenReceipt.replica_set_id',
        payload ->> '$.command.payload.RenameThread.replica_set_id'
    ),
    'action', CASE
        WHEN json_type(payload, '$.command.payload.Send') IS NOT NULL
            THEN json_object('Send', payload -> '$.command.payload.Send')
        WHEN json_type(payload, '$.command.payload.Edit') IS NOT NULL
            THEN json_object('Edit', payload -> '$.command.payload.Edit')
        WHEN json_type(payload, '$.command.payload.Delete') IS NOT NULL
            THEN json_object('Delete', json_object(
                'message_id', payload ->> '$.command.payload.Delete.message.id',
                'native_id', payload ->> '$.command.payload.Delete.native_id'
            ))
        WHEN json_type(payload, '$.command.payload.React') IS NOT NULL
            THEN json_object('React', json_object(
                'message_id', payload ->> '$.command.payload.React.message_id',
                'native_id', payload ->> '$.command.payload.React.native_id',
                'reactor', payload ->> '$.command.payload.React.reactor',
                'reactor_name', payload ->> '$.command.payload.React.reactor_name',
                'emoji', payload ->> '$.command.payload.React.emoji'
            ))
        WHEN json_type(payload, '$.command.payload.DeliveryAck') IS NOT NULL
            THEN json_object('Receipt', json_object(
                'message_id', payload ->> '$.command.payload.DeliveryAck.message_id',
                'native_id', payload ->> '$.command.payload.DeliveryAck.native_id',
                'kind', 'delivered'
            ))
        WHEN json_type(payload, '$.command.payload.SeenReceipt') IS NOT NULL
            THEN json_object('Receipt', json_object(
                'message_id', payload ->> '$.command.payload.SeenReceipt.message_id',
                'native_id', payload ->> '$.command.payload.SeenReceipt.native_id',
                'kind', 'read'
            ))
        WHEN json_type(payload, '$.command.payload.RenameThread') IS NOT NULL
            THEN json_object('RenameThread', json_object(
                'title', payload ->> '$.command.payload.RenameThread.title'
            ))
    END
)
WHERE json_type(payload, '$.command') IS NOT NULL;
