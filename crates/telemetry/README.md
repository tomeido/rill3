# rill3-telemetry

Shared structured logging initialization for RILL3 processes.

`init` installs one process-wide JSON tracing subscriber. `RUST_LOG` controls filtering and defaults to `info,rill3=debug`; `RILL3_ENVIRONMENT` is recorded in the initialization event. Call it once, before starting the server, worker, or indexer. Secrets, OAuth tokens, signatures, donation messages, and raw provider payloads must not be attached to spans or events.

OpenTelemetry exporting is intentionally deferred until an exporter and retention policy are selected. The public API keeps that future wiring behind this crate.

