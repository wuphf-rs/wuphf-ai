# Mock Client

This directory contains the Rust mock connector CLI used for the first MVP and
protocol testing. It exercises the same gRPC contract used by future real
platform clients and supports synthetic message submission, reply references,
automatic acknowledgement, and unacknowledged replay demonstrations.
Use `--ack-platform-id` when a stable mock-created platform ID is useful in a
reply walkthrough.
