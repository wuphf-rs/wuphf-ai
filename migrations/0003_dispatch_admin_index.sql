CREATE INDEX dispatch_terminal_lookup
    ON dispatch_commands(terminal_failure, target_connector_id, target_endpoint_id, endpoint_sequence);
