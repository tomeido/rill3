# RILL3 M0/M1 implementation plan

Scope: bootstrap the Rust workspace and deliver the external-stream discovery vertical slice only. This plan intentionally stops before creator authentication, wallets, payments, alerts, and media processing.

## Files and stages

1. **Decisions and boundaries**
   - Add `docs/ARCHITECTURE.md`, `docs/SECURITY.md`, `docs/PROVIDER_POLICY.md`, and ADRs 0001–0003.
   - Record the current milestone decisions in `DECISIONS.md`.
2. **Workspace and domain**
   - Add the root `Cargo.toml`/`Cargo.lock`, the `apps/rill3` binary, and bounded crates under `crates/`.
   - Model creators, channels, normalized live state, provider events, embed descriptors, provider health, and a future mock chain event.
3. **Persistence**
   - Add PostgreSQL migrations for `creators`, `external_channels`, `live_sessions`, and `provider_events`.
   - Implement readiness, bounded live snapshots, creator lookup, channel batching, idempotent event application, stale tracking, and advisory-lock helpers in `crates/db`.
4. **Providers**
   - Implement `StreamProvider`, deterministic mock and manual/link-only providers, Twitch Helix/reconciliation plus EventSub verification, YouTube manual live URL/WebSub handling, and the official CHZZK polling adapter.
   - Keep providers disabled when their credentials are absent and never fetch user-supplied arbitrary URLs.
5. **Server and worker**
   - Add Axum health/API/webhook routes, Askama pages, cache validators, security/request-ID middleware, graceful shutdown, and OpenAPI generation.
   - Add a provider-batched worker with a PostgreSQL advisory lock, timeouts, jitter, partial-failure isolation, and graceful shutdown. Keep `indexer` as a compiling boundary stub.
6. **Operations and verification**
   - Add Docker Compose, Caddy, environment examples, CI, a developer seed command, a small load-test script, and operating instructions.
   - Run formatting, clippy with warnings denied, all tests, migration checks, SQLx offline checks, and measurable asset/rendering budget checks.

## Validation commands

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo sqlx migrate run --source migrations
cargo sqlx prepare --workspace --check
docker compose -f deploy/compose.yml config
./tests/check-budgets.sh
```

Where Docker or an external provider credential is unavailable, the exact skipped command and reason will be reported. Provider behavior that does not require secrets is covered with deterministic local HTTP fixtures and mock-provider tests.

## Explicitly out of scope

- Creator login/studio, OAuth completion, wallet verification, public write/admin APIs
- Solidity deployment, on-chain payments, real indexing, TTS, moderation jobs, OBS/SSE overlays
- Uploaded or proxied video, streaming infrastructure, chat, multi-chain support
- Redis, NATS, Kafka, Kubernetes, a SPA, or a public-search polling loop

